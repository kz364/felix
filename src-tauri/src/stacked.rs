//! Stacked messages: in personal messengers, a dictation of a few sentences
//! goes out as a few messages, one per sentence, the way people text.
//!
//! Only when the message box was empty (so a half-written message isn't
//! split or sent), and only for 2 to [`MAX_MESSAGES`] sentences. The last
//! sentence is left in the box unless the dictation asked to send it.

use crate::settings::AutoSubmitKey;
use once_cell::sync::Lazy;
use regex::Regex;
use std::time::Duration;
use tauri::AppHandle;

/// More sentences than this read as a message, not a burst of texts.
const MAX_MESSAGES: usize = 6;
/// Time for the messenger to send one message and clear its box before the
/// next paste.
const BETWEEN_MESSAGES: Duration = Duration::from_millis(450);

/// Words ending in a period that don't end a sentence.
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "st", "vs", "etc", "e.g", "i.e", "approx", "no", "jr", "sr",
];

/// The sentences of a dictation, if it should go out as several messages.
/// `period_dropped`: casual style took off the last period, so each message
/// loses its period too.
pub fn split(text: &str, period_dropped: bool) -> Option<Vec<String>> {
    if text.contains('\n') {
        // Lists and paragraphs were laid out on purpose.
        return None;
    }
    let sentences = sentences(text);
    if !(2..=MAX_MESSAGES).contains(&sentences.len()) {
        return None;
    }
    Some(
        sentences
            .into_iter()
            .map(|s| {
                if period_dropped && s.ends_with('.') && !s.ends_with("..") {
                    s[..s.len() - 1].to_string()
                } else {
                    s
                }
            })
            .collect(),
    )
}

fn sentences(text: &str) -> Vec<String> {
    static END: Lazy<Regex> = Lazy::new(|| Regex::new(r"([.!?…]+)\s+").unwrap());
    let mut out = Vec::new();
    let mut start = 0;
    for m in END.captures_iter(text) {
        let whole = m.get(0).unwrap();
        let mark = &m[1];
        let before = &text[start..whole.start()];
        let last_word = before
            .rsplit(char::is_whitespace)
            .next()
            .unwrap_or("")
            .to_lowercase();
        let next = text[whole.end()..].chars().next();
        let is_abbreviation = mark == "." && ABBREVIATIONS.contains(&last_word.as_str());
        let is_initial = mark == "." && last_word.chars().count() == 1;
        // "so... anyway" trails off mid-sentence.
        let trails_off =
            (mark.starts_with("..") || mark == "…") && next.is_some_and(|c| c.is_lowercase());
        if is_abbreviation || is_initial || trails_off {
            continue;
        }
        let sentence = text[start..whole.start() + mark.len()].trim();
        if !sentence.is_empty() {
            out.push(sentence.to_string());
        }
        start = whole.end();
    }
    let rest = text[start..].trim();
    if !rest.is_empty() {
        out.push(rest.to_string());
    }
    out
}

/// Paste the messages one by one, sending all but the last (and the last
/// too when `submit` asks for it). Runs on the main thread for the first
/// and schedules the rest.
pub fn paste(app: &AppHandle, mut messages: Vec<String>, submit: Option<AutoSubmitKey>) {
    let last = messages.pop().unwrap_or_default();
    let app = app.clone();
    std::thread::spawn(move || {
        for message in messages {
            paste_one(&app, message, Some(AutoSubmitKey::Enter));
            std::thread::sleep(BETWEEN_MESSAGES);
        }
        paste_one(&app, last, submit);
    });
}

fn paste_one(app: &AppHandle, text: String, submit: Option<AutoSubmitKey>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let ah = app.clone();
    let queued = app.run_on_main_thread(move || {
        if let Err(e) = crate::utils::paste(text, ah, submit) {
            log::error!("Failed to paste a stacked message: {e}");
        }
        let _ = tx.send(());
    });
    if queued.is_ok() {
        let _ = rx.recv_timeout(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_into_sentences() {
        assert_eq!(
            split("Hey! Are you free tonight? Let's get dinner.", false).unwrap(),
            vec!["Hey!", "Are you free tonight?", "Let's get dinner."]
        );
    }

    #[test]
    fn drops_periods_when_casual_did() {
        assert_eq!(
            split("On my way. Be there in 10", true).unwrap(),
            vec!["On my way", "Be there in 10"]
        );
    }

    #[test]
    fn keeps_abbreviations_decimals_and_trailing_off_together() {
        assert_eq!(split("Meet Dr. Smith at 3.30 today", false), None);
        assert_eq!(split("I was like... whatever", false), None);
        assert_eq!(
            split("Bring snacks, e.g. chips. See you!", false).unwrap(),
            vec!["Bring snacks, e.g. chips.", "See you!"]
        );
        assert_eq!(split("See you at J. Street", false), None);
    }

    #[test]
    fn only_a_few_sentences_are_stacked() {
        assert_eq!(split("Just one sentence.", false), None);
        assert_eq!(
            split("One. Two! Three? Four. Five. Six. Seven.", false),
            None
        );
        assert_eq!(split("Items:\n1. Milk\n2. Eggs", false), None);
    }
}
