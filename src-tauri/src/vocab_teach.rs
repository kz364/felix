//! "Teach this word": the user says a vocabulary term a few times (spoken and
//! whispered); each take goes through the real pipeline (current model with
//! its native biasing, then canonical forms and correction rules) so we can
//! see what the model actually hears and offer the leftover mis-hearings as a
//! correction rule.

use crate::audio_toolkit::VadPolicy;
use crate::managers::audio::AudioRecordingManager;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::get_settings;
use once_cell::sync::Lazy;
use serde::Serialize;
use specta::Type;
use std::collections::HashSet;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

const TEACH_BINDING: &str = "vocab_teach";

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
    /// normal speech, so the UI leaves it unticked by default.
    pub variant_is_common_word: bool,
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

fn is_common_phrase(variant: &str) -> bool {
    let parts = words(variant);
    !parts.is_empty() && parts.iter().all(|w| COMMON_WORDS.contains(w))
}

pub fn analyze_take(heard: &str, corrected: &str, word: &str) -> TeachTake {
    let recognized = contains_phrase(corrected, word);
    let variant = (!recognized)
        .then(|| words(heard).join(" "))
        .filter(|v| !v.is_empty());
    let variant_is_common_word = variant.as_deref().is_some_and(is_common_phrase);
    TeachTake {
        heard: heard.trim().to_string(),
        corrected: corrected.trim().to_string(),
        recognized,
        variant,
        variant_is_common_word,
    }
}

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
pub async fn teach_stop_recording(app: AppHandle, word: String) -> Result<TeachTake, String> {
    let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
    let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
    let heard = tauri::async_runtime::spawn_blocking(move || {
        let generation = rm.cancel_generation();
        let samples = rm
            .stop_recording(TEACH_BINDING, generation)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "Nothing was recorded".to_string())?;
        tm.transcribe(samples).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("teach task failed: {e}"))??;

    let settings = get_settings(&app);
    let canonical = crate::vocabulary::apply_canonical_forms(&heard, &settings.custom_words);
    let corrected =
        crate::scratchpad::apply_text_replacements(&canonical, &settings.text_replacements);
    Ok(analyze_take(&heard, &corrected, &word))
}

#[tauri::command]
#[specta::specta]
pub fn teach_cancel_recording(app: AppHandle) {
    app.state::<Arc<AudioRecordingManager>>().cancel_recording();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognized_take_has_no_variant() {
        let take = analyze_take("Qwen.", "Qwen.", "Qwen");
        assert!(take.recognized);
        assert_eq!(take.variant, None);
    }

    #[test]
    fn mishearing_becomes_normalized_variant() {
        let take = analyze_take(" Quinn. ", "Quinn.", "Qwen");
        assert!(!take.recognized);
        assert_eq!(take.variant.as_deref(), Some("quinn"));
    }

    #[test]
    fn rules_that_already_fix_it_count_as_recognized() {
        let take = analyze_take("Quinn.", "Qwen.", "Qwen");
        assert!(take.recognized);
        assert_eq!(take.variant, None);
    }

    #[test]
    fn multi_word_terms_match_as_phrases() {
        assert!(contains_phrase("Use GLM-5.3 today", "GLM-5.3"));
        assert!(!contains_phrase("Use GLM today", "GLM-5.3"));
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
