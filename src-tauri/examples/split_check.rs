//! How many of a meeting's chunks the final pass would transcribe again,
//! with the live pass's text (`ahead.json`) reused: runs the pipeline with
//! a stand-in model that only counts. Works on a copy; writes transcript.json.
//!
//! ENGINE=<as in ahead.json> cargo run --release --example split_check -- <speaker model> <meeting copy>

use handy_app_lib::meetings::{pipeline, MeetingMode};
use std::path::Path;

fn main() {
    let mut args = std::env::args().skip(1);
    let model = args.next().expect("speaker model");
    let dir = args.next().expect("meeting dir");
    let _ = std::fs::remove_file(Path::new(&dir).join("transcript.json"));
    let (mut again, mut secs) = (0, 0.0);
    let t = pipeline::run(
        Path::new(&dir),
        MeetingMode::Call,
        Path::new("resources/models/silero_vad_v4.onnx"),
        None,
        Some(Path::new(&model)),
        None,
        &std::env::var("ENGINE").unwrap_or_default(),
        |audio| {
            again += 1;
            secs += audio.len() as f64 / 16_000.0;
            Ok("x".into())
        },
        |_| {},
    )
    .expect("run");
    let mut d: Vec<f64> = t
        .segments
        .iter()
        .map(|s| (s.end_ms - s.start_ms) as f64 / 1000.0)
        .collect();
    d.sort_by(f64::total_cmp);
    println!(
        "{} pieces, median {:.1} s, {} under 2 s; transcribed again: {again} ({secs:.0} s)",
        d.len(),
        d[d.len() / 2],
        d.iter().filter(|&&x| x < 2.0).count()
    );
}
