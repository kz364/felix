//! One computer task at a time, shown live on the overlay card, with the
//! user's permissions checked on every step.
//!
//! Every Cua call goes through `check_call`, whether it comes from the fast
//! path (in this process) or from Codex (through `cua_gate`, a relay between
//! Codex and the driver that asks this process over a local socket). That one
//! place:
//! - refuses driver tools a task has no business using (config, installs,
//!   reading the clipboard, recording the screen);
//! - asks before Felix first touches an app ("Always allow / Allow this time
//!   / Deny"), never allows password managers or System Settings, and asks
//!   every time for terminals;
//! - asks for a confirmation (button or a spoken "yes") before anything that
//!   sends, posts, deletes or buys;
//! - turns the call into a plain-language step on the card ("Clicking New
//!   Note in Notes");
//! - stops everything when the user presses Stop.

use crate::settings::{get_settings, write_settings, AgentAppAccess};
use log::{info, warn};
use once_cell::sync::{Lazy, OnceCell};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::AppHandle;

/// How long a question waits for an answer before counting as "no".
const ANSWER_TIMEOUT: Duration = Duration::from_secs(120);
/// Finished steps kept on the card (all of them go to the run log).
const CARD_STEPS: usize = 3;
const RUN_LOG: &str = "agent_runs.jsonl";

static APP: OnceCell<AppHandle> = OnceCell::new();
static RUN: Lazy<Mutex<Option<Run>>> = Lazy::new(|| Mutex::new(None));
static STOP: AtomicBool = AtomicBool::new(false);
/// The card is on screen (working, or showing how it ended).
static CARD_SHOWN: AtomicBool = AtomicBool::new(false);

/// What the overlay card shows.
#[derive(Clone, Debug, Default, Serialize, Type)]
pub struct AgentCard {
    /// Who's working ("Felix").
    pub name: String,
    /// The task, as the user asked for it.
    pub task: String,
    /// The last few finished steps, oldest first.
    pub steps: Vec<String>,
    /// What it's doing now.
    pub current: Option<String>,
    pub status: AgentStatus,
    /// How it ended, in a sentence or two.
    pub message: Option<String>,
    pub question: Option<AgentQuestion>,
    /// Auto-close delay once finished; 0 = stays until closed.
    pub timeout_ms: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    #[default]
    Working,
    Done,
    /// Stopped before a step only the user should do, or the user said no.
    NeedsYou,
    Failed,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentQuestion {
    /// First use of an app in this task.
    AppAccess { app: String, can_always: bool },
    /// A step that sends, posts, deletes or buys.
    Confirm { action: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentAnswer {
    AlwaysAllow,
    AllowOnce,
    Deny,
    AlwaysDeny,
    Confirm,
    Cancel,
}

struct Run {
    card: AgentCard,
    all_steps: Vec<String>,
    started: Instant,
    started_at: String,
    /// Apps allowed or denied for this task only (bundle id or name).
    allowed_once: HashSet<String>,
    denied: HashSet<String>,
    /// The app the task last touched, for tools that don't name one.
    last_app: Option<App>,
    answer: Option<mpsc::Sender<AgentAnswer>>,
    /// The user said no to a step or an app, so something is left for them.
    refused: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct App {
    name: String,
    bundle_id: String,
}

impl App {
    fn key(&self) -> String {
        if self.bundle_id.is_empty() {
            self.name.to_lowercase()
        } else {
            self.bundle_id.clone()
        }
    }
}

pub fn init(app: &AppHandle) {
    let _ = APP.set(app.clone());
}

fn app_handle() -> Option<&'static AppHandle> {
    APP.get()
}

fn emit() {
    let card = RUN.lock().unwrap().as_ref().map(|r| r.card.clone());
    if let (Some(app), Some(card)) = (app_handle(), card) {
        crate::overlay::show_agent_card(app, card);
    }
}

/// A task is running (not just its final card showing).
pub fn busy() -> bool {
    RUN.lock()
        .unwrap()
        .as_ref()
        .is_some_and(|r| r.card.status == AgentStatus::Working)
}

/// The card is on screen, so the overlay should come back to it after a
/// dictation instead of hiding.
pub fn card_shown() -> bool {
    CARD_SHOWN.load(Ordering::SeqCst) && RUN.lock().unwrap().is_some()
}

/// Show the card again (after a dictation took over the overlay).
pub fn reshow() {
    emit();
}

pub fn stopped() -> bool {
    STOP.load(Ordering::SeqCst)
}

/// Start a task's card. Fails when another task is still running.
pub fn start(name: &str, task: &str) -> Result<(), String> {
    let mut run = RUN.lock().unwrap();
    if run
        .as_ref()
        .is_some_and(|r| r.card.status == AgentStatus::Working)
    {
        return Err(format!("{name} is still working on the last task"));
    }
    STOP.store(false, Ordering::SeqCst);
    CARD_SHOWN.store(true, Ordering::SeqCst);
    *run = Some(Run {
        card: AgentCard {
            name: name.to_string(),
            task: task.to_string(),
            current: Some("Getting started".into()),
            ..Default::default()
        },
        all_steps: Vec::new(),
        started: Instant::now(),
        started_at: chrono::Local::now().to_rfc3339(),
        allowed_once: HashSet::new(),
        denied: HashSet::new(),
        last_app: None,
        answer: None,
        refused: false,
    });
    drop(run);
    emit();
    Ok(())
}

/// Show a new step; the previous one moves to the done list.
pub fn step(text: &str) {
    let changed = {
        let mut guard = RUN.lock().unwrap();
        let Some(run) = guard.as_mut() else { return };
        if run.card.current.as_deref() == Some(text) {
            false
        } else {
            if let Some(prev) = run.card.current.take() {
                if prev != "Getting started" {
                    run.card.steps.push(prev);
                    let extra = run.card.steps.len().saturating_sub(CARD_STEPS);
                    run.card.steps.drain(..extra);
                }
            }
            run.card.current = Some(text.to_string());
            run.all_steps.push(text.to_string());
            true
        }
    };
    if changed {
        emit();
    }
}

/// End the task: the card shows how it went, and the run is logged.
pub fn finish(status: AgentStatus, message: &str) {
    let record = {
        let mut guard = RUN.lock().unwrap();
        let Some(run) = guard.as_mut() else { return };
        if let Some(prev) = run.card.current.take() {
            if prev != "Getting started" {
                run.card.steps.push(prev);
                let extra = run.card.steps.len().saturating_sub(CARD_STEPS);
                run.card.steps.drain(..extra);
            }
        }
        run.card.status = if stopped() {
            AgentStatus::Stopped
        } else if status == AgentStatus::Done && run.refused {
            AgentStatus::NeedsYou
        } else {
            status
        };
        run.card.message = Some(message.trim().to_string());
        run.card.question = None;
        run.answer = None;
        run.card.timeout_ms = app_handle()
            .map(|a| get_settings(a).result_popup_seconds.saturating_mul(1000))
            .unwrap_or(0);
        AgentRunRecord {
            at: run.started_at.clone(),
            task: run.card.task.clone(),
            steps: run.all_steps.clone(),
            status: run.card.status,
            message: message.trim().to_string(),
            seconds: run.started.elapsed().as_secs_f32(),
        }
    };
    info!(
        "Computer task ended ({:?}) after {} steps",
        record.status,
        record.steps.len()
    );
    log_run(&record);
    emit();
}

/// The user closed the card: stop the task if it's still going.
pub fn close() {
    if busy() {
        stop();
    } else {
        CARD_SHOWN.store(false, Ordering::SeqCst);
        *RUN.lock().unwrap() = None;
    }
}

/// Stop the running task: pending questions are answered "no" and the next
/// driver call fails.
pub fn stop() {
    STOP.store(true, Ordering::SeqCst);
    let sender = RUN.lock().unwrap().as_mut().and_then(|r| r.answer.take());
    if let Some(tx) = sender {
        let _ = tx.send(AgentAnswer::Cancel);
    }
    step("Stopping");
}

/// A button on the card.
pub fn answer(answer: AgentAnswer) {
    let sender = RUN.lock().unwrap().as_mut().and_then(|r| r.answer.take());
    if let Some(tx) = sender {
        let _ = tx.send(answer);
    }
}

const YES_WORDS: &[&str] = &[
    "yes", "yeah", "yep", "yup", "sure", "ok", "okay", "do", "it", "go", "ahead", "for", "send",
    "confirm", "allow", "please", "that's", "fine", "thanks", "thank", "you",
];
const NO_WORDS: &[&str] = &[
    "no", "nope", "nah", "don't", "do", "not", "it", "that", "cancel", "stop", "deny", "never",
    "mind", "please", "thanks", "thank", "you",
];
const NEGATIVE: &[&str] = &[
    "no", "nope", "nah", "don't", "not", "cancel", "stop", "deny", "never",
];
const ALWAYS_WORDS: &[&str] = &["always", "allow", "yes", "yeah", "ok", "okay", "please"];
const NEVER_WORDS: &[&str] = &[
    "never", "always", "deny", "no", "don't", "allow", "it", "please",
];

/// Read a dictation as the answer to the card's question. Only a few words,
/// all of them answer words, count; anything else is an ordinary dictation.
fn spoken_answer(question: &AgentQuestion, text: &str) -> Option<AgentAnswer> {
    let words: Vec<String> = text
        .to_lowercase()
        .replace('’', "'")
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect();
    let all_in =
        |set: &[&str]| !words.is_empty() && words.iter().all(|w| set.contains(&w.as_str()));
    let has = |set: &[&str]| words.iter().any(|w| set.contains(&w.as_str()));
    if words.len() > 5 {
        return None;
    }
    let no = all_in(NO_WORDS) && has(NEGATIVE);
    let yes = all_in(YES_WORDS) && !has(NEGATIVE);
    match question {
        AgentQuestion::AppAccess { can_always, .. } => {
            if *can_always && all_in(ALWAYS_WORDS) && has(&["always"]) {
                Some(AgentAnswer::AlwaysAllow)
            } else if all_in(NEVER_WORDS)
                && (has(&["never"]) || (has(&["always"]) && has(&["deny"])))
            {
                Some(AgentAnswer::AlwaysDeny)
            } else if no {
                Some(AgentAnswer::Deny)
            } else if yes {
                Some(AgentAnswer::AllowOnce)
            } else {
                None
            }
        }
        AgentQuestion::Confirm { .. } => {
            if no {
                Some(AgentAnswer::Cancel)
            } else if yes {
                Some(AgentAnswer::Confirm)
            } else {
                None
            }
        }
    }
}

/// When the card is asking something and this dictation answers it, answer
/// and return true (the dictation isn't pasted).
pub fn answer_by_voice(text: &str) -> bool {
    let question = RUN
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|r| r.card.question.clone());
    let Some(question) = question else {
        return false;
    };
    match spoken_answer(&question, text) {
        Some(a) => {
            info!("Spoken answer to the task's question: {a:?}");
            answer(a);
            true
        }
        None => false,
    }
}

/// Put a question on the card and wait for the answer (Stop and the timeout
/// count as no).
fn ask(question: AgentQuestion) -> AgentAnswer {
    let (tx, rx) = mpsc::channel();
    {
        let mut guard = RUN.lock().unwrap();
        let Some(run) = guard.as_mut() else {
            return AgentAnswer::Cancel;
        };
        run.card.question = Some(question.clone());
        run.answer = Some(tx);
    }
    emit();
    let no = match question {
        AgentQuestion::AppAccess { .. } => AgentAnswer::Deny,
        AgentQuestion::Confirm { .. } => AgentAnswer::Cancel,
    };
    let started = Instant::now();
    let answer = loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(a) => break a,
            Err(mpsc::RecvTimeoutError::Timeout)
                if !stopped() && started.elapsed() < ANSWER_TIMEOUT => {}
            Err(_) => break no,
        }
    };
    if let Some(run) = RUN.lock().unwrap().as_mut() {
        run.card.question = None;
        run.answer = None;
    }
    emit();
    answer
}

// ---- Permissions -------------------------------------------------------

/// Never used by Felix: passwords, keys and the Mac's own settings.
const BLOCKED_APPS: &[&str] = &[
    "com.1password.1password",
    "com.agilebits.onepassword7",
    "com.bitwarden.desktop",
    "com.lastpass.lastpassmacdesktop",
    "com.dashlane.dashlanephonefinal",
    "com.apple.keychainaccess",
    "com.apple.Passwords",
    "com.apple.systempreferences",
    "com.apple.SystemSettings",
    "com.pais.handy",
];

/// Asked every time: a terminal can do anything.
const ASK_EVERY_TIME: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "com.mitchellh.ghostty",
    "dev.warp.Warp-Stable",
    "net.kovidgoyal.kitty",
    "org.alacritty",
    "co.zeit.hyper",
];

/// Apps where Return usually sends what's been typed.
const SENDS_ON_RETURN: &[&str] = &[
    "com.apple.MobileSMS",
    "com.tinyspeck.slackmacgap",
    "net.whatsapp.WhatsApp",
    "com.hnc.Discord",
    "ru.keepcoder.Telegram",
    "org.whispersystems.signal-desktop",
    "com.microsoft.teams2",
    "com.facebook.archon",
];

/// Driver tools a task may use; anything else is refused (and hidden from
/// Codex). Left out on purpose: changing the driver's config, installing
/// things, reading the clipboard, whole-desktop screenshots, recordings,
/// killing apps, running page scripts, downloads and file uploads.
pub const ALLOWED_TOOLS: &[&str] = &[
    "list_apps",
    "list_windows",
    "get_window_state",
    "verify_state",
    "launch_app",
    "bring_to_front",
    "set_window_frame",
    "invoke_menu",
    "click",
    "double_click",
    "right_click",
    "drag",
    "type_text",
    "press_key",
    "hotkey",
    "set_value",
    "scroll",
    "clipboard_write",
    "get_screen_size",
    "get_cursor_position",
    "move_cursor",
    "set_agent_cursor_enabled",
    "set_agent_cursor_motion",
    "set_agent_cursor_theme",
    "get_agent_cursor_state",
    "zoom",
    "get_browser_state",
    "browser_prepare",
    "browser_navigate",
    "browser_click",
    "browser_type",
    "browser_dialog",
    "browser_pointer",
    "start_session",
    "get_session",
    "get_session_state",
    "list_sessions",
    "end_session",
];

/// Tools that only look; they don't need a step on the card of their own.
const LOOKING: &[&str] = &[
    "list_windows",
    "get_window_state",
    "verify_state",
    "zoom",
    "get_browser_state",
];

/// Words on a button (or menu item) that make pressing it outward or
/// irreversible.
pub static RISKY: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(send|post|publish|share|reply|forward|delete|remove|erase|empty|trash|buy|purchase|pay|checkout|place order|transfer|submit|sign out|log ?out|unsubscribe)\b").unwrap()
});

fn app_for_pid(pid: i64) -> Option<App> {
    #[cfg(target_os = "macos")]
    {
        let app = objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(
            pid as i32,
        )?;
        Some(App {
            name: app
                .localizedName()
                .map(|n| n.to_string())
                .unwrap_or_default()
                .trim_start_matches('\u{200e}')
                .to_string(),
            bundle_id: app
                .bundleIdentifier()
                .map(|b| b.to_string())
                .unwrap_or_default(),
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        None
    }
}

/// The app a call acts on: its pid, the app it launches, or (for browser
/// tools that only name a tab) the last app the task touched.
fn app_of(tool: &str, args: &Value, last: Option<&App>) -> Option<App> {
    if let Some(pid) = args["pid"].as_i64() {
        return app_for_pid(pid);
    }
    if tool == "launch_app" {
        let bundle_id = args["bundle_id"].as_str().unwrap_or_default().to_string();
        let name = args["name"].as_str().unwrap_or_default().to_string();
        let name = if name.is_empty() {
            crate::installed_apps::installed_apps()
                .into_iter()
                .find(|a| a.bundle_id.as_deref() == Some(bundle_id.as_str()))
                .map(|a| a.name)
                .unwrap_or_else(|| bundle_id.clone())
        } else {
            name
        };
        let bundle_id = if bundle_id.is_empty() {
            crate::installed_apps::find(&name, &crate::installed_apps::installed_apps())
                .and_then(|a| a.bundle_id.clone())
                .unwrap_or_default()
        } else {
            bundle_id
        };
        return Some(App { name, bundle_id });
    }
    if tool.starts_with("browser_") || tool == "get_browser_state" {
        return last.cloned();
    }
    None
}

enum Access {
    Allowed,
    /// The user chose "Always deny".
    Denied,
    Blocked,
    Ask {
        can_always: bool,
    },
}

fn access(app: &App, always: &[AgentAppAccess]) -> Access {
    let id = app.bundle_id.as_str();
    if BLOCKED_APPS.iter().any(|b| b.eq_ignore_ascii_case(id)) {
        return Access::Blocked;
    }
    let saved = always.iter().find(|a| a.key() == app.key());
    if saved.is_some_and(|a| !a.allowed) {
        return Access::Denied;
    }
    if ASK_EVERY_TIME.iter().any(|b| b.eq_ignore_ascii_case(id)) {
        return Access::Ask { can_always: false };
    }
    if saved.is_some() {
        Access::Allowed
    } else {
        Access::Ask { can_always: true }
    }
}

/// Save "Always allow" or "Always deny" for an app.
fn remember_access(app: &App, allowed: bool) {
    let Some(handle) = app_handle() else { return };
    let mut settings = get_settings(handle);
    settings.agent_app_access.retain(|a| a.key() != app.key());
    settings.agent_app_access.push(AgentAppAccess {
        name: app.name.clone(),
        bundle_id: app.bundle_id.clone(),
        allowed,
    });
    write_settings(handle, settings);
}

/// Whether the task may use this app, asking the first time.
fn check_app(app: &App) -> Result<(), String> {
    let key = app.key();
    {
        let guard = RUN.lock().unwrap();
        let Some(run) = guard.as_ref() else {
            return Err("No task is running".into());
        };
        if run.allowed_once.contains(&key) {
            return Ok(());
        }
        if run.denied.contains(&key) {
            return Err(format!(
                "The user didn't allow Felix to use {}. Don't use it.",
                app.name
            ));
        }
    }
    let always = app_handle()
        .map(|a| get_settings(a).agent_app_access)
        .unwrap_or_default();
    match access(app, &always) {
        Access::Allowed => Ok(()),
        Access::Denied => Err(format!(
            "The user never lets Felix use {}. Don't use it.",
            app.name
        )),
        Access::Blocked => Err(format!(
            "Felix never uses {}: passwords and the Mac's settings are off limits.",
            app.name
        )),
        Access::Ask { can_always } => {
            let answer = ask(AgentQuestion::AppAccess {
                app: app.name.clone(),
                can_always,
            });
            let mut guard = RUN.lock().unwrap();
            let run = guard.as_mut().ok_or("No task is running")?;
            match answer {
                AgentAnswer::AlwaysAllow if can_always => {
                    drop(guard);
                    remember_access(app, true);
                    info!("Felix may always use {}", app.name);
                    Ok(())
                }
                AgentAnswer::AlwaysDeny => {
                    run.denied.insert(key);
                    run.refused = true;
                    drop(guard);
                    remember_access(app, false);
                    info!("Felix may never use {}", app.name);
                    Err(format!(
                        "The user never lets Felix use {}. Don't use it.",
                        app.name
                    ))
                }
                AgentAnswer::AlwaysAllow | AgentAnswer::AllowOnce | AgentAnswer::Confirm => {
                    run.allowed_once.insert(key);
                    Ok(())
                }
                _ => {
                    run.denied.insert(key);
                    run.refused = true;
                    Err(format!(
                        "The user didn't allow Felix to use {}. Don't use it.",
                        app.name
                    ))
                }
            }
        }
    }
}

/// The part of a press, keystroke or typed text that sends, posts, deletes
/// or buys, described for the confirmation ("Press Send in Slack").
fn risky_action(
    tool: &str,
    args: &Value,
    label: Option<&str>,
    app: Option<&App>,
) -> Option<String> {
    let in_app = app.map(|a| format!(" in {}", a.name)).unwrap_or_default();
    let sends_on_return = app.is_some_and(|a| SENDS_ON_RETURN.contains(&a.bundle_id.as_str()));
    match tool {
        "click" | "double_click" | "browser_click" | "browser_pointer" => {
            let label = label?;
            RISKY
                .is_match(label)
                .then(|| format!("Press “{label}”{in_app}"))
        }
        "invoke_menu" => {
            let path: Vec<&str> = args["path"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .collect();
            let item = path.last()?;
            RISKY
                .is_match(item)
                .then(|| format!("Choose {}{in_app}", path.join(" → ")))
        }
        "press_key" => {
            let key = args["key"].as_str()?.to_lowercase();
            (sends_on_return && matches!(key.as_str(), "return" | "enter"))
                .then(|| format!("Press Return{in_app}, which sends the message"))
        }
        "hotkey" => {
            let keys: Vec<String> = args["keys"]
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_lowercase)
                .collect();
            let has = |k: &str| keys.iter().any(|x| x == k);
            let enter = has("return") || has("enter");
            let mail_send = has("cmd") && has("shift") && has("d");
            let delete = has("cmd") && (has("delete") || has("backspace"));
            (enter && (sends_on_return || has("cmd") || has("ctrl")) || mail_send || delete)
                .then(|| format!("Press {}{in_app}", keys.join("+")))
        }
        "type_text" | "browser_type" | "set_value" => {
            let text = args["text"]
                .as_str()
                .or(args["value"].as_str())
                .unwrap_or_default();
            (sends_on_return && text.ends_with('\n'))
                .then(|| format!("Type and send the message{in_app}"))
        }
        _ => None,
    }
}

fn short(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        one_line
    } else {
        format!("{}…", one_line.chars().take(max).collect::<String>())
    }
}

/// A driver call as a step the user can follow ("Clicking New Note in
/// Notes"). `None` for calls that don't deserve a line of their own.
fn describe(tool: &str, args: &Value, label: Option<&str>, app: Option<&App>) -> Option<String> {
    let in_app = app.map(|a| format!(" in {}", a.name)).unwrap_or_default();
    let target = label
        .filter(|l| !l.trim().is_empty())
        .map(|l| format!(" {}", short(l, 40)))
        .unwrap_or_default();
    let text = match tool {
        "launch_app" => format!("Opening {}", app.map_or("an app", |a| a.name.as_str())),
        "bring_to_front" => format!(
            "Switching to {}",
            app.map_or("the app", |a| a.name.as_str())
        ),
        t if LOOKING.contains(&t) => {
            format!(
                "Looking at {}",
                app.map_or("the window", |a| a.name.as_str())
            )
        }
        "click" | "browser_click" | "browser_pointer" => {
            if target.is_empty() {
                format!("Clicking{in_app}")
            } else {
                format!("Clicking{target}{in_app}")
            }
        }
        "double_click" => format!("Double-clicking{target}{in_app}"),
        "right_click" => format!("Opening the menu for{target}{in_app}"),
        "invoke_menu" => {
            let path: Vec<&str> = args["path"]
                .as_array()
                .map(|p| p.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            format!("Choosing {}{in_app}", path.join(" → "))
        }
        "type_text" | "browser_type" => {
            let typed = args["text"].as_str().unwrap_or_default();
            format!("Typing “{}”{in_app}", short(typed, 40))
        }
        "set_value" => {
            let value = args["value"].as_str().unwrap_or_default();
            format!("Filling in{target} with “{}”{in_app}", short(value, 30))
        }
        "press_key" => format!(
            "Pressing {}{in_app}",
            args["key"].as_str().unwrap_or("a key")
        ),
        "hotkey" => {
            let keys: Vec<&str> = args["keys"]
                .as_array()
                .map(|k| k.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            format!("Pressing {}{in_app}", keys.join("+"))
        }
        "scroll" => format!("Scrolling{in_app}"),
        "drag" => format!("Dragging{in_app}"),
        "browser_navigate" => format!(
            "Going to {}",
            short(args["url"].as_str().unwrap_or("a page"), 50)
        ),
        "clipboard_write" => "Copying text".into(),
        _ => return None,
    };
    Some(text)
}

/// Check one driver call against the user's permissions, show it on the
/// card, and ask when it needs asking. `label` is the element's name when
/// the caller knows it. `Err` is a message for the agent: don't do it.
pub fn check_call(tool: &str, args: &Value, label: Option<&str>) -> Result<(), String> {
    if stopped() {
        return Err("The user stopped the task. Stop now.".into());
    }
    if !ALLOWED_TOOLS.contains(&tool) {
        warn!("Felix refused driver tool {tool}");
        return Err(format!("{tool} isn't available to Felix."));
    }
    let last = RUN
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|r| r.last_app.clone());
    let app = app_of(tool, args, last.as_ref());
    if let Some(app) = &app {
        check_app(app)?;
        if let Some(run) = RUN.lock().unwrap().as_mut() {
            run.last_app = Some(app.clone());
        }
    }
    if let Some(action) = risky_action(tool, args, label, app.as_ref()) {
        match ask(AgentQuestion::Confirm {
            action: action.clone(),
        }) {
            AgentAnswer::Confirm => info!("The user confirmed: {action}"),
            _ => {
                if let Some(run) = RUN.lock().unwrap().as_mut() {
                    run.refused = true;
                }
                return Err(format!(
                    "The user said no to this step ({action}). Leave it for them and finish."
                ));
            }
        }
    }
    // Reading a window is worth a line only before anything else happened;
    // after that it's between every step.
    let looked_before = LOOKING.contains(&tool)
        && RUN
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|r| !r.all_steps.is_empty());
    if let Some(text) = describe(tool, args, label, app.as_ref()).filter(|_| !looked_before) {
        step(&text);
    }
    if stopped() {
        return Err("The user stopped the task. Stop now.".into());
    }
    Ok(())
}

// ---- The gate's socket (for Codex, through `cua_gate`) -------------------

#[derive(Deserialize)]
struct GateRequest {
    tool: String,
    #[serde(default)]
    args: Value,
    #[serde(default)]
    label: Option<String>,
}

#[derive(Serialize)]
struct GateReply {
    ok: bool,
    message: String,
}

/// The socket `cua_gate` asks; started once, on first use.
pub fn gate_socket() -> Result<std::path::PathBuf, String> {
    static SOCKET: OnceCell<std::path::PathBuf> = OnceCell::new();
    SOCKET
        .get_or_try_init(|| {
            let path = std::env::temp_dir().join(format!("handy-gate-{}.sock", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let listener = std::os::unix::net::UnixListener::bind(&path)
                .map_err(|e| format!("Couldn't open Felix's gate: {e}"))?;
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    std::thread::spawn(move || serve_gate(stream));
                }
            });
            Ok(path)
        })
        .cloned()
}

fn serve_gate(stream: std::os::unix::net::UnixStream) {
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_err() {
        return;
    }
    let reply = match serde_json::from_str::<GateRequest>(&line) {
        Ok(req) => match check_call(&req.tool, &req.args, req.label.as_deref()) {
            Ok(()) => GateReply {
                ok: true,
                message: String::new(),
            },
            Err(message) => GateReply { ok: false, message },
        },
        Err(e) => GateReply {
            ok: false,
            message: format!("Bad request: {e}"),
        },
    };
    let mut stream = stream;
    let _ = writeln!(
        stream,
        "{}",
        serde_json::to_string(&reply).unwrap_or_default()
    );
}

// ---- Run log --------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct AgentRunRecord {
    pub at: String,
    pub task: String,
    pub steps: Vec<String>,
    pub status: AgentStatus,
    pub message: String,
    pub seconds: f32,
}

fn log_path() -> Option<std::path::PathBuf> {
    crate::portable::app_data_dir(app_handle()?)
        .ok()
        .map(|d| d.join(RUN_LOG))
}

fn log_run(record: &AgentRunRecord) {
    let Some(path) = log_path() else { return };
    let line = serde_json::to_string(record).unwrap_or_default();
    let result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{line}"));
    if let Err(e) = result {
        warn!("Couldn't write the task log: {e}");
    }
}

/// Recent tasks, newest first.
#[tauri::command]
#[specta::specta]
pub fn recent_agent_runs() -> Vec<AgentRunRecord> {
    let Some(text) = log_path().and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    let mut runs: Vec<AgentRunRecord> = text
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    runs.reverse();
    runs.truncate(20);
    runs
}

#[tauri::command]
#[specta::specta]
pub fn answer_agent_question(answer: AgentAnswer) {
    self::answer(answer);
}

/// Stop the task, or close the card once it's finished.
#[tauri::command]
#[specta::specta]
pub fn close_agent_card() {
    close();
    if !card_shown() {
        if let Some(app) = app_handle() {
            crate::overlay::hide_recording_overlay(app);
        }
    }
}

#[tauri::command]
#[specta::specta]
pub fn stop_agent_task() {
    stop();
}

#[tauri::command]
#[specta::specta]
pub fn remove_agent_app_access(app: AppHandle, key: String) {
    let mut settings = get_settings(&app);
    settings.agent_app_access.retain(|a| a.key() != key);
    write_settings(&app, settings);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn app(name: &str, bundle_id: &str) -> App {
        App {
            name: name.into(),
            bundle_id: bundle_id.into(),
        }
    }

    #[test]
    fn spoken_answers_only_count_when_clear() {
        let confirm = AgentQuestion::Confirm {
            action: "Press Send".into(),
        };
        assert_eq!(spoken_answer(&confirm, "Yes."), Some(AgentAnswer::Confirm));
        assert_eq!(
            spoken_answer(&confirm, "yeah go ahead"),
            Some(AgentAnswer::Confirm)
        );
        assert_eq!(
            spoken_answer(&confirm, "Don't do it."),
            Some(AgentAnswer::Cancel)
        );
        assert_eq!(spoken_answer(&confirm, "Send it to Sam"), None);
        assert_eq!(
            spoken_answer(&confirm, "Go ahead."),
            Some(AgentAnswer::Confirm)
        );
        assert_eq!(
            spoken_answer(&confirm, "Send it."),
            Some(AgentAnswer::Confirm)
        );
        assert_eq!(
            spoken_answer(&confirm, "No, thanks."),
            Some(AgentAnswer::Cancel)
        );
        assert_eq!(
            spoken_answer(&confirm, "Yes I think we should meet on Friday"),
            None
        );
        let access = AgentQuestion::AppAccess {
            app: "Notes".into(),
            can_always: true,
        };
        assert_eq!(
            spoken_answer(&access, "Always."),
            Some(AgentAnswer::AlwaysAllow)
        );
        assert_eq!(spoken_answer(&access, "Yes"), Some(AgentAnswer::AllowOnce));
        assert_eq!(spoken_answer(&access, "Deny"), Some(AgentAnswer::Deny));
        let terminal = AgentQuestion::AppAccess {
            app: "Terminal".into(),
            can_always: false,
        };
        assert_eq!(spoken_answer(&terminal, "Always"), None);
        assert_eq!(
            spoken_answer(&terminal, "Never."),
            Some(AgentAnswer::AlwaysDeny)
        );
        assert_eq!(
            spoken_answer(&access, "Always deny"),
            Some(AgentAnswer::AlwaysDeny)
        );
        assert_eq!(spoken_answer(&access, "No"), Some(AgentAnswer::Deny));
        assert_eq!(
            spoken_answer(&access, "Never mind"),
            Some(AgentAnswer::Deny)
        );
    }

    #[test]
    fn password_managers_are_off_limits_and_terminals_always_ask() {
        let always = vec![
            AgentAppAccess {
                name: "Notes".into(),
                bundle_id: "com.apple.Notes".into(),
                allowed: true,
            },
            AgentAppAccess {
                name: "Mail".into(),
                bundle_id: "com.apple.mail".into(),
                allowed: false,
            },
            AgentAppAccess {
                name: "Terminal".into(),
                bundle_id: "com.apple.Terminal".into(),
                allowed: true,
            },
        ];
        assert!(matches!(
            access(&app("Mail", "com.apple.mail"), &always),
            Access::Denied
        ));
        assert!(matches!(
            access(&app("1Password", "com.1password.1password"), &always),
            Access::Blocked
        ));
        assert!(matches!(
            access(
                &app("System Settings", "com.apple.systempreferences"),
                &always
            ),
            Access::Blocked
        ));
        assert!(matches!(
            access(&app("Terminal", "com.apple.Terminal"), &always),
            Access::Ask { can_always: false }
        ));
        assert!(matches!(
            access(&app("Notes", "com.apple.Notes"), &always),
            Access::Allowed
        ));
        assert!(matches!(
            access(&app("Slack", "com.tinyspeck.slackmacgap"), &always),
            Access::Ask { can_always: true }
        ));
    }

    #[test]
    fn outward_steps_need_a_confirmation() {
        let slack = app("Slack", "com.tinyspeck.slackmacgap");
        let notes = app("Notes", "com.apple.Notes");
        let risky = |tool, args: Value, label: Option<&str>, app: &App| {
            risky_action(tool, &args, label, Some(app))
        };
        assert_eq!(
            risky("click", json!({}), Some("Send"), &slack).as_deref(),
            Some("Press “Send” in Slack")
        );
        assert!(risky("click", json!({}), Some("New Note"), &notes).is_none());
        assert!(risky("press_key", json!({"key": "return"}), None, &slack).is_some());
        assert!(risky("press_key", json!({"key": "return"}), None, &notes).is_none());
        assert!(risky("hotkey", json!({"keys": ["cmd", "return"]}), None, &notes).is_some());
        assert!(risky("hotkey", json!({"keys": ["cmd", "n"]}), None, &notes).is_none());
        assert!(risky(
            "invoke_menu",
            json!({"path": ["File", "Delete Note"]}),
            None,
            &notes
        )
        .is_some());
        assert!(risky("type_text", json!({"text": "on my way\n"}), None, &slack).is_some());
        assert!(risky("type_text", json!({"text": "- socks\n"}), None, &notes).is_none());
    }

    #[test]
    fn steps_read_as_plain_language() {
        let notes = app("Notes", "com.apple.Notes");
        let d = |tool, args: Value, label: Option<&str>| describe(tool, &args, label, Some(&notes));
        assert_eq!(
            d("click", json!({}), Some("New Note")).as_deref(),
            Some("Clicking New Note in Notes")
        );
        assert_eq!(
            d("type_text", json!({"text": "Packing list\n- socks"}), None).as_deref(),
            Some("Typing “Packing list - socks” in Notes")
        );
        assert_eq!(
            d("get_window_state", json!({}), None).as_deref(),
            Some("Looking at Notes")
        );
        assert_eq!(
            d("hotkey", json!({"keys": ["cmd", "n"]}), None).as_deref(),
            Some("Pressing cmd+n in Notes")
        );
        assert_eq!(d("get_session", json!({}), None), None);
    }

    #[test]
    fn only_task_tools_are_allowed() {
        for tool in [
            "set_config",
            "install_extension",
            "clipboard_read",
            "kill_app",
            "get_desktop_state",
            "page",
        ] {
            assert!(!ALLOWED_TOOLS.contains(&tool), "{tool}");
        }
    }
}
