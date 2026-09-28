//! Getting a dictation back when something cut it short:
//!
//! * a cancelled dictation can be undone for a few seconds: the audio is
//!   transcribed and pasted as if the dictation had finished;
//! * when the microphone disconnects mid-recording, the dictation stops
//!   there and what was heard up to then is used.

use crate::actions::{process_transcription_output, transcribe_dictation};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::history::HistoryManager;
use crate::managers::transcription::TranscriptionManager;
use crate::notices::{self, Action, Notice};
use crate::tray::{set_tray_state, TrayIconState};
use crate::TranscriptionCoordinator;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

/// Longest wait for a cancelled recording's audio to come off the recorder.
const WAIT_FOR_AUDIO: Duration = Duration::from_millis(1500);
/// How often a recording checks that its microphone is still there.
const MIC_POLL: Duration = Duration::from_millis(250);

/// A recording was cancelled: offer Undo once its audio is in hand.
pub fn recording_cancelled(app: &AppHandle, cancel_generation: u64) {
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
            return;
        };
        let started = Instant::now();
        loop {
            if let Some(samples) = rm.take_cancelled_samples(cancel_generation) {
                offer_undo(&app, samples, false);
                return;
            }
            if started.elapsed() > WAIT_FOR_AUDIO {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

/// A dictation was cancelled after recording, while it was transcribed or
/// polished: offer Undo with its audio.
pub fn processing_cancelled(app: &AppHandle, post_process: bool) {
    if let Some(samples) = crate::rules::last_audio() {
        offer_undo(app, samples, post_process);
    }
}

/// Offer to finish a cancelled dictation after all.
pub fn offer_undo(app: &AppHandle, samples: Vec<f32>, post_process: bool) {
    // Shorter than a second is a tap, not a dictation.
    if samples.len() < 16_000 {
        return;
    }
    let notice = Notice::new("cancelled", "Dictation cancelled", "")
        .action(Action::new("Undo", move |app| {
            finish(app, samples, post_process)
        }))
        .seconds(5);
    notices::show(app, notice);
}

/// Transcribe, polish and paste audio the way a dictation would have been.
fn finish(app: &AppHandle, samples: Vec<f32>, post_process: bool) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
        let hm = Arc::clone(&app.state::<Arc<HistoryManager>>());
        crate::overlay::show_transcribing_overlay(&app);
        set_tray_state(&app, TrayIconState::Transcribing);
        let (result, model) = transcribe_dictation(&app, &tm, samples).await;
        tm.maybe_unload_immediately("undone cancel");
        let transcription = match result {
            Ok(text) if !text.trim().is_empty() => text,
            Ok(_) => {
                idle(&app);
                return;
            }
            Err(e) => {
                log::error!("Undoing the cancel: transcription failed: {e}");
                idle(&app);
                return;
            }
        };
        let processed = process_transcription_output(&app, &transcription, post_process).await;
        if let Err(e) = hm.save_entry(
            String::new(),
            transcription,
            post_process,
            processed.post_processed_text.clone(),
            processed.post_process_prompt.clone(),
            model,
        ) {
            log::error!("Failed to save history entry: {e}");
        }
        let text = processed.final_text;
        let submit_key = processed.submit_key;
        let ah = app.clone();
        let _ = app.run_on_main_thread(move || {
            if text.is_empty() && submit_key.is_none() {
                idle(&ah);
                return;
            }
            let shown = text.clone();
            match crate::utils::paste(text, ah.clone(), submit_key) {
                Ok(()) => idle(&ah),
                Err(e) => {
                    log::error!("Undoing the cancel: paste failed: {e}");
                    set_tray_state(&ah, TrayIconState::Idle);
                    crate::overlay::show_result_overlay_titled(
                        &ah,
                        shown,
                        Some("Couldn't paste the dictation".into()),
                    );
                }
            }
        });
    });
}

fn idle(app: &AppHandle) {
    crate::overlay::hide_recording_overlay(app);
    set_tray_state(app, TrayIconState::Idle);
}

/// Watch a recording's microphone: if it disconnects, stop the dictation
/// there so what was heard gets used, and say what happened.
pub fn watch_microphone(app: &AppHandle, binding_id: String) {
    let app = app.clone();
    std::thread::spawn(move || {
        let Some(rm) = app.try_state::<Arc<AudioRecordingManager>>() else {
            return;
        };
        let rm = Arc::clone(&rm);
        let cancel_generation = rm.cancel_generation();
        loop {
            std::thread::sleep(MIC_POLL);
            if !rm.is_recording() || rm.was_cancelled_since(cancel_generation) {
                return;
            }
            if rm.microphone_lost() {
                break;
            }
        }
        let mic = rm
            .open_device_name()
            .unwrap_or_else(|| "Your microphone".into());
        log::warn!("{mic} disconnected mid-recording; stopping the dictation");
        if let Some(c) = app.try_state::<TranscriptionCoordinator>() {
            c.send_external_input(&binding_id, "mic-disconnected");
        }
        let notice = Notice::new(
            "mic_lost",
            "Microphone disconnected",
            format!("{mic} stopped sending audio, so Felix stopped listening and used what it heard up to then."),
        )
        .seconds(8);
        notices::show(&app, notice);
    });
}
