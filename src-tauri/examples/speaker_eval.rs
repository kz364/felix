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

/// Settings for a run, from the environment: GROUP=old for 1e32bda's
/// online pass, otherwise the whole-meeting grouping with NORM=0|1,
/// FIRST=<similarity>, STOP=<similarity>, MINW=<windows>.
fn grouping_from_env() -> Option<diarize::Grouping> {
    if std::env::var("GROUP").as_deref() == Ok("old") {
        return None;
    }
    let mut g = diarize::Grouping::default();
    let num = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f32>().ok());
    if let Some(v) = num("FIRST") {
        g.first_pass = v;
    }
    if let Some(v) = num("STOP") {
        g.stop = v;
    }
    if let Some(v) = num("MINW") {
        g.min_windows = v as usize;
    }
    if let Ok(v) = std::env::var("NORM") {
        g.normalise = v != "0";
    }
    Some(g)
}

/// Fingerprints for a meeting, from the cache when there.
struct Prints {
    speech: Vec<bool>,
    wins: Vec<(usize, usize)>,
    embs: Vec<Vec<f32>>,
}

fn cached(path: &Path, make: impl FnOnce() -> Result<Prints, String>) -> Result<Prints, String> {
    if let Ok(bytes) = std::fs::read(path) {
        if let Ok((speech, wins, embs)) =
            serde_json::from_slice::<(Vec<bool>, Vec<(usize, usize)>, Vec<Vec<f32>>)>(&bytes)
        {
            return Ok(Prints { speech, wins, embs });
        }
    }
    let p = make()?;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(
        path,
        serde_json::to_vec(&(&p.speech, &p.wins, &p.embs)).unwrap_or_default(),
    );
    Ok(p)
}

fn ami(dir: &Path, model: &Path) -> Result<(), String> {
    let vad = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/models/silero_vad_v4.onnx");
    let grouping = grouping_from_env();
    let quiet = std::env::var("QUIET").is_ok();
    // SEG=1: cut fingerprint windows at the segmentation model's turns.
    let seg_model = std::env::var("SEG").ok().filter(|v| v == "1").map(|_| {
        app_data()
            .join("models")
            .join(handy_app_lib::meetings::segment::MODEL_DIR)
            .join(handy_app_lib::meetings::segment::MODEL_FILE)
    });
    let mut model_name = model
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    if seg_model.is_some() {
        model_name.push_str("+seg");
    }
    let cache = dir.join("cache").join(&model_name);
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
    if let Ok(only) = std::env::var("ONLY") {
        ids.retain(|id| only.split(',').any(|o| o == id));
    }
    let (mut total, mut total_oracle) = (Score::default(), Score::default());
    let started = Instant::now();
    let mut rows = Vec::new();
    for id in &ids {
        let wav = dir.join("audio").join(format!("{id}.Mix-Headset.wav"));
        let (Ok(rttm), Ok(uem)) = (
            std::fs::read_to_string(dir.join("rttm").join(format!("{id}.rttm"))),
            std::fs::read_to_string(dir.join("uem").join(format!("{id}.uem"))),
        ) else {
            continue;
        };
        if !wav.is_file() {
            continue;
        }
        let t = Instant::now();
        let p = cached(&cache.join(format!("{id}.vad.json")), || {
            let speech = pipeline::analyze(&wav, &vad, true)?.speech;
            let (wins, embs, _) =
                diarize::fingerprint_with(&wav, &speech, model, seg_model.as_deref())?;
            Ok(Prints { speech, wins, embs })
        })?;
        let frames = p.speech.len();
        let reference = from_rttm(&rttm, FRAME_MS, frames);
        let scored = from_uem(&uem, FRAME_MS, frames);
        // HINTS=<share>: name hints from the reference (one person talking)
        // on that share of the speech, as the meetings extension would give.
        let share: f32 = std::env::var("HINTS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0);
        let hints: Vec<Option<String>> = reference
            .iter()
            .enumerate()
            .map(|(i, r)| match r.as_slice() {
                // Whole 3 s stretches on or off, so hints come in runs.
                [one] if ((i / 100) as f32 * 0.618).fract() < share => Some(format!("p{one}")),
                _ => None,
            })
            .collect();
        let label = |p: &Prints| -> Vec<Option<u32>> {
            if p.wins.is_empty() {
                return vec![None; p.speech.len()];
            }
            let speakers = match &grouping {
                Some(g) => {
                    let names = diarize::window_names(&p.wins, &hints);
                    diarize::group_voices_with(&p.embs, g, &names).0
                }
                None => diarize::cluster(&p.embs),
            };
            diarize::frame_labels(&p.speech, &p.wins, &speakers)
        };
        let s = score(&reference, &label(&p), &scored);
        let secs = t.elapsed().as_secs_f64();
        total.add(&s);

        let t = Instant::now();
        let o = cached(&cache.join(format!("{id}.ref.json")), || {
            let speech: Vec<bool> = reference.iter().map(|r| !r.is_empty()).collect();
            let (wins, embs, _) =
                diarize::fingerprint_with(&wav, &speech, model, seg_model.as_deref())?;
            Ok(Prints { speech, wins, embs })
        })?;
        let so = score(&reference, &label(&o), &scored);
        total_oracle.add(&so);
        rows.push((id.clone(), s, secs, so, t.elapsed().as_secs_f64()));
    }
    if !quiet {
        println!("Pipeline (VAD + voices):");
        for (id, s, secs, _, _) in &rows {
            row(id, s, *secs);
        }
    }
    row("all", &total, started.elapsed().as_secs_f64());
    if !quiet {
        println!("\nReference speech (voices only):");
        for (id, _, _, s, secs) in &rows {
            row(id, s, *secs);
        }
    }
    row("all ref", &total_oracle, 0.0);
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
