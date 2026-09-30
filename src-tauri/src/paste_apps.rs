//! Whether to trust an app's "nothing is focused". Felix shows a dictation
//! instead of pasting it when Accessibility says nothing that takes text is
//! focused, but an app update can hide its text boxes from Accessibility
//! (or change its bundle id) while typing still works. So "nothing to paste
//! into" is only believed for an app version that has shown Felix a focused
//! text box before, and never for an app the user said to always paste in
//! ("Paste anyway" on the card). Kept in `paste_apps.json`.

use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const FILE: &str = "paste_apps.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Store {
    /// Bundle id -> the version in which a focused text box was seen.
    #[serde(default)]
    shows_text_fields: BTreeMap<String, String>,
    /// Bundle ids to always paste in, whatever Accessibility says.
    #[serde(default)]
    always_paste: BTreeSet<String>,
}

static PATH: Mutex<Option<PathBuf>> = Mutex::new(None);
static STORE: Lazy<Mutex<Store>> = Lazy::new(|| Mutex::new(Store::default()));
/// Bundle id and version by process, so a lookup is cheap.
static APPS: Lazy<Mutex<HashMap<i32, (String, String)>>> = Lazy::new(|| Mutex::new(HashMap::new()));

pub fn init(dir: &Path) {
    let path = dir.join(FILE);
    if let Some(store) = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
    {
        *STORE.lock().unwrap_or_else(|e| e.into_inner()) = store;
    }
    *PATH.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
}

fn save(store: &Store) {
    let Some(path) = PATH.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
        return;
    };
    match serde_json::to_vec_pretty(store) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                log::warn!("Couldn't save {}: {e}", path.display());
            }
        }
        Err(e) => log::warn!("Couldn't encode the paste apps: {e}"),
    }
}

/// Bundle id and version of a running app.
pub fn app_of(pid: i32) -> Option<(String, String)> {
    if let Some(app) = APPS.lock().unwrap_or_else(|e| e.into_inner()).get(&pid) {
        return Some(app.clone());
    }
    let app = lookup(pid)?;
    APPS.lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(pid, app.clone());
    Some(app)
}

#[cfg(target_os = "macos")]
fn lookup(pid: i32) -> Option<(String, String)> {
    use objc2_app_kit::NSRunningApplication;
    use objc2_foundation::{NSBundle, NSString};
    let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
    let bundle_id = app.bundleIdentifier()?.to_string();
    let url = app.bundleURL()?;
    let bundle = NSBundle::bundleWithURL(&url)?;
    let key = |k: &str| {
        bundle
            .objectForInfoDictionaryKey(&NSString::from_str(k))
            .and_then(|v| v.downcast::<NSString>().ok())
            .map(|s| s.to_string())
    };
    let version = format!(
        "{} ({})",
        key("CFBundleShortVersionString").unwrap_or_default(),
        key("CFBundleVersion").unwrap_or_default()
    );
    Some((bundle_id, version))
}

#[cfg(not(target_os = "macos"))]
fn lookup(_pid: i32) -> Option<(String, String)> {
    None
}

/// A focused text box was seen in this app: its "nothing focused" can be
/// believed from now on, until it updates.
pub fn saw_text_field(pid: i32) {
    let Some((bundle_id, version)) = app_of(pid) else {
        return;
    };
    let mut store = STORE.lock().unwrap_or_else(|e| e.into_inner());
    if store.shows_text_fields.get(&bundle_id) == Some(&version) {
        return;
    }
    log::debug!("{bundle_id} {version} shows its text fields");
    store.shows_text_fields.insert(bundle_id, version);
    save(&store);
}

/// Whether "nothing to paste into" can be believed for this app.
pub fn trust_no_text(pid: i32) -> bool {
    let Some((bundle_id, version)) = app_of(pid) else {
        return true;
    };
    let store = STORE.lock().unwrap_or_else(|e| e.into_inner());
    decide(&store, &bundle_id, &version)
}

fn decide(store: &Store, bundle_id: &str, version: &str) -> bool {
    if store.always_paste.contains(bundle_id) {
        return false;
    }
    // Apple's own apps report focus reliably.
    bundle_id.starts_with("com.apple.")
        || store.shows_text_fields.get(bundle_id).map(String::as_str) == Some(version)
}

/// "Paste anyway": always paste in this app from now on.
pub fn always_paste_in(bundle_id: &str) {
    let mut store = STORE.lock().unwrap_or_else(|e| e.into_inner());
    if store.always_paste.insert(bundle_id.to_string()) {
        log::info!("Always pasting in {bundle_id} from now on");
        save(&store);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_version_seen_with_text_fields_is_believed() {
        let mut store = Store::default();
        // Never seen a text field: paste.
        assert!(!decide(&store, "com.openai.codex", "1.0 (1)"));
        store
            .shows_text_fields
            .insert("com.openai.codex".into(), "1.0 (1)".into());
        assert!(decide(&store, "com.openai.codex", "1.0 (1)"));
        // Updated since: paste until it shows a text field again.
        assert!(!decide(&store, "com.openai.codex", "1.1 (2)"));
        // Apple's apps are believed.
        assert!(decide(&store, "com.apple.finder", "15.0 (1)"));
        // "Paste anyway" wins.
        store.always_paste.insert("com.openai.codex".into());
        assert!(!decide(&store, "com.openai.codex", "1.0 (1)"));
    }
}
