//! Live draft: while you dictate, a small streaming model (Moonshine
//! Streaming Tiny) transcribes alongside the main one, and its rough text
//! shows in a see-through bubble by the cursor that Felix draws itself
//! (`draft_bubble`). The bubble never touches the field; the main model's
//! text is pasted exactly as without the draft. Anything that isn't ready
//! (setting off, model missing, secure input) means no draft.
//!
//! Felix used to show the draft in the field through an input method of its
//! own (Felix Draft); [`remove_old_input_method`] takes that off the Mac.

use log::{debug, info, warn};
use serde::Serialize;
use specta::Type;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use transcribe_cpp::{Model, ModelOptions, RunOptions, Session, StreamOptions};

/// The draft model, from the catalog.
pub const MODEL_FILE: &str = "moonshine-streaming-tiny-Q8_0.gguf";
pub const MODEL_ID: &str = "moonshine-streaming-tiny-Q8_0";
/// The old in-field input method.
const IME_ID: &str = "com.pais.inputmethod.FelixDraft";
const IME_APP: &str = "FelixDraft.app";

enum Cmd {
    Feed(Vec<f32>),
}

/// Frames go to the worker only while a draft runs (one atomic load otherwise).
static OPEN: AtomicBool = AtomicBool::new(false);
static TX: Mutex<Option<mpsc::Sender<Cmd>>> = Mutex::new(None);
/// Kept loaded between dictations while the setting is on.
static SESSION: Mutex<Option<Session>> = Mutex::new(None);

/// Feed one 16 kHz frame from the recorder.
pub fn feed(frame: &[f32]) {
    if !OPEN.load(Ordering::Relaxed) {
        return;
    }
    if let Some(tx) = TX.lock().unwrap().as_ref() {
        let _ = tx.send(Cmd::Feed(frame.to_vec()));
    }
}

/// Start a draft for the recording that's starting, if everything's ready.
pub fn start(app: &AppHandle) {
    let settings = crate::settings::get_settings(app);
    if !settings.live_draft || OPEN.load(Ordering::Relaxed) {
        return;
    }
    let (tx, rx) = mpsc::channel();
    *TX.lock().unwrap() = Some(tx);
    OPEN.store(true, Ordering::Relaxed);
    let app = app.clone();
    std::thread::spawn(move || {
        if let Err(reason) = run_bubble(&app, rx) {
            debug!("No live draft: {reason}");
        }
    });
}

/// The draft in Felix's own bubble: nothing in the field.
fn run_bubble(app: &AppHandle, rx: mpsc::Receiver<Cmd>) -> Result<(), String> {
    // Whatever's typed with secure input on (passwords) stays off screen.
    if crate::secure_input::is_enabled_now() {
        return Err("secure input is on".into());
    }
    let path = model_path(app).ok_or("the draft model isn't downloaded")?;
    if !OPEN.load(Ordering::Relaxed) {
        return Err("recording ended first".into());
    }
    crate::draft_bubble::begin(app);
    let mut sent = 0usize;
    let result = stream_draft(&path, rx, |text| {
        // Frames still queued when recording stopped mustn't bring it back.
        if OPEN.load(Ordering::Relaxed) {
            sent += 1;
            crate::draft_bubble::set_text(app, text);
        }
    });
    // In case an update slipped in as recording stopped.
    if !OPEN.load(Ordering::Relaxed) {
        crate::draft_bubble::end(app);
    }
    debug!("Live draft bubble: {sent} updates");
    result
}

/// The small draft model sometimes gets stuck repeating a word or two ("the
/// plan the plan the plan the plan") before it recovers. Show such a run
/// once: a phrase of 2–4 words repeated 3+ times in a row, or one word 4+
/// times ("no no no" is real speech).
fn without_loops(text: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    let key = |w: &str| {
        w.trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase()
    };
    let mut out: Vec<&str> = Vec::with_capacity(words.len());
    let mut i = 0;
    'next: while i < words.len() {
        for n in 1..=4 {
            let min_repeats = if n == 1 { 4 } else { 3 };
            let same = |a: usize, b: usize| (0..n).all(|k| key(words[a + k]) == key(words[b + k]));
            let mut repeats = 1;
            while i + (repeats + 1) * n <= words.len() && same(i, i + repeats * n) {
                repeats += 1;
            }
            if repeats >= min_repeats {
                // Keep the last copy, which carries the latest punctuation.
                out.extend_from_slice(&words[i + (repeats - 1) * n..i + repeats * n]);
                i += repeats * n;
                continue 'next;
            }
        }
        out.push(words[i]);
        i += 1;
    }
    out.join(" ")
}

/// Holds the draft steady: shows only the words two guesses in a row agree
/// on, so a word the model is still unsure of doesn't flicker in and out.
/// Once shown, words stay until two guesses agree on something else.
#[derive(Default)]
struct Steady {
    last: Vec<String>,
    shown: Vec<String>,
}

impl Steady {
    fn next(&mut self, guess: &str) -> String {
        let key = |w: &str| {
            w.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        };
        let words: Vec<String> = guess.split_whitespace().map(str::to_string).collect();
        let agreed = words
            .iter()
            .zip(&self.last)
            .take_while(|(a, b)| key(a) == key(b))
            .count();
        let contradicts =
            (0..agreed.min(self.shown.len())).any(|i| key(&words[i]) != key(&self.shown[i]));
        if agreed >= self.shown.len() || contradicts {
            // The newest guess carries the latest punctuation.
            self.shown = words[..agreed].to_vec();
        }
        self.last = words;
        self.shown.join(" ")
    }
}

/// Feed the recording to the draft model and hand each new guess to `show`
/// until the recording stops.
fn stream_draft(
    path: &std::path::Path,
    rx: mpsc::Receiver<Cmd>,
    mut show: impl FnMut(&str),
) -> Result<(), String> {
    let mut session = SESSION.lock().unwrap();
    if session.is_none() {
        let model = Model::load_with(path, &ModelOptions::default())
            .map_err(|e| format!("draft model: {e}"))?;
        *session = Some(model.session().map_err(|e| format!("draft session: {e}"))?);
        info!("Loaded the live draft model");
    }
    let session = session.as_mut().unwrap();
    let mut stream = session
        .stream(&RunOptions::default(), &StreamOptions::default())
        .map_err(|e| format!("draft stream: {e}"))?;
    let mut last = String::new();
    let mut steady = Steady::default();
    while let Ok(Cmd::Feed(pcm)) = rx.recv() {
        match stream.feed(&pcm) {
            Ok(update) if update.committed_changed || update.tentative_changed => {
                // The whole current guess: committed + tentative can lose
                // the space where they meet.
                let guess = without_loops(stream.text().full.replace(['\n', '\r'], " ").trim());
                let draft = steady.next(&guess);
                if draft != last {
                    show(&draft);
                    last = draft;
                }
            }
            Ok(_) => {}
            Err(e) => warn!("Draft stream feed failed: {e}"),
        }
    }
    Ok(())
}

/// End the draft. Safe to call when no draft runs.
pub fn stop(app: &AppHandle) {
    crate::draft_bubble::end(app);
    OPEN.store(false, Ordering::Relaxed);
    *TX.lock().unwrap() = None; // the worker's loop ends
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn model_path(app: &AppHandle) -> Option<PathBuf> {
    let models = app.state::<std::sync::Arc<crate::managers::model::ModelManager>>();
    models
        .get_model_path(MODEL_ID)
        .ok()
        .filter(|p| p.exists())
        .or_else(|| {
            let local = crate::portable::app_data_dir(app)
                .ok()?
                .join("models")
                .join(MODEL_FILE);
            local.exists().then_some(local)
        })
}

/// Run `work` on the main thread (TIS calls want it) and wait for it.
fn on_main<T: Send + 'static>(
    app: &AppHandle,
    work: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    extern "C" {
        fn pthread_main_np() -> i32;
    }
    if unsafe { pthread_main_np() } != 0 {
        return Some(work());
    }
    let (tx, rx) = mpsc::channel();
    app.run_on_main_thread(move || {
        let _ = tx.send(work());
    })
    .ok()?;
    rx.recv_timeout(Duration::from_secs(2)).ok()
}

// ------------------------------------------------------------ setup & status

/// What the Dictation page shows about the live draft.
#[derive(Debug, Clone, Serialize, Type)]
pub struct LiveDraftStatus {
    pub model_ready: bool,
}

/// Take the old Felix Draft input method off the Mac: switch it off in
/// Input Sources (so it leaves the keyboard menu), stop it, and delete it
/// from ~/Library/Input Methods. Nothing to do once it's gone.
pub fn remove_old_input_method(app: &AppHandle) {
    let Some(target) = home().map(|h| h.join("Library/Input Methods").join(IME_APP)) else {
        return;
    };
    if !target.exists() {
        return;
    }
    let disabled = on_main(app, || tis::disable(IME_ID));
    if let Some(Err(e)) = disabled {
        warn!("Felix Draft: {e}");
    }
    let _ = std::process::Command::new("/usr/bin/pkill")
        .args(["-x", "FelixDraft"])
        .status();
    match std::fs::remove_dir_all(&target) {
        Ok(()) => info!("Removed the old Felix Draft input method"),
        Err(e) => warn!("Couldn't remove Felix Draft: {e}"),
    }
}

#[tauri::command]
#[specta::specta]
pub fn live_draft_status(app: AppHandle) -> LiveDraftStatus {
    LiveDraftStatus {
        model_ready: model_path(&app).is_some(),
    }
}

/// Turn the live draft on or off.
#[tauri::command]
#[specta::specta]
pub fn set_live_draft(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = crate::settings::get_settings(&app);
    settings.live_draft = enabled;
    crate::settings::write_settings(&app, settings);
    if !enabled {
        *SESSION.lock().unwrap() = None;
    }
    Ok(())
}

/// Text Input Sources (Carbon), to switch the old input method off.
mod tis {
    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::string::{CFString, CFStringRef};
    use std::ffi::c_void;

    type TISInputSourceRef = *const c_void;

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        static kTISPropertyInputSourceID: CFStringRef;
        fn TISCreateInputSourceList(props: CFDictionaryRef, all: u8) -> CFArrayRef;
        fn TISDisableInputSource(source: TISInputSourceRef) -> i32;
    }

    /// Switch off the input source with this ID, if macOS knows it.
    pub fn disable(id: &str) -> Result<(), String> {
        let key = unsafe { CFString::wrap_under_get_rule(kTISPropertyInputSourceID) };
        let filter = CFDictionary::from_CFType_pairs(&[(key, CFString::new(id))]);
        let list = unsafe { TISCreateInputSourceList(filter.as_concrete_TypeRef(), 1) };
        if list.is_null() {
            return Ok(());
        }
        let list: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(list) };
        let Some(source) = list.get(0) else {
            return Ok(());
        };
        match unsafe { TISDisableInputSource(source.as_CFTypeRef()) } {
            0 => Ok(()),
            err => Err(format!("couldn't switch {id} off ({err})")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{without_loops, Steady};

    #[test]
    fn draft_shows_what_two_guesses_agree_on() {
        let mut s = Steady::default();
        assert_eq!(s.next("we ship"), "");
        assert_eq!(s.next("we ship on"), "we ship");
        // An unsure last word flickering doesn't show.
        assert_eq!(s.next("we ship at"), "we ship");
        assert_eq!(s.next("we ship at noon."), "we ship at");
        // One odd guess doesn't take shown words back...
        assert_eq!(s.next("with chip"), "we ship at");
        // ...two that agree do.
        assert_eq!(s.next("with chip at noon"), "with chip");
    }

    #[test]
    fn loops_show_once_and_real_speech_stays() {
        assert_eq!(
            without_loops("so the plan the plan the plan the plan is to ship"),
            "so the plan is to ship"
        );
        assert_eq!(without_loops("I think the the the the the"), "I think the");
        assert_eq!(
            without_loops("we should, we should, we should, we should go"),
            "we should go"
        );
        for text in [
            "no no no, not that one",
            "that that is fine",
            "one two three",
        ] {
            assert_eq!(without_loops(text), text);
        }
    }
}
