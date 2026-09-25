//! Turn mistake reports into a `bias_eval` manifest, to see which speech
//! models make the reported mistakes:
//!
//!     cargo run --example report_manifest > reports.json
//!     cargo run --release --example bias_eval -- <model.gguf> reports.json
//!
//! Each report with kept audio becomes a clip. Its reference is the reported
//! dictation as the current rules fix it (so a model that hears it right
//! from the start scores well), and the rules' vocabulary is the vocabulary.
//! Reads Handy's own folder; everything stays on this Mac.

use handy_app_lib::rules::{self, TestCase};
use std::path::PathBuf;

fn main() {
    let dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Library/Application Support/com.pais.handy");
    let rules_text = std::fs::read_to_string(dir.join(rules::FILE)).unwrap_or_default();
    let vocab = rules::parse(&rules_text)
        .map(|r| r.vocabulary)
        .unwrap_or_default();
    let reports = std::fs::read_to_string(dir.join(rules::REPORTS_FILE)).unwrap_or_default();
    let mut clips = Vec::new();
    for line in reports.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let (Some(audio), Some(said)) = (
            v["audio"].as_str(),
            v["last_dictation"]["transcribed"].as_str(),
        ) else {
            continue;
        };
        let fixed = rules::check_against(
            &rules_text,
            &[TestCase {
                said: said.into(),
                expect: String::new(),
                ..Default::default()
            }],
        )
        .ok()
        .and_then(|r| r.into_iter().next())
        .map(|r| r.got)
        .unwrap_or_else(|| said.to_string());
        clips.push(serde_json::json!({
            "wav": dir.join(audio),
            "ref": fixed,
            "kind": "pos",
            "model": v["last_dictation"]["model"],
            "report": v["report"],
        }));
    }
    eprintln!("{} reports with audio", clips.len());
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({"vocab": vocab, "clips": clips}))
            .unwrap_or_default()
    );
}
