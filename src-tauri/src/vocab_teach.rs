//! "Teach this word": the user says a vocabulary term a few times (spoken and
//! whispered); each take goes through the real pipeline (current model with
//! its native biasing, then canonical forms and correction rules) so we can
//! see what the model actually hears.
//!
//! Clips are kept (outside dictation-history retention) and results are
//! stored per transcription model: which mishearings to rewrite to the word.
//! Mishearings are model-specific, so when the model changes the saved clips
//! are re-run on the new model in the background and its results computed —
//! or reloaded, if that model was checked before. No model is ever trained.

use crate::audio_toolkit::VadPolicy;
use crate::managers::audio::AudioRecordingManager;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{get_settings, write_settings, AppSettings};
use log::{debug, error, info, warn};
use once_cell::sync::Lazy;
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};

const TEACH_BINDING: &str = "vocab_teach";
const UPDATED_EVENT: &str = "taught-words-updated";

/// One recorded take of a taught word.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Type)]
pub struct TeachClip {
    /// WAV file name in the vocab clips directory.
    pub file: String,
    pub whispered: bool,
}

/// What one transcription model made of a taught word's clips.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Type)]
pub struct ModelVariants {
    pub model_id: String,
    /// Transcript of each clip, in clip order.
    pub heard: Vec<String>,
    /// Normalized mishearings ("quinn") that get rewritten to the word.
    pub variants: Vec<String>,
    /// Mishearings the user chose not to rewrite (e.g. common words).
    pub excluded: Vec<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Type)]
pub struct TaughtWord {
    pub word: String,
    pub clips: Vec<TeachClip>,
    pub by_model: Vec<ModelVariants>,
}

/// Result of one teaching take.
#[derive(Serialize, Debug, Clone, Type)]
pub struct TeachTake {
    /// What the model transcribed (native biasing applied).
    pub heard: String,
    /// After canonical vocabulary forms and correction rules.
    pub corrected: String,
    /// Whether the corrected text already contains the word.
    pub recognized: bool,
    /// Normalized mis-hearing to offer as a rule ("quinn"), if any.
    pub variant: Option<String>,
    /// The variant is an ordinary English word; a rule for it would rewrite
    /// normal speech, so it is excluded by default.
    pub variant_is_common_word: bool,
    /// Saved clip for this take.
    pub clip: TeachClip,
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn contains_phrase(text: &str, phrase: &str) -> bool {
    let hay = words(text);
    let needle = words(phrase);
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle.as_slice())
}

/// Lowercase entries of the system word list; capitalized entries are proper
/// nouns ("Quinn") and don't count as common words.
static COMMON_WORDS: Lazy<HashSet<String>> = Lazy::new(|| {
    std::fs::read_to_string("/usr/share/dict/words")
        .map(|list| {
            list.lines()
                .filter(|w| w.chars().next().is_some_and(char::is_lowercase))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
});

pub fn is_common_phrase(variant: &str) -> bool {
    let parts = words(variant);
    !parts.is_empty() && parts.iter().all(|w| COMMON_WORDS.contains(w))
}

/// Recognized?, and the normalized mishearing if not. Takes are the word on
/// its own; a transcript much longer than the word (a stray sentence) can't
/// be turned into a rule, so it yields no variant.
fn analyze(heard: &str, corrected: &str, word: &str) -> (bool, Option<String>) {
    let recognized = contains_phrase(corrected, word);
    let heard_words = words(heard);
    let max_len = words(word).len() + 2;
    let variant = (!recognized && !heard_words.is_empty() && heard_words.len() <= max_len)
        .then(|| heard_words.join(" "));
    (recognized, variant)
}

/// The text rules a take is judged against: canonical forms and the user's
/// correction rules, but not taught rules (we want to see what's left).
fn apply_static_rules(heard: &str, settings: &AppSettings) -> String {
    let canonical = crate::vocabulary::apply_canonical_forms(heard, &settings.custom_words);
    crate::scratchpad::apply_text_replacements(&canonical, &settings.text_replacements)
}

fn clips_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = crate::portable::app_data_dir(app)
        .map_err(|e| e.to_string())?
        .join("vocab_clips");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn delete_clip_files(app: &AppHandle, files: &[String]) {
    let Ok(dir) = clips_dir(app) else { return };
    for file in files {
        // Only bare names we generated; never follow paths.
        if file.contains('/') || file.contains("..") || file.is_empty() {
            continue;
        }
        let _ = std::fs::remove_file(dir.join(file));
    }
}

// ---------------------------------------------------------------- dictation

/// Rewrite the current model's taught mishearings to their words. Falls back
/// to the most recently computed model's results while the current model's
/// are being recomputed.
pub fn apply_taught_rules(text: &str, settings: &AppSettings) -> String {
    let mut text = text.to_string();
    for taught in &settings.taught_words {
        let entry = taught
            .by_model
            .iter()
            .find(|m| m.model_id == settings.selected_model)
            .or_else(|| taught.by_model.last());
        let Some(entry) = entry else { continue };
        let active: Vec<String> = entry
            .variants
            .iter()
            .filter(|v| !entry.excluded.contains(v))
            .map(|v| {
                v.split_whitespace()
                    .map(regex::escape)
                    .collect::<Vec<_>>()
                    .join(r"\s+")
            })
            .collect();
        if active.is_empty() {
            continue;
        }
        let pattern = format!(r"\b(?:{})\b", active.join("|"));
        match RegexBuilder::new(&pattern).case_insensitive(true).build() {
            Ok(re) => {
                text = re
                    .replace_all(&text, regex::NoExpand(&taught.word))
                    .into_owned()
            }
            Err(e) => warn!("Taught rule for '{}' skipped: {e}", taught.word),
        }
    }
    text
}

// ------------------------------------------------------------------ teaching

#[tauri::command]
#[specta::specta]
pub fn teach_start_recording(app: AppHandle) -> Result<(), String> {
    let settings = get_settings(&app);
    app.state::<Arc<TranscriptionManager>>()
        .initiate_model_load();
    let policy = if settings.vad_enabled {
        VadPolicy::Offline
    } else {
        VadPolicy::Disabled
    };
    app.state::<Arc<AudioRecordingManager>>()
        .try_start_recording(TEACH_BINDING, policy)
        .map(|_| ())
}

#[tauri::command]
#[specta::specta]
pub async fn teach_stop_recording(
    app: AppHandle,
    word: String,
    whispered: bool,
) -> Result<TeachTake, String> {
    let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
    let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
    let dir = clips_dir(&app)?;
    let file = format!(
        "clip-{}-{}.wav",
        chrono::Utc::now().timestamp_millis(),
        if whispered { "whisper" } else { "spoken" }
    );
    let path = dir.join(&file);
    let heard = tauri::async_runtime::spawn_blocking(move || {
        let generation = rm.cancel_generation();
        let samples = rm
            .stop_recording(TEACH_BINDING, generation)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Nothing was recorded".to_string())?;
        crate::audio_toolkit::save_wav_file(&path, &samples).map_err(|e| e.to_string())?;
        tm.transcribe(samples).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("teach task failed: {e}"))??;

    let settings = get_settings(&app);
    let corrected = apply_static_rules(&heard, &settings);
    let (recognized, variant) = analyze(&heard, &corrected, &word);
    Ok(TeachTake {
        heard: heard.trim().to_string(),
        corrected: corrected.trim().to_string(),
        recognized,
        variant_is_common_word: variant.as_deref().is_some_and(is_common_phrase),
        variant,
        clip: TeachClip { file, whispered },
    })
}

#[tauri::command]
#[specta::specta]
pub fn teach_cancel_recording(app: AppHandle) {
    app.state::<Arc<AudioRecordingManager>>().cancel_recording();
}

/// Delete clips of an abandoned teaching session.
#[tauri::command]
#[specta::specta]
pub fn teach_discard_clips(app: AppHandle, files: Vec<String>) {
    delete_clip_files(&app, &files);
}

/// Save a teaching session for the current model, replacing any earlier one
/// for the same word (its old clips are deleted).
#[tauri::command]
#[specta::specta]
pub fn teach_save_word(
    app: AppHandle,
    word: String,
    clips: Vec<TeachClip>,
    heard: Vec<String>,
    variants: Vec<String>,
    excluded: Vec<String>,
) -> Result<(), String> {
    let mut settings = get_settings(&app);
    let model_id = settings.selected_model.clone();
    if let Some(old) = settings.taught_words.iter().find(|t| t.word == word) {
        let keep: HashSet<&String> = clips.iter().map(|c| &c.file).collect();
        let stale: Vec<String> = old
            .clips
            .iter()
            .filter(|c| !keep.contains(&c.file))
            .map(|c| c.file.clone())
            .collect();
        delete_clip_files(&app, &stale);
    }
    settings.taught_words.retain(|t| t.word != word);
    settings.taught_words.push(TaughtWord {
        word,
        clips,
        by_model: vec![ModelVariants {
            model_id,
            heard,
            variants,
            excluded,
        }],
    });
    write_settings(&app, settings);
    let _ = app.emit(UPDATED_EVENT, ());
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn teach_delete_word(app: AppHandle, word: String) -> Result<(), String> {
    let mut settings = get_settings(&app);
    if let Some(old) = settings.taught_words.iter().find(|t| t.word == word) {
        let files: Vec<String> = old.clips.iter().map(|c| c.file.clone()).collect();
        delete_clip_files(&app, &files);
    }
    settings.taught_words.retain(|t| t.word != word);
    write_settings(&app, settings);
    let _ = app.emit(UPDATED_EVENT, ());
    Ok(())
}

/// Change which mishearings of a word are rewritten for the current model.
#[tauri::command]
#[specta::specta]
pub fn teach_set_excluded(
    app: AppHandle,
    word: String,
    excluded: Vec<String>,
) -> Result<(), String> {
    let mut settings = get_settings(&app);
    let model_id = settings.selected_model.clone();
    let entry = settings
        .taught_words
        .iter_mut()
        .find(|t| t.word == word)
        .and_then(|t| t.by_model.iter_mut().find(|m| m.model_id == model_id))
        .ok_or_else(|| format!("'{word}' has no results for the current model"))?;
    entry.excluded = excluded;
    write_settings(&app, settings);
    let _ = app.emit(UPDATED_EVENT, ());
    Ok(())
}

/// Re-run saved clips on the current model. `force` recomputes words that
/// already have results for it; otherwise only missing ones are computed.
#[tauri::command]
#[specta::specta]
pub fn teach_recheck(app: AppHandle, force: bool) {
    recheck_in_background(&app, force);
}

// ------------------------------------------------------------ model changes

static RECHECK_RUNNING: AtomicBool = AtomicBool::new(false);

/// Called after the transcription model changes and at startup: make sure
/// every taught word has results for the current model, re-running its clips
/// if needed. Runs on a background thread; the UI is told via an event.
pub fn recheck_in_background(app: &AppHandle, force: bool) {
    let settings = get_settings(app);
    let model_id = settings.selected_model.clone();
    if model_id.is_empty() {
        return;
    }
    let pending: Vec<String> = settings
        .taught_words
        .iter()
        .filter(|t| !t.clips.is_empty())
        .filter(|t| force || !t.by_model.iter().any(|m| m.model_id == model_id))
        .map(|t| t.word.clone())
        .collect();
    if pending.is_empty() || RECHECK_RUNNING.swap(true, Ordering::AcqRel) {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        info!(
            "Re-checking {} taught word(s) on model {}",
            pending.len(),
            model_id
        );
        let _ = app.emit(UPDATED_EVENT, ());
        for word in pending {
            if let Err(e) = recheck_word(&app, &word, &model_id) {
                error!("Re-checking '{word}' failed: {e}");
            }
            // Stop if the model changed underneath us; the new model's
            // recheck will pick the remaining words up.
            if get_settings(&app).selected_model != model_id {
                break;
            }
        }
        RECHECK_RUNNING.store(false, Ordering::Release);
        let _ = app.emit(UPDATED_EVENT, ());
        // The model may have changed while we ran.
        if get_settings(&app).selected_model != model_id {
            recheck_in_background(&app, false);
        }
    });
}

fn recheck_word(app: &AppHandle, word: &str, model_id: &str) -> Result<(), String> {
    let settings = get_settings(app);
    let Some(taught) = settings.taught_words.iter().find(|t| t.word == word) else {
        return Ok(());
    };
    let dir = clips_dir(app)?;
    let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
    tm.initiate_model_load();

    let mut heard = Vec::new();
    let mut variants: Vec<String> = Vec::new();
    for clip in &taught.clips {
        let samples = match crate::audio_toolkit::read_wav_samples(dir.join(&clip.file)) {
            Ok(samples) => samples,
            Err(e) => {
                warn!("Taught clip {} unreadable: {e}", clip.file);
                heard.push(String::new());
                continue;
            }
        };
        let text = tm.transcribe(samples).map_err(|e| e.to_string())?;
        let corrected = apply_static_rules(&text, &settings);
        if let (false, Some(variant)) = analyze(&text, &corrected, word) {
            if !variants.contains(&variant) {
                variants.push(variant);
            }
        }
        heard.push(text.trim().to_string());
    }
    let excluded: Vec<String> = variants
        .iter()
        .filter(|v| is_common_phrase(v))
        .cloned()
        .collect();
    debug!("Taught '{word}' on {model_id}: variants {variants:?}, excluded {excluded:?}");

    // Re-read before writing: the user may have changed settings meanwhile.
    let mut settings = get_settings(app);
    if let Some(taught) = settings.taught_words.iter_mut().find(|t| t.word == word) {
        taught.by_model.retain(|m| m.model_id != model_id);
        taught.by_model.push(ModelVariants {
            model_id: model_id.to_string(),
            heard,
            variants,
            excluded,
        });
        write_settings(app, settings);
    }
    let _ = app.emit(UPDATED_EVENT, ());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taught(word: &str, entries: Vec<(&str, Vec<&str>, Vec<&str>)>) -> TaughtWord {
        TaughtWord {
            word: word.into(),
            clips: vec![],
            by_model: entries
                .into_iter()
                .map(|(model, variants, excluded)| ModelVariants {
                    model_id: model.into(),
                    heard: vec![],
                    variants: variants.into_iter().map(String::from).collect(),
                    excluded: excluded.into_iter().map(String::from).collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn analysis_of_takes() {
        assert_eq!(analyze("Qwen.", "Qwen.", "Qwen"), (true, None));
        assert_eq!(
            analyze(" Quinn. ", "Quinn.", "Qwen"),
            (false, Some("quinn".into()))
        );
        // Already fixed by a correction rule.
        assert_eq!(analyze("Quinn.", "Qwen.", "Qwen"), (true, None));
        assert!(contains_phrase("Use GLM-5.3 today", "GLM-5.3"));
        // A whole sentence is not a usable mishearing.
        assert_eq!(
            analyze(
                "I asked Quinn to review it.",
                "I asked Quinn to review it.",
                "Qwen"
            ),
            (false, None)
        );
        assert_eq!(
            analyze("Quinn Wen.", "Quinn Wen.", "Qwen"),
            (false, Some("quinn wen".into()))
        );
    }

    #[test]
    fn taught_rules_follow_the_current_model() {
        let mut settings = crate::settings::get_default_settings();
        settings.selected_model = "cohere".into();
        settings.taught_words = vec![taught(
            "Qwen",
            vec![
                ("cohere", vec!["quinn", "when"], vec!["when"]),
                ("qwen3", vec!["quen"], vec![]),
            ],
        )];
        assert_eq!(
            apply_taught_rules("Ask Quinn when it's done", &settings),
            "Ask Qwen when it's done"
        );
        settings.selected_model = "qwen3".into();
        assert_eq!(
            apply_taught_rules("Ask Quinn and Quen", &settings),
            "Ask Quinn and Qwen"
        );
    }

    #[test]
    fn unchecked_model_falls_back_to_latest_results() {
        let mut settings = crate::settings::get_default_settings();
        settings.selected_model = "brand-new".into();
        settings.taught_words = vec![taught("Qwen", vec![("cohere", vec!["quinn"], vec![])])];
        assert_eq!(apply_taught_rules("Ask Quinn", &settings), "Ask Qwen");
    }

    #[test]
    fn common_words_are_flagged_when_the_word_list_exists() {
        if COMMON_WORDS.is_empty() {
            return; // no system word list (non-macOS CI)
        }
        assert!(is_common_phrase("when"));
        assert!(!is_common_phrase("quinn"));
    }
}
