//! Nudges after a dictation: notices that say what probably went wrong and
//! offer the fix, instead of leaving you to guess.
//!
//! * the microphone gave (almost) nothing: hear the recording, or choose
//!   another microphone;
//! * you spoke a language other than the one dictation is set to;
//! * the text reads like a mishearing (local model only).
//!
//! All of them can be turned off from the notice and are rate-limited.

use crate::managers::audio::AudioRecordingManager;
use crate::managers::model::ModelManager;
use crate::managers::transcription::TranscriptionManager;
use crate::notices::{self, Action, Notice};
use crate::settings::{get_settings, write_settings, AppSettings};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};

const RATE: usize = 16_000;
/// 20 ms frames for loudness.
const FRAME: usize = RATE / 50;
/// Loudness of the recording's loud parts (95th percentile of frame RMS)
/// below which the microphone gave almost nothing, in dBFS. Speech into a
/// laptop mic sits around −35 to −20; a muted or dead mic, far below −60.
const QUIET_DB: f32 = -50.0;
/// Shorter presses are taken to be accidental taps.
const MIN_QUIET_SECONDS: f32 = 2.0;
/// Fewest words for a language guess to mean anything.
const MIN_LANGUAGE_WORDS: usize = 6;
/// Fewest words worth a sense check.
const MIN_SENSE_WORDS: usize = 4;
/// The local model's confidence that the text makes sense, below which the
/// dictation is flagged. Strict: a false alarm is worse than a miss.
const SENSE_THRESHOLD: f64 = 0.15;

/// Loudness of the loud parts of a recording, in dBFS.
pub fn loud_level_db(samples: &[f32]) -> f32 {
    let mut rms: Vec<f32> = samples
        .chunks(FRAME)
        .filter(|c| c.len() == FRAME)
        .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt())
        .collect();
    if rms.is_empty() {
        return f32::NEG_INFINITY;
    }
    rms.sort_by(|a, b| a.total_cmp(b));
    let p95 = rms[(rms.len() * 95 / 100).min(rms.len() - 1)];
    20.0 * p95.max(1e-9).log10()
}

/// Whether a recording that produced no text sounds like a mic problem.
pub fn sounds_like_quiet_mic(raw: &[f32]) -> bool {
    raw.len() as f32 / RATE as f32 >= MIN_QUIET_SECONDS && loud_level_db(raw) < QUIET_DB
}

fn save_temp(samples: &[f32], name: &str) -> Option<PathBuf> {
    let path = std::env::temp_dir().join(name);
    crate::audio_toolkit::save_wav_file(&path, samples)
        .map_err(|e| log::warn!("Couldn't save {}: {e}", path.display()))
        .ok()
        .map(|_| path)
}

fn hear_it(samples: Vec<f32>) -> Action {
    Action::new("Hear it", move |app| {
        if let Some(path) = save_temp(&samples, "felix-last-recording.wav") {
            crate::audio_feedback::play_recording(app, path);
        }
    })
}

/// Nothing came out of a dictation: if the microphone gave almost nothing,
/// say so and offer to play it back or pick another microphone.
pub fn quiet_mic(app: &AppHandle, raw: &[f32]) {
    if !sounds_like_quiet_mic(raw) {
        return;
    }
    let mic = app
        .try_state::<Arc<AudioRecordingManager>>()
        .and_then(|rm| rm.open_device_name())
        .unwrap_or_else(|| "your microphone".into());
    log::info!(
        "Nothing heard: loud parts at {:.0} dBFS over {:.1}s from {mic}",
        loud_level_db(raw),
        raw.len() as f32 / RATE as f32
    );
    let notice = Notice::new(
        "quiet_mic",
        "Didn't hear you",
        format!(
            "Almost nothing came through from {mic}. It may be muted, too far away, or not the mic you're speaking into."
        ),
    )
    .action(Action::new("Choose microphone", |app| {
        notices::open_settings(app, "dictation")
    }))
    .action(hear_it(raw.to_vec()))
    .nudge(Duration::from_secs(30 * 60))
    .seconds(12);
    notices::show(app, notice);
}

fn language_name(code: &str) -> String {
    let primary = code.split(['-', '_']).next().unwrap_or(code);
    isolang::Language::from_639_1(primary)
        .map(|l| l.to_name().to_string())
        .unwrap_or_else(|| code.to_string())
}

fn primary(code: &str) -> String {
    code.split(['-', '_'])
        .next()
        .unwrap_or(code)
        .to_ascii_lowercase()
}

/// The language the text is in, if it's clearly not the one dictation is
/// set to.
pub fn other_language(text: &str, selected: &str, supported: &[String]) -> Option<String> {
    if selected == "auto" || text.split_whitespace().count() < MIN_LANGUAGE_WORDS {
        return None;
    }
    let found = crate::audio_toolkit::lang_id::detect_output_language(text, supported)?;
    (primary(&found) != primary(selected)).then_some(found)
}

/// A dictation came out in another language than the one selected: offer
/// to detect the language automatically.
pub fn wrong_language(app: &AppHandle, text: &str, settings: &AppSettings) {
    let model = app
        .try_state::<Arc<TranscriptionManager>>()
        .and_then(|tm| tm.get_current_model())
        .unwrap_or_else(|| settings.selected_model.clone());
    let Some(supported) = app
        .try_state::<Arc<ModelManager>>()
        .and_then(|mm| mm.get_model_info(&model))
        .map(|info| info.supported_languages)
    else {
        return;
    };
    let Some(found) = other_language(text, &settings.selected_language, &supported) else {
        return;
    };
    let spoken = language_name(&found);
    let notice = Notice::new(
        "wrong_language",
        format!("Speaking {spoken}?"),
        format!(
            "Dictation is set to {}, so other languages can come out wrong. Felix can detect the language instead.",
            language_name(&settings.selected_language)
        ),
    )
    .action(Action::new("Detect automatically", |app| {
        let mut settings = get_settings(app);
        settings.selected_language = "auto".into();
        write_settings(app, settings);
    }))
    .action(Action::new("Language settings", |app| {
        notices::open_settings(app, "dictation")
    }))
    .nudge(Duration::from_secs(12 * 60 * 60))
    .seconds(12);
    notices::show(app, notice);
}

const SENSE_PROMPT: &str = "You check dictations for speech recognition errors. You get the text around the cursor in an app and the dictated text that was inserted there. Answer yes if the dictation reads like something a person meant to write there, even if it's casual, terse or unusual. Answer no only if it's clearly garbled: nonsense words, a sentence that makes no sense, or words that don't fit together, the way misheard speech does. Answer only yes or no.";

/// After a paste: ask the local model whether the dictation makes sense
/// where it went, and flag it if it clearly doesn't.
pub fn check_sense(app: &AppHandle, text: String, before: String, after: String, audio: Vec<f32>) {
    let settings = get_settings(app);
    let Some(model) = crate::soundalikes::local_model(&settings) else {
        return;
    };
    if text.split_whitespace().count() < MIN_SENSE_WORDS
        || settings.muted_notices.iter().any(|k| k == "sense_check")
    {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let place = crate::app_context::current()
            .app_name
            .unwrap_or_else(|| "an app".into());
        let question = format!(
            "App: {place}\nBefore the cursor: \"{before}\"\nAfter the cursor: \"{after}\"\nDictation: \"{text}\"\nDoes the dictation make sense here?"
        );
        let p = match crate::local_llm::yes_probability(
            &model,
            SENSE_PROMPT,
            &question,
            settings.local_model_keep_loaded,
        )
        .await
        {
            Ok(p) => p,
            Err(e) => {
                log::debug!("Sense check failed: {e}");
                return;
            }
        };
        log::debug!("Sense check: {p:.2}");
        if p >= SENSE_THRESHOLD {
            return;
        }
        let notice = Notice::new(
            "sense_check",
            "This might not be what you said",
            format!("“{text}”"),
        )
        .action(Action::new("Report a mistake", |app| {
            notices::open_settings(app, "vocabulary")
        }))
        .action(hear_it(audio))
        .nudge(Duration::from_secs(10 * 60))
        .seconds(10);
        notices::show(&app, notice);
    });
}

/// The paste was never picked up by the app in front: show the text so it
/// isn't lost.
pub fn paste_not_taken(app: &AppHandle, text: String) {
    crate::overlay::show_result_overlay_titled(
        app,
        text,
        Some("The app didn't take the paste".into()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amplitude: f32, seconds: f32) -> Vec<f32> {
        (0..(seconds * RATE as f32) as usize)
            .map(|i| amplitude * (i as f32 * 0.05).sin())
            .collect()
    }

    #[test]
    fn a_muted_mic_is_quiet_and_speech_is_not() {
        assert!(sounds_like_quiet_mic(&vec![0.0; RATE * 3]));
        assert!(sounds_like_quiet_mic(&tone(0.001, 3.0)));
        assert!(!sounds_like_quiet_mic(&tone(0.1, 3.0)));
        // A quick tap isn't worth a warning.
        assert!(!sounds_like_quiet_mic(&vec![0.0; RATE]));
    }

    #[test]
    fn loudness_is_in_dbfs() {
        let db = loud_level_db(&tone(0.1, 1.0));
        assert!((-24.0..-20.0).contains(&db), "{db}");
    }

    #[test]
    fn only_a_clear_other_language_counts() {
        let supported = vec!["en".to_string(), "de".to_string()];
        let german = "Ich gehe heute Abend mit meinen Freunden ins Kino und danach essen";
        assert_eq!(
            other_language(german, "en", &supported).as_deref(),
            Some("de")
        );
        assert_eq!(other_language(german, "de", &supported), None);
        assert_eq!(other_language(german, "auto", &supported), None);
        assert_eq!(other_language("Ich gehe heute", "en", &supported), None);
    }

    #[test]
    fn names_languages() {
        assert_eq!(language_name("de"), "German");
        assert_eq!(language_name("pt-BR"), "Portuguese");
    }
}
