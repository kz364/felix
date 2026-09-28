//! Live draft: while you dictate, a small streaming model (Moonshine
//! Streaming Tiny) transcribes alongside the main one, and its rough text
//! shows either in a see-through bubble by the cursor that Felix draws
//! itself (`draft_bubble`, the default), or as marked text (underlined, not
//! yet final) in the focused field through the Felix Draft input method
//! (`draft-ime/`).
//!
//! The bubble never touches the field. For the inline draft:
//!
//! The draft never becomes the final text. It's cleared the moment recording
//! stops, before anything reads the field, and the input source switched
//! back; the main model's text is then pasted exactly as without the draft.
//! Anything that isn't ready (setting off, model or input method missing,
//! secure input, a selection that marked text would replace) means no draft.

use log::{debug, info, warn};
use serde::Serialize;
use specta::Type;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
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
const IME_ID: &str = "com.pais.inputmethod.FelixDraft";
const IME_APP: &str = "FelixDraft.app";

enum Cmd {
    Feed(Vec<f32>),
}

/// Frames go to the worker only while a draft runs (one atomic load otherwise).
static OPEN: AtomicBool = AtomicBool::new(false);
static TX: Mutex<Option<mpsc::Sender<Cmd>>> = Mutex::new(None);
/// The connection to the input method; whoever holds it may change the draft.
/// `stop` takes it, so nothing can show a draft after it's cleared.
static CONN: Mutex<Option<UnixStream>> = Mutex::new(None);
/// The input source to go back to.
static PREVIOUS: Mutex<Option<String>> = Mutex::new(None);
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
    let style = settings.live_draft_style;
    std::thread::spawn(move || {
        let result = match style {
            crate::settings::LiveDraftStyle::Bubble => run_bubble(&app, rx),
            crate::settings::LiveDraftStyle::Inline => run(&app, rx),
        };
        if let Err(reason) = result {
            debug!("No live draft: {reason}");
        }
    });
}

/// The draft in Felix's own bubble: no input method, nothing in the field.
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
    while let Ok(Cmd::Feed(pcm)) = rx.recv() {
        match stream.feed(&pcm) {
            Ok(update) if update.committed_changed || update.tentative_changed => {
                // The whole current guess: committed + tentative can lose
                // the space where they meet.
                let draft = stream
                    .text()
                    .full
                    .replace(['\n', '\r'], " ")
                    .trim()
                    .to_string();
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

fn run(app: &AppHandle, rx: mpsc::Receiver<Cmd>) -> Result<(), String> {
    if crate::secure_input::is_enabled_now() {
        return Err("secure input is on".into());
    }
    // Marked text would replace a selection, and clearing it would lose it.
    if crate::text_field::focused_field()
        .and_then(|f| f.selection)
        .is_some_and(|(_, len)| len > 0)
    {
        return Err("text is selected".into());
    }
    let path = model_path(app).ok_or("the draft model isn't downloaded")?;

    let previous = on_main(app, || {
        let previous = tis::current_id();
        tis::select(IME_ID).map(|_| previous)
    })
    .ok_or("couldn't reach the main thread")??;
    if !OPEN.load(Ordering::Relaxed) {
        restore(app, previous);
        return Err("recording ended first".into());
    }
    *PREVIOUS.lock().unwrap() = previous;
    let conn = connect().ok_or("the input method isn't answering")?;
    *CONN.lock().unwrap() = Some(conn);

    stream_draft(&path, rx, send_draft)
}

fn send_draft(text: &str) {
    let mut conn = CONN.lock().unwrap();
    if let Some(stream) = conn.as_mut() {
        if writeln!(stream, "D {text}").is_err() {
            *conn = None;
        }
    }
}

/// End the draft: clear it from the field and switch the input source back,
/// before returning. Safe to call when no draft runs.
pub fn stop(app: &AppHandle) {
    crate::draft_bubble::end(app);
    let was_open = OPEN.swap(false, Ordering::Relaxed);
    *TX.lock().unwrap() = None; // the worker's loop ends
    if let Some(mut conn) = CONN.lock().unwrap().take() {
        let _ = conn.set_read_timeout(Some(Duration::from_millis(400)));
        if writeln!(conn, "C").is_ok() {
            let mut reply = String::new();
            let _ = BufReader::new(&conn).read_line(&mut reply);
        }
    }
    let previous = PREVIOUS.lock().unwrap().take();
    if was_open || previous.is_some() {
        restore(app, previous);
    }
}

fn restore(app: &AppHandle, previous: Option<String>) {
    let Some(previous) = previous else { return };
    if previous == IME_ID {
        return;
    }
    let restored = on_main(app, move || {
        // Only if Felix Draft is still the one selected: the user may have
        // switched input sources themselves meanwhile.
        if tis::current_id().as_deref() == Some(IME_ID) {
            let _ = tis::select(&previous);
        }
    });
    if restored.is_none() {
        warn!("Couldn't switch the input source back");
    }
}

/// The input method's socket; it may take a moment to start after it's
/// first selected.
fn connect() -> Option<UnixStream> {
    let path = socket_path()?;
    for _ in 0..20 {
        if let Ok(stream) = UnixStream::connect(&path) {
            return Some(stream);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

fn socket_path() -> Option<PathBuf> {
    home().map(|h| h.join("Library/Application Support/com.pais.handy/draft-ime.sock"))
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
    /// Felix Draft is in ~/Library/Input Methods and macOS knows it.
    pub installed: bool,
    /// Added in System Settings → Keyboard → Input Sources (only the user
    /// can do this).
    pub enabled: bool,
}

/// Put Felix Draft in ~/Library/Input Methods (replacing an older copy) and
/// register it. macOS only lets the user turn it on.
pub fn install(app: &AppHandle) -> Result<(), String> {
    let source = app
        .path()
        .resolve(
            format!("resources/draft-ime/{IME_APP}"),
            tauri::path::BaseDirectory::Resource,
        )
        .map_err(|e| e.to_string())?;
    if !source.exists() {
        return Err("Felix Draft isn't bundled in this build".into());
    }
    let dir = home()
        .ok_or("no home folder")?
        .join("Library/Input Methods");
    let target = dir.join(IME_APP);
    let binary = |app: &PathBuf| std::fs::read(app.join("Contents/MacOS/FelixDraft")).ok();
    if binary(&source) != binary(&target) {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let _ = std::fs::remove_dir_all(&target);
        let status = std::process::Command::new("/usr/bin/ditto")
            .arg(&source)
            .arg(&target)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("couldn't copy Felix Draft".into());
        }
        // The old copy may still be running.
        let _ = std::process::Command::new("/usr/bin/pkill")
            .args(["-x", "FelixDraft"])
            .status();
        info!("Installed Felix Draft in {}", dir.display());
    }
    tis::register(&target)?;
    let _ = tis::enable(IME_ID); // macOS usually leaves this to the user
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn live_draft_status(app: AppHandle) -> LiveDraftStatus {
    let (installed, enabled) = on_main(&app, || tis::state(IME_ID)).unwrap_or((false, false));
    LiveDraftStatus {
        model_ready: model_path(&app).is_some(),
        installed,
        enabled,
    }
}

/// Turn the live draft on or off; turning it on installs Felix Draft.
#[tauri::command]
#[specta::specta]
pub fn set_live_draft(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = crate::settings::get_settings(&app);
    settings.live_draft = enabled;
    let inline = settings.live_draft_style == crate::settings::LiveDraftStyle::Inline;
    crate::settings::write_settings(&app, settings);
    if enabled && inline {
        install(&app)?;
    } else if enabled {
        // Nothing to install for the bubble.
    } else {
        *SESSION.lock().unwrap() = None;
    }
    Ok(())
}

/// Choose where the draft shows; the in-field draft needs Felix Draft.
#[tauri::command]
#[specta::specta]
pub fn set_live_draft_style(
    app: AppHandle,
    style: crate::settings::LiveDraftStyle,
) -> Result<(), String> {
    let mut settings = crate::settings::get_settings(&app);
    settings.live_draft_style = style;
    let install_now = settings.live_draft && style == crate::settings::LiveDraftStyle::Inline;
    crate::settings::write_settings(&app, settings);
    if install_now {
        install(&app)?;
    }
    Ok(())
}

/// Open System Settings → Keyboard, where input sources are added.
#[tauri::command]
#[specta::specta]
pub fn open_input_sources() -> Result<(), String> {
    std::process::Command::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.Keyboard-Settings.extension")
        .status()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Text Input Sources (Carbon), for switching to Felix Draft and back.
mod tis {
    use core_foundation::array::{CFArray, CFArrayRef};
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
    use core_foundation::string::{CFString, CFStringRef};
    use core_foundation::url::{CFURLRef, CFURL};
    use std::ffi::c_void;
    use std::path::Path;

    type TISInputSourceRef = *const c_void;

    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        static kTISPropertyInputSourceID: CFStringRef;
        static kTISPropertyInputSourceIsEnabled: CFStringRef;
        fn TISCreateInputSourceList(props: CFDictionaryRef, all: u8) -> CFArrayRef;
        fn TISCopyCurrentKeyboardInputSource() -> TISInputSourceRef;
        fn TISGetInputSourceProperty(
            source: TISInputSourceRef,
            key: *const c_void,
        ) -> *const c_void;
        fn TISSelectInputSource(source: TISInputSourceRef) -> i32;
        fn TISEnableInputSource(source: TISInputSourceRef) -> i32;
        fn TISRegisterInputSource(url: CFURLRef) -> i32;
        fn CFRelease(cf: *const c_void);
    }

    fn string_prop(source: TISInputSourceRef, key: CFStringRef) -> Option<String> {
        let value = unsafe { TISGetInputSourceProperty(source, key as *const c_void) };
        (!value.is_null())
            .then(|| unsafe { CFString::wrap_under_get_rule(value as CFStringRef) }.to_string())
    }

    /// Calls `f` with the input source with this ID, if macOS knows it.
    fn with_source<T>(id: &str, f: impl FnOnce(TISInputSourceRef) -> T) -> Option<T> {
        let key = unsafe { CFString::wrap_under_get_rule(kTISPropertyInputSourceID) };
        let filter = CFDictionary::from_CFType_pairs(&[(key, CFString::new(id))]);
        let list = unsafe { TISCreateInputSourceList(filter.as_concrete_TypeRef(), 1) };
        if list.is_null() {
            return None;
        }
        let list: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(list) };
        let source = list.get(0)?.as_CFTypeRef();
        Some(f(source))
    }

    pub fn current_id() -> Option<String> {
        let source = unsafe { TISCopyCurrentKeyboardInputSource() };
        if source.is_null() {
            return None;
        }
        let id = string_prop(source, unsafe { kTISPropertyInputSourceID });
        unsafe { CFRelease(source) };
        id
    }

    pub fn select(id: &str) -> Result<(), String> {
        match with_source(id, |s| unsafe { TISSelectInputSource(s) }) {
            Some(0) => Ok(()),
            Some(err) => Err(format!(
                "couldn't switch to {id} ({err}); is it added in Input Sources?"
            )),
            None => Err(format!("{id} isn't installed")),
        }
    }

    pub fn enable(id: &str) -> Result<(), String> {
        match with_source(id, |s| unsafe { TISEnableInputSource(s) }) {
            Some(0) => Ok(()),
            Some(err) => Err(format!("couldn't enable {id} ({err})")),
            None => Err(format!("{id} isn't installed")),
        }
    }

    /// (known to macOS, enabled)
    pub fn state(id: &str) -> (bool, bool) {
        with_source(id, |s| {
            let value = unsafe {
                TISGetInputSourceProperty(s, kTISPropertyInputSourceIsEnabled as *const c_void)
            };
            !value.is_null() && bool::from(unsafe { CFBoolean::wrap_under_get_rule(value as _) })
        })
        .map_or((false, false), |enabled| (true, enabled))
    }

    pub fn register(app: &Path) -> Result<(), String> {
        let url = CFURL::from_path(app, true).ok_or("bad path")?;
        match unsafe { TISRegisterInputSource(url.as_concrete_TypeRef()) } {
            0 => Ok(()),
            err => Err(format!("couldn't register Felix Draft ({err})")),
        }
    }
}
