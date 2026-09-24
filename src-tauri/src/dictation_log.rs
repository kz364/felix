//! In-memory log of recent dictations and the text field they went into:
//! the field when recording started, just before the paste and just after,
//! plus the exact span the dictation inserted. This is the state an undo or
//! "edit what I just dictated" voice command needs. Field contents are kept
//! in memory only (last 20 dictations), never written to disk.

use crate::text_field::{self, FieldSnapshot};
use once_cell::sync::Lazy;
use serde::Serialize;
use specta::Type;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::Duration;

const MAX_RECORDS: usize = 20;
/// How long to wait after pasting before reading the field back; apps apply
/// a Cmd+V asynchronously.
const SETTLE_DELAY: Duration = Duration::from_millis(250);

#[derive(Serialize, Debug, Clone, PartialEq, Eq, Type)]
pub struct Selection {
    pub location: u32,
    pub length: u32,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq, Type)]
pub struct FieldState {
    pub role: String,
    pub text: String,
    pub selection: Option<Selection>,
}

impl From<&FieldSnapshot> for FieldState {
    fn from(s: &FieldSnapshot) -> Self {
        Self {
            role: s.role.clone(),
            text: String::from_utf16_lossy(&s.value),
            selection: s.selection.map(|(location, length)| Selection {
                location: location as u32,
                length: length as u32,
            }),
        }
    }
}

/// Where the dictation landed in the field (UTF-16 offsets, as the
/// Accessibility API uses them).
#[derive(Serialize, Debug, Clone, PartialEq, Eq, Type)]
pub struct InsertedSpan {
    pub start: u32,
    pub length: u32,
    pub text: String,
}

#[derive(Serialize, Debug, Clone, PartialEq, Type)]
pub struct DictationRecord {
    pub id: u32,
    pub timestamp_ms: f64,
    pub app_name: Option<String>,
    pub bundle_id: Option<String>,
    pub url_host: Option<String>,
    /// Exactly what was pasted (after context adaptation).
    pub pasted_text: String,
    pub first_word: Option<String>,
    pub last_word: Option<String>,
    pub at_start: Option<FieldState>,
    pub before_paste: Option<FieldState>,
    pub after_paste: Option<FieldState>,
    /// `None` when the field couldn't be read or changed unexpectedly.
    pub inserted: Option<InsertedSpan>,
}

static AT_START: Lazy<Mutex<Option<FieldSnapshot>>> = Lazy::new(|| Mutex::new(None));
static RECORDS: Lazy<Mutex<VecDeque<DictationRecord>>> = Lazy::new(|| Mutex::new(VecDeque::new()));
static NEXT_ID: Lazy<Mutex<u32>> = Lazy::new(|| Mutex::new(1));

/// Snapshot the focused field as recording starts (in the background, so a
/// slow app never delays capture).
pub fn capture_at_start() {
    *AT_START.lock().unwrap() = None;
    std::thread::spawn(|| {
        *AT_START.lock().unwrap() = text_field::focused_field();
    });
}

fn edge_words(text: &str) -> (Option<String>, Option<String>) {
    let words: Vec<&str> = text
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect();
    (
        words.first().map(|w| w.to_string()),
        words.last().map(|w| w.to_string()),
    )
}

/// Build the record from the three snapshots. The inserted span comes from
/// the before/after diff and is only trusted if it matches what was pasted.
pub fn build_record(
    id: u32,
    pasted_text: &str,
    context: &crate::app_context::DictationContext,
    at_start: Option<&FieldSnapshot>,
    before: Option<&FieldSnapshot>,
    after: Option<&FieldSnapshot>,
) -> DictationRecord {
    let inserted = match (before, after) {
        (Some(b), Some(a)) if b.pid == a.pid => text_field::inserted_span(&b.value, &a.value)
            .and_then(|(start, length)| {
                let text = String::from_utf16_lossy(&a.value[start..start + length]);
                // The field may normalize whitespace; compare trimmed.
                (text.trim() == pasted_text.trim()).then(|| InsertedSpan {
                    start: start as u32,
                    length: length as u32,
                    text,
                })
            }),
        _ => None,
    };
    let (first_word, last_word) = edge_words(pasted_text);
    DictationRecord {
        id,
        timestamp_ms: chrono::Utc::now().timestamp_millis() as f64,
        app_name: context.app_name.clone(),
        bundle_id: context.bundle_id.clone(),
        url_host: context.url_host.clone(),
        pasted_text: pasted_text.to_string(),
        first_word,
        last_word,
        at_start: at_start.map(FieldState::from),
        before_paste: before.map(FieldState::from),
        after_paste: after.map(FieldState::from),
        inserted,
    }
}

/// After a paste: read the field back once it settles and log the record.
pub fn record_after_paste(pasted_text: String, before: Option<FieldSnapshot>) {
    let context = crate::app_context::current();
    let at_start = AT_START.lock().unwrap().take();
    std::thread::spawn(move || {
        std::thread::sleep(SETTLE_DELAY);
        let after = text_field::focused_field();
        let id = {
            let mut next = NEXT_ID.lock().unwrap();
            let id = *next;
            *next += 1;
            id
        };
        let record = build_record(
            id,
            &pasted_text,
            &context,
            at_start.as_ref(),
            before.as_ref(),
            after.as_ref(),
        );
        log::debug!(
            "Dictation {} into {:?}: inserted span {:?}",
            id,
            record.app_name,
            record.inserted.as_ref().map(|s| (s.start, s.length))
        );
        let mut records = RECORDS.lock().unwrap();
        records.push_front(record);
        records.truncate(MAX_RECORDS);
    });
}

/// Most recent dictations first.
#[tauri::command]
#[specta::specta]
pub fn get_recent_dictations() -> Vec<DictationRecord> {
    RECORDS.lock().unwrap().iter().cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_context::DictationContext;

    fn snap(pid: i32, text: &str, caret: usize) -> FieldSnapshot {
        FieldSnapshot {
            pid,
            role: "AXTextArea".into(),
            value: text.encode_utf16().collect(),
            selection: Some((caret, 0)),
        }
    }

    #[test]
    fn record_captures_inserted_span_and_edges() {
        let before = snap(7, "Hi Sam. Thanks", 8);
        let after = snap(7, "Hi Sam. Sounds good to me. Thanks", 27);
        let record = build_record(
            1,
            "Sounds good to me. ",
            &DictationContext::default(),
            None,
            Some(&before),
            Some(&after),
        );
        let span = record.inserted.expect("span");
        assert_eq!((span.start, span.length), (8, 19));
        assert_eq!(span.text, "Sounds good to me. ");
        assert_eq!(record.first_word.as_deref(), Some("Sounds"));
        assert_eq!(record.last_word.as_deref(), Some("me"));
    }

    #[test]
    fn no_span_when_field_changed_otherwise() {
        let before = snap(7, "draft", 5);
        let other_app = snap(9, "draft hello", 11);
        assert!(build_record(
            1,
            "hello",
            &DictationContext::default(),
            None,
            Some(&before),
            Some(&other_app)
        )
        .inserted
        .is_none());
        let unrelated_edit = snap(7, "draft plus something else", 25);
        assert!(build_record(
            1,
            "hello",
            &DictationContext::default(),
            None,
            Some(&before),
            Some(&unrelated_edit)
        )
        .inserted
        .is_none());
    }
}
