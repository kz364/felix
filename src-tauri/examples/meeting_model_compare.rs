//! Like `meeting_transcribe`, but for comparing local models against each
//! other on a recording where at least one model is known to choke on a
//! chunk: instead of aborting the whole run on a chunk failure, this logs the
//! failure (chunk start time + error), counts it, and carries on with empty
//! text for that chunk so the rest of the meeting still gets transcribed.
//!
//!   cargo run --release --example meeting_model_compare -- <model.gguf> <meeting dir> [call|in_person]
//!
//! A model path of `openrouter` (or `openrouter:<model>`) transcribes with
//! OpenRouter as a meeting would, with your key; it needs LIKE_APP.
//!
//! LIKE_APP=<language> runs each chunk as Felix does, with your settings and
//! vocabulary, held to that language (`auto` for none); otherwise the model
//! runs bare.
//!
//! Writes `transcript.json` (the app's resumable format) and `transcript.txt`
//! (plain text) into the folder. At the end prints a summary: model, wall
//! time, real-time factor, chunk total/failed counts, word count, and flags
//! any chunk whose text has a phrase repeated 4+ times in a row (a
//! hallucination loop).

use handy_app_lib::meetings::pipeline::{self, Stopped};
use handy_app_lib::meetings::transcript::{self, Source};
use handy_app_lib::meetings::MeetingMode;
use std::path::Path;
use std::time::Instant;
use transcribe_cpp::{Model, RunOptions};

/// A chunk that errored out instead of producing text.
struct FailedChunk {
    index: usize,
    error: String,
}

/// True if `text` contains some phrase (of a few words or more) repeated 4+
/// times back to back — the shape of a decode stuck in a loop.
fn has_repeat_loop(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 8 {
        return false;
    }
    // Try phrase lengths from 1 to 8 words.
    for phrase_len in 1..=8 {
        let mut run = 1;
        let mut i = phrase_len;
        while i + phrase_len <= words.len() {
            if words[i..i + phrase_len] == words[i - phrase_len..i] {
                run += 1;
                if run >= 4 {
                    return true;
                }
            } else {
                run = 1;
            }
            i += phrase_len;
        }
    }
    false
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let model_path = args.next().expect("model path");
    let dir_arg = args.next().expect("meeting dir");
    let mode = match args.next().as_deref() {
        Some("in_person") => MeetingMode::InPerson,
        _ => MeetingMode::Call,
    };
    let dir = Path::new(&dir_arg);
    let vad = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/models/silero_vad_v4.onnx");

    let model_name = Path::new(&model_path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| model_path.clone());

    let cloud = model_path
        .strip_prefix("openrouter")
        .map(|m| m.trim_start_matches(':').to_string());
    let mut session = match &cloud {
        Some(_) => None,
        None => {
            eprintln!("loading {model_name}...");
            let load_started = Instant::now();
            let model = Model::load(&model_path)?;
            eprintln!("loaded in {:.1}s", load_started.elapsed().as_secs_f64());
            Some(model.session()?)
        }
    };

    let options = RunOptions::default();
    let like_app = std::env::var("LIKE_APP").ok();
    let setup = match &like_app {
        Some(_) => {
            let app_data = Path::new(&std::env::var("HOME")?)
                .join("Library/Application Support/com.pais.handy");
            let mut setup = handy_app_lib::eval::Setup::load(&app_data)?;
            setup.as_in_meetings();
            // This meeting's invite names, as Felix adds them.
            setup.add_vocabulary(
                handy_app_lib::meetings::calendar::name_words(dir, 0, 0)
                    .iter()
                    .map(String::as_str),
            );
            Some(setup)
        }
        None => None,
    };
    let remote = match (&cloud, &setup) {
        (Some(m), Some(setup)) => {
            let langs: Vec<String> = like_app.iter().filter(|l| *l != "auto").cloned().collect();
            Some(
                setup
                    .remote("openrouter", Some(m.as_str()).filter(|m| !m.is_empty()))?
                    .for_languages(&langs),
            )
        }
        (Some(_), None) => return Err("openrouter needs LIKE_APP".into()),
        _ => None,
    };
    let language = like_app.as_deref().filter(|l| *l != "auto");
    let started = Instant::now();
    let mut audio_secs = 0.0f64;
    let mut chunk_index = 0usize;
    let mut failed: Vec<FailedChunk> = Vec::new();

    let level = match std::env::var("LEVEL").as_deref() {
        Ok("off") => None,
        _ => Some(handy_app_lib::meetings::level::LevelSettings::new(
            0.0, true,
        )),
    };
    let speakers = std::env::var("SPEAKERS").ok();

    let t = pipeline::run(
        dir,
        mode,
        &vad,
        level,
        speakers.as_deref().map(Path::new),
        None,
        "example-compare",
        |audio| -> Result<String, Stopped> {
            let this_index = chunk_index;
            chunk_index += 1;
            audio_secs += audio.len() as f64 / 16_000.0;
            let result = match (&remote, &setup, session.as_mut()) {
                (Some(remote), Some(setup), _) => {
                    // Rate limits: wait and try again rather than lose the chunk.
                    let mut tries = 0;
                    loop {
                        match tauri::async_runtime::block_on(remote.transcribe(&audio)) {
                            Err(e) if e.contains("429") && tries < 6 => {
                                tries += 1;
                                std::thread::sleep(std::time::Duration::from_secs(10 * tries));
                            }
                            other => break other.map(|raw| setup.finish_cloud_text(raw)),
                        }
                    }
                }
                (_, Some(setup), Some(session)) => {
                    setup.transcribe_meeting(session, &audio, language)
                }
                (_, _, Some(session)) => session
                    .run(&audio, &options)
                    .map(|r| r.text)
                    .map_err(|e| e.to_string()),
                _ => Err("no model".to_string()),
            };
            match result {
                Ok(text) => Ok(text),
                Err(e) => {
                    eprintln!(
                        "\nchunk #{this_index} failed ({:.1}s of audio): {e}",
                        audio.len() as f64 / 16_000.0
                    );
                    failed.push(FailedChunk {
                        index: this_index,
                        error: e.to_string(),
                    });
                    // Continue with empty text instead of aborting the run.
                    Ok(String::new())
                }
            }
        },
        |step| match step {
            pipeline::Step::Identifying => eprintln!("telling speakers apart"),
            pipeline::Step::Transcribing { done, total } => eprint!("\r{done}/{total} chunks"),
        },
    )
    .map_err(|e| format!("{e:?}"))?;

    let wall = started.elapsed().as_secs_f64();
    eprintln!(
        "\n{:.1}s of speech in {:.1}s wall ({} model)",
        audio_secs, wall, model_name
    );

    for s in &t.segments {
        eprintln!(
            "  {:?}{} {:>6}-{:>6} ms{} {}",
            s.source,
            s.speaker
                .map(|n| format!(" S{}", n + 1))
                .unwrap_or_default(),
            s.start_ms,
            s.end_ms,
            if s.echo { " ECHO" } else { "" },
            s.text
        );
    }

    let label = |p: &transcript::Paragraph| match (mode, p.source) {
        (MeetingMode::Call, Source::Mic) => Some("Me".to_string()),
        (MeetingMode::Call, Source::System) => Some("Them".to_string()),
        _ => p.speaker.map(|n| format!("Speaker {}", n + 1)),
    };
    let paragraphs = transcript::paragraphs(&t.segments);
    let text = transcript::to_text(&paragraphs, label);
    println!("{}", text);
    std::fs::write(dir.join("transcript.txt"), &text)?;

    // Flag segments whose own text loops (in addition to chunks that errored
    // out and were left empty).
    let mut loop_flags: Vec<(usize, u64, String)> = Vec::new();
    for (i, s) in t.segments.iter().enumerate() {
        if has_repeat_loop(&s.text) {
            let excerpt: String = s.text.chars().take(120).collect();
            loop_flags.push((i, s.start_ms, excerpt));
        }
    }

    let word_count = t
        .segments
        .iter()
        .map(|s| s.text.split_whitespace().count())
        .sum::<usize>();
    let total_chunks = t.segments.len();
    let rtf = if wall > 0.0 { audio_secs / wall } else { 0.0 };

    println!("\n==== summary: {model_name} ====");
    println!("wall time:      {:.1}s", wall);
    println!(
        "real-time factor: {:.2}x (audio_secs / wall_secs; >1 = faster than real time)",
        rtf
    );
    println!(
        "chunks:         {} total, {} failed",
        total_chunks,
        failed.len()
    );
    println!("words:          {}", word_count);
    if failed.is_empty() {
        println!("failed chunks:  none");
    } else {
        println!("failed chunks:");
        for f in &failed {
            println!("  #{} : {}", f.index, f.error);
        }
    }
    if loop_flags.is_empty() {
        println!("loop flags:     none");
    } else {
        println!("loop flags (phrase repeated 4+ times in a row):");
        for (i, start_ms, excerpt) in &loop_flags {
            println!("  segment #{i} at {}ms: \"{excerpt}...\"", start_ms);
        }
    }

    Ok(())
}
