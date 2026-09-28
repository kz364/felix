//! Learning words from the corrections you make after a paste.
//!
//! After each dictation Felix follows the field it went into for a few
//! minutes (Accessibility, every 2 s): the text between what was before and
//! after the dictation is read back until focus moves, the field is cleared
//! (a message was sent), or the next dictation starts. Only that span is
//! kept, never the rest of the field.
//!
//! When the dictation was corrected rather than rewritten, the pasted and
//! corrected words are lined up and each replaced run becomes a candidate
//! ("kasper" → "Kaspar", "git hub" → "GitHub"). Candidates that are ordinary
//! words, look nothing like what was heard, or are already known are
//! dropped; the local model, when it's the cleanup model, gets the last word
//! on the rest. At most `MAX_PER_EDIT` are added to the vocabulary, and a
//! notice says so with an Undo. Only vocabulary is added: it biases
//! recognition and fixes spelling, nothing is fine-tuned.

use crate::notices::{Action, Notice};
use crate::settings::AppSettings;
use crate::text_field::{self, FieldSnapshot};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tauri::AppHandle;

const SETTLE: Duration = Duration::from_millis(250);
const POLL: Duration = Duration::from_secs(2);
const WATCH_FOR: Duration = Duration::from_secs(180);
/// Characters of field text either side of the dictation used to find it
/// again after edits.
const ANCHOR: usize = 40;
/// Most words learned from one correction.
const MAX_PER_EDIT: usize = 4;
/// Longest run of words that counts as one term ("Visual Studio Code").
const MAX_TERM_WORDS: usize = 3;
/// How alike (0–1, by letters) the heard and corrected forms must be to be
/// a mishearing rather than a change of mind.
const MIN_LIKENESS: f32 = 0.5;
/// The local model's confidence that a candidate is worth learning.
const MODEL_THRESHOLD: f64 = 0.5;

/// Bumped per watched paste so an older watcher stops when a newer one starts.
static WATCH_GENERATION: AtomicU64 = AtomicU64::new(0);

/// After a paste: follow the field, then hand the dictation's final text to
/// the benchmark record (`bench_id`) and to learning.
pub fn watch(
    app: &AppHandle,
    pasted: String,
    before: Option<FieldSnapshot>,
    bench_id: Option<String>,
) {
    let learn = crate::settings::get_settings(app).learn_from_edits;
    if !learn && bench_id.is_none() {
        return;
    }
    let app = app.clone();
    let generation = WATCH_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        let Some((last, why)) = follow(&pasted, before.as_ref(), generation) else {
            if let Some(id) = &bench_id {
                crate::benchmark::update(&app, id, |r| r.edit = Some("field not readable".into()));
            }
            return;
        };
        let kind = edit_kind(&pasted, &last);
        log::debug!("Dictation edits: {kind} ({why})");
        if let Some(id) = &bench_id {
            let edited = last.clone();
            crate::benchmark::update(&app, id, move |r| {
                r.edited = Some(edited);
                r.edit = Some(kind.to_string());
            });
        }
        if learn && kind == "edited" {
            tauri::async_runtime::block_on(learn_from(&app, &pasted, &last));
        }
    });
}

/// Follow the field until the watch ends; the dictation's last text and why
/// the watch stopped. `None` when the dictation can't be found in the field.
fn follow(
    pasted: &str,
    before: Option<&FieldSnapshot>,
    generation: u64,
) -> Option<(String, &'static str)> {
    std::thread::sleep(SETTLE);
    let after = text_field::focused_field();
    let (prefix, suffix, pid) = before
        .zip(after.as_ref())
        .and_then(|(b, a)| anchors(b, a, pasted).map(|(p, s)| (p, s, a.pid)))?;
    let mut last = pasted.trim().to_string();
    let mut why = "watched";
    let started = Instant::now();
    while started.elapsed() < WATCH_FOR {
        std::thread::sleep(POLL);
        if WATCH_GENERATION.load(Ordering::SeqCst) != generation {
            why = "next dictation";
            break;
        }
        let Some(field) = text_field::focused_field().filter(|f| f.pid == pid) else {
            why = "focus moved";
            break;
        };
        if field
            .value
            .iter()
            .all(|c| char::from_u32(*c as u32).is_some_and(char::is_whitespace))
        {
            why = "field cleared";
            break;
        }
        if let Some(text) = span_between(&field.value, &prefix, &suffix) {
            last = text;
        }
    }
    Some((last, why))
}

/// Where the dictation sits in `after` (the field just after pasting), as
/// the text before and after it.
fn anchors(
    before: &FieldSnapshot,
    after: &FieldSnapshot,
    pasted: &str,
) -> Option<(Vec<u16>, Vec<u16>)> {
    if before.pid != after.pid {
        return None;
    }
    let (start, len) = text_field::inserted_span(&before.value, &after.value)?;
    let inserted = String::from_utf16_lossy(&after.value[start..start + len]);
    if inserted.trim() != pasted.trim() {
        return None;
    }
    let prefix = &after.value[..start];
    let suffix = &after.value[start + len..];
    Some((
        prefix[prefix.len().saturating_sub(ANCHOR)..].to_vec(),
        suffix[..suffix.len().min(ANCHOR)].to_vec(),
    ))
}

fn find(haystack: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from);
    }
    (from..=haystack.len().checked_sub(needle.len())?)
        .find(|&i| haystack[i..i + needle.len()] == *needle)
}

/// The dictation's text in `value`, found between its anchors.
pub fn span_between(value: &[u16], prefix: &[u16], suffix: &[u16]) -> Option<String> {
    let start = find(value, prefix, 0)? + prefix.len();
    let end = if suffix.is_empty() {
        value.len()
    } else {
        find(value, suffix, start)?
    };
    Some(
        String::from_utf16_lossy(&value[start..end])
            .trim()
            .to_string(),
    )
}

fn normalize(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'')
        .flat_map(char::to_lowercase)
        .collect()
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(normalize)
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word edits between two texts, over the longer one's length.
fn word_change(a: &str, b: &str) -> f32 {
    let (a, b) = (words(a), words(b));
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 0.0;
    }
    edit_distance(&a, &b) as f32 / longest as f32
}

fn edit_distance<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, wa) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, wb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(wa != wb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// How the text changed: small corrections are ground truth; a rewrite
/// (more than half the words) is a change of mind, not a correction.
pub fn edit_kind(pasted: &str, edited: &str) -> &'static str {
    if pasted.trim() == edited.trim() {
        "unchanged"
    } else if word_change(pasted, edited) <= 0.5 {
        "edited"
    } else {
        "rewritten"
    }
}

/// A word (or short term) you typed over what Felix heard.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub word: String,
    pub heard: String,
}

/// A word as typed, without the punctuation around it ("GitHub," → "GitHub").
fn bare(word: &str) -> &str {
    word.trim_matches(|c: char| !(c.is_alphanumeric() || c == '\'' || c == '+' || c == '#'))
}

/// 0–1: how alike two terms are by their letters ("git hub" and "GitHub"
/// are the same; "cloud" and "Claude" are close).
fn likeness(a: &str, b: &str) -> f32 {
    let letters = |s: &str| -> Vec<char> {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect()
    };
    let (a, b) = (letters(a), letters(b));
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 0.0;
    }
    1.0 - edit_distance(&a, &b) as f32 / longest as f32
}

/// Runs of words replaced between the pasted text and your corrected text,
/// as (heard, corrected), including case-only fixes ("felix" → "Felix").
fn replaced_runs(pasted: &str, edited: &str) -> Vec<(Vec<String>, Vec<String>)> {
    let a: Vec<&str> = pasted.split_whitespace().collect();
    let b: Vec<&str> = edited.split_whitespace().collect();
    let (na, nb): (Vec<String>, Vec<String>) = (
        a.iter().map(|w| normalize(w)).collect(),
        b.iter().map(|w| normalize(w)).collect(),
    );
    // Edit-distance table, then walk back for the alignment.
    let mut d = vec![vec![0usize; nb.len() + 1]; na.len() + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in d[0].iter_mut().enumerate() {
        *cell = j;
    }
    for i in 1..=na.len() {
        for j in 1..=nb.len() {
            let same = na[i - 1] == nb[j - 1];
            d[i][j] = (d[i - 1][j - 1] + usize::from(!same))
                .min(d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1);
        }
    }
    // (heard index, corrected index), None where a side has no word.
    let mut steps: Vec<(Option<usize>, Option<usize>)> = Vec::new();
    let (mut i, mut j) = (na.len(), nb.len());
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && d[i][j] == d[i - 1][j - 1] + usize::from(na[i - 1] != nb[j - 1]) {
            steps.push((Some(i - 1), Some(j - 1)));
            i -= 1;
            j -= 1;
        } else if i > 0 && d[i][j] == d[i - 1][j] + 1 {
            steps.push((Some(i - 1), None));
            i -= 1;
        } else {
            steps.push((None, Some(j - 1)));
            j -= 1;
        }
    }
    steps.reverse();

    let mut runs = Vec::new();
    let (mut heard, mut fixed): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let mut flush = |heard: &mut Vec<String>, fixed: &mut Vec<String>| {
        if !heard.is_empty() && !fixed.is_empty() {
            runs.push((std::mem::take(heard), std::mem::take(fixed)));
        }
        heard.clear();
        fixed.clear();
    };
    for (ai, bi) in steps {
        let unchanged = match (ai, bi) {
            (Some(x), Some(y)) => na[x] == nb[y] && bare(a[x]) == bare(b[y]),
            _ => false,
        };
        if unchanged {
            flush(&mut heard, &mut fixed);
            continue;
        }
        if let Some(x) = ai {
            heard.push(bare(a[x]).to_string());
        }
        if let Some(y) = bi {
            fixed.push(bare(b[y]).to_string());
        }
    }
    flush(&mut heard, &mut fixed);
    runs
}

/// Words worth learning from a correction, before the model's say.
pub fn candidates(pasted: &str, edited: &str, known: &[String]) -> Vec<Candidate> {
    let known: Vec<String> = known.iter().map(|w| w.to_lowercase()).collect();
    let mut out: Vec<Candidate> = Vec::new();
    for (heard, fixed) in replaced_runs(pasted, edited) {
        if fixed.len() > MAX_TERM_WORDS || heard.len() > MAX_TERM_WORDS + 1 {
            continue;
        }
        let word = fixed.join(" ");
        let heard = heard.join(" ");
        let letters = word.chars().filter(|c| c.is_alphabetic()).count();
        let lower = word.to_lowercase();
        if letters < 2
            || lower == heard.to_lowercase() && word == word.to_lowercase()
            || crate::vocab_teach::is_common_phrase(&word)
            || known.contains(&lower)
            || likeness(&heard, &word) < MIN_LIKENESS
            || out.iter().any(|c| c.word.to_lowercase() == lower)
        {
            continue;
        }
        out.push(Candidate { word, heard });
    }
    out
}

const MODEL_PROMPT: &str = "You help a dictation app learn the user's vocabulary. After a dictation was pasted, the user corrected what the speech recognizer heard. Say whether the corrected term is a name, brand, product, technical term, acronym or other specialised word the recognizer should learn to spell this way. Answer no for ordinary words, grammar fixes, and changes of mind (a different word, not a misspelling). Answer only yes or no.";

/// The local model's view, when it's the cleanup model; everything passes
/// without it.
async fn model_approves(settings: &AppSettings, candidate: &Candidate, edited: &str) -> bool {
    let Some(model) = crate::soundalikes::local_model(settings) else {
        return true;
    };
    let question = format!(
        "Recognizer heard: \"{}\"\nUser corrected it to: \"{}\"\nIn: \"{}\"\nLearn \"{}\"?",
        candidate.heard, candidate.word, edited, candidate.word
    );
    match crate::local_llm::yes_probability(
        &model,
        MODEL_PROMPT,
        &question,
        settings.local_model_keep_loaded,
    )
    .await
    {
        Ok(p) => {
            log::debug!("Learn \"{}\"?: {p:.2}", candidate.word);
            p > MODEL_THRESHOLD
        }
        Err(e) => {
            log::warn!(
                "Couldn't ask the local model about \"{}\": {e}",
                candidate.word
            );
            true
        }
    }
}

pub(crate) async fn learn_from(app: &AppHandle, pasted: &str, edited: &str) {
    let settings = crate::rules::with_rules(crate::settings::get_settings(app));
    let mut known = settings.custom_words.clone();
    known.extend(crate::rules::current().vocabulary);
    let mut learned = Vec::new();
    for candidate in candidates(pasted, edited, &known) {
        if learned.len() >= MAX_PER_EDIT {
            break;
        }
        if !model_approves(&settings, &candidate, edited).await {
            continue;
        }
        match crate::rules::add_word(&candidate.word) {
            Ok(()) => {
                crate::rules::log_learned(&candidate.word, &candidate.heard);
                learned.push(candidate);
            }
            Err(e) => log::warn!("Couldn't learn \"{}\": {e}", candidate.word),
        }
    }
    if learned.is_empty() {
        return;
    }
    log::info!(
        "Learned from an edit: {:?}",
        learned.iter().map(|c| &c.word).collect::<Vec<_>>()
    );
    let quoted = |words: Vec<&str>| {
        words
            .iter()
            .map(|w| format!("“{w}”"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let title = if learned.len() == 1 {
        format!("Learned {}", quoted(vec![&learned[0].word]))
    } else {
        format!("Learned {} words", learned.len())
    };
    let text = format!(
        "You corrected {} to {}, so Felix will spell it your way from now on.",
        quoted(learned.iter().map(|c| c.heard.as_str()).collect()),
        quoted(learned.iter().map(|c| c.word.as_str()).collect()),
    );
    let words: Vec<String> = learned.into_iter().map(|c| c.word).collect();
    let notice = Notice::new("learned_word", title, text).action(Action::new("Undo", move |_| {
        for word in &words {
            if let Err(e) = crate::rules::remove_word(word) {
                log::warn!("Couldn't undo learning \"{word}\": {e}");
            }
        }
    }));
    crate::notices::show(app, notice);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn words_of(c: &[Candidate]) -> Vec<&str> {
        c.iter().map(|c| c.word.as_str()).collect()
    }

    #[test]
    fn finds_the_dictation_after_edits() {
        let value = u("Hi Sam, let's meet at eight tomorrow. Thanks");
        assert_eq!(
            span_between(&value, &u("Hi Sam, "), &u(" Thanks")).as_deref(),
            Some("let's meet at eight tomorrow.")
        );
        // Dictated at the end of the field.
        assert_eq!(
            span_between(&u("Note: push the fix"), &u("Note: "), &[]).as_deref(),
            Some("push the fix")
        );
        // Text before it was changed: can't place it any more.
        assert_eq!(span_between(&value, &u("Hello Sam, "), &u(" Thanks")), None);
    }

    #[test]
    fn classifies_edits() {
        assert_eq!(
            edit_kind("Push it to github.", "Push it to github."),
            "unchanged"
        );
        assert_eq!(
            edit_kind("Push it to git hub.", "Push it to GitHub."),
            "edited"
        );
        assert_eq!(
            edit_kind(
                "Push it to github.",
                "Actually, never mind, I'll do it myself tomorrow."
            ),
            "rewritten"
        );
    }

    #[test]
    fn anchors_need_the_pasted_text() {
        let snap = |t: &str| FieldSnapshot {
            pid: 1,
            role: "AXTextArea".into(),
            value: u(t),
            selection: None,
        };
        let (prefix, suffix) = anchors(
            &snap("Hi  Thanks"),
            &snap("Hi hello there Thanks"),
            "hello there",
        )
        .unwrap();
        assert_eq!(String::from_utf16_lossy(&prefix), "Hi ");
        assert_eq!(String::from_utf16_lossy(&suffix), " Thanks");
        assert!(anchors(&snap("Hi  Thanks"), &snap("Hi hello there Thanks"), "other").is_none());
    }

    #[test]
    fn learns_misheard_names() {
        let c = candidates(
            "Thanks kasper, I'll ask cloud to review it.",
            "Thanks Kaspar, I'll ask Claude to review it.",
            &[],
        );
        assert_eq!(words_of(&c), ["Kaspar", "Claude"]);
        assert_eq!(c[0].heard, "kasper");
    }

    #[test]
    fn joins_split_words() {
        let c = candidates("Push it to git hub today.", "Push it to GitHub today.", &[]);
        assert_eq!(words_of(&c), ["GitHub"]);
        assert_eq!(c[0].heard, "git hub");
    }

    #[test]
    fn skips_ordinary_words_and_changes_of_mind() {
        if crate::vocab_teach::is_common_phrase("tuesday meeting") {
            // A different word, not a misspelling.
            assert!(candidates("See you Tuesday.", "See you Wednesday.", &[]).is_empty());
            // Grammar and punctuation.
            assert!(candidates("their going home", "they're going home", &[]).is_empty());
        }
        // Unrelated to what was heard.
        assert!(candidates(
            "Let's talk about the meeting.",
            "Let's talk about Kubernetes.",
            &[]
        )
        .is_empty());
        // Only punctuation changed.
        assert!(candidates("Hi Sam how are you", "Hi Sam, how are you?", &[]).is_empty());
    }

    #[test]
    fn skips_known_words() {
        let known = vec!["kaspar".to_string()];
        assert!(candidates("Thanks kasper.", "Thanks Kaspar.", &known).is_empty());
    }

    #[test]
    fn keeps_case_fixes_of_names() {
        let c = candidates("ask felix about it", "ask Felix about it", &[]);
        assert_eq!(words_of(&c), ["Felix"]);
    }
}
