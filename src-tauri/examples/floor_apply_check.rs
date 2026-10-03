//! What naming lines from the conversation does to a meeting's voices, on a
//! copy: finds who held the floor (unless the copy has `floor.json`), works
//! the voices and names out again as Felix does, and prints seconds per
//! voice before and after, names only.
//!
//! cargo run --release --example floor_apply_check -- <copy of a meeting dir>

use handy_app_lib::meetings::llm::Llm;
use handy_app_lib::meetings::transcript::{Segment, Source};
use handy_app_lib::meetings::{calendar, floor, pipeline, speakers, summary};
use std::collections::BTreeMap;
use std::path::Path;

fn talk(segments: &[Segment], names: &BTreeMap<u32, String>) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for s in segments
        .iter()
        .filter(|s| s.source == Source::System && !s.echo)
    {
        let who = match s.speaker {
            Some(v) => names.get(&v).cloned().unwrap_or(format!("voice {v}")),
            None => "?".into(),
        };
        *out.entry(who).or_default() += (s.end_ms - s.start_ms) / 1000;
    }
    out
}

fn main() -> Result<(), String> {
    let dir = std::env::args().nth(1).ok_or("meeting dir (a copy)")?;
    let dir = Path::new(&dir);
    let before = pipeline::load(dir).ok_or("no transcript")?;
    println!("before: {:?}", talk(&before.segments, &BTreeMap::new()));
    if !dir.join(floor::FILE).exists() {
        let invite = calendar::load(dir).unwrap_or_default();
        let llm = Llm::Chatgpt {
            model: handy_app_lib::meetings::llm::CAREFUL_CHATGPT_MODEL.into(),
        };
        let label = |s: &Segment| match (s.source, s.speaker) {
            (Source::Mic, _) => "Me".to_string(),
            (_, Some(v)) => format!("Voice {v}"),
            _ => "Them".to_string(),
        };
        let turns = tauri::async_runtime::block_on(floor::find(
            &llm,
            &before.segments,
            &label,
            &invite.attendees,
            invite.me.as_deref(),
            summary::CLEANUP_EFFORT,
        ))?;
        summary::save_json(dir, floor::FILE, &turns)?;
    }
    let names = speakers::apply(dir);
    let after = pipeline::load(dir).ok_or("no transcript")?;
    println!("after:  {:?}", talk(&after.segments, &names));
    Ok(())
}
