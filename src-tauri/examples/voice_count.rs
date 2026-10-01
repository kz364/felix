//! How many voices one track splits into at different grouping stops, and
//! how much each talks. Scratch tool for tuning on a real meeting.
//!   cargo run --release --example voice_count -- <track.wav>
use handy_app_lib::meetings::{diarize, pipeline, segment};
use std::path::{Path, PathBuf};

fn main() -> Result<(), String> {
    let wav = PathBuf::from(std::env::args().nth(1).ok_or("voice_count <wav>")?);
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let models = home.join("Library/Application Support/com.pais.handy/models");
    let model = models.join(diarize::MODEL_FILE);
    let seg = models.join(segment::MODEL_DIR).join(segment::MODEL_FILE);
    let vad = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/models/silero_vad_v4.onnx");
    let speech = pipeline::analyze(&wav, &vad, false)?.speech;
    let (wins, embs, _) = diarize::fingerprint_with(&wav, &speech, &model, Some(&seg))?;
    println!("{} windows", wins.len());
    for stop in [0.03f32, 0.0, -0.03, -0.06, -0.1, -0.15, -0.2] {
        let normalise = std::env::var("NORM").map_or(true, |v| v != "0");
        let g = diarize::Grouping {
            stop,
            normalise,
            ..Default::default()
        };
        let (labels, _) = diarize::group_voices_with(&embs, &g, &vec![None; embs.len()]);
        let mut talk = std::collections::BTreeMap::<u32, usize>::new();
        for (l, (a, b)) in labels.iter().zip(&wins) {
            *talk.entry(*l).or_default() += b - a;
        }
        let mut v: Vec<usize> = talk.values().map(|f| f * 30 / 1000).collect();
        v.sort_unstable_by(|a, b| b.cmp(a));
        println!("stop {stop:.2}: {} voices, seconds {:?}", v.len(), v);
        if stop == 0.0 {
            // Talk per voice in each quarter of the track.
            let end = wins.last().map_or(1, |w| w.1).max(1);
            let mut q = std::collections::BTreeMap::<u32, [usize; 4]>::new();
            for (l, (a, b)) in labels.iter().zip(&wins) {
                q.entry(*l).or_default()[(a * 4 / end).min(3)] += (b - a) * 30 / 1000;
            }
            for (l, s) in q {
                println!("   voice {l}: seconds by quarter {s:?}");
            }
        }
    }
    Ok(())
}
