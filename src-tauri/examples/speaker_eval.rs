//! Score who-spoke-when, to compare speaker detection before and after a
//! change (see notes/speaker-identification-spec).
//!
//!   cargo run --release --example speaker_eval -- ami <ami dir> [<speaker model.onnx>]
//!   cargo run --release --example speaker_eval -- meetings [<meetings dir>]
//!
//! `ami`: every `rttm/<id>.rttm` with `audio/<id>.Mix-Headset.wav` and
//! `uem/<id>.uem` is run through the in-person speaker step as the app does
//! it (VAD with the in-person gain, then voice clustering) and scored
//! against the reference: diarization error rate, and the same with the
//! reference's speech in place of the VAD, which leaves only how well
//! voices are told apart. The model defaults to the app's download.
//!
//! `meetings`: for each meeting the user has gone through, the share of
//! speech whose speaker they changed with "Who said this?" (the fixes in
//! `speaker_fixes.json`), i.e. how much the automatic labels got wrong.
//! Prints no transcript text.

use handy_app_lib::meetings::manager::{apply_speaker_fixes, read_info, speaker_fixes};
use handy_app_lib::meetings::speaker_score::{from_rttm, from_uem, score, Score};
use handy_app_lib::meetings::transcript::{self, FRAME_MS};
use handy_app_lib::meetings::{diarize, pipeline};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn app_data() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME"))
        .join("Library/Application Support/com.pais.handy")
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("ami") => {
            let dir = PathBuf::from(args.next().ok_or("ami <dir>")?);
            let model = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| app_data().join("models").join(diarize::MODEL_FILE));
            ami(&dir, &model)
        }
        Some("meetings") => {
            let dir = args
                .next()
                .map(PathBuf::from)
                .unwrap_or_else(|| app_data().join("meetings"));
            meetings(&dir)
        }
        _ => Err("usage: speaker_eval ami <dir> [model] | meetings [dir]".into()),
    }
}

fn row(name: &str, s: &Score, secs: f64) {
    println!(
        "{name:<10} DER {:>5.1}%  missed {:>4.1}%  false alarm {:>4.1}%  confusion {:>4.1}%  people {:>2} → voices {:>2}  {secs:>5.0} s",
        100.0 * s.der(),
        100.0 * s.missed as f64 / s.reference.max(1) as f64,
        100.0 * s.false_alarm as f64 / s.reference.max(1) as f64,
        100.0 * s.confusion_rate(),
        s.reference_speakers,
        s.guessed_speakers,
    );
}

fn ami(dir: &Path, model: &Path) -> Result<(), String> {
    let vad = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/models/silero_vad_v4.onnx");
    let mut ids: Vec<String> = std::fs::read_dir(dir.join("rttm"))
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.path()
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
        })
        .collect();
    ids.sort();
    let (mut total, mut total_oracle) = (Score::default(), Score::default());
    let mut audio_secs = 0.0;
    let started = Instant::now();
    println!("Pipeline (VAD + voices):");
    let mut oracle_rows = Vec::new();
    for id in &ids {
        let wav = dir.join("audio").join(format!("{id}.Mix-Headset.wav"));
        let (Ok(rttm), Ok(uem)) = (
            std::fs::read_to_string(dir.join("rttm").join(format!("{id}.rttm"))),
            std::fs::read_to_string(dir.join("uem").join(format!("{id}.uem"))),
        ) else {
            continue;
        };
        if !wav.is_file() {
            eprintln!("{id}: no audio yet, skipped");
            continue;
        }
        let t = Instant::now();
        let analysis = pipeline::analyze(&wav, &vad, true)?;
        let frames = analysis.speech.len();
        audio_secs += frames as f64 * FRAME_MS as f64 / 1000.0;
        let reference = from_rttm(&rttm, FRAME_MS, frames);
        let scored = from_uem(&uem, FRAME_MS, frames);
        let guess = diarize::speakers(&wav, &analysis.speech, model, None)?;
        let s = score(&reference, &guess, &scored);
        row(id, &s, t.elapsed().as_secs_f64());
        total.add(&s);

        // The reference's speech instead of the VAD: voices only.
        let t = Instant::now();
        let speech: Vec<bool> = reference.iter().map(|r| !r.is_empty()).collect();
        let guess = diarize::speakers(&wav, &speech, model, None)?;
        let s = score(&reference, &guess, &scored);
        oracle_rows.push((id.clone(), s, t.elapsed().as_secs_f64()));
        total_oracle.add(&s);
    }
    row("all", &total, started.elapsed().as_secs_f64());
    println!("\nReference speech (voices only):");
    for (id, s, secs) in &oracle_rows {
        row(id, s, *secs);
    }
    row("all", &total_oracle, 0.0);
    println!(
        "\n{} meetings, {:.1} h of audio, {:.0} s per hour (both runs)",
        oracle_rows.len(),
        audio_secs / 3600.0,
        started.elapsed().as_secs_f64() / (audio_secs / 3600.0).max(1e-9)
    );
    Ok(())
}

fn meetings(dir: &Path) -> Result<(), String> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join(transcript::FILE).is_file())
        .collect();
    dirs.sort();
    let (mut all_ms, mut all_wrong) = (0u64, 0u64);
    for d in dirs {
        let name = d
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let (Some(info), Some(t)) = (read_info(&d), pipeline::load(&d)) else {
            continue;
        };
        let fixes = speaker_fixes(&d);
        if fixes.is_empty() {
            println!("{name}: not gone through (no speaker fixes)");
            continue;
        }
        let auto = transcript::paragraphs(&t.segments);
        let fixed = apply_speaker_fixes(auto.clone(), &fixes);
        let (mut ms, mut wrong) = (0u64, 0u64);
        for (a, f) in auto.iter().zip(&fixed) {
            let len = a.end_ms.saturating_sub(a.start_ms);
            ms += len;
            if info.speaker_label(a) != info.speaker_label(f) {
                wrong += len;
            }
        }
        println!(
            "{name}: {:.1}% of speech relabelled ({} paragraphs fixed)",
            100.0 * wrong as f64 / ms.max(1) as f64,
            fixes.len()
        );
        all_ms += ms;
        all_wrong += wrong;
    }
    if all_ms > 0 {
        println!(
            "all: {:.1}% of speech relabelled",
            100.0 * all_wrong as f64 / all_ms as f64
        );
    }
    Ok(())
}
