//! Benchmark recording (Settings → For developers). While it's on, every
//! dictation leaves two files in `benchmark/` under the app data folder:
//!
//! * `<id>.wav`: the microphone audio *before* input gain and VAD (16 kHz
//!   mono), so it can be replayed with gain on, off or at other settings;
//! * `<id>.json`: how it was recorded (gain, learned levels, VAD, mic,
//!   model), the raw transcript, what was pasted and, as a candidate ground
//!   truth, the pasted text after any edits you made in the field.
//!
//! Edits come from `edit_learning`, which follows the field the dictation
//! went into; only the dictation's span is stored, never the rest of the
//! field. Nothing here runs unless the setting is on, and all of it runs off
//! the dictation's path.

use crate::audio_toolkit::GainState;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::PathBuf;
use std::sync::Mutex;
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

pub const DIR: &str = "benchmark";

#[derive(Serialize, Deserialize, Debug, Clone, Default, Type)]
#[serde(default, rename = "BenchmarkRecord")]
pub struct Record {
    pub id: String,
    pub at: String,
    /// Raw audio file next to this record.
    pub audio: String,
    pub seconds: f32,
    /// Audio the speech model got after VAD (0 = VAD dropped everything).
    pub kept_seconds: f32,
    pub gain_db: f32,
    pub auto_gain: bool,
    /// What the AGC had learned when this recording started.
    pub gain_at_start: GainState,
    pub vad_backend: String,
    pub microphone: Option<String>,
    /// Silero threshold override for this microphone (`None`: default 0.3).
    pub vad_threshold: Option<f32>,
    pub language: String,
    pub app: Option<String>,
    pub bundle_id: Option<String>,
    /// Speech model that produced `transcript`.
    pub model: Option<String>,
    /// What the speech model heard, before rules and cleanup.
    pub transcript: Option<String>,
    pub error: Option<String>,
    /// Exactly what was pasted.
    pub pasted: Option<String>,
    /// The pasted text as it stood in the field after your edits.
    pub edited: Option<String>,
    /// "unchanged", "edited", "rewritten" (too different to trust as ground
    /// truth), or why the field couldn't be followed.
    pub edit: Option<String>,
    /// What was actually said, typed or confirmed by you.
    pub ground_truth: Option<String>,
    /// An OpenAI model's best guess at what was said, when asked for.
    pub guess: Option<Guess>,
    /// The quiet-speech safety net, when it transcribed a second time.
    pub rescue: Option<Rescue>,
}

/// A second transcription of audio a more sensitive silence threshold kept.
#[derive(Serialize, Deserialize, Debug, Clone, Default, Type)]
#[serde(default)]
pub struct Rescue {
    pub threshold: f32,
    pub kept_seconds: f32,
    pub transcript: Option<String>,
    /// Whether it replaced the first transcript.
    pub used: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, Type)]
#[serde(default)]
pub struct Guess {
    /// Word for word as spoken; only obvious mishearings corrected.
    pub text: String,
    pub confident: bool,
    /// Words the sources disagreed on.
    pub unsure: Vec<String>,
    pub notes: String,
    /// Model that wrote the guess.
    pub by: String,
    /// The transcripts it was given.
    pub heard: Vec<Heard>,
    pub at: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default, Type)]
pub struct Heard {
    pub by: String,
    pub text: String,
}

type Job = Box<dyn FnOnce() + Send>;

/// All file work runs in order on one background thread, so an update never
/// lands before its record exists or races another update.
static QUEUE: Lazy<Mutex<std::sync::mpsc::Sender<Job>>> = Lazy::new(|| {
    let (tx, rx) = std::sync::mpsc::channel::<Job>();
    std::thread::spawn(move || {
        for job in rx {
            job();
        }
    });
    Mutex::new(tx)
});

fn enqueue(job: impl FnOnce() + Send + 'static) {
    let _ = QUEUE.lock().unwrap().send(Box::new(job));
}

/// Run a job on the queue and wait for its result.
fn queued<T: Send + 'static>(job: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (tx, rx) = std::sync::mpsc::channel();
    enqueue(move || {
        let _ = tx.send(job());
    });
    rx.recv().ok()
}

fn read(dir: &std::path::Path, id: &str) -> Option<Record> {
    std::fs::read_to_string(dir.join(format!("{id}.json")))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
}

/// Read, change and save one record in order with every other write.
fn change_now(
    app: &AppHandle,
    id: &str,
    change: impl FnOnce(&mut Record) + Send + 'static,
) -> Result<Record, String> {
    let dir = dir(app).ok_or("No app data folder")?;
    let id = id.to_string();
    queued(move || {
        let mut record = read(&dir, &id).ok_or("That recording is gone")?;
        change(&mut record);
        write(&dir, &record);
        Ok(record)
    })
    .unwrap_or_else(|| Err("Couldn't save".into()))
}
static LAST_ID: Lazy<Mutex<String>> = Lazy::new(|| Mutex::new(String::new()));

pub fn dir(app: &AppHandle) -> Option<PathBuf> {
    crate::portable::app_data_dir(app).ok().map(|d| d.join(DIR))
}

fn new_id() -> String {
    let base = chrono::Local::now().format("%Y%m%d-%H%M%S-%3f").to_string();
    let mut last = LAST_ID.lock().unwrap();
    let id = if *last == base {
        format!("{base}b")
    } else {
        base
    };
    *last = id.clone();
    id
}

fn write(dir: &std::path::Path, record: &Record) {
    let path = dir.join(format!("{}.json", record.id));
    match serde_json::to_string_pretty(record) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                log::warn!("Benchmark: couldn't write {}: {e}", path.display());
            }
        }
        Err(e) => log::warn!("Benchmark: couldn't serialize a record: {e}"),
    }
}

/// Change a saved record, in the background.
pub fn update(app: &AppHandle, id: &str, change: impl FnOnce(&mut Record) + Send + 'static) {
    let Some(dir) = dir(app) else { return };
    let id = id.to_string();
    enqueue(move || {
        if let Some(mut record) = read(&dir, &id) {
            change(&mut record);
            write(&dir, &record);
        }
    });
}

/// Right after a recording stops: save its raw audio and settings. Returns
/// the record's id, or `None` when benchmarking is off.
pub fn begin(
    app: &AppHandle,
    rm: &crate::managers::audio::AudioRecordingManager,
    kept_samples: usize,
    recording: &crate::vad_rescue::Raw,
) -> Option<String> {
    let settings = crate::settings::get_settings(app);
    if !settings.benchmark_recording || recording.samples.is_empty() {
        return None;
    }
    let raw = recording.samples.clone();
    let (gain_at_start, gain_db, auto_gain) = (
        recording.gain_at_start,
        recording.gain_db,
        recording.auto_gain,
    );
    let dir = dir(app)?;
    let id = new_id();
    let rate = crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE as f32;
    let context = crate::app_context::current();
    let record = Record {
        id: id.clone(),
        at: chrono::Local::now().to_rfc3339(),
        audio: format!("{id}.wav"),
        seconds: raw.len() as f32 / rate,
        kept_seconds: kept_samples as f32 / rate,
        gain_db,
        auto_gain,
        gain_at_start,
        vad_backend: format!("{:?}", settings.vad_backend),
        vad_threshold: settings.vad_threshold_for(rm.open_device_name().as_deref()),
        microphone: Some(
            rm.open_device_name()
                .unwrap_or_else(|| "System default".into()),
        ),
        language: settings.selected_language.clone(),
        app: context.app_name,
        bundle_id: context.bundle_id,
        ..Default::default()
    };
    enqueue(move || {
        if let Err(e) = std::fs::create_dir_all(&dir) {
            log::warn!("Benchmark: couldn't create {}: {e}", dir.display());
            return;
        }
        if let Err(e) = crate::audio_toolkit::save_wav_file(dir.join(&record.audio), &raw) {
            log::warn!("Benchmark: couldn't save audio: {e}");
            return;
        }
        write(&dir, &record);
    });
    Some(id)
}

#[derive(Serialize, Type, Debug, Clone, Default)]
pub struct BenchmarkSummary {
    pub dictations: u32,
    pub edited: u32,
    /// Records with a ground truth from you.
    pub confirmed: u32,
    pub guessed: u32,
    pub megabytes: f64,
}

#[tauri::command]
#[specta::specta]
pub fn benchmark_summary(app: AppHandle) -> BenchmarkSummary {
    let mut summary = BenchmarkSummary::default();
    let Some(entries) = dir(&app).and_then(|d| std::fs::read_dir(d).ok()) else {
        return summary;
    };
    let mut bytes = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        bytes += entry.metadata().map_or(0, |m| m.len());
        if path.extension().is_some_and(|e| e == "json") {
            summary.dictations += 1;
            if let Some(r) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<Record>(&s).ok())
            {
                summary.edited += u32::from(r.edit.as_deref() == Some("edited"));
                summary.confirmed += u32::from(r.ground_truth.is_some());
                summary.guessed += u32::from(r.guess.is_some());
            }
        }
    }
    summary.megabytes = bytes as f64 / 1_000_000.0;
    summary
}

#[tauri::command]
#[specta::specta]
pub fn open_benchmark_folder(app: AppHandle) -> Result<(), String> {
    let dir = dir(&app).ok_or("No app data folder")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<String>)
        .map_err(|e| e.to_string())
}

#[derive(Serialize, Type, Debug, Clone)]
pub struct BenchmarkItem {
    pub record: Record,
    /// Absolute path of the raw audio, for playback.
    pub audio_path: String,
}

/// Recorded dictations, newest first.
#[tauri::command]
#[specta::specta]
pub fn benchmark_records(app: AppHandle, offset: u32, limit: u32) -> Vec<BenchmarkItem> {
    let Some(dir) = dir(&app) else {
        return Vec::new();
    };
    let mut ids: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.strip_suffix(".json").map(str::to_string)
        })
        .collect();
    ids.sort_unstable_by(|a, b| b.cmp(a));
    ids.into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .filter_map(|id| read(&dir, &id))
        .map(|record| BenchmarkItem {
            audio_path: dir.join(&record.audio).to_string_lossy().to_string(),
            record,
        })
        .collect()
}

/// Save (or, with an empty text, clear) what was actually said.
#[tauri::command]
#[specta::specta]
pub fn set_benchmark_ground_truth(
    app: AppHandle,
    id: String,
    text: String,
) -> Result<Record, String> {
    let text = text.trim().to_string();
    change_now(&app, &id, move |r| {
        r.ground_truth = (!text.is_empty()).then_some(text);
    })
}

const GUESS_INSTRUCTIONS: &str = "\
You write the reference transcript for a speech-recognition benchmark: exactly \
the words the speaker said in one short dictation. The benchmark scores speech \
models against your text, so any word you add, drop or change is an error in \
the benchmark. Be conservative.

You get transcripts of the same audio from one or more speech models, what was \
pasted after automatic cleanup, the speaker's own later edit of it (if any), the \
app, and the speaker's vocabulary.

Start from the transcripts and keep their words. Change a word only when it is \
obviously misheard: another transcript, the edit, or the vocabulary shows the \
real word (e.g. \"cube cuddle\" when the speaker said kubectl, \"cloud code\" for \
Claude Code). When transcripts disagree, pick the reading most of them share \
unless it is obviously wrong.

Never remove or add anything else: fillers (um, uh, like, you know), repetitions, \
false starts, self-corrections and ungrammatical bits all stay exactly as spoken. \
The pasted text and the edit are cleaned up and may be reworded; use them only to \
settle what a misheard word was, never to copy their wording, fillers or \
punctuation. Don't fix grammar, don't rephrase. Keep punctuation and casing \
minimal and plain.

If there's no speech, return an empty text. `confident` is false when you couldn't \
settle some words; list those words (as you wrote them) in `unsure`. `notes`: one \
short sentence on what you changed and why, or empty.";

fn guess_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["text", "confident", "unsure", "notes"],
        "properties": {
            "text": {"type": "string"},
            "confident": {"type": "boolean"},
            "unsure": {"type": "array", "items": {"type": "string"}},
            "notes": {"type": "string"}
        }
    })
}

/// Ask for a best guess at what was said: transcribe the raw audio with the
/// cloud speech models that have an API key, then have ChatGPT reconcile
/// those with the live transcript, what was pasted and your edits.
#[tauri::command]
#[specta::specta]
pub async fn guess_benchmark_ground_truth(app: AppHandle, id: String) -> Result<Record, String> {
    let settings = crate::rules::with_rules(crate::settings::get_settings(&app));
    let dir = dir(&app).ok_or("No app data folder")?;
    let record = {
        let (dir, id) = (dir.clone(), id.clone());
        queued(move || read(&dir, &id))
            .flatten()
            .ok_or("That recording is gone")?
    };
    // Dev-only: assumes you're signed in with ChatGPT.
    let llm = crate::meetings::llm::Llm::Chatgpt {
        model: settings.assistant_model.clone(),
    };

    let mut heard = Vec::new();
    if let Some(text) = &record.transcript {
        heard.push(Heard {
            by: format!(
                "{} (live)",
                record.model.as_deref().unwrap_or("speech model")
            ),
            text: text.clone(),
        });
    }
    let audio = crate::audio_toolkit::read_wav_samples(dir.join(&record.audio))
        .map_err(|e| format!("Couldn't read the audio: {e}"))?;
    for provider in crate::meetings::remote::AUTO_PROVIDERS {
        let Ok(remote) = crate::meetings::remote::Remote::for_provider(&settings, provider, false)
        else {
            continue; // no API key for this one
        };
        match remote.transcribe(&audio).await {
            Ok(text) => heard.push(Heard {
                by: format!("{} {}", remote.name, remote.model),
                text,
            }),
            Err(e) => log::warn!("Benchmark guess: {} failed: {e}", remote.name),
        }
    }

    let guess = reconcile(&llm, &settings.custom_words, &record, heard).await?;
    change_now(&app, &id, move |r| r.guess = Some(guess))
}

/// Have the model reconcile what the speech models heard with what was
/// pasted and your edits into a best guess at what was said.
pub(crate) async fn reconcile(
    llm: &crate::meetings::llm::Llm,
    vocabulary: &[String],
    record: &Record,
    heard: Vec<Heard>,
) -> Result<Guess, String> {
    let input = serde_json::json!({
        "transcripts": heard,
        "pasted": record.pasted,
        "edited": record.edited.as_ref().filter(|_| record.edit.as_deref() == Some("edited")),
        "app": record.app,
        "vocabulary": vocabulary,
    });
    let reply = llm
        .ask_json(
            GUESS_INSTRUCTIONS,
            &input.to_string(),
            &guess_schema(),
            "medium",
        )
        .await?;
    let text = |k: &str| reply[k].as_str().unwrap_or_default().trim().to_string();
    Ok(Guess {
        text: text("text"),
        confident: reply["confident"].as_bool().unwrap_or(false),
        unsure: reply["unsure"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        notes: text("notes"),
        by: llm.label(),
        heard,
        at: chrono::Local::now().to_rfc3339(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_records_still_load() {
        let r: Record = serde_json::from_str(r#"{"id":"a","transcript":"hi"}"#).unwrap();
        assert_eq!(r.transcript.as_deref(), Some("hi"));
        assert!(r.ground_truth.is_none() && r.guess.is_none());
    }
}
