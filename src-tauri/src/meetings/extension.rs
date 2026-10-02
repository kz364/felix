//! The Felix Meetings extension for Chrome: the call page itself says who's
//! talking (Meet's tiles light up, its participant list, and captions when
//! the user turns them on in the extension). That's sharper than reading
//! the accessibility tree, which stays as the fallback.
//!
//! Chrome only talks to a "native messaging host" it starts itself, so the
//! path is: content script → the extension's background worker → Felix's
//! own binary started by Chrome as the host (`handy chrome-extension://…`,
//! see [`run_host`]) → an owner-only Unix socket in the app data folder →
//! [`spawn_listener`] in the running app, which logs what it hears into the
//! meeting being recorded (`extension.jsonl`, at the recording's clock).
//! Nothing leaves the Mac. The host manifest names only this extension.

use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::{AppHandle, Manager};

pub const FILE: &str = "extension.jsonl";
pub const HOST_NAME: &str = "com.felix.meetings";
/// Pinned by the `key` in the extension's manifest.json.
pub const EXTENSION_ID: &str = "hjplcceplekocehjfmhjfcfnidlknjfj";
const SOCKET: &str = "extension.sock";
/// Chrome caps messages to the host at 4 GB; anything near this is junk.
const MAX_MESSAGE: usize = 1 << 20;

/// What the extension sends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Message {
    /// The extension started on a call page (or Felix asked).
    Hello { app: String, version: String },
    /// These people's tiles show them talking now (one or two names).
    Speaking { app: String, names: Vec<String> },
    /// Everyone on the call, when it changes.
    Participants { app: String, names: Vec<String> },
    /// A finished caption line (only when captions are on).
    Caption {
        app: String,
        name: String,
        text: String,
    },
}

/// A message as kept with the meeting: when it came, on the recording's clock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Logged {
    pub at_ms: u64,
    #[serde(flatten)]
    pub message: Message,
}

pub fn load(dir: &Path) -> Vec<Logged> {
    std::fs::read_to_string(dir.join(FILE))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .filter_map(|mut l: Logged| {
            match &mut l.message {
                Message::Speaking { names, .. } | Message::Participants { names, .. } => {
                    names.retain(|n| is_name(n));
                    if names.is_empty() {
                        return None;
                    }
                }
                Message::Caption { name, .. } if !is_name(name) => return None,
                _ => {}
            }
            Some(l)
        })
        .collect()
}

/// Whether the page gave a person's name, not an icon's: Meet draws its
/// icons from words ("frame_person", "keep_outline"), which an older
/// extension read as names, so every voice it heard looked like one person.
pub fn is_name(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && !n
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/com.pais.handy"))
}

fn socket_path() -> Option<PathBuf> {
    Some(data_dir()?.join(SOCKET))
}

// ---- The host: Chrome starts Felix's binary with the extension's origin ----

/// Whether these are the arguments Chrome starts a native host with.
pub fn is_host_launch(args: &[String]) -> bool {
    args.get(1)
        .is_some_and(|a| a.starts_with(&format!("chrome-extension://{EXTENSION_ID}/")))
}

fn read_native(input: &mut impl Read) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    input.read_exact(&mut len).ok()?;
    let len = u32::from_ne_bytes(len) as usize;
    if len > MAX_MESSAGE {
        return None;
    }
    let mut buf = vec![0u8; len];
    input.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn write_native(out: &mut impl Write, value: &serde_json::Value) {
    let bytes = value.to_string().into_bytes();
    let _ = out.write_all(&(bytes.len() as u32).to_ne_bytes());
    let _ = out.write_all(&bytes);
    let _ = out.flush();
}

/// Relay the extension's messages to the running app until Chrome closes
/// the pipe. Answers each hello with whether Felix is listening, which the
/// extension shows. Returns the exit code.
pub fn run_host() -> i32 {
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut felix: Option<std::os::unix::net::UnixStream> = None;
    while let Some(buf) = read_native(&mut stdin) {
        let Ok(message) = serde_json::from_slice::<Message>(&buf) else {
            continue;
        };
        if felix.is_none() {
            felix = socket_path().and_then(|p| std::os::unix::net::UnixStream::connect(p).ok());
        }
        let mut sent = false;
        if let Some(s) = felix.as_mut() {
            if let Ok(line) = serde_json::to_string(&message) {
                sent = writeln!(s, "{line}").is_ok();
            }
            if !sent {
                felix = None;
            }
        }
        if matches!(message, Message::Hello { .. }) {
            write_native(&mut stdout, &serde_json::json!({ "felix": sent }));
        }
    }
    0
}

// ---- The app: listen on the socket, log into the meeting being recorded ----

#[derive(Default)]
struct Heard {
    at: Option<Instant>,
    app: Option<String>,
}

static HEARD: Mutex<Heard> = Mutex::new(Heard {
    at: None,
    app: None,
});

fn app_of(m: &Message) -> &str {
    match m {
        Message::Hello { app, .. }
        | Message::Speaking { app, .. }
        | Message::Participants { app, .. }
        | Message::Caption { app, .. } => app,
    }
}

/// Start listening for the host. Only this user can open the socket.
pub fn spawn_listener(app: &AppHandle) {
    let Some(path) = socket_path() else {
        return;
    };
    let app = app.clone();
    std::thread::spawn(move || {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::remove_file(&path);
        let listener = match std::os::unix::net::UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                log::warn!("Couldn't listen for the meetings extension: {e}");
                return;
            }
        };
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        for stream in listener.incoming().flatten() {
            let app = app.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stream).lines() {
                    let Ok(line) = line else { break };
                    if line.len() > MAX_MESSAGE {
                        continue;
                    }
                    if let Ok(message) = serde_json::from_str::<Message>(&line) {
                        heard(&app, message);
                    }
                }
            });
        }
    });
}

fn heard(app: &AppHandle, message: Message) {
    {
        let mut h = HEARD.lock().unwrap_or_else(|e| e.into_inner());
        h.at = Some(Instant::now());
        h.app = Some(app_of(&message).to_string());
    }
    if matches!(message, Message::Hello { .. }) {
        return;
    }
    let Some((dir, at_ms)) = app
        .try_state::<Arc<super::manager::MeetingManager>>()
        .and_then(|m| m.recording_at())
    else {
        return;
    };
    let Ok(line) = serde_json::to_string(&Logged { at_ms, message }) else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(FILE))
    {
        let _ = writeln!(f, "{line}");
    }
}

// ---- Settings: status and setup ----

/// Browsers on this Mac that take Chrome extensions, by the folder their
/// native host manifests go in.
fn browser_host_dirs() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return vec![];
    };
    let support = home.join("Library/Application Support");
    [
        "Google/Chrome",
        "Google/Chrome Beta",
        "Google/Chrome Canary",
        "Chromium",
        "BraveSoftware/Brave-Browser",
        "Microsoft Edge",
        "Arc/User Data",
    ]
    .iter()
    .map(|b| support.join(b))
    .filter(|d| d.is_dir())
    .map(|d| d.join("NativeMessagingHosts"))
    .collect()
}

fn manifest_for(exe: &Path) -> serde_json::Value {
    serde_json::json!({
        "name": HOST_NAME,
        "description": "Felix meeting speaker names",
        "path": exe,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{EXTENSION_ID}/")],
    })
}

#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct ExtensionStatus {
    /// The host manifest is in at least one browser and points at this app.
    pub host_installed: bool,
    /// Seconds since the extension last said anything, this run.
    pub heard_secs_ago: Option<u64>,
    /// The call page it last spoke from ("meet", "zoom", "teams").
    pub app: Option<String>,
    /// The folder to load unpacked in chrome://extensions.
    pub extension_dir: String,
}

fn extension_dir(app: &AppHandle) -> PathBuf {
    app.path()
        .resolve(
            "resources/meet-extension",
            tauri::path::BaseDirectory::Resource,
        )
        .unwrap_or_default()
}

#[tauri::command]
#[specta::specta]
pub fn extension_status(app: AppHandle) -> ExtensionStatus {
    let exe = std::env::current_exe().unwrap_or_default();
    let host_installed = browser_host_dirs().iter().any(|d| {
        std::fs::read_to_string(d.join(format!("{HOST_NAME}.json")))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .is_some_and(|v| v["path"].as_str() == exe.to_str())
    });
    let h = HEARD.lock().unwrap_or_else(|e| e.into_inner());
    ExtensionStatus {
        host_installed,
        heard_secs_ago: h.at.map(|t| t.elapsed().as_secs()),
        app: h.app.clone(),
        extension_dir: extension_dir(&app).to_string_lossy().to_string(),
    }
}

/// Put the host manifest in each Chromium browser's folder, so the
/// extension can reach Felix.
#[tauri::command]
#[specta::specta]
pub fn install_extension_host() -> Result<u32, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let manifest = serde_json::to_string_pretty(&manifest_for(&exe)).map_err(|e| e.to_string())?;
    let dirs = browser_host_dirs();
    if dirs.is_empty() {
        return Err("No Chrome-based browser found.".into());
    }
    let mut n = 0;
    for d in dirs {
        std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
        std::fs::write(d.join(format!("{HOST_NAME}.json")), &manifest)
            .map_err(|e| format!("Couldn't set up the extension helper: {e}"))?;
        n += 1;
    }
    Ok(n)
}

/// Show the extension's folder in Finder, for "Load unpacked".
#[tauri::command]
#[specta::specta]
pub fn reveal_extension_folder(app: AppHandle) -> Result<(), String> {
    let dir = extension_dir(&app);
    std::process::Command::new("open")
        .arg(&dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_and_logged_lines_round_trip() {
        let m: Message =
            serde_json::from_str(r#"{"type":"speaking","app":"meet","names":["Sam Rivera"]}"#)
                .unwrap();
        assert_eq!(
            m,
            Message::Speaking {
                app: "meet".into(),
                names: vec!["Sam Rivera".into()]
            }
        );
        let line = serde_json::to_string(&Logged {
            at_ms: 1200,
            message: m.clone(),
        })
        .unwrap();
        assert!(line.contains("\"at_ms\":1200") && line.contains("\"type\":\"speaking\""));
        assert_eq!(serde_json::from_str::<Logged>(&line).unwrap().message, m);
    }

    #[test]
    fn native_messages_are_length_prefixed() {
        let mut buf = Vec::new();
        write_native(&mut buf, &serde_json::json!({"felix": true}));
        let back = read_native(&mut buf.as_slice()).unwrap();
        assert_eq!(back, br#"{"felix":true}"#);
    }

    #[test]
    fn only_this_extension_starts_the_host() {
        let args = |a: &str| vec!["handy".to_string(), a.to_string()];
        assert!(is_host_launch(&args(&format!(
            "chrome-extension://{EXTENSION_ID}/"
        ))));
        assert!(!is_host_launch(&args("chrome-extension://abc/")));
        assert!(!is_host_launch(&args("--start-hidden")));
        let m = manifest_for(Path::new("/Applications/Felix.app/Contents/MacOS/handy"));
        assert_eq!(
            m["allowed_origins"][0],
            format!("chrome-extension://{EXTENSION_ID}/")
        );
    }
}
