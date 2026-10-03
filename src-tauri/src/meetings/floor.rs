//! Who holds the floor, from the conversation: in a standup someone hands
//! over by name ("Joseph, you're up") and that person talks until the next
//! hand-over. The meeting's language model reads the transcript line by
//! line and gives the stretches one named person said; each must quote the
//! words that name them (the hand-over, an introduction, a thank-you), word
//! for word from a line at or just before the stretch, or it's dropped.
//!
//! Unlike [`super::clues`], which vote a name onto a voice, a stretch names
//! the lines themselves, so it still helps when the voices merged several
//! people into one (a call's audio squeezes voices alike).

use super::clues::norm;
use super::llm::Llm;
use super::transcript::{Segment, Source};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const FILE: &str = "floor.json";
/// Lines before a stretch its quote may come from (the hand-over).
const CUE_BEFORE: usize = 3;
/// Characters of transcript per request.
const BUDGET: usize = 30_000;

/// A line said by the named person.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    pub source: Source,
    pub start_ms: u64,
    pub end_ms: u64,
    pub name: String,
    /// The name as the transcript has it, when the model matched it to
    /// someone invited.
    #[serde(default)]
    pub said: String,
}

const INSTRUCTIONS: &str = "You read a meeting transcript, one numbered line per stretch of speech, and work out who said which lines. The speaker labels are a voice-matching guess and are often wrong: one label can be several people whose voices sound alike, so don't trust them.\n\nFind stretches of consecutive lines said by one person you can name from what is said: someone hands over by name (\"Joseph, you're up\", \"Paul, go ahead\"), so the lines that follow are that person's until someone else talks; someone introduces themself (\"Sam here\"); someone is asked by name and the next lines answer; someone is thanked by name for what they just said. Standups and round-robins are full of these.\n\nSpeech recognition often mishears names. When you're given the people invited, `name` is the invited person meant, written as in that list, and `said` is the name as the transcript has it; otherwise both are the name as said. The user's own lines are labelled \"Me\"; never give the user's name to lines labelled otherwise.\n\nFor each stretch give the first and last line numbers, `name`, `said`, `cue_line`: the number of the line where the name is said, and `quote`: a short quote copied exactly from that line containing the name. A stretch ends where someone else speaks, even briefly (a question, a \"thanks\"); leave those lines out. Leave out lines you can't place. Don't guess: only stretches the conversation makes clear. An empty list is fine.";

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "stretches": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "first": { "type": "integer" },
                        "last": { "type": "integer" },
                        "name": { "type": "string" },
                        "said": { "type": "string" },
                        "cue_line": { "type": "integer" },
                        "quote": { "type": "string" }
                    },
                    "required": ["first", "last", "name", "said", "cue_line", "quote"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["stretches"],
        "additionalProperties": false
    })
}

/// The lines the model sees: the transcript's segments with words, in time
/// order (echo left out).
pub fn lines(segments: &[Segment]) -> Vec<&Segment> {
    let mut out: Vec<&Segment> = segments
        .iter()
        .filter(|s| !s.echo && !s.text.trim().is_empty())
        .collect();
    out.sort_by_key(|s| (s.start_ms, s.source == Source::System));
    out
}

/// Turn the model's reply into named lines. `numbers` maps the batch's line
/// numbers to indices into `lines`.
pub fn from_reply(reply: &Value, lines: &[&Segment], numbers: &[usize]) -> Vec<Turn> {
    let mut out = Vec::new();
    let Some(items) = reply["stretches"].as_array() else {
        return out;
    };
    for item in items {
        let (Some(first), Some(last), Some(name), Some(cue), Some(quote)) = (
            item["first"].as_u64(),
            item["last"].as_u64(),
            item["name"].as_str(),
            item["cue_line"].as_u64(),
            item["quote"].as_str(),
        ) else {
            continue;
        };
        let said = item["said"].as_str().unwrap_or(name);
        let (first, last, cue) = (first as usize, last as usize, cue as usize);
        if last < first || last >= numbers.len() || cue > last || cue + CUE_BEFORE < first {
            continue;
        }
        let (q, name_n) = (norm(quote), norm(said));
        if q.is_empty()
            || name_n.is_empty()
            || !norm(&lines[numbers[cue]].text).contains(&q)
            || !format!(" {q} ").contains(&format!(" {name_n} "))
        {
            continue;
        }
        let name = name.trim().to_string();
        out.extend(numbers[first..=last].iter().map(|&i| Turn {
            source: lines[i].source,
            start_ms: lines[i].start_ms,
            end_ms: lines[i].end_ms,
            name: name.clone(),
            said: said.trim().to_string(),
        }));
    }
    out
}

/// Ask the model who holds the floor where; `label` is how a line's
/// speaker is shown, `invited` who the calendar says was asked (with the
/// user's own name, when known).
pub async fn find(
    llm: &Llm,
    segments: &[Segment],
    label: &(dyn Fn(&Segment) -> String + Sync),
    invited: &[String],
    me: Option<&str>,
    effort: &str,
) -> Result<Vec<Turn>, String> {
    let mut head = String::new();
    if !invited.is_empty() {
        head.push_str(&format!("People invited: {}.\n", invited.join(", ")));
    }
    if let Some(me) = me {
        head.push_str(&format!("The user (\"Me\") is {me}.\n"));
    }
    if !head.is_empty() {
        head.push('\n');
    }
    let lines = lines(segments);
    let schema = schema();
    let mut batches: Vec<Vec<usize>> = vec![Vec::new()];
    let mut size = 0;
    for (i, l) in lines.iter().enumerate() {
        let len = l.text.len() + 20;
        if size + len > BUDGET && !batches.last().is_some_and(Vec::is_empty) {
            batches.push(Vec::new());
            size = 0;
        }
        size += len;
        batches.last_mut().map(|b| b.push(i));
    }
    let requests = batches.into_iter().filter(|b| !b.is_empty()).map(|batch| {
        let input: String = head.clone()
            + &batch
                .iter()
                .enumerate()
                .map(|(n, &i)| format!("[{n}] {}: {}\n", label(lines[i]), lines[i].text.trim()))
                .collect::<String>();
        let schema = &schema;
        async move {
            (
                llm.ask_json(INSTRUCTIONS, &input, schema, effort).await,
                batch,
            )
        }
    });
    let mut out = Vec::new();
    let mut replies = futures_util::stream::iter(requests).buffered(3);
    while let Some((reply, batch)) = replies.next().await {
        out.extend(from_reply(&reply?, &lines, &batch));
    }
    out.sort_by_key(|t| (t.start_ms, t.source == Source::System));
    out.dedup_by(|a, b| a.source == b.source && a.start_ms == b.start_ms);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_s: u64, speaker: u32, text: &str) -> Segment {
        Segment {
            source: Source::System,
            start_ms: start_s * 1000,
            end_ms: (start_s + 10) * 1000,
            text: text.into(),
            echo: false,
            speaker: Some(speaker),
        }
    }

    #[test]
    fn stretches_need_a_quote_naming_them_at_or_just_before() {
        let segments = vec![
            seg(0, 100, "Okay, Joseph, you're up."),
            seg(10, 100, "Thanks. Yesterday I fixed the batching."),
            seg(20, 100, "Today the cache."),
            seg(30, 101, "Cool."),
        ];
        let lines = lines(&segments);
        let numbers: Vec<usize> = (0..lines.len()).collect();
        let reply = json!({ "stretches": [
            { "first": 1, "last": 2, "name": "Joseph", "cue_line": 0, "quote": "Joseph, you're up" },
            // The quote isn't in the cue line.
            { "first": 3, "last": 3, "name": "Paul", "cue_line": 3, "quote": "Paul, go" },
            // The quote doesn't name them.
            { "first": 3, "last": 3, "name": "Sam", "cue_line": 3, "quote": "Cool" },
        ]});
        let misheard = json!({ "stretches": [
            { "first": 1, "last": 1, "name": "Josephine", "said": "Joseph", "cue_line": 0, "quote": "Joseph, you're up" },
        ]});
        let turns_misheard = from_reply(&misheard, &lines, &numbers);
        assert_eq!(turns_misheard.len(), 1);
        assert_eq!(turns_misheard[0].name, "Josephine");
        let turns = from_reply(&reply, &lines, &numbers);
        assert_eq!(turns.len(), 2);
        assert!(turns.iter().all(|t| t.name == "Joseph"));
        assert_eq!(turns[0].start_ms, 10_000);
        assert_eq!(turns[1].start_ms, 20_000);
    }
}
