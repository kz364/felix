//! How many voices a meeting's system track comes out as with and without
//! the call app's name hints, from its saved windows (no voice model run).
//!
//! cargo run --release --example hint_check -- <meeting dir>

use handy_app_lib::meetings::transcript::Source;
use handy_app_lib::meetings::{diarize, pipeline, speakers, windows};
use std::collections::BTreeMap;
use std::path::Path;

fn main() -> Result<(), String> {
    let dir = std::env::args().nth(1).ok_or("meeting dir")?;
    let dir = Path::new(&dir);
    let vad = Path::new("resources/models/silero_vad_v4.onnx");
    let speech = pipeline::analyze(&dir.join(Source::System.file()), vad, false)?.speech;
    let (_, (wins, embs)) = windows::load(dir)
        .ok_or("no windows.bin")?
        .into_iter()
        .find(|(s, _)| *s == Source::System)
        .ok_or("no system windows")?;
    let hints = speakers::name_hints(dir, speech.len());
    for (what, h) in [
        ("with hints", hints.clone()),
        ("without", vec![None; speech.len()]),
    ] {
        let v = diarize::label_with(&speech, &wins, &embs, None, &h);
        let mut talk: BTreeMap<u32, usize> = BTreeMap::new();
        for l in v.labels.iter().flatten() {
            *talk.entry(*l).or_default() += 1;
        }
        let mut secs: Vec<f32> = talk.values().map(|n| *n as f32 * 0.03).collect();
        secs.sort_by(|a, b| b.total_cmp(a));
        println!(
            "{what}: {} voices, seconds each {:?}",
            secs.len(),
            secs.iter().map(|s| s.round() as u32).collect::<Vec<_>>()
        );
    }
    Ok(())
}
