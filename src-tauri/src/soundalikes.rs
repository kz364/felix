//! Words that sound like an ordinary word and get transcribed as it ("Claude"
//! heard as "cloud"). A plain replacement rule would break the real word, and
//! biasing the transcription model makes it hear the name everywhere, so each
//! occurrence is decided from the sentence around it:
//! - followed by the rest of a compound name ("cloud code") it's the name;
//! - right after "the", "my", "our"… it's the ordinary word ("the cloud");
//! - otherwise the local cleanup model is asked, and the name goes in only
//!   when it thinks so more likely than not.
//!
//! On the test sentences the local 4B model got every ordinary "cloud" right;
//! the cost is that "the Claude app" and "my Claude subscription" stay as
//! "cloud", which is the safer mistake.

use crate::settings::{AppSettings, Soundalike};
use log::{debug, warn};
use regex::Regex;

/// How likely the model must think the name is meant.
const THRESHOLD: f64 = 0.5;

/// Words that, right before the heard word, make it the ordinary noun.
const DETERMINERS: &[&str] = &[
    "the", "a", "an", "my", "our", "your", "their", "his", "her", "its", "this", "these", "those",
    "some", "any", "private", "public", "hybrid",
];

/// One occurrence of a sound-alike in the text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Occurrence {
    start: usize,
    end: usize,
    /// Index into the settings' sound-alike list.
    entry: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Name,
    Ordinary,
    AskModel,
}

fn pattern(heard: &str) -> Option<Regex> {
    let heard = heard.trim();
    if heard.is_empty() {
        return None;
    }
    // A possessive stays part of the word ("cloud's").
    Regex::new(&format!(r"(?i)\b{}('s)?\b", regex::escape(heard))).ok()
}

fn find(text: &str, entries: &[Soundalike]) -> Vec<Occurrence> {
    let mut found: Vec<Occurrence> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| pattern(&e.heard).map(|re| (i, re)))
        .flat_map(|(entry, re)| {
            re.find_iter(text)
                .map(move |m| Occurrence {
                    start: m.start(),
                    end: m.end(),
                    entry,
                })
                .collect::<Vec<_>>()
        })
        .collect();
    found.sort_by_key(|o| o.start);
    found.dedup_by(|b, a| b.start < a.end);
    found
}

fn is_possessive(text: &str, occurrence: &Occurrence) -> bool {
    text[occurrence.start..occurrence.end]
        .to_lowercase()
        .ends_with("'s")
}

/// The compound name the occurrence starts, with the byte where it ends
/// ("cloud code" → "Claude Code").
fn compound_at(text: &str, occurrence: &Occurrence, entry: &Soundalike) -> Option<(String, usize)> {
    if is_possessive(text, occurrence) {
        return None;
    }
    let rest = &text[occurrence.end..];
    entry.compounds.iter().find_map(|compound| {
        let tail = compound
            .strip_prefix(entry.word.as_str())
            .filter(|t| t.starts_with(' '))?;
        let matches = rest
            .get(..tail.len())
            .is_some_and(|r| r.eq_ignore_ascii_case(tail))
            && rest[tail.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric());
        matches.then(|| (compound.clone(), occurrence.end + tail.len()))
    })
}

fn verdict(text: &str, occurrence: &Occurrence, entry: &Soundalike) -> Verdict {
    let previous = text[..occurrence.start]
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .rfind(|w| !w.is_empty())
        .map(str::to_lowercase);
    if compound_at(text, occurrence, entry).is_some() {
        Verdict::Name
    } else if previous.is_some_and(|w| DETERMINERS.contains(&w.as_str())) {
        Verdict::Ordinary
    } else {
        Verdict::AskModel
    }
}

fn system_prompt() -> &'static str {
    "You check a speech-to-text transcript for one kind of mistake: a name transcribed as an ordinary word that sounds like it. Answer yes if the marked word stands for the name, no if it means the ordinary word. Judge only from what the sentence is about. Answer with yes or no."
}

fn question(text: &str, occurrence: &Occurrence, entry: &Soundalike) -> String {
    let marked = format!(
        "{}[[{}]]{}",
        &text[..occurrence.start],
        &text[occurrence.start..occurrence.end],
        &text[occurrence.end..]
    );
    format!(
        "Transcript: {marked}\n\nThe name is \"{word}\": {meaning}. It's often transcribed as \"{heard}\".\n\nDoes the word in [[ ]] stand for \"{word}\"?",
        word = entry.word,
        meaning = entry.meaning,
        heard = entry.heard,
    )
}

/// The name in place of the heard word, as a compound name if it starts one,
/// keeping a possessive.
fn replacement(text: &str, occurrence: &Occurrence, entry: &Soundalike) -> (String, usize) {
    if let Some(compound) = compound_at(text, occurrence, entry) {
        return compound;
    }
    let mut name = entry.word.clone();
    if is_possessive(text, occurrence) {
        name.push_str("'s");
    }
    (name, occurrence.end)
}

fn apply(
    text: &str,
    occurrences: &[Occurrence],
    is_name: &[bool],
    entries: &[Soundalike],
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (occurrence, &is_name) in occurrences.iter().zip(is_name) {
        if !is_name || occurrence.start < at {
            continue;
        }
        let (name, end) = replacement(text, occurrence, &entries[occurrence.entry]);
        out.push_str(&text[at..occurrence.start]);
        out.push_str(&name);
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

/// The local model to ask, when it's the cleanup provider (so it's loaded or
/// loading while the user speaks).
fn local_model(settings: &AppSettings) -> Option<String> {
    let model = settings
        .post_process_models
        .get(crate::local_llm::LOCAL_PROVIDER_ID)?;
    (settings.post_process_enabled
        && settings.post_process_provider_id == crate::local_llm::LOCAL_PROVIDER_ID
        && !model.trim().is_empty())
    .then(|| model.clone())
}

/// Put the names back where the sound-alike word stood for one. Occurrences
/// the model would decide are left as they are without the local model, or
/// if it fails.
pub async fn resolve(text: &str, settings: &AppSettings) -> String {
    let entries = &settings.soundalikes;
    let occurrences = find(text, entries);
    if occurrences.is_empty() {
        return text.to_string();
    }
    let model = local_model(settings);
    let started = std::time::Instant::now();
    let mut is_name = Vec::with_capacity(occurrences.len());
    for occurrence in &occurrences {
        let entry = &entries[occurrence.entry];
        let decided = match (verdict(text, occurrence, entry), &model) {
            (Verdict::Name, _) => true,
            (Verdict::Ordinary, _) | (Verdict::AskModel, None) => false,
            (Verdict::AskModel, Some(model)) => {
                match crate::local_llm::yes_probability(
                    model,
                    system_prompt(),
                    &question(text, occurrence, entry),
                    settings.local_model_keep_loaded,
                )
                .await
                {
                    Ok(p) => {
                        debug!("Sound-alike \"{}\": {p:.2}", entry.heard);
                        p > THRESHOLD
                    }
                    Err(e) => {
                        warn!("Sound-alike check failed: {e}");
                        false
                    }
                }
            }
        };
        is_name.push(decided);
    }
    debug!(
        "Sound-alikes: {} occurrence(s) in {:?}, names {is_name:?}",
        occurrences.len(),
        started.elapsed()
    );
    apply(text, &occurrences, &is_name, entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude() -> Vec<Soundalike> {
        crate::settings::default_soundalikes()
    }

    fn verdicts(text: &str) -> Vec<Verdict> {
        let entries = claude();
        find(text, &entries)
            .iter()
            .map(|o| verdict(text, o, &entries[o.entry]))
            .collect()
    }

    #[test]
    fn finds_whole_words_only() {
        let text = "Cloud code and the cloud's cost, not cloudy or clouds";
        let found: Vec<&str> = find(text, &claude())
            .iter()
            .map(|o| &text[o.start..o.end])
            .collect();
        assert_eq!(found, vec!["Cloud", "cloud's"]);
    }

    #[test]
    fn decides_by_grammar_before_asking_the_model() {
        use Verdict::*;
        assert_eq!(
            verdicts("we could run cloud code in the cloud"),
            vec![Name, Ordinary]
        );
        assert_eq!(
            verdicts("Our cloud bill, my cloud storage"),
            vec![Ordinary, Ordinary]
        );
        assert_eq!(
            verdicts("Ask cloud. Cloud's answer"),
            vec![AskModel, AskModel]
        );
        assert_eq!(verdicts("see the cloud code docs"), vec![Name]);
        // "that" is usually a relative pronoun here, not "that cloud".
        assert_eq!(verdicts("bits that cloud models need"), vec![AskModel]);
        // A possessive doesn't start "Claude Code".
        assert_eq!(verdicts("so cloud's code"), vec![AskModel]);
    }

    #[test]
    fn replaces_only_the_names() {
        let entries = claude();
        let text = "Ask cloud whether it's saved in the cloud";
        let found = find(text, &entries);
        assert_eq!(
            apply(text, &found, &[true, false], &entries),
            "Ask Claude whether it's saved in the cloud"
        );
    }

    #[test]
    fn keeps_possessives_and_fixes_compound_names() {
        let entries = claude();
        let text = "wrap amp inside cloud code. cloud's terms, cloud coder";
        let found = find(text, &entries);
        assert_eq!(
            apply(text, &found, &[true, true, true], &entries),
            "wrap amp inside Claude Code. Claude's terms, Claude coder"
        );
    }

    #[test]
    fn question_marks_the_occurrence() {
        let entries = claude();
        let text = "is it in the cloud or cloud code";
        let found = find(text, &entries);
        let q = question(text, &found[1], &entries[0]);
        assert!(q.contains("in the cloud or [[cloud]] code"), "{q}");
    }

    /// Needs llama-server and `ollama pull qwen3.5:4b`. Stops any running
    /// Handy llama-server first (it restarts on the next dictation):
    /// cargo test --lib soundalikes -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn local_model_tells_claude_from_cloud() {
        let mut settings = crate::settings::get_default_settings();
        settings.post_process_enabled = true;
        settings.post_process_provider_id = crate::local_llm::LOCAL_PROVIDER_ID.to_string();
        settings.post_process_models.insert(
            crate::local_llm::LOCAL_PROVIDER_ID.to_string(),
            crate::local_llm::LOCAL_LARGE_MODEL.to_string(),
        );
        let cases = [
            (
                "I know we can't do it with Cloud Code because of the terms",
                "I know we can't do it with Claude Code because of the terms",
            ),
            (
                "Ask cloud to review the pull request",
                "Ask Claude to review the pull request",
            ),
            (
                "Honestly cloud is better at writing than GPT",
                "Honestly Claude is better at writing than GPT",
            ),
            (
                "tell cloud to write the tests first",
                "tell Claude to write the tests first",
            ),
            (
                "Are they saved in the cloud?",
                "Are they saved in the cloud?",
            ),
            (
                "Our cloud bill went up again",
                "Our cloud bill went up again",
            ),
            (
                "cloud computing is getting cheaper every year",
                "cloud computing is getting cheaper every year",
            ),
            (
                "not a single cloud in the sky",
                "not a single cloud in the sky",
            ),
        ];
        let mut wrong = 0;
        for (text, expected) in cases {
            let started = std::time::Instant::now();
            let out = resolve(text, &settings).await;
            println!("{:>6?} {out}", started.elapsed());
            if out != expected {
                wrong += 1;
            }
        }
        crate::local_llm::stop();
        assert_eq!(wrong, 0);
    }
}
