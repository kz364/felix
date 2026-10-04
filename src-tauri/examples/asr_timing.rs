//! Where a local speech model's time goes on benchmark clips: load, the
//! first (cold) run, then per clip mel/encode/decode: plain, with a vocabulary
//! on a fresh session per clip, and with the vocabulary on one reused session
//! (where a model can keep the vocabulary prompt cached), checking the reused
//! session's text matches the fresh one. Set TRANSCRIBE_PERF_DEBUG=1 for the
//! finer split.
//!
//!     cargo run --release --example asr_timing -- <model.gguf> <words.txt|-> <benchmark/id.wav>...

use handy_app_lib::audio_toolkit::audio::read_wav_samples;
use std::time::Instant;
use transcribe_cpp::{Feature, Model, RunOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("model path");
    let words: Vec<String> = match args.next().as_deref() {
        Some("-") | None => Vec::new(),
        Some(file) => std::fs::read_to_string(file)?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
    };
    let clips: Vec<Vec<f32>> = args
        .map(|p| read_wav_samples(p).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;

    let t = Instant::now();
    let model = Model::load(&path)?;
    let load = t.elapsed();
    let t = Instant::now();
    let mut session = model.session()?;
    let session_ms = t.elapsed();
    let takes_context = model.supports(Feature::Context);
    let takes_boost = model.supports(Feature::KeywordBoost);
    println!(
        "{}: load {:.0} ms, session {:.0} ms, context={takes_context} boost={takes_boost}",
        model.arch(),
        load.as_secs_f64() * 1e3,
        session_ms.as_secs_f64() * 1e3
    );
    let mut fresh_texts = Vec::new();
    for (pass, with_words, fresh) in [
        ("plain", false, false),
        ("fresh", true, true),
        ("reuse", true, false),
    ] {
        if with_words && words.is_empty() {
            break;
        }
        let options = RunOptions {
            language: Some("en".into()),
            context: (with_words && takes_context)
                .then(|| format!("Vocabulary: {}", words.join(", "))),
            bias_phrases: if with_words && takes_boost {
                words.clone()
            } else {
                Vec::new()
            },
            ..Default::default()
        };
        let (mut total, mut audio_s) = (0.0, 0.0);
        for (i, clip) in clips.iter().enumerate() {
            if fresh {
                session = model.session()?;
            }
            let t = Instant::now();
            let out = match session.run(clip, &options) {
                Ok(out) => out,
                Err(e) => {
                    let e = e.to_string();
                    println!(
                        "  {pass} {i}: failed: {}",
                        e.chars().take(120).collect::<String>()
                    );
                    if fresh {
                        fresh_texts.push(String::new());
                    }
                    continue;
                }
            };
            let wall = t.elapsed().as_secs_f64() * 1e3;
            let tm = out.timings;
            println!(
                "  {pass} {i}: {:.1}s audio, {:>4} words: wall {wall:>6.0} ms = mel {:.0} + encode {:.0} + decode {:.0}{}",
                clip.len() as f32 / 16000.0,
                out.text.split_whitespace().count(),
                tm.mel_ms,
                tm.encode_ms,
                tm.decode_ms,
                match (pass, fresh_texts.get(i)) {
                    ("reuse", Some(f)) if *f == out.text => " (same text as fresh)",
                    ("reuse", Some(_)) => " (TEXT DIFFERS from fresh)",
                    _ => "",
                }
            );
            if std::env::var("SHOW_TEXT").is_ok() {
                println!(
                    "    text: {}",
                    out.text.chars().take(300).collect::<String>()
                );
            }
            if fresh {
                fresh_texts.push(out.text.clone());
            }
            if i > 0 {
                total += wall;
                audio_s += clip.len() as f64 / 16000.0;
            }
        }
        println!(
            "  {pass} warm: {:.0} ms per audio second",
            total / audio_s.max(1e-9)
        );
    }
    Ok(())
}
