//! Apps installed in the standard application folders, for assigning apps
//! to style categories.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub name: String,
    pub path: String,
    /// CFBundleIdentifier, used to assign the app to a style category.
    pub bundle_id: Option<String>,
}

#[cfg(target_os = "macos")]
fn bundle_identifier(path: &str) -> Option<String> {
    use objc2_foundation::{NSBundle, NSString};
    let bundle = NSBundle::bundleWithPath(&NSString::from_str(path))?;
    bundle.bundleIdentifier().map(|id| id.to_string())
}

#[cfg(not(target_os = "macos"))]
fn bundle_identifier(_path: &str) -> Option<String> {
    None
}

fn app_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/Applications/Utilities"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Applications/Utilities"),
    ];
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(Path::new(&home).join("Applications"));
    }
    dirs
}

/// `.app` bundles in the standard application folders, sorted by name. The
/// first folder wins when two bundles share a name.
pub fn installed_apps() -> Vec<InstalledApp> {
    let mut apps: Vec<InstalledApp> = Vec::new();
    for dir in app_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("app") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if apps.iter().any(|a| a.name.eq_ignore_ascii_case(name)) {
                continue;
            }
            let path = path.to_string_lossy().into_owned();
            apps.push(InstalledApp {
                name: name.to_string(),
                bundle_id: bundle_identifier(&path),
                path,
            });
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// Maximum normalized edit distance for a fuzzy name match.
const MAX_FUZZY_DISTANCE: f64 = 0.34;

/// Lowercase words, so "Visual Studio Code.app" and "visual studio code" match.
fn normalize(name: &str) -> String {
    name.trim_end_matches(".app")
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The installed app a name refers to: an exact match (ignoring case and
/// punctuation, a leading "the" and a trailing "app"), otherwise the closest name within a
/// small edit distance or that sounds the same ("cloud" → Claude).
pub fn find<'a>(name: &str, apps: &'a [InstalledApp]) -> Option<&'a InstalledApp> {
    use natural::phonetics::soundex;
    use strsim::levenshtein;

    let mut target = normalize(name);
    if let Some(stripped) = target.strip_prefix("the ") {
        target = stripped.to_string();
    }
    if let Some(stripped) = target.strip_suffix(" app") {
        target = stripped.to_string();
    }
    if target.is_empty() {
        return None;
    }
    if let Some(app) = apps.iter().find(|a| normalize(&a.name) == target) {
        return Some(app);
    }
    let compact_target: String = target.split(' ').collect();
    apps.iter()
        .filter_map(|app| {
            let compact: String = normalize(&app.name).split(' ').collect();
            let longest = compact.chars().count().max(compact_target.chars().count());
            if longest < 3 {
                return None;
            }
            let distance = levenshtein(&compact, &compact_target) as f64 / longest as f64;
            let alphabetic = |s: &str| s.chars().all(|c| c.is_ascii_alphabetic());
            let phonetic = alphabetic(&compact)
                && alphabetic(&compact_target)
                && compact.len().abs_diff(compact_target.len()) <= 2
                && soundex(&compact, &compact_target);
            (distance <= MAX_FUZZY_DISTANCE || phonetic).then_some((distance, app))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, app)| app)
}

/// Launch the app at `path`, or bring it to the front if it's running.
pub fn activate(path: &str) -> Result<(), String> {
    let status = std::process::Command::new("/usr/bin/open")
        .arg(path)
        .status()
        .map_err(|e| format!("Couldn't run open: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("open {path} exited with {status}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apps() -> Vec<InstalledApp> {
        [
            "Claude",
            "Codex",
            "Slack",
            "Visual Studio Code",
            "Ghostty",
            "Notes",
        ]
        .iter()
        .map(|name| InstalledApp {
            name: name.to_string(),
            path: format!("/Applications/{name}.app"),
            bundle_id: None,
        })
        .collect()
    }

    fn found(name: &str) -> Option<String> {
        find(name, &apps()).map(|a| a.name.clone())
    }

    #[test]
    fn finds_apps_by_name_and_near_misses() {
        assert_eq!(found("Codex").as_deref(), Some("Codex"));
        assert_eq!(found("the Slack app").as_deref(), Some("Slack"));
        assert_eq!(found("Slack app").as_deref(), Some("Slack"));
        assert_eq!(
            found("visual studio code").as_deref(),
            Some("Visual Studio Code")
        );
        assert_eq!(found("cloud").as_deref(), Some("Claude"));
        assert_eq!(found("ghosty").as_deref(), Some("Ghostty"));
    }

    #[test]
    fn unrelated_names_find_nothing() {
        assert_eq!(found("the pod bay doors"), None);
        assert_eq!(found("bed"), None);
        assert_eq!(found(""), None);
    }
}
