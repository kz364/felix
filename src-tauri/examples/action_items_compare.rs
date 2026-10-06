//! The action items a meeting's saved notes have, next to the ones the
//! action-item pass finds now, to compare prompts on real meetings.
//!
//!   ME="Kaspar Hidayat" cargo run --example action_items_compare -- <chatgpt model> <meeting dir>...
//!
//! Reads the folders only; point it at copies.

use handy_app_lib::meetings::llm::Llm;
use handy_app_lib::meetings::summary::{self, ActionItem, Summary};
use handy_app_lib::meetings::{about_meeting, manager, pipeline, transcript};
use std::path::Path;

fn show(items: &[ActionItem]) -> String {
    if items.is_empty() {
        return "  (none)\n".into();
    }
    items
        .iter()
        .map(|a| {
            format!(
                "  - [{}] {}: {}{}{}{}\n",
                a.at_ms.map(transcript::timestamp).unwrap_or_default(),
                if a.owner.is_empty() {
                    "(no owner)"
                } else {
                    &a.owner
                },
                a.task,
                if a.due.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", a.due)
                },
                if a.tentative { " (tentative)" } else { "" },
                if a.quote.is_empty() {
                    String::new()
                } else {
                    format!("\n      “{}”", a.quote)
                },
            )
        })
        .collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let model = args.next().expect("chatgpt model");
    let me = std::env::var("ME").ok();
    let llm = Llm::Chatgpt { model };
    for dir in args {
        let dir = Path::new(&dir);
        let info = manager::read_info(dir).expect("meeting.json");
        let t = pipeline::load(dir).expect("transcript.json");
        let paragraphs = manager::paragraphs_of(dir, &t);
        let lines: Vec<String> = paragraphs
            .iter()
            .map(|p| {
                let at = transcript::timestamp(p.start_ms);
                match info.speaker_label(p) {
                    Some(who) => format!("[{at}] {who}: {}", p.text),
                    None => format!("[{at}] {}", p.text),
                }
            })
            .collect();
        let about = about_meeting(&info, &paragraphs, me.as_deref());
        let notes = std::fs::read_to_string(dir.join("notes.md")).unwrap_or_default();
        let notes = if notes.trim().is_empty() {
            "(The user didn't type any notes.)".to_string()
        } else {
            notes
        };
        let old: Option<Summary> = summary::load_json(dir, summary::SUMMARY_FILE);
        let started = std::time::Instant::now();
        let new = tauri::async_runtime::block_on(summary::find_action_items(
            &llm, &about, &notes, &lines,
        ));
        println!(
            "# {} — {}\n\n## Old notes\n{}\n## New pass ({:.0}s)\n{}",
            dir.display(),
            info.title.clone().unwrap_or_default(),
            show(&old.map(|s| s.action_items).unwrap_or_default()),
            started.elapsed().as_secs_f64(),
            match new {
                Ok(items) => show(&items),
                Err(e) => format!("  error: {e}\n"),
            }
        );
    }
}
