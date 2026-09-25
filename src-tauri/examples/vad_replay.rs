//! Replay benchmark recordings through input gain and silence detection to
//! see what the speech model would have been given.
//!
//! cargo run --example vad_replay -- <benchmark/id.json>... [--out DIR]
//!
//! For each recording and variant, prints the seconds kept and a timeline
//! (one character per 250 ms: `#` kept, `.` dropped) plus the median level.
//! With `--out`, writes each variant's kept audio as a WAV.

use handy_app_lib::audio_toolkit::{
    audio::{read_wav_samples, GainConfig, GainState, InputGain},
    vad::{
        frames_for_duration_ms, SmoothedVad, VAD_OFFLINE_HANGOVER_MS, VAD_ONSET_MS, VAD_PREFILL_MS,
    },
    SileroVad, VoiceActivityDetector,
};
use serde_json::Value;
use std::path::PathBuf;

const MODEL: &str = "resources/models/silero_vad_v4.onnx";

struct Variant {
    name: String,
    gain_db: f32,
    auto_gain: bool,
    threshold: f32,
}

fn run(audio: &[f32], state: GainState, v: &Variant) -> (Vec<f32>, Vec<bool>) {
    let silero = SileroVad::new(MODEL, v.threshold).unwrap();
    let n = silero.frame_samples();
    let mut vad = SmoothedVad::new(
        Box::new(silero),
        frames_for_duration_ms(VAD_PREFILL_MS, n),
        frames_for_duration_ms(VAD_OFFLINE_HANGOVER_MS, n),
        frames_for_duration_ms(VAD_ONSET_MS, n),
    );
    let mut gain = InputGain::new(GainConfig::seeded(v.gain_db, v.auto_gain, state));
    let mut kept = Vec::new();
    let mut marks = Vec::new();
    for frame in audio.chunks_exact(n) {
        let frame = gain.process(frame).to_vec();
        marks.push(false);
        if let Ok(handy_app_lib::audio_toolkit::vad::VadFrame::Speech(s)) = vad.push_frame(&frame) {
            let frames = s.len() / n;
            let len = marks.len();
            for m in &mut marks[len.saturating_sub(frames)..] {
                *m = true;
            }
            kept.extend_from_slice(s);
        }
    }
    (kept, marks)
}

fn median_dbfs(audio: &[f32]) -> f32 {
    let mut levels: Vec<f32> = audio
        .chunks(1600)
        .map(|c| {
            let rms = (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt();
            20.0 * rms.max(1e-9).log10()
        })
        .collect();
    levels.sort_by(|a, b| a.partial_cmp(b).unwrap());
    levels.get(levels.len() / 2).copied().unwrap_or(-100.0)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let out = args.iter().position(|a| a == "--out").map(|i| {
        let dir = PathBuf::from(args.remove(i + 1));
        args.remove(i);
        dir
    });

    for path in args {
        let path = PathBuf::from(path);
        let record: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let audio = read_wav_samples(path.with_extension("wav")).unwrap();
        let state: GainState =
            serde_json::from_value(record["gain_at_start"].clone()).unwrap_or_default();
        let gain_db = record["gain_db"].as_f64().unwrap_or(0.0) as f32;
        let auto = record["auto_gain"].as_bool().unwrap_or(true);
        println!(
            "\n{}  {:.1}s  raw median {:.0} dBFS\n  heard: {}",
            record["id"].as_str().unwrap_or("?"),
            audio.len() as f32 / 16000.0,
            median_dbfs(&audio),
            record["transcript"].as_str().unwrap_or("")
        );
        let mut variants = vec![Variant {
            name: "app (0.30)".into(),
            gain_db,
            auto_gain: auto,
            threshold: 0.3,
        }];
        for t in [0.2, 0.1, 0.05] {
            variants.push(Variant {
                name: format!("thr {t:.2}"),
                gain_db,
                auto_gain: auto,
                threshold: t,
            });
        }
        variants.push(Variant {
            name: "no gain 0.30".into(),
            gain_db: 0.0,
            auto_gain: false,
            threshold: 0.3,
        });
        variants.push(Variant {
            name: "+20 dB 0.30".into(),
            gain_db: 20.0,
            auto_gain: false,
            threshold: 0.3,
        });
        for v in &variants {
            let (kept, marks) = run(&audio, state, v);
            let per = (0.25 * 16000.0 / (audio.len() as f32 / marks.len().max(1) as f32))
                .round()
                .max(1.0) as usize;
            let line: String = marks
                .chunks(per)
                .map(|c| if c.iter().any(|m| *m) { '#' } else { '.' })
                .collect();
            println!(
                "  {:<13} kept {:>5.1}s  {line}",
                v.name,
                kept.len() as f32 / 16000.0
            );
            if let Some(dir) = &out {
                let name = format!(
                    "{}-{}.wav",
                    record["id"].as_str().unwrap_or("x"),
                    v.name.replace([' ', '(', ')', '+'], "")
                );
                std::fs::create_dir_all(dir).unwrap();
                save_wav_file_sync(dir.join(name), &kept);
            }
        }
    }
}

fn save_wav_file_sync(path: PathBuf, samples: &[f32]) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)
            .unwrap();
    }
    w.finalize().unwrap();
}
