//! Transcribe a recorded meeting folder (mic.wav, system.wav) the way the app
//! does, with a transcribe-cpp model loaded directly, and print the transcript.
//!
//!   cargo run --example meeting_transcribe -- <model.gguf> <meeting dir> [call|in_person] [summarize <chatgpt model>]
//!
//! Writes `transcript.json` into the folder (delete it to start over). With
//! `summarize`, also cleans up the transcript and writes the summary with
//! ChatGPT (the app's own sign-in), using `notes.md` from the folder, and
//! prints the Markdown.
//!
//! ENGINE=<name> is recorded in the transcript (default "example"). Chunks a
//! model loops on are split and tried again, as in the app. On a call, the
//! names the call app showed are put on the voices and written to
//! `app_speakers.json` for `meeting.json`.

use handy_app_lib::meetings::llm::Llm;
use handy_app_lib::meetings::transcript::{self, Source};
use handy_app_lib::meetings::MeetingMode;
use handy_app_lib::meetings::{pipeline, summary};
use std::path::Path;
use std::time::Instant;
use transcribe_cpp::{Model, RunOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let model_path = args.next().expect("model path");
    let dir = args.next().expect("meeting dir");
    let mode = match args.next().as_deref() {
        Some("in_person") => MeetingMode::InPerson,
        _ => MeetingMode::Call,
    };
    let summarize_with = match args.next().as_deref() {
        Some("summarize") => Some(args.next().unwrap_or_else(|| "gpt-5.4-mini".into())),
        _ => None,
    };
    let vad = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/models/silero_vad_v4.onnx");

    let model = Model::load(&model_path)?;
    let mut session = model.session()?;
    let options = RunOptions::default();
    let started = Instant::now();
    let mut audio_secs = 0.0;
    let level = match std::env::var("LEVEL").as_deref() {
        Ok("off") => None,
        _ => Some(handy_app_lib::meetings::level::LevelSettings::new(
            0.0, true,
        )),
    };
    // SPEAKERS=<wespeaker .onnx> tells voices apart in person.
    let speakers = std::env::var("SPEAKERS").ok();
    let t = pipeline::run(
        Path::new(&dir),
        mode,
        &vad,
        level,
        speakers.as_deref().map(Path::new),
        None,
        &std::env::var("ENGINE").unwrap_or_else(|_| "example".into()),
        |audio| {
            audio_secs += audio.len() as f64 / 16_000.0;
            let mut run = |a: &[f32]| {
                session
                    .run(a, &options)
                    .map(|r| r.text)
                    .map_err(|e| e.to_string())
            };
            handy_app_lib::meetings::split_runaways(&mut run, &audio)
        },
        |step| match step {
            pipeline::Step::Identifying => eprintln!("telling speakers apart"),
            pipeline::Step::Transcribing { done, total } => eprint!("\r{done}/{total} chunks"),
        },
    )
    .map_err(|e| format!("{e:?}"))?;
    eprintln!(
        "\n{:.1}s of speech in {:.1}s",
        audio_secs,
        started.elapsed().as_secs_f64()
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
    if mode == MeetingMode::Call {
        let names = handy_app_lib::meetings::speakers::apply(Path::new(&dir));
        eprintln!("names from the call app: {names:?}");
        std::fs::write(
            Path::new(&dir).join("app_speakers.json"),
            serde_json::to_string(&names)?,
        )?;
    }
    let label = |p: &transcript::Paragraph| match (mode, p.source) {
        (MeetingMode::Call, Source::Mic) => Some("Me".to_string()),
        (MeetingMode::Call, Source::System) => Some("Them".to_string()),
        _ => p.speaker.map(|n| format!("Speaker {}", n + 1)),
    };
    let paragraphs = transcript::paragraphs(&t.segments);
    println!("{}", transcript::to_text(&paragraphs, label));

    if let Some(model) = summarize_with {
        let llm = Llm::Chatgpt { model };
        let dir = Path::new(&dir);
        let notes = std::fs::read_to_string(dir.join("notes.md")).unwrap_or_default();
        let speaker = |p: &transcript::Paragraph| label(p).unwrap_or_else(|| "Speaker".into());
        let (paragraphs, s) = tauri::async_runtime::block_on(async {
            let started = Instant::now();
            let mut cleaned = summary::Cleaned::default();
            if let Some(e) = summary::clean(
                &llm,
                &paragraphs,
                &speaker,
                &[],
                &[],
                &mut cleaned,
                |_, _| {},
            )
            .await
            {
                eprintln!("cleanup error: {e}");
            }
            eprintln!("cleanup took {:.1}s", started.elapsed().as_secs_f64());
            let paragraphs = summary::apply_cleaned(paragraphs.clone(), Some(&cleaned));
            let lines: Vec<String> = transcript::to_text(&paragraphs, label)
                .lines()
                .map(String::from)
                .collect();
            let about = match mode {
                MeetingMode::Call => {
                    "A video call. \"Me\" is the user; \"Them\" is everyone else on the call."
                }
                MeetingMode::InPerson => "An in-person meeting; speakers aren't labelled.",
            };
            let started = Instant::now();
            let s = summary::summarize(
                &llm,
                "",
                about,
                &notes,
                &lines,
                &Default::default(),
                |_, _| {},
            )
            .await;
            eprintln!("summary took {:.1}s", started.elapsed().as_secs_f64());
            (paragraphs, s)
        });
        let s = s?;
        let lines: Vec<String> = paragraphs
            .iter()
            .map(|p| match label(p) {
                Some(who) => format!(
                    "**[{}] {who}:** {}",
                    transcript::timestamp(p.start_ms),
                    p.text
                ),
                None => format!("**[{}]** {}", transcript::timestamp(p.start_ms), p.text),
            })
            .collect();
        println!(
            "\n----\n{}",
            summary::to_markdown(&s.title, "test", Some(&s), &notes, &lines)
        );
    }
    Ok(())
}
