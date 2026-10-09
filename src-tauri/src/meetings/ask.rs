//! Asking about a meeting: "What did I miss?" and "Suggest a question"
//! while it records (from the live transcript), and questions, a follow-up
//! email or changes to the notes afterwards. The summary is only rewritten
//! when the user asks for a change to it.

use super::jobs::about_meeting;
use super::llm::Llm;
use super::manager::{paragraphs_of, read_info, MeetingInfo, MeetingManager, NOTES_FILE};
use super::summary::{self, Summary};
use super::transcript::{self, timestamp, Paragraph};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use specta::Type;
use std::path::Path;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

/// "What did I miss?" looks back this many words.
const MISSED_WORDS: usize = 400;
/// Fewer words than this and there's nothing to say yet.
const MIN_WORDS: usize = 6;

const ASK_FRAME: &str = "You help the user with a meeting they're in or just had, from its transcript and the notes they typed. \
The user is \"Me\" in the transcript. Transcript lines start with [m:ss] timestamps. Answer only from the transcript, \
the notes and the meeting notes; if something isn't there, say so. Be brief and plain, no preamble. \
Reply in the language the user wrote in, or the meeting's language.";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum LiveQuestion {
    /// What was said lately, for someone who drifted off.
    Missed,
    /// A good question the user could ask now.
    Question,
}

/// An answer about a finished meeting.
#[derive(Debug, Clone, Serialize, Type)]
pub struct MeetingAnswer {
    pub answer: String,
    /// The summary was rewritten as asked.
    pub summary_updated: bool,
}

/// `[m:ss] Who: text` lines for the model.
pub(super) fn transcript_lines(info: &MeetingInfo, paragraphs: &[Paragraph]) -> Vec<String> {
    paragraphs
        .iter()
        .map(|p| {
            let at = timestamp(p.start_ms);
            match info.speaker_label(p) {
                Some(who) => format!("[{at}] {who}: {}", p.text),
                None => format!("[{at}] {}", p.text),
            }
        })
        .collect()
}

fn word_count(lines: &[String]) -> usize {
    lines.iter().map(|l| l.split_whitespace().count()).sum()
}

/// The last lines holding about `words` words.
pub fn last_words(lines: &[String], words: usize) -> Vec<String> {
    let mut total = 0;
    let mut start = lines.len();
    while start > 0 && total < words {
        start -= 1;
        total += lines[start].split_whitespace().count();
    }
    lines[start..].to_vec()
}

/// Keep the end of a long transcript within `budget` characters.
fn fit(lines: &[String], budget: usize) -> String {
    let mut total = 0;
    let mut start = lines.len();
    while start > 0 && total + lines[start - 1].len() < budget {
        start -= 1;
        total += lines[start].len() + 1;
    }
    let kept = lines[start..].join("\n");
    if start > 0 {
        format!("(The start of the meeting is left out; the notes above cover it.)\n{kept}")
    } else {
        kept
    }
}

fn answer_schema() -> Value {
    json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"],
        "additionalProperties": false
    })
}

fn llm(app: &AppHandle) -> Result<Llm, String> {
    Llm::from_settings(&crate::rules::for_meetings(crate::settings::get_settings(
        app,
    )))
}

fn summary_text(summary: Option<&Summary>) -> String {
    let Some(s) = summary else {
        return "(No notes yet.)".into();
    };
    let mut out = format!("{}\n{}\n", s.title, s.overview);
    for p in &s.key_points {
        out.push_str(&format!("- {p}\n"));
    }
    for d in &s.decisions {
        out.push_str(&format!("- Decided: {d}\n"));
    }
    for a in &s.action_items {
        let owner = if a.owner.is_empty() {
            String::new()
        } else {
            format!(" ({})", a.owner)
        };
        let due = if a.due.is_empty() {
            String::new()
        } else {
            format!(", {}", a.due)
        };
        out.push_str(&format!("- To do: {}{owner}{due}\n", a.task));
    }
    out
}

/// A quick answer from the live transcript of the meeting being recorded.
/// Empty when too little has been said yet.
#[tauri::command]
#[specta::specta]
pub async fn ask_live(app: AppHandle, question: LiveQuestion) -> Result<String, String> {
    let (dir, info) = app
        .state::<Arc<MeetingManager>>()
        .with_recording(|r, info| (r.dir.clone(), info.clone()))
        .ok_or("Nothing is being recorded")?;
    let paragraphs = transcript::paragraphs(&super::live::load(&dir));
    let lines = transcript_lines(&info, &paragraphs);
    if word_count(&lines) < MIN_WORDS {
        return Ok(String::new());
    }
    let llm = llm(&app)?;
    let about = about_meeting(
        &info,
        &paragraphs,
        &super::jobs::call_names(&dir),
        user_name(&app).as_deref(),
    );
    let notes = std::fs::read_to_string(dir.join(NOTES_FILE)).unwrap_or_default();
    let (task, transcript) = match question {
        LiveQuestion::Missed => (
            "The user looked away for a bit. In 2 to 4 short bullets, say what was just said: \
the latest topic, anything asked of the user, and anything decided. Most recent last.",
            last_words(&lines, MISSED_WORDS).join("\n"),
        ),
        LiveQuestion::Question => (
            "Suggest one good question the user could ask now: specific to what's being discussed, \
something not yet answered, in their voice, one sentence. Reply with only the question.",
            fit(&lines, llm.budget().summary),
        ),
    };
    let input = format!(
        "# Meeting\n{about}\n\n# My notes\n{notes}\n\n# Transcript so far (rough)\n{transcript}\n\n# Task\n{task}"
    );
    let v = llm
        .ask_json(ASK_FRAME, &input, &answer_schema(), "low")
        .await?;
    Ok(v["answer"].as_str().unwrap_or("").trim().to_string())
}

fn user_name(app: &AppHandle) -> Option<String> {
    crate::rules::user_name(&crate::settings::get_settings(app))
}

/// What a finished meeting's questions are asked with.
fn meeting_context(
    dir: &Path,
    info: &MeetingInfo,
    me: Option<&str>,
    budget: usize,
) -> Result<(String, String), String> {
    let paragraphs = match super::pipeline::load(dir).filter(|t| t.complete) {
        Some(t) => paragraphs_of(dir, &t),
        None => transcript::paragraphs(&super::live::load(dir)),
    };
    let lines = transcript_lines(info, &paragraphs);
    if lines.is_empty() {
        return Err("The meeting isn't transcribed yet".into());
    }
    let summary: Option<Summary> = summary::load_json(dir, summary::SUMMARY_FILE);
    let notes = std::fs::read_to_string(dir.join(NOTES_FILE)).unwrap_or_default();
    let about = about_meeting(info, &paragraphs, &super::jobs::call_names(dir), me);
    let context = format!(
        "# Meeting\n{about}\n\n# Meeting notes (written from the transcript)\n{}\n# My notes\n{notes}\n\n# Transcript\n{}",
        summary_text(summary.as_ref()),
        fit(&lines, budget)
    );
    Ok((context, about))
}

fn meeting(app: &AppHandle, id: &str) -> Result<(std::path::PathBuf, MeetingInfo), String> {
    let dir = app.state::<Arc<MeetingManager>>().dir_of(id)?;
    let info = read_info(&dir).ok_or("The meeting isn't there any more")?;
    Ok((dir, info))
}

/// Words that ask for the notes themselves to change.
fn asks_to_change_notes(question: &str) -> bool {
    let q = question.to_lowercase();
    [
        "summary",
        "notes",
        "action item",
        "key point",
        "decision",
        "title",
        "overview",
    ]
    .iter()
    .any(|w| q.contains(w))
        && [
            "add", "remove", "change", "fix", "rewrite", "update", "shorten", "expand", "edit",
            "put", "drop", "delete", "rename", "include", "make",
        ]
        .iter()
        .any(|w| {
            q.split(|c: char| !c.is_alphanumeric())
                .any(|word| word == *w)
        })
}

/// Ask about a finished meeting. Asking for a change to the notes ("add
/// the budget to the action items") rewrites the summary; anything else
/// leaves it alone.
#[tauri::command]
#[specta::specta]
pub async fn ask_meeting(
    app: AppHandle,
    id: String,
    question: String,
) -> Result<MeetingAnswer, String> {
    let question = question.trim().to_string();
    if question.is_empty() {
        return Err("Ask something first".into());
    }
    let (dir, info) = meeting(&app, &id)?;
    let llm = llm(&app)?;
    let (context, _) = meeting_context(
        &dir,
        &info,
        user_name(&app).as_deref(),
        llm.budget().summary,
    )?;

    if asks_to_change_notes(&question) {
        if let Some(current) = summary::load_json::<Summary>(&dir, summary::SUMMARY_FILE) {
            let instructions = format!(
                "{}\n\nThe user wants a change to the meeting notes. Rewrite them with exactly that change and \
keep everything else as it is.",
                summary::SUMMARY_FRAME
            );
            let input = format!(
                "{context}\n\n# Current notes (JSON)\n{}\n\n# Change asked for\n{question}",
                serde_json::to_string(&current).unwrap_or_default()
            );
            let v = llm
                .ask_json(&instructions, &input, &summary::summary_schema(), "medium")
                .await?;
            let mut updated = summary::parse_summary(&v)?;
            updated.model = llm.label();
            updated.generated_at = chrono::Utc::now().timestamp_millis();
            summary::save_json(&dir, summary::SUMMARY_FILE, &updated)?;
            let manager = app.state::<Arc<MeetingManager>>();
            manager.update_info(&id, |i| {
                if i.title_is_auto && !updated.title.is_empty() {
                    i.title = Some(updated.title.clone());
                }
            });
            let _ = app.emit("meetings-changed", ());
            return Ok(MeetingAnswer {
                answer: "Updated the notes.".into(),
                summary_updated: true,
            });
        }
    }

    let input = format!("{context}\n\n# Question\n{question}");
    let v = llm
        .ask_json(ASK_FRAME, &input, &answer_schema(), "low")
        .await?;
    Ok(MeetingAnswer {
        answer: v["answer"].as_str().unwrap_or("").trim().to_string(),
        summary_updated: false,
    })
}

/// A follow-up email to the others, from the user.
#[tauri::command]
#[specta::specta]
pub async fn draft_follow_up_email(app: AppHandle, id: String) -> Result<String, String> {
    let (dir, info) = meeting(&app, &id)?;
    let llm = llm(&app)?;
    let (context, _) = meeting_context(
        &dir,
        &info,
        user_name(&app).as_deref(),
        llm.budget().summary,
    )?;
    let task = "Draft the follow-up email the user sends the others after this meeting: a subject line, \
a short thank-you, what was agreed, and next steps with owners (the user's own only where they committed). \
Plain text, first person, friendly and brief. No placeholders for things the transcript doesn't say.";
    let input = format!("{context}\n\n# Task\n{task}");
    let v = llm
        .ask_json(ASK_FRAME, &input, &answer_schema(), "medium")
        .await?;
    Ok(v["answer"].as_str().unwrap_or("").trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_words_looks_back_far_enough() {
        let lines: Vec<String> = (0..10)
            .map(|i| format!("[0:0{i}] Them: one two three"))
            .collect();
        // Each line is 5 words.
        assert_eq!(last_words(&lines, 12).len(), 3);
        assert_eq!(last_words(&lines, 1000).len(), 10);
    }

    #[test]
    fn only_asking_for_a_change_rewrites_the_notes() {
        assert!(asks_to_change_notes(
            "Add the budget review to the action items"
        ));
        assert!(asks_to_change_notes("Please rewrite the summary shorter"));
        assert!(!asks_to_change_notes("What did Sam say about the budget?"));
        assert!(!asks_to_change_notes("What are the action items?"));
        assert!(!asks_to_change_notes("Does the summary mention pricing?"));
    }
}
