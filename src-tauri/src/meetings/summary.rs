//! After a meeting is transcribed: tidy the transcript with the LLM, write
//! the summary from the transcript and the user's own notes, and export it
//! all as Markdown.
//!
//! Cleanup sends paragraphs with ids and gets the same ids back, so the model
//! never touches timestamps or speakers. A paragraph whose cleaned text looks
//! wrong (much shorter or longer, or missing) keeps its raw text.

use super::llm::Llm;
use super::transcript::{timestamp, Paragraph, Source};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use specta::Type;
use std::collections::BTreeMap;
use std::path::Path;

pub const CLEANED_FILE: &str = "cleaned.json";
pub const SUMMARY_FILE: &str = "summary.json";

/// Cleaned paragraph texts, keyed by [`paragraph_key`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Cleaned {
    pub texts: BTreeMap<String, String>,
}

pub fn paragraph_key(p: &Paragraph) -> String {
    let source = match p.source {
        Source::Mic => "mic",
        Source::System => "system",
    };
    format!("{source}-{}", p.start_ms)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, Type)]
pub struct ActionItem {
    pub task: String,
    /// Who does it, if said; empty otherwise. "Me" is the user.
    pub owner: String,
    /// Where in the meeting it came up.
    pub at_ms: Option<u64>,
    /// When it's due, in the meeting's words ("by Friday"); empty if no one said.
    #[serde(default)]
    pub due: String,
    /// What was said when it was taken on.
    #[serde(default)]
    pub quote: String,
    /// "I'll try to", "maybe I can": taken on, but not firmly.
    #[serde(default)]
    pub tentative: bool,
}

/// `summary.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
pub struct Summary {
    pub title: String,
    pub overview: String,
    pub key_points: Vec<String>,
    pub decisions: Vec<String>,
    pub action_items: Vec<ActionItem>,
    /// Which model wrote it.
    pub model: String,
    /// Unix milliseconds.
    pub generated_at: i64,
}

pub fn load_json<T: serde::de::DeserializeOwned>(dir: &Path, file: &str) -> Option<T> {
    serde_json::from_slice(&std::fs::read(dir.join(file)).ok()?).ok()
}

pub fn save_json<T: Serialize>(dir: &Path, file: &str, value: &T) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("{file}.tmp"));
    std::fs::write(&tmp, json)
        .and_then(|_| std::fs::rename(&tmp, dir.join(file)))
        .map_err(|e| format!("Couldn't save {file}: {e}"))
}

/// A transcript's text with the user's word corrections, as dictation
/// makes them: vocabulary spellings, taught words, replacements.
pub fn corrected(text: &str, settings: &crate::settings::AppSettings) -> String {
    let text = crate::vocabulary::apply_canonical_forms(text, &settings.custom_words);
    let text = crate::vocab_teach::apply_taught_rules(&text, settings);
    crate::scratchpad::apply_text_replacements(&text, &settings.text_replacements)
}

fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Whether a cleaned paragraph can replace the raw one: cleanup removes
/// fillers and false starts, so it may shrink a fair bit, but a model that
/// summarises or invents shows up as a big change in length.
pub fn accept_cleaned(raw: &str, cleaned: &str) -> bool {
    let (before, after) = (word_count(raw), word_count(cleaned));
    if after == 0 {
        // Only allowed for a paragraph that was nothing but filler.
        return before <= 3;
    }
    if before < 6 {
        return after <= before + 2;
    }
    let ratio = after as f32 / before as f32;
    (0.4..=1.25).contains(&ratio)
}

/// Group paragraph indices into requests of about `budget` characters.
pub fn batches(paragraphs: &[Paragraph], budget: usize) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut size = 0;
    for (i, p) in paragraphs.iter().enumerate() {
        let len = p.text.len() + 40;
        if out.is_empty() || (size + len > budget && !out.last().unwrap().is_empty()) {
            out.push(Vec::new());
            size = 0;
        }
        out.last_mut().unwrap().push(i);
        size += len;
    }
    out
}

const CLEANUP_INSTRUCTIONS: &str = "You tidy a meeting transcript made by speech recognition. \
Each paragraph has an id and a speaker. Return every paragraph with the same id and its cleaned text.

- Remove filler words (um, uh, you know, filler \"like\"), stutters, repeated words and false starts.
- Fix punctuation, capitalisation and obvious recognition mistakes.
- Keep the speaker's own words, meaning and tone. Don't summarise, paraphrase, merge or split paragraphs, and don't add anything.
- If a paragraph is nothing but filler, return an empty text for it.";

fn cleanup_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "paragraphs": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": {"type": "string"},
                        "text": {"type": "string"}
                    },
                    "required": ["id", "text"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["paragraphs"],
        "additionalProperties": false
    })
}

fn with_vocabulary(
    instructions: &str,
    vocabulary: &[String],
    soundalikes: &[crate::settings::Soundalike],
) -> String {
    let mut out = instructions.to_string();
    if !vocabulary.is_empty() {
        out.push_str(&format!(
            "\n\nSpell these names and terms exactly like this: {}.",
            vocabulary.join(", ")
        ));
    }
    // What dictation decides with its local model, cleanup decides from
    // the conversation.
    for s in soundalikes {
        out.push_str(&format!(
            "\n- \"{}\" may be speech recognition mishearing {} ({}): write {} where that's what's meant, and leave the ordinary word otherwise.",
            s.heard, s.word, s.meaning, s.word
        ));
    }
    out
}

/// Meeting cleanup's reasoning effort, with [`Llm::careful`]'s model.
pub const CLEANUP_EFFORT: &str = "medium";

/// Clean the paragraphs `todo` (indices into `paragraphs`) and add the
/// accepted results to `cleaned`. A batch that fails keeps its raw text; the
/// first error is returned so it can be shown.
pub async fn clean(
    llm: &Llm,
    paragraphs: &[Paragraph],
    label: &(dyn Fn(&Paragraph) -> String + Sync),
    vocabulary: &[String],
    soundalikes: &[crate::settings::Soundalike],
    cleaned: &mut Cleaned,
    mut progress: impl FnMut(usize, usize),
) -> Option<String> {
    let careful = llm.careful();
    let llm = careful.as_ref().unwrap_or(llm);
    let instructions = with_vocabulary(CLEANUP_INSTRUCTIONS, vocabulary, soundalikes);
    let schema = cleanup_schema();
    let todo: Vec<usize> = (0..paragraphs.len())
        .filter(|&i| !cleaned.texts.contains_key(&paragraph_key(&paragraphs[i])))
        .collect();
    let subset: Vec<Paragraph> = todo.iter().map(|&i| paragraphs[i].clone()).collect();
    let groups = batches(&subset, llm.budget().cleanup);
    let total = groups.len();
    let requests = groups.into_iter().map(|group| {
        let items: Vec<Value> = group
            .iter()
            .map(|&j| {
                json!({"id": format!("p{j}"), "speaker": label(&subset[j]), "text": subset[j].text})
            })
            .collect();
        let input = json!({ "paragraphs": items }).to_string();
        let (instructions, schema) = (&instructions, &schema);
        async move {
            let reply = llm
                .ask_json(instructions, &input, schema, CLEANUP_EFFORT)
                .await;
            (group, reply)
        }
    });
    let mut stream = futures_util::stream::iter(requests).buffered(3);
    let mut done = 0;
    let mut first_error = None;
    progress(0, total);
    while let Some((group, reply)) = stream.next().await {
        done += 1;
        progress(done, total);
        let reply = match reply {
            Ok(r) => r,
            Err(e) => {
                log::warn!("Meeting cleanup batch failed: {e}");
                first_error.get_or_insert(e);
                continue;
            }
        };
        let answers: BTreeMap<String, String> = reply["paragraphs"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| {
                Some((
                    p["id"].as_str()?.to_string(),
                    p["text"].as_str()?.to_string(),
                ))
            })
            .collect();
        for j in group {
            let p = &subset[j];
            let text = match answers.get(&format!("p{j}")) {
                Some(t) if accept_cleaned(&p.text, t) => t.trim().to_string(),
                Some(t) => {
                    log::debug!(
                        "Kept the raw text of a meeting paragraph ({} → {} words)",
                        word_count(&p.text),
                        word_count(t)
                    );
                    p.text.clone()
                }
                None => p.text.clone(),
            };
            cleaned.texts.insert(paragraph_key(p), text);
        }
    }
    first_error
}

/// The paragraphs with cleaned text where there is one; `raw` keeps the
/// original when it differs.
pub fn apply_cleaned(paragraphs: Vec<Paragraph>, cleaned: Option<&Cleaned>) -> Vec<Paragraph> {
    let Some(cleaned) = cleaned else {
        return paragraphs;
    };
    paragraphs
        .into_iter()
        .filter_map(|mut p| {
            match cleaned.texts.get(&paragraph_key(&p)) {
                Some(text) if text.is_empty() => return None,
                Some(text) if *text != p.text => {
                    p.raw = Some(std::mem::replace(&mut p.text, text.clone()));
                }
                _ => {}
            }
            Some(p)
        })
        .collect()
}

pub(super) const SUMMARY_FRAME: &str = "You write the notes for a meeting from its transcript and the notes the user typed during it. \
The user is \"Me\" in the transcript. Their notes show what they cared about: build on them, keep their points \
(and their wording where it works), and add what they missed. Transcript lines start with [m:ss] timestamps. \
An action item is the user's (\"Me\") only if they committed to it themselves; \"we\" said on the user's side means \
the user's side, not everyone. \
Write in the language of the meeting. Don't invent anything that isn't in the transcript or the user's notes.";

pub const DEFAULT_SUMMARY_GUIDANCE: &str = "- Title: short and specific, at most 8 words.
- Overview: 2 to 4 sentences on what the meeting was about and where it landed.
- Key points: the substance, in a sensible order, one concise bullet each. Keep names, numbers and dates.
- Decisions: only what was actually decided.
- Action items: tasks someone took on or was asked to do. Give the owner if it's clear (\"Me\" for the user) and the [m:ss] where it came up.";

/// What makes an action item, for finding them in a transcript. They get
/// their own pass over the transcript itself: a summary loses the small
/// ones, the numbers and the conditional ones.
const ACTIONS_FRAME: &str = "You find the action items in a meeting transcript: every commitment someone made to do something after the meeting. \
The user is \"Me\" in the transcript. Transcript lines start with [m:ss] timestamps.

What counts:
- Someone taking a task on: \"I'll…\", \"I can…\", \"let me…\", \"I'll try to…\", or being asked (\"can you…?\", \"X, please…\") and agreeing or not objecting.
- Small ones said in passing count, and so do tentative ones (\"I'll try\", \"maybe I can\": mark them tentative) and conditional ones (put the condition in the task: \"If the p99 is bad, open a ticket\").
- \"We should…\", \"it'd be good to…\", \"what if…\" with no one taking it on is an idea, not an action item. Leave it out.
- Leave out topics (\"pricing\"), things done during the meeting itself, and things the meeting decided not to do.

How to write each one:
- task: starts with a verb and names the concrete thing to deliver. Keep the specifics: names, numbers, systems, who it's for (\"Send Kevin the rack power figures for the Helios site\", not \"Follow up on power\").
- owner: who took it on, as labelled in the transcript, \"Me\" for the user. If it was asked of someone and they agreed, it's theirs. \"We\" on the user's side is \"Me\" only if the user is the one doing it. If you can't tell who will do it, leave the owner empty. Never guess from someone's role.
- due: copy when it's due as said (\"by Friday\", \"before the next call\"). Empty if no one said. Never make one up.
- quote: the words where it was taken on, short and verbatim from the transcript.
- at: the [m:ss] of that line.
- If the same task comes up more than once, list it once with the final wording, owner and due date.

An empty list is a fine answer. Don't pad.";

fn candidate_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"action_items": {"type": "array", "items": item_schema()}},
        "required": ["action_items"],
        "additionalProperties": false
    })
}

fn item_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "task": {"type": "string"},
            "owner": {"type": "string", "description": "Who does it, or empty"},
            "due": {"type": "string", "description": "When it's due as said, or empty"},
            "quote": {"type": "string", "description": "Where it was taken on, verbatim, or empty"},
            "tentative": {"type": "boolean"},
            "at": {"type": "string", "description": "m:ss where it came up, or empty"}
        },
        "required": ["task", "owner", "due", "quote", "tentative", "at"],
        "additionalProperties": false
    })
}

pub(super) fn summary_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string"},
            "overview": {"type": "string"},
            "key_points": {"type": "array", "items": {"type": "string"}},
            "decisions": {"type": "array", "items": {"type": "string"}},
            "action_items": {"type": "array", "items": item_schema()}
        },
        "required": ["title", "overview", "key_points", "decisions", "action_items"],
        "additionalProperties": false
    })
}

fn section_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "points": {"type": "array", "items": {"type": "string"}},
            "decisions": {"type": "array", "items": {"type": "string"}},
            "action_items": {"type": "array", "items": item_schema()}
        },
        "required": ["points", "decisions", "action_items"],
        "additionalProperties": false
    })
}

/// `m:ss` or `h:mm:ss` (optionally in brackets) to milliseconds.
pub fn parse_timestamp(text: &str) -> Option<u64> {
    let text = text.trim().trim_start_matches('[').trim_end_matches(']');
    let parts: Vec<u64> = text
        .split(':')
        .map(|p| p.trim().parse().ok())
        .collect::<Option<_>>()?;
    let secs = match parts[..] {
        [m, s] if s < 60 => m * 60 + s,
        [h, m, s] if m < 60 && s < 60 => h * 3600 + m * 60 + s,
        _ => return None,
    };
    Some(secs * 1000)
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn action_items(v: &Value) -> Vec<ActionItem> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let task = item["task"].as_str()?.trim().to_string();
            (!task.is_empty()).then(|| ActionItem {
                task,
                owner: item["owner"].as_str().unwrap_or("").trim().to_string(),
                at_ms: item["at"].as_str().and_then(parse_timestamp),
                due: item["due"].as_str().unwrap_or("").trim().to_string(),
                quote: item["quote"].as_str().unwrap_or("").trim().to_string(),
                tentative: item["tentative"].as_bool().unwrap_or(false),
            })
        })
        .collect()
}

pub fn parse_summary(v: &Value) -> Result<Summary, String> {
    let summary = Summary {
        title: v["title"].as_str().unwrap_or("").trim().to_string(),
        overview: v["overview"].as_str().unwrap_or("").trim().to_string(),
        key_points: strings(&v["key_points"]),
        decisions: strings(&v["decisions"]),
        action_items: action_items(&v["action_items"]),
        model: String::new(),
        generated_at: 0,
    };
    if summary.overview.is_empty() && summary.key_points.is_empty() {
        return Err("The model's summary was empty".into());
    }
    Ok(summary)
}

/// Split transcript lines into sections of about `budget` characters.
pub fn sections(lines: &[String], budget: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in lines {
        match out.last_mut() {
            Some(cur) if cur.len() + line.len() < budget => {
                cur.push('\n');
                cur.push_str(line);
            }
            _ => out.push(line.clone()),
        }
    }
    out
}

/// Below this many characters of transcript, the notes stay as short as the
/// meeting was instead of padding out every section.
pub const SHORT_TRANSCRIPT_CHARS: usize = 200;

/// What the summariser is told beyond the meeting itself.
#[derive(Default)]
pub struct SummaryContext {
    /// The last meeting of the same name: its overview and open actions.
    pub previous: Option<String>,
    /// The Mac is set to British (or Commonwealth) English.
    pub british: bool,
}

/// Locales that spell the British way.
pub fn spells_british(locale: &str) -> bool {
    let l = locale.replace('_', "-").to_lowercase();
    ["en-gb", "en-au", "en-nz", "en-ie", "en-za", "en-in"]
        .iter()
        .any(|p| l == *p || l.starts_with(&format!("{p}-")))
}

/// The previous meeting in a series, as the summariser reads it.
pub fn previous_block(title: &str, when: &str, previous: &Summary) -> String {
    let mut text = format!("{title} ({when}): {}", previous.overview.trim());
    let open: Vec<String> = previous
        .action_items
        .iter()
        .map(|a| {
            if a.owner.is_empty() {
                a.task.clone()
            } else {
                format!("{} ({})", a.task, a.owner)
            }
        })
        .collect();
    if !open.is_empty() {
        text.push_str(&format!("\nAction items from then: {}", open.join("; ")));
    }
    text
}

const CHECK_FRAME: &str = "You check a list of possible action items from a meeting against what was said. \
The user is \"Me\". Each candidate comes with the part of the transcript around it.

For each candidate:
- Keep it only if someone really took it on or agreed to it. Drop ideas no one took on (\"we should…\"), topics, and anything the excerpt shows was done during the meeting or dropped.
- Fix the owner if the excerpt shows someone else took it on. Leave it empty if it's unclear; never guess.
- Keep due only if it was said. Keep the quote verbatim.
- Merge candidates that are the same task, keeping the final wording, owner and due date and the earliest [m:ss].

Return the action items that survive. Keep the task wording concrete. An empty list is a fine answer.";

/// The `at` of a transcript line (`[m:ss] Who: text`).
fn line_ms(line: &str) -> Option<u64> {
    let end = line.find(']')?;
    parse_timestamp(&line[..=end])
}

/// The transcript around `at_ms`: a few lines before it and more after, to
/// see who took it on and whether it was finished before the meeting ended.
fn excerpt(lines: &[String], at_ms: Option<u64>) -> String {
    let Some(at) = at_ms else {
        return "(no timestamp)".into();
    };
    let i = lines
        .iter()
        .rposition(|l| line_ms(l).is_some_and(|ms| ms <= at))
        .unwrap_or(0);
    lines[i.saturating_sub(4)..(i + 9).min(lines.len())].join("\n")
}

fn describe(a: &ActionItem) -> String {
    let at = a.at_ms.map(timestamp).unwrap_or_default();
    format!(
        "task: {}\nowner: {}\ndue: {}\ntentative: {}\nquote: {}\nat: {at}",
        a.task, a.owner, a.due, a.tentative, a.quote
    )
}

/// Every commitment in the meeting: found in the transcript part by part,
/// then each checked against the lines around it.
pub async fn find_action_items(
    llm: &Llm,
    about: &str,
    notes: &str,
    lines: &[String],
) -> Result<Vec<ActionItem>, String> {
    let parts = sections(lines, llm.budget().summary);
    let total = parts.len();
    let schema = candidate_schema();
    let mut candidates = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let heading = if total > 1 {
            format!("Transcript, part {} of {total}", i + 1)
        } else {
            "Transcript".to_string()
        };
        let input = format!("# Meeting\n{about}\n\n# My notes\n{notes}\n\n# {heading}\n{part}");
        let v = llm
            .ask_json(ACTIONS_FRAME, &input, &schema, "medium")
            .await?;
        candidates.extend(action_items(&v["action_items"]));
    }
    if candidates.is_empty() {
        return Ok(candidates);
    }
    let listed: Vec<String> = candidates
        .iter()
        .enumerate()
        .map(|(i, a)| {
            format!(
                "## Candidate {}\n{}\n\nTranscript around it:\n{}",
                i + 1,
                describe(a),
                excerpt(lines, a.at_ms)
            )
        })
        .collect();
    let input = format!("# Meeting\n{about}\n\n{}", listed.join("\n\n"));
    let v = llm.ask_json(CHECK_FRAME, &input, &schema, "medium").await?;
    let mut checked = action_items(&v["action_items"]);
    checked.sort_by_key(|a| a.at_ms.unwrap_or(u64::MAX));
    Ok(checked)
}

/// Action items the transcript gave the user's name (in person, where their
/// voice is labelled with it) are "Me", like the rest of theirs.
pub fn own_items_as_me(summary: &mut Summary, me: Option<&str>) {
    let Some(me) = me.map(str::trim).filter(|m| !m.is_empty()) else {
        return;
    };
    for a in &mut summary.action_items {
        if a.owner.trim().eq_ignore_ascii_case(me) {
            a.owner = "Me".into();
        }
    }
}

/// Write the summary. `about` describes the meeting (date, length, who's
/// who); `lines` is the transcript, one `[m:ss] Speaker: text` per line.
/// Long meetings are noted section by section first, then summarised from
/// those notes.
pub async fn summarize(
    llm: &Llm,
    guidance: &str,
    about: &str,
    notes: &str,
    lines: &[String],
    context: &SummaryContext,
    mut progress: impl FnMut(usize, usize),
) -> Result<Summary, String> {
    let guidance = if guidance.trim().is_empty() {
        DEFAULT_SUMMARY_GUIDANCE
    } else {
        guidance
    };
    let mut instructions = format!("{SUMMARY_FRAME}\n\n{guidance}");
    let spoken: usize = lines.iter().map(|l| l.len()).sum();
    if spoken < SHORT_TRANSCRIPT_CHARS {
        instructions.push_str("\n\nThe transcript is very short. Keep the notes as short as it is: leave out any section it has nothing for, and don't pad.");
    }
    if context.previous.is_some() {
        instructions.push_str("\n\nThe previous meeting of the same name is given for context. Use it to follow up (what's changed, which earlier action items came up), but only note what was said in this meeting.");
    }
    if context.british {
        instructions.push_str("\n\nUse British spelling.");
    }
    // The action items are this meeting's alone, so not the previous one's.
    let this_meeting = about;
    let about = match &context.previous {
        Some(p) => format!("{about}\n\n# Previous meeting\n{p}"),
        None => about.to_string(),
    };
    let notes_block = if notes.trim().is_empty() {
        "(The user didn't type any notes.)".to_string()
    } else {
        notes.trim().to_string()
    };
    let parts = sections(lines, llm.budget().summary);
    let transcript_block = if parts.len() <= 1 {
        parts.into_iter().next().unwrap_or_default()
    } else {
        let total = parts.len();
        let section_instructions = format!(
            "{SUMMARY_FRAME}\n\nThis is one part of a longer meeting. Note everything from it that the final notes need: \
the points made, decisions, and action items with owners and [m:ss] timestamps. Be complete but brief."
        );
        let schema = section_schema();
        let mut noted = Vec::new();
        progress(0, total + 1);
        for (i, part) in parts.iter().enumerate() {
            let input = format!(
                "# Meeting\n{about}\n\n# My notes\n{notes_block}\n\n# Transcript, part {} of {total}\n{part}",
                i + 1
            );
            let v = llm
                .ask_json(&section_instructions, &input, &schema, "low")
                .await?;
            let mut text = format!("## Part {} of {total}\n", i + 1);
            for p in strings(&v["points"]) {
                text.push_str(&format!("- {p}\n"));
            }
            for d in strings(&v["decisions"]) {
                text.push_str(&format!("- Decided: {d}\n"));
            }
            for a in action_items(&v["action_items"]) {
                text.push_str(&format!(
                    "- To do: {}{}{}\n",
                    a.task,
                    if a.owner.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", a.owner)
                    },
                    a.at_ms
                        .map(|ms| format!(" [{}]", timestamp(ms)))
                        .unwrap_or_default()
                ));
            }
            noted.push(text);
            progress(i + 1, total + 1);
        }
        format!(
            "(The meeting was too long to send whole; these are notes on each part, in order.)\n\n{}",
            noted.join("\n")
        )
    };
    let input = format!(
        "# Meeting\n{about}\n\n# My notes\n{notes_block}\n\n# Transcript\n{transcript_block}"
    );
    let schema = summary_schema();
    let (v, actions) = futures_util::join!(
        llm.ask_json(&instructions, &input, &schema, "medium"),
        find_action_items(llm, this_meeting, &notes_block, lines)
    );
    let mut summary = parse_summary(&v?)?;
    // The summary's own list stays only if the closer look failed.
    match actions {
        Ok(actions) => summary.action_items = actions,
        Err(e) => log::warn!("Meeting action items: {e}; keeping the summary's own"),
    }
    summary.model = llm.label();
    summary.generated_at = chrono::Utc::now().timestamp_millis();
    Ok(summary)
}

/// The meeting as Markdown, for copying.
pub fn to_markdown(
    title: &str,
    meta: &str,
    summary: Option<&Summary>,
    notes: &str,
    transcript: &[String],
) -> String {
    let mut md = format!("# {title}\n{meta}\n");
    if let Some(s) = summary {
        if !s.overview.is_empty() {
            md.push_str(&format!("\n## Summary\n{}\n", s.overview));
        }
        let list = |md: &mut String, heading: &str, items: &[String]| {
            if !items.is_empty() {
                md.push_str(&format!("\n### {heading}\n"));
                for i in items {
                    md.push_str(&format!("- {i}\n"));
                }
            }
        };
        list(&mut md, "Key points", &s.key_points);
        list(&mut md, "Decisions", &s.decisions);
        if !s.action_items.is_empty() {
            md.push_str("\n## Action items\n");
            for a in &s.action_items {
                md.push_str(&format!("- [ ] {}", a.task));
                if !a.owner.is_empty() {
                    md.push_str(&format!(" — {}", a.owner));
                }
                if !a.due.is_empty() {
                    md.push_str(if a.owner.is_empty() { " — " } else { ", " });
                    md.push_str(&a.due);
                }
                if a.tentative {
                    md.push_str(" (tentative)");
                }
                if let Some(ms) = a.at_ms {
                    md.push_str(&format!(" ([{}])", timestamp(ms)));
                }
                md.push('\n');
            }
        }
    }
    if !notes.trim().is_empty() {
        md.push_str(&format!("\n## My notes\n{}\n", notes.trim()));
    }
    if !transcript.is_empty() {
        md.push_str("\n## Transcript\n");
        for line in transcript {
            md.push_str(line);
            md.push_str("\n\n");
        }
    }
    md.trim_end().to_string() + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(source: Source, start_s: u64, text: &str) -> Paragraph {
        Paragraph {
            source,
            start_ms: start_s * 1000,
            end_ms: start_s * 1000 + 3000,
            text: text.into(),
            raw: None,
            speaker: None,
        }
    }

    #[test]
    fn cleanup_that_rewrites_too_much_is_refused() {
        let raw = "um so I I think we should uh ship the the beta on Friday if the tests pass";
        assert!(accept_cleaned(
            raw,
            "I think we should ship the beta on Friday if the tests pass."
        ));
        // A summary instead of a cleanup.
        assert!(!accept_cleaned(raw, "Ship Friday."));
        // Made up extra content.
        assert!(!accept_cleaned(
            "we ship friday",
            "We ship on Friday, as agreed with the whole team last week in the planning meeting."
        ));
        assert!(accept_cleaned("um uh", ""));
        assert!(!accept_cleaned(raw, ""));
    }

    #[test]
    fn batches_respect_the_budget_but_never_split_a_paragraph() {
        let ps = vec![
            para(Source::Mic, 0, &"a".repeat(100)),
            para(Source::Mic, 5, &"b".repeat(100)),
            para(Source::Mic, 9, &"c".repeat(500)),
        ];
        assert_eq!(batches(&ps, 300), vec![vec![0, 1], vec![2]]);
        assert_eq!(batches(&ps, 10), vec![vec![0], vec![1], vec![2]]);
    }

    #[test]
    fn cleaned_text_replaces_raw_and_filler_paragraphs_disappear() {
        let ps = vec![
            para(Source::Mic, 0, "um so hi everyone"),
            para(Source::System, 4, "uh"),
            para(Source::Mic, 8, "next"),
        ];
        let mut cleaned = Cleaned::default();
        cleaned.texts.insert("mic-0".into(), "Hi everyone.".into());
        cleaned.texts.insert("system-4000".into(), "".into());
        let out = apply_cleaned(ps, Some(&cleaned));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].text, "Hi everyone.");
        assert_eq!(out[0].raw.as_deref(), Some("um so hi everyone"));
        assert_eq!(out[1].text, "next");
        assert_eq!(out[1].raw, None);
    }

    #[test]
    fn british_spelling_follows_the_locale() {
        for l in ["en-GB", "en_GB", "en-AU", "en-GB-u-mu-celsius"] {
            assert!(spells_british(l), "{l}");
        }
        for l in ["en-US", "en", "de-DE", "en-GBX"] {
            assert!(!spells_british(l), "{l}");
        }
    }

    #[test]
    fn summaries_are_read_leniently() {
        let v = json!({
            "title": " Launch sync ",
            "overview": "We agreed to ship.",
            "key_points": ["Beta is ready", ""],
            "decisions": ["Ship Friday"],
            "action_items": [
                {"task": "Send the deck", "owner": "Me", "at": "[1:05]"},
                {"task": "Book room", "owner": "", "at": ""},
                {"task": "", "owner": "x", "at": "0:01"}
            ]
        });
        let s = parse_summary(&v).unwrap();
        assert_eq!(s.title, "Launch sync");
        assert_eq!(s.key_points, vec!["Beta is ready"]);
        assert_eq!(s.action_items.len(), 2);
        assert_eq!(s.action_items[0].at_ms, Some(65_000));
        assert_eq!(s.action_items[1].at_ms, None);
        assert_eq!(parse_timestamp("1:02:03"), Some(3_723_000));
        assert_eq!(parse_timestamp("1:75"), None);
        assert!(parse_summary(&json!({"title": "x"})).is_err());
    }

    #[test]
    fn long_transcripts_are_split_into_sections_on_line_boundaries() {
        let lines: Vec<String> = (0..10)
            .map(|i| format!("[0:{i:02}] Me: {}", "x".repeat(40)))
            .collect();
        let parts = sections(&lines, 200);
        assert!(parts.len() >= 3);
        assert_eq!(parts.join("\n"), lines.join("\n"));
    }

    #[test]
    fn markdown_has_the_summary_notes_and_transcript() {
        let summary = Summary {
            title: "Launch sync".into(),
            overview: "We agreed to ship.".into(),
            key_points: vec!["Beta is ready".into()],
            decisions: vec![],
            action_items: vec![ActionItem {
                task: "Send the deck".into(),
                owner: "Me".into(),
                at_ms: Some(65_000),
                due: "by Friday".into(),
                tentative: true,
                ..Default::default()
            }],
            model: String::new(),
            generated_at: 0,
        };
        let md = to_markdown(
            "Launch sync",
            "Thu 24 Sep · 12:30",
            Some(&summary),
            "ask about budget",
            &["**[0:00] Me:** Hi.".to_string()],
        );
        assert_eq!(
            md,
            "# Launch sync\nThu 24 Sep · 12:30\n\n## Summary\nWe agreed to ship.\n\n### Key points\n- Beta is ready\n\n## Action items\n- [ ] Send the deck — Me, by Friday (tentative) ([1:05])\n\n## My notes\nask about budget\n\n## Transcript\n**[0:00] Me:** Hi.\n"
        );
    }

    #[test]
    fn notes_written_before_due_dates_and_quotes_still_open() {
        let old = r#"{"title":"t","overview":"o","key_points":[],"decisions":[],
            "action_items":[{"task":"Send it","owner":"Me","at_ms":5000}],"model":"m","generated_at":0}"#;
        let s: Summary = serde_json::from_str(old).unwrap();
        assert_eq!(s.action_items[0].due, "");
        assert!(!s.action_items[0].tentative);
    }

    #[test]
    fn a_candidate_is_checked_against_the_lines_around_it() {
        let lines: Vec<String> = (0..20)
            .map(|i| format!("[0:{i:02}] Me: line {i}"))
            .collect();
        let text = excerpt(&lines, Some(10_500));
        assert!(text.starts_with("[0:06]"));
        assert!(text.ends_with("line 18"));
        assert_eq!(
            excerpt(&lines, Some(0)).lines().next(),
            Some("[0:00] Me: line 0")
        );
        assert_eq!(excerpt(&lines, None), "(no timestamp)");
    }

    #[test]
    fn the_users_own_name_as_owner_is_me() {
        let item = |owner: &str| ActionItem {
            task: "t".into(),
            owner: owner.into(),
            ..Default::default()
        };
        let mut s = Summary {
            action_items: vec![item("Sam Rivera"), item("Baidi"), item("Me")],
            ..Default::default()
        };
        own_items_as_me(&mut s, Some("Sam Rivera"));
        let owners: Vec<&str> = s.action_items.iter().map(|a| a.owner.as_str()).collect();
        assert_eq!(owners, ["Me", "Baidi", "Me"]);
    }
}
