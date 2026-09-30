//! Dump WeSpeaker fingerprints for dictation clips and meeting segments, to
//! check whether a voiceprint of the user separates them from other voices.
//!
//! Usage: cargo run --release --example voice_check -- <model.onnx> <out.json>
//!        <benchmark/*.wav | meetings/<id>/mic.wav[#whole]>...
//! A meeting's transcript.json next to mic.wav supplies the segments; each
//! 1.5 s speech window inside a segment is embedded with its speaker label.

use handy_app_lib::meetings::diarize::Embedder;
use std::path::Path;

const RATE: usize = 16_000;
const WIN: usize = RATE * 3 / 2;

fn read(path: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).expect("wav");
    r.samples::<i16>()
        .map(|s| s.unwrap() as f32 / 32768.0)
        .collect()
}

/// 1.5 s windows, half of which is `above` times louder than the floor.
fn speech_windows(audio: &[f32], from: usize, to: usize, above: f32) -> Vec<(usize, usize)> {
    let rms = |a: &[f32]| (a.iter().map(|x| x * x).sum::<f32>() / a.len().max(1) as f32).sqrt();
    let frame = RATE / 50;
    let mut levels: Vec<f32> = audio.chunks(frame).map(rms).collect();
    levels.sort_by(f32::total_cmp);
    let floor = levels.get(levels.len() / 10).copied().unwrap_or(0.0);
    let mut out = Vec::new();
    let mut at = from;
    while at + WIN <= to.min(audio.len()) {
        let w = &audio[at..at + WIN];
        let loud = w
            .chunks(frame)
            .filter(|c| rms(c) > floor * above + 1e-4)
            .count();
        if loud * frame * 2 >= WIN {
            out.push((at, at + WIN));
        }
        at += WIN;
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut emb = Embedder::new(Path::new(&args[0])).expect("model");
    let mut rows = Vec::new();
    for f in &args[2..] {
        // "<wav>#whole": every speech window, ignoring any transcript.
        let whole = f.ends_with("#whole");
        let path = Path::new(f.trim_end_matches("#whole"));
        let audio = read(path);
        let transcript = path.with_file_name("transcript.json");
        let spans: Vec<(usize, usize, String)> = if !whole && path.file_name().unwrap() == "mic.wav"
        {
            let t: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&transcript).unwrap()).unwrap();
            t["segments"]
                .as_array()
                .unwrap()
                .iter()
                .map(|s| {
                    let ms = |k: &str| s[k].as_u64().unwrap() as usize * RATE / 1000;
                    (ms("start_ms"), ms("end_ms"), s["speaker"].to_string())
                })
                .collect()
        } else {
            vec![(0, audio.len(), String::new())]
        };
        for (from, to, speaker) in spans {
            for (a, b) in speech_windows(&audio, from, to, if whole { 2.0 } else { 4.0 }) {
                if let Ok(e) = emb.embed(&audio[a..b]) {
                    rows.push(serde_json::json!({
                        "file": f, "speaker": speaker, "at": a / RATE, "e": e
                    }));
                }
            }
        }
    }
    std::fs::write(&args[1], serde_json::to_string(&rows).unwrap()).unwrap();
    eprintln!("{} windows", rows.len());
}
