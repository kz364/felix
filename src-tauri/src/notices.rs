//! Short cards in the overlay that say what just went wrong, or what Felix
//! just did on its own, with a button or two to act on it (Undo, Choose
//! microphone, Hear the recording). Built on the result card.
//!
//! Each kind of notice can be turned off by the user ("Don't show again"),
//! and most are rate-limited so a bad microphone doesn't nag every dictation.

use crate::managers::audio::AudioRecordingManager;
use crate::settings::{get_settings, write_settings};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

type Run = Box<dyn FnOnce(&AppHandle) + Send>;

pub struct Action {
    label: String,
    run: Run,
}

impl Action {
    pub fn new(label: impl Into<String>, run: impl FnOnce(&AppHandle) + Send + 'static) -> Self {
        Self {
            label: label.into(),
            run: Box::new(run),
        }
    }
}

pub struct Notice {
    /// Stable name, for muting and rate-limiting ("quiet_mic").
    pub kind: &'static str,
    pub title: String,
    pub text: String,
    pub actions: Vec<Action>,
    /// Offer "Don't show again".
    pub mutable: bool,
    /// Don't show this kind again sooner than this.
    pub min_gap: Duration,
    pub seconds: u32,
}

impl Notice {
    pub fn new(kind: &'static str, title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            kind,
            title: title.into(),
            text: text.into(),
            actions: Vec::new(),
            mutable: false,
            min_gap: Duration::ZERO,
            seconds: 8,
        }
    }

    pub fn action(mut self, action: Action) -> Self {
        self.actions.push(action);
        self
    }

    /// A tip or warning: can be turned off, and shows at most every `gap`.
    pub fn nudge(mut self, gap: Duration) -> Self {
        self.mutable = true;
        self.min_gap = gap;
        self
    }

    pub fn seconds(mut self, seconds: u32) -> Self {
        self.seconds = seconds;
        self
    }
}

/// Buttons of the notice on screen; a new notice replaces them.
static PENDING: Lazy<Mutex<HashMap<String, Run>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static LAST_SHOWN: Lazy<Mutex<HashMap<&'static str, Instant>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Longest a notice waits for a dictation in progress to finish.
const WAIT_FOR_IDLE: Duration = Duration::from_secs(60);

fn busy(app: &AppHandle) -> bool {
    app.try_state::<Arc<AudioRecordingManager>>()
        .is_some_and(|rm| rm.is_recording())
        || crate::actions::assistant_working()
        || crate::actions::dictation_busy()
}

/// Show a notice, unless it's muted or shown too recently. Waits (off the
/// calling thread) while a dictation is being recorded, so it never covers
/// the recording pill.
pub fn show(app: &AppHandle, notice: Notice) {
    if get_settings(app)
        .muted_notices
        .iter()
        .any(|k| k == notice.kind)
    {
        return;
    }
    {
        let mut last = LAST_SHOWN.lock().unwrap();
        if last
            .get(notice.kind)
            .is_some_and(|at| at.elapsed() < notice.min_gap)
        {
            return;
        }
        last.insert(notice.kind, Instant::now());
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        while busy(&app) {
            if started.elapsed() > WAIT_FOR_IDLE {
                return;
            }
            std::thread::sleep(Duration::from_millis(300));
        }
        present(&app, notice);
    });
}

fn present(app: &AppHandle, notice: Notice) {
    let mut pending = PENDING.lock().unwrap();
    pending.clear();
    let mut buttons = Vec::new();
    let mut add = |label: String, run: Run| {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst).to_string();
        buttons.push(crate::overlay::NoticeButton {
            id: id.clone(),
            label,
        });
        pending.insert(id, run);
    };
    for action in notice.actions {
        add(action.label, action.run);
    }
    if notice.mutable {
        let kind = notice.kind;
        add(
            "Don't show again".into(),
            Box::new(move |app| mute(app, kind)),
        );
    }
    drop(pending);
    log::info!("Notice ({}): {}", notice.kind, notice.title);
    crate::overlay::show_notice(app, notice.title, notice.text, buttons, notice.seconds);
}

/// A dictation kept back because nothing seemed focused in `bundle_id`:
/// the card with Copy and "Paste anyway", which pastes it and always pastes
/// in that app from now on (for apps that hide their text boxes from
/// Accessibility).
pub fn show_kept_back(app: &AppHandle, text: String, bundle_id: Option<String>) {
    let mut pending = PENDING.lock().unwrap();
    pending.clear();
    let mut buttons = Vec::new();
    if let Some(bundle_id) = bundle_id {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst).to_string();
        buttons.push(crate::overlay::NoticeButton {
            id: id.clone(),
            label: "Paste anyway".into(),
        });
        let pasted = text.clone();
        let run: Run = Box::new(move |app: &AppHandle| {
            crate::paste_apps::always_paste_in(&bundle_id);
            let app2 = app.clone();
            // The card is gone and the app still has the keyboard.
            let _ = app.run_on_main_thread(move || {
                if let Err(e) = crate::clipboard::paste(pasted, app2, None) {
                    log::error!("Paste anyway failed: {e}");
                }
            });
        });
        pending.insert(id, run);
    }
    drop(pending);
    crate::overlay::show_result_with_actions(app, text, buttons);
}

/// Bring up the settings window on a page ("dictation", "vocabulary"…).
pub fn open_settings(app: &AppHandle, section: &str) {
    crate::show_main_window(app);
    let _ = tauri::Emitter::emit(app, "open-section", section);
}

fn mute(app: &AppHandle, kind: &str) {
    let mut settings = get_settings(app);
    if !settings.muted_notices.iter().any(|k| k == kind) {
        settings.muted_notices.push(kind.to_string());
        write_settings(app, settings);
    }
}

/// A button on the notice card was clicked.
#[tauri::command]
#[specta::specta]
pub fn notice_action(app: AppHandle, id: String) {
    let run = PENDING.lock().unwrap().remove(&id);
    crate::overlay::dismiss_result_overlay(app.clone());
    if let Some(run) = run {
        run(&app);
    }
}

/// Show the tips and warnings the user turned off again.
#[tauri::command]
#[specta::specta]
pub fn unmute_notices(app: AppHandle) {
    let mut settings = get_settings(&app);
    settings.muted_notices.clear();
    write_settings(&app, settings);
}
