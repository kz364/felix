//! Replay benchmark recordings through the quiet-speech safety net: the
//! live silence threshold, then the sensitive one, both transcribed, and
//! whether the app would use the second transcript.
//!
//!     cargo run --release --example rescue_eval -- <model.gguf> <benchmark/id.json>...
//!
//! Only recordings where the sensitive pass keeps clearly more audio are
//! transcribed; the others are counted.

use handy_app_lib::audio_toolkit::audio::{read_wav_samples, GainState};
use handy_app_lib::vad_rescue::{self, Raw};
use std::path::{Path, PathBuf};
use transcribe_cpp::{Model, RunOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let model = Model::load(args.next().expect("model path"))?;
    let mut session = model.session()?;
    let options = RunOptions::default();
    let vad = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/models/silero_vad_v4.onnx");
    let (mut skipped, mut tried, mut used) = (0, 0, 0);
    for path in args.map(PathBuf::from) {
        let record: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path)?)?;
        let raw = Raw {
            samples: read_wav_samples(path.with_extension("wav"))?,
            gain_at_start: serde_json::from_value::<GainState>(record["gain_at_start"].clone())
                .unwrap_or_default(),
            gain_db: record["gain_db"].as_f64().unwrap_or(0.0) as f32,
            auto_gain: record["auto_gain"].as_bool().unwrap_or(true),
        };
        let base = record["vad_threshold"].as_f64().unwrap_or(0.3) as f32;
        let live = vad_rescue::sensitive_pass(&vad, &raw, base)?;
        let threshold = vad_rescue::rescue_threshold(base);
        let rescued = vad_rescue::sensitive_pass(&vad, &raw, threshold)?;
        if !vad_rescue::worth_transcribing(live.len(), rescued.len()) {
            skipped += 1;
            continue;
        }
        tried += 1;
        let first = if live.is_empty() {
            String::new()
        } else {
            session.run(&live, &options)?.text
        };
        let second = session.run(&rescued, &options)?.text;
        let first_for_check = if live.is_empty() { "" } else { first.as_str() };
        let ok = vad_rescue::accept(first_for_check, &second);
        used += ok as usize;
        println!(
            "\n{} ({base} → {threshold}): {:.1}s → {:.1}s, {}\n  first:  {}\n  second: {}\n  truth:  {}",
            record["id"].as_str().unwrap_or("?"),
            live.len() as f32 / 16_000.0,
            rescued.len() as f32 / 16_000.0,
            if ok { "USED" } else { "not used" },
            first.trim(),
            second.trim(),
            record["ground_truth"]
                .as_str()
                .or(record["guess"]["text"].as_str())
                .or(record["edited"].as_str())
                .unwrap_or("-"),
        );
    }
    println!("\n{skipped} skipped, {tried} transcribed twice, {used} used the second pass");
    Ok(())
}
