//! Things Felix can do on the computer without a screenshot loop: fixed steps
//! through URL schemes and the accessibility API, well under a second each.
//!
//! Starting a Claude Code session opens the Claude desktop app's
//! `claude://code/new` link (folder + prompt), then presses "Trust" on the
//! workspace dialog and sends the prompt through accessibility, and checks
//! the session shows the project folder (the app sometimes opens a scratch
//! workspace instead).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const CLAUDE_BUNDLE: &str = "com.anthropic.claudefordesktop";

/// Where new projects go.
pub fn projects_dir() -> Option<PathBuf> {
    dirs_home().map(|home| home.join("Documents").join("Agent Work"))
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// A folder name from a spoken project name: lowercase words joined by "-".
pub fn folder_name(project: &str) -> String {
    let name = project
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join("-");
    if name.is_empty() {
        "new-project".into()
    } else {
        name
    }
}

/// The project's folder: an existing one with that name, or a new one.
pub fn project_folder(base: &Path, project: &str) -> Result<(PathBuf, bool), String> {
    let path = base.join(folder_name(project));
    if path.is_dir() {
        return Ok((path, false));
    }
    std::fs::create_dir_all(&path)
        .map_err(|e| format!("Couldn't create {}: {e}", path.display()))?;
    Ok((path, true))
}

/// The deep link that opens a new Claude Code session with the prompt filled in.
pub fn claude_link(folder: &Path, prompt: &str) -> String {
    let mut url = url::Url::parse("claude://code/new").expect("valid base URL");
    url.query_pairs_mut()
        .append_pair("folder", &folder.to_string_lossy())
        .append_pair("q", prompt);
    url.to_string()
}

/// What happened, for the card shown when it's done.
#[derive(Debug, Clone)]
pub struct SessionOutcome {
    pub folder: PathBuf,
    pub created: bool,
    pub sent: bool,
    /// The session showed the folder name.
    pub verified: bool,
}

/// Retry `step` until it succeeds or `timeout` passes.
fn retry<T>(timeout: Duration, mut step: impl FnMut() -> Option<T>) -> Option<T> {
    let started = Instant::now();
    loop {
        if let Some(v) = step() {
            return Some(v);
        }
        if started.elapsed() > timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// Labels the controls might carry, in the order tried.
const TRUST_LABELS: &[&str] = &["Trust workspace", "Trust folder", "Trust"];
const SEND_LABELS: &[&str] = &["Send message", "Send"];

/// Press the first button found with one of the labels; the label pressed.
fn press_any(pid: i32, labels: &[&'static str]) -> Option<&'static str> {
    labels
        .iter()
        .copied()
        .find(|label| crate::ax_tree::press(pid, "AXButton", label).is_ok())
}

/// Open a Claude Code session in the project's folder and send the prompt.
/// Blocking; run it off the main thread.
pub fn start_claude_session(
    project: &str,
    prompt: &str,
    send: bool,
) -> Result<SessionOutcome, String> {
    let base = projects_dir().ok_or("No home folder")?;
    let (folder, created) = project_folder(&base, project)?;
    let link = claude_link(&folder, prompt);
    let status = std::process::Command::new("open")
        .arg(&link)
        .status()
        .map_err(|e| format!("Couldn't open the Claude app: {e}"))?;
    if !status.success() {
        return Err("The Claude app didn't open the link".into());
    }
    let mut outcome = SessionOutcome {
        folder: folder.clone(),
        created,
        sent: false,
        verified: false,
    };
    if !crate::ax_tree::is_trusted() {
        log::info!("No accessibility access; left the Claude session for you to send");
        return Ok(outcome);
    }
    let pid = retry(Duration::from_secs(10), || {
        crate::ax_tree::pid_of(CLAUDE_BUNDLE)
    })
    .ok_or("The Claude app didn't start")?;
    crate::ax_tree::expose_electron_tree(pid);

    // The trust dialog shows for every new session; give it a few seconds.
    match retry(Duration::from_secs(6), || press_any(pid, TRUST_LABELS)) {
        Some(label) => log::debug!("Pressed \"{label}\""),
        None => log::info!("No trust dialog found in the Claude app"),
    }
    if send {
        outcome.sent = retry(Duration::from_secs(4), || press_any(pid, SEND_LABELS)).is_some();
    }
    let folder_label = folder
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    outcome.verified = retry(Duration::from_secs(3), || {
        crate::ax_tree::dump(pid, crate::ax_tree::DEFAULT_DEPTH)
            .iter()
            .any(|n| n.label.to_lowercase().contains(&folder_label))
            .then_some(())
    })
    .is_some();
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_names_come_from_spoken_names() {
        assert_eq!(folder_name("Recipe Scraper"), "recipe-scraper");
        assert_eq!(folder_name("  my new app!! "), "my-new-app");
        assert_eq!(folder_name("???"), "new-project");
    }

    #[test]
    fn link_carries_folder_and_prompt() {
        let link = claude_link(Path::new("/Users/me/Agent Work/x"), "Build a CLI & tests");
        let url = url::Url::parse(&link).unwrap();
        let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        assert_eq!(
            pairs,
            vec![
                ("folder".into(), "/Users/me/Agent Work/x".into()),
                ("q".into(), "Build a CLI & tests".into()),
            ]
        );
        assert!(link.starts_with("claude://code/new?"));
    }

    #[test]
    fn existing_projects_are_reused() {
        let base = std::env::temp_dir().join(format!("handy-skills-{}", std::process::id()));
        let (first, created) = project_folder(&base, "Demo App").unwrap();
        assert!(created);
        let (second, created) = project_folder(&base, "demo app").unwrap();
        assert!(!created);
        assert_eq!(first, second);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
