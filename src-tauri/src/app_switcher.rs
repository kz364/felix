//! Voice Control app switcher: a dictation that is *only* "go to <app>",
//! "switch to <app>" or "open <app>" brings that app to the front instead of
//! being typed.
//!
//! Targets resolve from the user's spoken aliases first, then (optionally) any
//! installed app by name. A small fuzzy step (edit distance / Soundex) absorbs
//! ASR near-misses like "cloud" for "Claude"; anything that doesn't resolve is
//! pasted as normal text, so ordinary sentences starting with "open" are safe.

use crate::settings::{AppAlias, AppSettings};
use natural::phonetics::soundex;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::Serialize;
use specta::Type;
use std::path::{Path, PathBuf};
use strsim::levenshtein;

#[derive(Serialize, Debug, Clone, PartialEq, Eq, Type)]
pub struct InstalledApp {
    pub name: String,
    pub path: String,
    /// CFBundleIdentifier, used to assign the app to a style category.
    pub bundle_id: Option<String>,
}

/// Leading words that make a dictation an app-switch command.
const COMMAND_PREFIXES: &[&[&str]] = &[&["go", "to"], &["switch", "to"], &["open"]];
/// Longest app name (in words) we try to resolve; longer utterances are prose.
const MAX_TARGET_WORDS: usize = 4;
/// Maximum normalized edit distance for a fuzzy match.
const MAX_FUZZY_DISTANCE: f64 = 0.34;

static WORD_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[\p{L}\p{N}]+(?:'[\p{L}\p{N}]+)*").unwrap());

fn words(text: &str) -> Vec<String> {
    WORD_RE
        .find_iter(text)
        .map(|m| m.as_str().to_lowercase())
        .collect()
}

fn normalize(name: &str) -> String {
    words(name).join(" ")
}

/// If the whole dictation is an app-switch command, return the spoken target
/// ("go to the Slack app" → "slack").
pub fn parse_command(text: &str) -> Option<String> {
    let words = words(text);
    let prefix = COMMAND_PREFIXES
        .iter()
        .find(|prefix| words.len() > prefix.len() && words[..prefix.len()] == prefix[..])?;
    let mut target: &[String] = &words[prefix.len()..];
    if target.first().is_some_and(|w| w == "the") {
        target = &target[1..];
    }
    if target.len() > 1 && target.last().is_some_and(|w| w == "app") {
        target = &target[..target.len() - 1];
    }
    (!target.is_empty() && target.len() <= MAX_TARGET_WORDS).then(|| target.join(" "))
}

/// Candidate spoken names and the app each one opens.
fn candidates<'a>(
    aliases: &'a [AppAlias],
    apps: &'a [InstalledApp],
    any_installed: bool,
) -> Vec<(String, &'a str)> {
    let mut out: Vec<(String, &str)> = aliases
        .iter()
        .map(|a| (normalize(&a.phrase), a.app_path.as_str()))
        .collect();
    if any_installed {
        out.extend(apps.iter().map(|a| (normalize(&a.name), a.path.as_str())));
    }
    out.retain(|(name, _)| !name.is_empty());
    out
}

/// Resolve a spoken target to an app path. Exact matches win (aliases before
/// installed apps); otherwise the closest fuzzy match within tolerance.
pub fn resolve(
    target: &str,
    aliases: &[AppAlias],
    apps: &[InstalledApp],
    any_installed: bool,
) -> Option<String> {
    let target = normalize(target);
    let candidates = candidates(aliases, apps, any_installed);

    if let Some((_, path)) = candidates.iter().find(|(name, _)| *name == target) {
        return Some(path.to_string());
    }

    let compact_target: String = target.split(' ').collect();
    candidates
        .iter()
        .filter_map(|(name, path)| {
            let compact: String = name.split(' ').collect();
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
            (distance <= MAX_FUZZY_DISTANCE || phonetic).then_some((distance, *path))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, path)| path.to_string())
}

/// App to switch to for this dictation, if it is an app-switch command that
/// resolves. Requires Voice Control and the app switcher to be enabled.
pub fn detect(text: &str, settings: &AppSettings) -> Option<String> {
    if !settings.voice_control_enabled || !settings.app_switch_enabled {
        return None;
    }
    let target = parse_command(text)?;
    let apps = if settings.app_switch_any_installed {
        installed_apps()
    } else {
        Vec::new()
    };
    let resolved = resolve(
        &target,
        &settings.app_aliases,
        &apps,
        settings.app_switch_any_installed,
    );
    log::debug!("App switch command '{target}' resolved to {resolved:?}");
    resolved
}

/// Launch or bring the app at `path` to the front.
pub fn activate(path: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("/usr/bin/open")
            .arg(path)
            .status()
            .map_err(|e| format!("Failed to run open: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("open {path} exited with {status}"))
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(format!(
            "App switching is not supported on this platform ({path})"
        ))
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn apps() -> Vec<InstalledApp> {
        [
            "Claude",
            "Slack",
            "Safari",
            "Visual Studio Code",
            "Notes",
            "Ghostty",
        ]
        .iter()
        .map(|name| InstalledApp {
            name: name.to_string(),
            path: format!("/Applications/{name}.app"),
            bundle_id: None,
        })
        .collect()
    }

    #[test]
    fn parses_command_forms() {
        assert_eq!(parse_command("Go to Claude."), Some("claude".into()));
        assert_eq!(parse_command("switch to slack"), Some("slack".into()));
        assert_eq!(
            parse_command("Open the Visual Studio Code app"),
            Some("visual studio code".into())
        );
        assert_eq!(parse_command("go to"), None);
        assert_eq!(
            parse_command("open the file and check the second paragraph again please"),
            None
        );
        assert_eq!(parse_command("I want to go to Claude"), None);
    }

    #[test]
    fn resolves_installed_apps_exactly() {
        assert_eq!(
            resolve("claude", &[], &apps(), true),
            Some("/Applications/Claude.app".into())
        );
        assert_eq!(
            resolve("visual studio code", &[], &apps(), true),
            Some("/Applications/Visual Studio Code.app".into())
        );
    }

    #[test]
    fn aliases_win_and_work_without_installed_matching() {
        let aliases = vec![AppAlias {
            phrase: "code".into(),
            app_path: "/Applications/Visual Studio Code.app".into(),
        }];
        assert_eq!(
            resolve("code", &aliases, &apps(), false),
            Some("/Applications/Visual Studio Code.app".into())
        );
        assert_eq!(resolve("claude", &aliases, &apps(), false), None);
    }

    #[test]
    fn fuzzy_matches_asr_near_misses() {
        assert_eq!(
            resolve("cloud", &[], &apps(), true),
            Some("/Applications/Claude.app".into())
        );
        assert_eq!(
            resolve("ghosty", &[], &apps(), true),
            Some("/Applications/Ghostty.app".into())
        );
    }

    #[test]
    fn unrelated_targets_do_not_resolve() {
        assert_eq!(resolve("the pod bay doors", &[], &apps(), true), None);
        assert_eq!(resolve("bed", &[], &apps(), true), None);
    }

    #[test]
    fn detect_respects_toggles() {
        let mut settings = crate::settings::get_default_settings();
        settings.app_switch_any_installed = false;
        settings.app_aliases = vec![AppAlias {
            phrase: "chat".into(),
            app_path: "/Applications/Claude.app".into(),
        }];
        assert_eq!(
            detect("go to chat", &settings),
            Some("/Applications/Claude.app".into())
        );
        settings.app_switch_enabled = false;
        assert_eq!(detect("go to chat", &settings), None);
        settings.app_switch_enabled = true;
        settings.voice_control_enabled = false;
        assert_eq!(detect("go to chat", &settings), None);
    }
}
