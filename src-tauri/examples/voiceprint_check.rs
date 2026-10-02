//! Does a voiceprint made on one mic find the user on another? Tells the
//! voices apart on a meeting's mic (echo cancelled, if it was on speakers)
//! and prints how alike each voice is to the given prints: the user's saved
//! print for a mic, and the main mic voice of another call (made here, not
//! saved).
//!
//! cargo run --release --example voiceprint_check -- <speaker model> <scratch dir> <meeting> <print source>...
//! A print source is a saved voiceprint .json or a meeting folder.

use handy_app_lib::meetings::transcript::Source;
use handy_app_lib::meetings::{aec, diarize, pipeline};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn unit(v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
    v.into_iter().map(|x| x / n).collect()
}
fn cos(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The meeting's mic voices: (seconds, mean print, window prints).
fn voices(
    model: &Path,
    scratch: &Path,
    dir: &Path,
) -> Result<Vec<(f32, Vec<f32>, Vec<Vec<f32>>)>, String> {
    let vad = Path::new("resources/models/silero_vad_v4.onnx");
    let mic = dir.join(Source::Mic.file());
    let system = dir.join(Source::System.file());
    let wav: PathBuf = if system.is_file() {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        let out = scratch.join(format!("{name}-mic-clean.wav"));
        if !out.is_file() {
            aec::cancel(&mic, &system, &out)?;
        }
        out
    } else {
        mic
    };
    let speech = pipeline::analyze(&wav, vad, false)?.speech;
    let v = diarize::speakers_with(&wav, &speech, model, None, &[])?;
    let mut by: BTreeMap<u32, (usize, Vec<f32>, Vec<Vec<f32>>)> = BTreeMap::new();
    // Each window's voice: the label most of its frames got.
    for (&(from, to), e) in v.windows.iter().zip(&v.embeddings) {
        let mut count = BTreeMap::<u32, usize>::new();
        for l in v.labels[from..to.min(v.labels.len())].iter().flatten() {
            *count.entry(*l).or_default() += 1;
        }
        let Some((&l, _)) = count.iter().max_by_key(|(_, c)| **c) else {
            continue;
        };
        let e = unit(e.clone());
        let entry = by
            .entry(l)
            .or_insert_with(|| (0, vec![0.0; e.len()], Vec::new()));
        entry.0 += to - from;
        for (a, x) in entry.1.iter_mut().zip(&e) {
            *a += x;
        }
        entry.2.push(e);
    }
    let mut out: Vec<_> = by
        .into_values()
        .map(|(frames, sum, wins)| (frames as f32 * 0.03, unit(sum), wins))
        .collect();
    out.sort_by(|a, b| b.0.total_cmp(&a.0));
    Ok(out)
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let model = PathBuf::from(args.next().ok_or("model")?);
    let scratch = PathBuf::from(args.next().ok_or("scratch")?);
    let meeting = PathBuf::from(args.next().ok_or("meeting")?);
    let mut prints: Vec<(String, Vec<f32>)> = Vec::new();
    for src in args {
        let p = Path::new(&src);
        if p.is_dir() {
            let v = voices(&model, &scratch, p)?;
            let (secs, print, _) = v.first().ok_or("no voices")?;
            println!(
                "print from {} main mic voice ({secs:.0} s of {:.0} s)",
                p.file_name().unwrap().to_string_lossy(),
                v.iter().map(|x| x.0).sum::<f32>()
            );
            prints.push((
                format!("{} main voice", p.file_name().unwrap().to_string_lossy()),
                print.clone(),
            ));
        } else {
            let j: serde_json::Value =
                serde_json::from_slice(&std::fs::read(p).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            let e: Vec<f32> =
                serde_json::from_value(j["embedding"].clone()).map_err(|e| e.to_string())?;
            prints.push((
                format!("saved print {}", j["mic"].as_str().unwrap_or("?").trim()),
                unit(e),
            ));
        }
    }
    let v = voices(&model, &scratch, &meeting)?;
    println!(
        "{} mic voices:",
        meeting.file_name().unwrap().to_string_lossy()
    );
    for (i, (secs, mean, wins)) in v.iter().enumerate() {
        let sims: Vec<String> = prints
            .iter()
            .map(|(name, p)| {
                let mut w: Vec<f32> = wins.iter().map(|x| cos(x, p)).collect();
                w.sort_by(|a, b| a.total_cmp(b));
                format!(
                    "{name}: mean {:.2}, windows median {:.2}",
                    cos(mean, p),
                    w[w.len() / 2]
                )
            })
            .collect();
        println!("  voice {i} ({secs:.0} s): {}", sims.join(" | "));
    }
    for (i, a) in v.iter().enumerate() {
        for (j, b) in v.iter().enumerate().skip(i + 1) {
            println!("  voice {i} vs {j}: {:.2}", cos(&a.1, &b.1));
        }
    }
    Ok(())
}
