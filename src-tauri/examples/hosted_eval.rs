//! Compare speech models on your benchmark dictations: hosted ones with
//! your vocabulary, and models on this Mac, each given the audio the live
//! silence threshold keeps (what a dictation sends). Each transcript also
//! goes through Felix's cleanup. Scored against a reference reconciled by
//! ChatGPT from all transcripts (or your own ground truth), with fillers
//! and punctuation ignored.
//!
//!     cargo run --release --example hosted_eval -- --plan
//!     cargo run --release --example hosted_eval -- --pay [--key-file openrouter.key]
//!
//! `--plan` makes no network calls: clips, minutes and estimated cost.
//! Hosted models run only with `--pay`. `--only a,b` keeps candidates whose
//! label contains one of those; `--no-local`, `--no-cleanup`, `--limit N`,
//! `--local <model.gguf>` (repeatable; default: the four baselines).
//!
//! Results are cached per clip and model under the benchmark folder
//! (`hosted_eval/`), so reruns only do what's new. Transcripts stay there,
//! never printed; the report has numbers only.

use futures_util::StreamExt;
use handy_app_lib::audio_toolkit::audio::read_wav_samples;
use handy_app_lib::eval::{Guess, Heard, Record, Setup};
use handy_app_lib::meetings::remote::wav_bytes;
use handy_app_lib::vad_rescue::{self, Raw};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// (label, provider, OpenRouter model, list price per hour of audio).
const HOSTED: &[(&str, &str, Option<&str>, f64)] = &[
    (
        "MAI-Transcribe-2",
        "openrouter",
        Some("microsoft/mai-transcribe-2"),
        0.10,
    ),
    (
        "Gemini 3.5 Transcribe",
        "openrouter",
        Some("google/gemini-3.5-transcribe"),
        0.30,
    ),
    (
        "gpt-transcribe (OR)",
        "openrouter",
        Some("openai/gpt-transcribe"),
        0.27,
    ),
    (
        "AssemblyAI U-3.5 Pro",
        "openrouter",
        Some("assemblyai/universal-3-5-pro"),
        0.45,
    ),
    // Needs the account's 18+ confirmation on OpenRouter.
    // ("Muse Voice 1.0", "openrouter", Some("meta/muse-voice-transcribe-1.0"), 0.18),
    (
        "Grok STT 1.0",
        "openrouter",
        Some("x-ai/grok-stt-1.0"),
        0.10,
    ),
    ("Scribe v2", "elevenlabs", None, 0.27),
    ("gpt-transcribe", "openai", None, 0.27),
    ("Groq whisper turbo", "groq", None, 0.04),
];

const LOCAL: &[(&str, &str)] = &[
    ("Cohere (local)", "cohere-transcribe-03-2026-gguf"),
    ("Whisper turbo (local)", "whisper-large-v3-turbo-gguf"),
    ("Qwen3-ASR 1.7B (local)", "Qwen3-ASR-1.7B-gguf"),
    ("Parakeet v2 (local)", "parakeet-tdt-0.6b-v2-gguf"),
];

const FILLERS: &[&str] = &[
    "um", "uh", "uhm", "umm", "erm", "er", "ah", "hmm", "mm", "mhm",
];

#[derive(Serialize, Deserialize, Clone, Default)]
struct Heard1 {
    text: String,
    error: Option<String>,
    seconds: f64,
    cost: Option<f64>,
}

#[derive(Serialize, Deserialize, Clone, Default)]
struct Cleaned {
    input: String,
    output: String,
    used_ai: bool,
}

struct Clip {
    id: String,
    record: Record,
    audio: Vec<f32>,
    kind: &'static str,
}

fn rt<F: std::future::Future>(f: F) -> F::Output {
    tauri::async_runtime::block_on(f)
}

/// Clips still to do for a model: not cached, or cached as failed.
fn todo<'a>(clips: &'a [Clip], dir: &Path) -> Vec<&'a Clip> {
    clips
        .iter()
        .filter(|c| !c.audio.is_empty())
        .filter(|c| {
            read_json::<Heard1>(&dir.join(format!("{}.json", c.id)))
                .is_none_or(|h| h.error.is_some())
        })
        .collect()
}

fn slug(label: &str) -> String {
    label
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn read_json<T: for<'a> Deserialize<'a>>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, serde_json::to_string_pretty(value).unwrap());
}

/// Whispered, quiet or normal, from the speech frames: whispers have
/// almost no pitch (periodic frames), quiet speech has pitch but little
/// level.
fn kind_of(audio: &[f32]) -> (&'static str, f32, f32) {
    const FRAME: usize = 640;
    const HOP: usize = 320;
    let mut frames: Vec<(f32, &[f32])> = audio
        .windows(FRAME)
        .step_by(HOP)
        .map(|f| {
            (
                (f.iter().map(|x| x * x).sum::<f32>() / FRAME as f32).sqrt(),
                f,
            )
        })
        .collect();
    if frames.is_empty() {
        return ("silent", 0.0, -100.0);
    }
    frames.sort_by(|a, b| b.0.total_cmp(&a.0));
    let loud = frames[frames.len() / 10].0;
    let speech: Vec<&(f32, &[f32])> = frames.iter().filter(|f| f.0 > loud * 0.2).collect();
    let voiced = speech
        .iter()
        .filter(|(_, f)| {
            let energy: f32 = f.iter().map(|x| x * x).sum();
            (40..230).any(|lag| {
                let r: f32 = f[lag..].iter().zip(f.iter()).map(|(a, b)| a * b).sum();
                r / energy > 0.5
            })
        })
        .count();
    let ratio = voiced as f32 / speech.len().max(1) as f32;
    let rms = speech.iter().map(|f| f.0 * f.0).sum::<f32>() / speech.len().max(1) as f32;
    let db = 10.0 * rms.max(1e-10).log10();
    let kind = if ratio < 0.3 {
        "whispered"
    } else if db < -32.0 {
        "quiet"
    } else {
        "normal"
    };
    (kind, ratio, db)
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .replace(['-', '/', '_'], " ")
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty() && !FILLERS.contains(&w.as_str()))
        .collect()
}

/// (substitutions, deletions, insertions) turning `reference` into `hyp`.
#[allow(clippy::needless_range_loop)]
fn errors(reference: &[String], hyp: &[String]) -> (usize, usize, usize) {
    let (n, m) = (reference.len(), hyp.len());
    let mut d = vec![vec![(0usize, 0usize, 0usize, 0usize); m + 1]; n + 1];
    for i in 1..=n {
        d[i][0] = (i, 0, i, 0);
    }
    for j in 1..=m {
        d[0][j] = (j, 0, 0, j);
    }
    for i in 1..=n {
        for j in 1..=m {
            let same = reference[i - 1] == hyp[j - 1];
            let sub = d[i - 1][j - 1];
            let sub = (sub.0 + !same as usize, sub.1 + !same as usize, sub.2, sub.3);
            let del = d[i - 1][j];
            let del = (del.0 + 1, del.1, del.2 + 1, del.3);
            let ins = d[i][j - 1];
            let ins = (ins.0 + 1, ins.1, ins.2, ins.3 + 1);
            d[i][j] = [sub, del, ins].into_iter().min_by_key(|x| x.0).unwrap();
        }
    }
    let (_, s, del, ins) = d[n][m];
    (s, del, ins)
}

fn contains_term(text: &[String], term: &[String]) -> bool {
    !term.is_empty() && text.windows(term.len()).any(|w| w == term)
}

#[derive(Default)]
struct Score {
    ref_words: usize,
    errors: usize,
    insertions: usize,
    by_kind: BTreeMap<&'static str, (usize, usize)>,
    confident: (usize, usize),
    silent_words: usize,
    terms: (usize, usize),
    clean: (usize, usize),
    clean_terms: (usize, usize),
    used_ai: usize,
    cleaned: usize,
    latencies: Vec<f64>,
    cost: f64,
    failed: usize,
    clips: usize,
}

fn pct(a: (usize, usize)) -> String {
    if a.1 == 0 {
        "–".into()
    } else {
        format!("{:.1}%", 100.0 * a.0 as f64 / a.1 as f64)
    }
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let (mut plan, mut pay, mut no_local, mut no_cleanup) = (false, false, false, false);
    let (mut only, mut locals, mut limit, mut key_file): (Vec<String>, Vec<PathBuf>, usize, _) =
        (Vec::new(), Vec::new(), usize::MAX, None::<PathBuf>);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--plan" => plan = true,
            "--pay" => pay = true,
            "--no-local" => no_local = true,
            "--no-cleanup" => no_cleanup = true,
            "--only" => {
                only = args
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .map(|s| s.trim().to_lowercase())
                    .collect()
            }
            "--local" => locals.push(args.next().ok_or("--local <gguf>")?.into()),
            "--limit" => {
                limit = args
                    .next()
                    .and_then(|n| n.parse().ok())
                    .ok_or("--limit N")?
            }
            "--key-file" => key_file = Some(args.next().ok_or("--key-file <path>")?.into()),
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let home = PathBuf::from(std::env::var("HOME").map_err(|e| e.to_string())?);
    let app_data = home.join("Library/Application Support/com.pais.handy");
    let bench = app_data.join("benchmark");
    let cache = bench.join("hosted_eval");
    let mut setup = Setup::load(&app_data)?;
    if let Some(path) = key_file {
        let key = std::fs::read_to_string(&path).map_err(|e| format!("key file: {e}"))?;
        setup.use_api_key("openrouter", &key);
    }
    let keep =
        |label: &str| only.is_empty() || only.iter().any(|o| label.to_lowercase().contains(o));

    // Clips: the audio the live silence threshold keeps.
    let vad = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/models/silero_vad_v4.onnx");
    let mut ids: Vec<String> = std::fs::read_dir(&bench)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".json")
                .map(str::to_string)
        })
        .filter(|id| bench.join(format!("{id}.wav")).exists())
        .collect();
    ids.sort();
    ids.truncate(limit);
    let mut clips = Vec::new();
    for id in ids {
        let Some(record) = read_json::<Record>(&bench.join(format!("{id}.json"))) else {
            continue;
        };
        let raw = Raw {
            samples: read_wav_samples(bench.join(format!("{id}.wav")))
                .map_err(|e| e.to_string())?,
            gain_at_start: record.gain_at_start,
            gain_db: record.gain_db,
            auto_gain: record.auto_gain,
        };
        let audio = vad_rescue::sensitive_pass(&vad, &raw, record.vad_threshold.unwrap_or(0.3))
            .map_err(|e| e.to_string())?;
        let (kind, _, _) = if audio.is_empty() {
            ("silent", 0.0, 0.0)
        } else {
            kind_of(&audio)
        };
        clips.push(Clip {
            id,
            record,
            audio,
            kind,
        });
    }
    let minutes = clips.iter().map(|c| c.audio.len()).sum::<usize>() as f64 / 16000.0 / 60.0;
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &clips {
        *kinds.entry(c.kind).or_default() += 1;
    }
    println!(
        "{} clips, {:.1} min of kept audio; {:?}; vocabulary {} terms; cleanup {}",
        clips.len(),
        minutes,
        kinds,
        setup.vocabulary().len(),
        setup.cleanup_label()
    );

    // Candidates.
    let mut hosted = Vec::new();
    let mut total = 0.0;
    for &(label, provider, model, per_hour) in HOSTED {
        if !keep(label) {
            continue;
        }
        let todo: f64 = todo(&clips, &cache.join(slug(label)))
            .iter()
            .map(|c| c.audio.len() as f64 / 16000.0 / 3600.0 * per_hour)
            .sum();
        match setup.remote(provider, model) {
            Ok(remote) => {
                total += todo;
                println!("  {label:<24} ready, ~${todo:.2} to run");
                hosted.push((label, remote));
            }
            Err(e) => println!("  {label:<24} skipped: {e}"),
        }
    }
    let local_paths: Vec<(String, PathBuf)> = if no_local {
        Vec::new()
    } else if !locals.is_empty() {
        locals
            .iter()
            .map(|p| {
                (
                    p.file_stem().unwrap().to_string_lossy().to_string() + " (local)",
                    p.clone(),
                )
            })
            .collect()
    } else {
        let hub = home.join(".cache/huggingface/hub");
        LOCAL
            .iter()
            .filter(|(label, _)| keep(label))
            .filter_map(|(label, repo)| {
                let snaps = hub.join(format!("models--handy-computer--{repo}/snapshots"));
                let file = std::fs::read_dir(snaps).ok()?.flatten().find_map(|s| {
                    std::fs::read_dir(s.path())
                        .ok()?
                        .flatten()
                        .map(|f| f.path())
                        .find(|p| p.extension().is_some_and(|e| e == "gguf"))
                })?;
                Some((label.to_string(), file))
            })
            .collect()
    };
    for (label, path) in &local_paths {
        println!(
            "  {label:<24} {}",
            path.file_name().unwrap().to_string_lossy()
        );
    }
    println!("estimated hosted cost: ~${total:.2} (list prices; references and cleanup are free)");
    if plan {
        return Ok(());
    }

    let mut results: HashMap<String, HashMap<String, Heard1>> = HashMap::new();

    // Hosted, a few requests at a time per model.
    for (label, remote) in &hosted {
        let dir = cache.join(slug(label));
        let todo = todo(&clips, &dir);
        if !todo.is_empty() && !pay {
            println!("{label}: {} clips not run (add --pay)", todo.len());
        } else if !todo.is_empty() {
            let started = Instant::now();
            rt(futures_util::stream::iter(todo.iter().map(|c| async {
                let t = Instant::now();
                let r = remote.transcribe_wav(&wav_bytes(&c.audio)).await;
                let seconds = t.elapsed().as_secs_f64();
                let heard = match r {
                    Ok(r) => Heard1 {
                        text: setup.finish_cloud_text(r.text),
                        error: None,
                        seconds,
                        cost: r.cost,
                    },
                    Err(e) => Heard1 {
                        error: Some(e),
                        seconds,
                        ..Default::default()
                    },
                };
                write_json(&dir.join(format!("{}.json", c.id)), &heard);
            }))
            .buffer_unordered(2)
            .collect::<Vec<_>>());
            println!(
                "{label}: {} clips in {:.0} s",
                todo.len(),
                started.elapsed().as_secs_f64()
            );
        }
    }

    // Local, one model loaded at a time.
    for (label, path) in &local_paths {
        let dir = cache.join(slug(label));
        let todo = todo(&clips, &dir);
        if todo.is_empty() {
            continue;
        }
        let started = Instant::now();
        let model = transcribe_cpp::Model::load(path).map_err(|e| e.to_string())?;
        let mut session = model.session().map_err(|e| e.to_string())?;
        for c in &todo {
            let t = Instant::now();
            let heard = match setup.transcribe_local(&mut session, &c.audio) {
                Ok(text) => Heard1 {
                    text,
                    seconds: t.elapsed().as_secs_f64(),
                    ..Default::default()
                },
                Err(e) => Heard1 {
                    error: Some(e),
                    ..Default::default()
                },
            };
            write_json(&dir.join(format!("{}.json", c.id)), &heard);
        }
        println!(
            "{label}: {} clips in {:.0} s",
            todo.len(),
            started.elapsed().as_secs_f64()
        );
    }

    let labels: Vec<String> = hosted
        .iter()
        .map(|(l, _)| l.to_string())
        .chain(local_paths.iter().map(|(l, _)| l.clone()))
        .collect();
    for label in &labels {
        let dir = cache.join(slug(label));
        let got = results.entry(label.clone()).or_default();
        for c in &clips {
            if let Some(h) = read_json::<Heard1>(&dir.join(format!("{}.json", c.id))) {
                got.insert(c.id.clone(), h);
            }
        }
    }

    // References: your ground truth, else reconciled from every transcript.
    let mut references: HashMap<String, Guess> = HashMap::new();
    let mut asked = 0;
    for c in &clips {
        if let Some(truth) = &c.record.ground_truth {
            references.insert(
                c.id.clone(),
                Guess {
                    text: truth.clone(),
                    confident: true,
                    ..Default::default()
                },
            );
            continue;
        }
        if c.audio.is_empty() {
            references.insert(
                c.id.clone(),
                Guess {
                    confident: true,
                    ..Default::default()
                },
            );
            continue;
        }
        let path = cache.join("reference").join(format!("{}.json", c.id));
        if let Some(g) = read_json::<Guess>(&path) {
            references.insert(c.id.clone(), g);
            continue;
        }
        let live = c.record.transcript.iter().map(|t| Heard {
            by: "live dictation".into(),
            text: t.clone(),
        });
        let heard: Vec<Heard> = live
            .chain(labels.iter().filter_map(|l| {
                let h = results.get(l)?.get(&c.id)?;
                h.error.is_none().then(|| Heard {
                    by: l.clone(),
                    text: h.text.clone(),
                })
            }))
            .collect();
        if heard.len() < 3 {
            continue;
        }
        match rt(setup.reference(&c.record, heard)) {
            Ok(g) => {
                write_json(&path, &g);
                references.insert(c.id.clone(), g);
                asked += 1;
            }
            Err(e) => println!("reference for {} failed: {e}", c.id),
        }
    }
    if asked > 0 {
        println!("{asked} new references");
    }

    // Felix's cleanup of each transcript.
    let mut cleaned: HashMap<(String, String), Cleaned> = HashMap::new();
    if !no_cleanup {
        let started = Instant::now();
        let mut ran = 0;
        for c in clips.iter().filter(|c| references.contains_key(&c.id)) {
            for label in &labels {
                let Some(h) = results.get(label).and_then(|r| r.get(&c.id)) else {
                    continue;
                };
                if h.error.is_some() {
                    continue;
                }
                let path = cache
                    .join("cleanup")
                    .join(slug(label))
                    .join(format!("{}.json", c.id));
                let done = read_json::<Cleaned>(&path).filter(|d| d.input == h.text);
                let done = match done {
                    Some(d) => d,
                    None => {
                        let (output, used_ai) = rt(setup.clean(
                            &h.text,
                            c.record.app.as_deref(),
                            c.record.bundle_id.as_deref(),
                        ));
                        ran += 1;
                        let d = Cleaned {
                            input: h.text.clone(),
                            output,
                            used_ai,
                        };
                        write_json(&path, &d);
                        d
                    }
                };
                cleaned.insert((label.clone(), c.id.clone()), done);
            }
        }
        setup.stop_local_cleanup();
        if ran > 0 {
            println!("{ran} cleanups in {:.0} s", started.elapsed().as_secs_f64());
        }
    }

    // Score.
    let vocab: Vec<Vec<String>> = setup
        .vocabulary()
        .iter()
        .map(|w| words(w))
        .filter(|w| !w.is_empty())
        .collect();
    let mut report = String::new();
    report.push_str(&format!(
        "{} clips ({:?}), {:.1} min; scored on {} with a reference\n\n",
        clips.len(),
        kinds,
        minutes,
        references.len()
    ));
    report.push_str("| model | WER | confident clips | whispered | quiet | normal | inserted | words on silence | vocab hits | after cleanup | vocab after | AI kept | median s | p90 s | cost | failed |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for label in &labels {
        let mut s = Score::default();
        let per_hour = HOSTED.iter().find(|h| h.0 == label).map(|h| h.3);
        for c in &clips {
            let (Some(reference), Some(h)) = (
                references.get(&c.id),
                results.get(label).and_then(|r| r.get(&c.id)),
            ) else {
                continue;
            };
            s.clips += 1;
            if h.error.is_some() {
                s.failed += 1;
                continue;
            }
            s.latencies.push(h.seconds);
            s.cost += h
                .cost
                .or(per_hour.map(|p| c.audio.len() as f64 / 16000.0 / 3600.0 * p))
                .unwrap_or(0.0);
            let r = words(&reference.text);
            let hyp = words(&h.text);
            if r.is_empty() {
                s.silent_words += hyp.len();
                continue;
            }
            let (sub, del, ins) = errors(&r, &hyp);
            let e = sub + del + ins;
            s.ref_words += r.len();
            s.errors += e;
            s.insertions += ins;
            let k = s.by_kind.entry(c.kind).or_default();
            k.0 += e;
            k.1 += r.len();
            if reference.confident {
                s.confident.0 += e;
                s.confident.1 += r.len();
            }
            for term in vocab.iter().filter(|t| contains_term(&r, t)) {
                s.terms.1 += 1;
                s.terms.0 += contains_term(&hyp, term) as usize;
            }
            if let Some(cl) = cleaned.get(&(label.clone(), c.id.clone())) {
                let out = words(&cl.output);
                let (a, b, d) = errors(&r, &out);
                s.clean.0 += a + b + d;
                s.clean.1 += r.len();
                s.cleaned += 1;
                s.used_ai += cl.used_ai as usize;
                for term in vocab.iter().filter(|t| contains_term(&r, t)) {
                    s.clean_terms.1 += 1;
                    s.clean_terms.0 += contains_term(&out, term) as usize;
                }
            }
        }
        s.latencies.sort_by(f64::total_cmp);
        let at = |q: f64| {
            s.latencies
                .get(((s.latencies.len() as f64 - 1.0) * q).round() as usize)
                .copied()
                .unwrap_or(0.0)
        };
        let kind = |k: &str| pct(s.by_kind.get(k).copied().unwrap_or_default());
        let cost = if per_hour.is_some() {
            format!("${:.3}", s.cost)
        } else {
            "free".into()
        };
        report.push_str(&format!(
            "| {label} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {}/{} | {:.2} | {:.2} | {cost} | {}/{} |\n",
            pct((s.errors, s.ref_words)),
            pct(s.confident),
            kind("whispered"),
            kind("quiet"),
            kind("normal"),
            pct((s.insertions, s.ref_words)),
            s.silent_words,
            pct(s.terms),
            pct(s.clean),
            pct(s.clean_terms),
            s.used_ai,
            s.cleaned,
            at(0.5),
            at(0.9),
            s.failed,
            s.clips,
        ));
    }
    println!("\n{report}");
    write_json(&cache.join("report.json"), &report);
    let _ = std::fs::write(cache.join("report.md"), &report);
    Ok(())
}
