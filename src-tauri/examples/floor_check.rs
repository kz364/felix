//! Who holds the floor, read from the conversation: asks the meeting model
//! for stretches of paragraphs one named person said (a standup's "Joseph,
//! you're up" hands the next stretch to Joseph) and prints them against the
//! voices the transcript has, names and times only.
//!
//! cargo run --release --example floor_check -- <meeting dir> [model] [effort]

use handy_app_lib::meetings::llm::Llm;
use handy_app_lib::meetings::transcript::{Segment, Source};
use handy_app_lib::meetings::{calendar, floor, pipeline};
use std::collections::BTreeMap;
use std::path::Path;

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().ok_or("meeting dir")?;
    let model = args.next().unwrap_or_else(|| "gpt-6.1-sol".into());
    let effort = args.next().unwrap_or_else(|| "medium".into());
    let t = pipeline::load(Path::new(&dir)).ok_or("no transcript")?;
    let label = |p: &Segment| match (p.source, p.speaker) {
        (Source::Mic, _) => "Me".to_string(),
        (_, Some(v)) => format!("Voice {v}"),
        _ => "Them".to_string(),
    };
    let invite = calendar::load(Path::new(&dir)).unwrap_or_default();
    let llm = Llm::Chatgpt { model };
    let started = std::time::Instant::now();
    let turns = tauri::async_runtime::block_on(floor::find(
        &llm,
        &t.segments,
        &label,
        &invite.attendees,
        invite.me.as_deref(),
        &effort,
    ))?;
    println!(
        "{} lines, {} named in {:.0} s",
        floor::lines(&t.segments).len(),
        turns.len(),
        started.elapsed().as_secs_f64()
    );
    let mut by_name: BTreeMap<&str, BTreeMap<String, u64>> = BTreeMap::new();
    for turn in &turns {
        let line = t
            .segments
            .iter()
            .find(|s| s.source == turn.source && s.start_ms == turn.start_ms);
        if let Some(l) = line {
            *by_name
                .entry(&turn.name)
                .or_default()
                .entry(label(l))
                .or_default() += (l.end_ms - l.start_ms) / 1000;
        }
        println!("  {:>4} s  {}", turn.start_ms / 1000, turn.name);
    }
    println!("by name, seconds per voice:");
    for (name, voices) in by_name {
        println!("  {name}: {voices:?}");
    }
    Ok(())
}
