//! Compare gain settings for in-person meetings on real recordings, by word
//! error rate (spec: "Gain for in-person meetings", test set).
//!
//!   cargo run --example meeting_gain_eval -- <model.gguf> <clips dir>
//!
//! The clips dir holds `name.wav` (16 kHz mono 16-bit, e.g. from a Voice Memo:
//! `afconvert -f WAVE -d LEI16@16000 -c 1 memo.m4a name.wav`) with `name.txt`
//! next to it: what was actually said. Keep the recordings out of git:
//! they're private.
//!
//! Each clip is transcribed like an in-person meeting with:
//! no gain · dictation's AGC · a percentile gain over the whole clip (p95 of
//! the last 10 s to RMS 0.06, ×8 at most, slewing 2/s) · the app's own
//! in-person auto gain (the same gain, run per chunk).

use handy_app_lib::audio_toolkit::read_wav_samples;
use handy_app_lib::meetings::level::LevelSettings;
use handy_app_lib::meetings::{pipeline, MeetingMode};
use std::path::{Path, PathBuf};
use transcribe_cpp::{Model, RunOptions};

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Word error rate: edits to turn the hypothesis into the reference.
fn wer(reference: &str, hypothesis: &str) -> f64 {
    let (r, h) = (words(reference), words(hypothesis));
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    let mut prev: Vec<usize> = (0..=h.len()).collect();
    for (i, rw) in r.iter().enumerate() {
        let mut cur = vec![i + 1; h.len() + 1];
        for (j, hw) in h.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(rw != hw))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[h.len()] as f64 / r.len() as f64
}

fn write_wav(path: &Path, samples: &[f32]) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
            .unwrap();
    }
    w.finalize().unwrap();
}

/// Run dictation's AGC over the whole clip, frame by frame.
fn dictation_agc(samples: &[f32]) -> Vec<f32> {
    use handy_app_lib::audio_toolkit::GainConfig;
    let mut gain = handy_app_lib::audio_toolkit::audio::InputGain::new(GainConfig::new(0.0, true));
    samples
        .chunks(480)
        .flat_map(|f| gain.process(f).to_vec())
        .collect()
}

/// Granola-style live gain: aim the loudest 5% of the last 10 s at RMS 0.06,
/// at most ×8, changing by at most 2 (linear) a second.
fn percentile_gain(samples: &[f32]) -> Vec<f32> {
    const FRAME: usize = 480;
    const HISTORY: usize = 10_000 / 30;
    let step = 2.0 * FRAME as f32 / 16_000.0;
    let mut history: std::collections::VecDeque<f32> = Default::default();
    let mut gain = 1.0f32;
    let mut out = Vec::with_capacity(samples.len());
    for frame in samples.chunks(FRAME) {
        let rms = (frame.iter().map(|v| v * v).sum::<f32>() / frame.len() as f32).sqrt();
        history.push_back(rms);
        if history.len() > HISTORY {
            history.pop_front();
        }
        let mut sorted: Vec<f32> = history.iter().copied().collect();
        sorted.sort_by(f32::total_cmp);
        let p95 = sorted[sorted.len() * 95 / 100];
        let want = if p95 > 0.0 {
            (0.06 / p95).clamp(1.0, 8.0)
        } else {
            1.0
        };
        gain += (want - gain).clamp(-step, step);
        out.extend(
            frame
                .iter()
                .map(|v| handy_app_lib::audio_toolkit::audio::soft_limit(v * gain)),
        );
    }
    out
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let model_path = args.next().expect("model path");
    let clips = PathBuf::from(args.next().expect("clips dir"));
    let vad = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/models/silero_vad_v4.onnx");
    let model = Model::load(&model_path)?;
    let mut session = model.session()?;
    let options = RunOptions::default();

    // (name, levelling, pre-processing: 1 dictation AGC, 2 percentile gain)
    let variants: Vec<(&str, Option<LevelSettings>, u8)> = vec![
        ("no gain", None, 0),
        ("dictation AGC", None, 1),
        ("percentile", None, 2),
        ("app (auto gain)", Some(LevelSettings::new(0.0, true)), 0),
    ];
    let mut totals = vec![0.0f64; variants.len()];
    let mut n = 0;
    let mut entries: Vec<_> = std::fs::read_dir(&clips)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "wav"))
        .collect();
    entries.sort();
    println!(
        "{:<24} {}",
        "clip",
        variants
            .iter()
            .map(|v| format!("{:>14}", v.0))
            .collect::<String>()
    );
    for wav in entries {
        let Ok(reference) = std::fs::read_to_string(wav.with_extension("txt")) else {
            eprintln!("skipping {} (no .txt)", wav.display());
            continue;
        };
        let samples = read_wav_samples(&wav)?;
        let mut row = format!("{:<24}", wav.file_stem().unwrap().to_string_lossy());
        for (k, (_, level, agc)) in variants.iter().enumerate() {
            let dir =
                std::env::temp_dir().join(format!("handy-gain-eval-{}-{k}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir)?;
            let audio = match agc {
                1 => dictation_agc(&samples),
                2 => percentile_gain(&samples),
                _ => samples.clone(),
            };
            write_wav(&dir.join("mic.wav"), &audio);
            let t = pipeline::run(
                &dir,
                MeetingMode::InPerson,
                &vad,
                *level,
                None,
                None,
                "eval",
                |a| {
                    session
                        .run(&a, &options)
                        .map(|r| r.text)
                        .map_err(|e| pipeline::Stopped::Failed(e.to_string()))
                },
                |_| {},
            )
            .map_err(|e| format!("{e:?}"))?;
            let text: Vec<&str> = t.segments.iter().map(|s| s.text.as_str()).collect();
            let score = wer(&reference, &text.join(" "));
            if std::env::var_os("SHOW_TEXT").is_some() {
                eprintln!(
                    "--- {} / {}\n{}",
                    wav.display(),
                    variants[k].0,
                    text.join(" | ")
                );
            }
            totals[k] += score;
            row.push_str(&format!("{:>13.1}%", score * 100.0));
            let _ = std::fs::remove_dir_all(&dir);
        }
        n += 1;
        println!("{row}");
    }
    if n > 0 {
        println!(
            "{:<24}{}",
            "average",
            totals
                .iter()
                .map(|t| format!("{:>13.1}%", t / n as f64 * 100.0))
                .collect::<String>()
        );
    }
    Ok(())
}
