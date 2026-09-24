//! Evaluate custom-vocabulary biasing for one model.
//!
//!     cargo run --release --example bias_eval -- <model.gguf> <manifest.json> [strength ...]
//!
//! manifest.json: {"vocab": [..], "clips": [{"wav", "ref", "kind": "pos"|"ctrl"}]}
//!
//! Runs a no-vocabulary baseline, then the model's native mechanism: Whisper
//! initial prompt, Qwen3-ASR recognition context, or keyword boosting at each
//! given strength (0 = the family's calibrated default). Reports
//! vocabulary-term recall on "pos" clips, false insertions of vocabulary terms
//! anywhere, WER (all clips and controls only), runaway decodes (output
//! truncated) and dropped content (output under 70% of the reference length).

use handy_app_lib::audio_toolkit::read_wav_samples;
use serde::Deserialize;
use transcribe_cpp::{Feature, Model, RunExtension, RunOptions, WhisperRunOptions};

#[derive(Deserialize)]
struct Manifest {
    vocab: Vec<String>,
    clips: Vec<Clip>,
}

#[derive(Deserialize)]
struct Clip {
    wav: String,
    #[serde(rename = "ref")]
    reference: String,
    kind: String,
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn edit_distance(a: &[String], b: &[String]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, y) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(x != y))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Occurrences of a (possibly multi-word) term as whole words.
fn count_term(haystack: &[String], term: &[String]) -> usize {
    if term.is_empty() || haystack.len() < term.len() {
        return 0;
    }
    haystack.windows(term.len()).filter(|w| *w == term).count()
}

#[derive(Default)]
struct Score {
    term_total: usize,
    term_hits: usize,
    false_insertions: usize,
    errs_all: usize,
    words_all: usize,
    errs_ctrl: usize,
    words_ctrl: usize,
    runaways: usize,
    dropped: usize,
    ms: u128,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let model_path = args.next().expect("model path");
    let manifest: Manifest = serde_json::from_str(&std::fs::read_to_string(
        args.next().expect("manifest path"),
    )?)?;
    let strengths: Vec<f32> = args.map(|s| s.parse().expect("strength")).collect();

    let model = Model::load(&model_path)?;
    let arch = model.arch().to_string();
    let boost = model.supports(Feature::KeywordBoost);
    let context = model.supports(Feature::Context);
    let whisper = arch == "whisper";
    let mut session = model.session()?;

    let audio: Vec<Vec<f32>> = manifest
        .clips
        .iter()
        .map(|c| read_wav_samples(&c.wav))
        .collect::<Result<_, _>>()?;
    let vocab: Vec<Vec<String>> = manifest.vocab.iter().map(|t| words(t)).collect();

    // (label, options builder)
    let mut configs: Vec<(String, RunOptions)> = vec![("baseline".into(), base_options())];
    if boost {
        for &s in &strengths {
            let mut o = base_options();
            o.bias_phrases = manifest.vocab.clone();
            o.bias_strength = s;
            configs.push((format!("boost {s:.1}"), o));
        }
    }
    if context {
        let mut o = base_options();
        o.context = Some(format!("Vocabulary: {}", manifest.vocab.join(", ")));
        configs.push(("context".into(), o));
    }
    if whisper {
        let mut o = base_options();
        o.family = Some(RunExtension::Whisper(WhisperRunOptions {
            initial_prompt: Some(manifest.vocab.join(", ")),
            ..Default::default()
        }));
        configs.push(("prompt".into(), o));
    }

    println!("model: {arch}  ({model_path})");
    println!(
        "{:<12} {:>12} {:>10} {:>8} {:>9} {:>8} {:>8} {:>8}",
        "config", "term recall", "false ins", "WER", "ctrl WER", "runaway", "dropped", "ms/clip"
    );
    for (label, options) in &configs {
        let mut sc = Score::default();
        for (clip, pcm) in manifest.clips.iter().zip(&audio) {
            let t0 = std::time::Instant::now();
            let hyp = match session.run(pcm, options) {
                Ok(t) => t.text,
                Err(transcribe_cpp::Error::OutputTruncated { partial, .. }) => {
                    sc.runaways += 1;
                    partial.map(|p| p.text).unwrap_or_default()
                }
                Err(e) => return Err(e.into()),
            };
            sc.ms += t0.elapsed().as_millis();
            let r = words(&clip.reference);
            let h = words(&hyp);
            let errs = edit_distance(&r, &h);
            // Content loss: the decoder skipped part of the utterance.
            if (h.len() as f64) < 0.7 * r.len() as f64 {
                sc.dropped += 1;
            }
            sc.errs_all += errs;
            sc.words_all += r.len();
            if clip.kind == "ctrl" {
                sc.errs_ctrl += errs;
                sc.words_ctrl += r.len();
            }
            for term in &vocab {
                let in_ref = count_term(&r, term);
                let in_hyp = count_term(&h, term);
                sc.term_total += in_ref;
                sc.term_hits += in_hyp.min(in_ref);
                sc.false_insertions += in_hyp.saturating_sub(in_ref);
            }
            if std::env::var_os("BIAS_EVAL_VERBOSE").is_some() {
                println!("  [{label}] {} | {}", clip.reference, hyp.trim());
            }
        }
        let pct = |a: usize, b: usize| 100.0 * a as f64 / b.max(1) as f64;
        println!(
            "{:<12} {:>11.1}% {:>10} {:>7.1}% {:>8.1}% {:>8} {:>8} {:>8}",
            label,
            pct(sc.term_hits, sc.term_total),
            sc.false_insertions,
            pct(sc.errs_all, sc.words_all),
            pct(sc.errs_ctrl, sc.words_ctrl),
            sc.runaways,
            sc.dropped,
            sc.ms / manifest.clips.len() as u128
        );
    }
    Ok(())
}

fn base_options() -> RunOptions {
    RunOptions {
        language: Some("en".into()),
        ..Default::default()
    }
}
