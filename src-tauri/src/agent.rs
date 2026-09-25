//! Felix's computer tasks ("Felix, open Notes and make a shopping list"):
//! Codex CLI is the agent and Cua Driver its hands.
//!
//! Cua Driver runs as Handy's own child in its "embedded" mode, so macOS
//! checks Handy's Accessibility and Screen Recording grants rather than asking
//! for new ones. Handy starts the daemon on a private socket and gives Codex
//! the driver's MCP proxy as its only tool server, behind Felix's gate
//! (`cua_gate`), so every call is checked, confirmed when it needs to be and
//! shown on the card (`agent_run`). Codex drives apps in the background
//! through the accessibility tree (element indices, not screenshots), and its
//! own shell stays sandboxed.

use log::{debug, info, warn};
use once_cell::sync::Lazy;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A task that runs longer than this is stopped.
const TASK_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const HOST_BUNDLE_ID: &str = "com.pais.handy";

struct Daemon {
    child: Child,
    socket: PathBuf,
}

static DAEMON: Lazy<Mutex<Option<Daemon>>> = Lazy::new(|| Mutex::new(None));

/// Environment for the driver: embedded in Handy, telemetry off.
pub(crate) fn driver_env() -> [(&'static str, &'static str); 4] {
    [
        ("CUA_DRIVER_EMBEDDED", "1"),
        ("CUA_DRIVER_HOST_BUNDLE_ID", HOST_BUNDLE_ID),
        ("CUA_TELEMETRY_ENABLED", "false"),
        ("CUA_DRIVER_RS_TELEMETRY_ENABLED", "0"),
    ]
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The driver bundled next to Handy's executable, or a local build.
fn driver_path() -> Option<PathBuf> {
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("cua-driver")));
    let candidates = [
        bundled,
        std::env::var_os("CUA_DRIVER_PATH").map(PathBuf::from),
        home().map(|h| h.join(".cache/cua-driver-target/release/cua-driver")),
    ];
    candidates.into_iter().flatten().find(|p| p.is_file())
}

/// Codex CLI. Apps don't get the shell's PATH, so look where installers put it.
fn codex_path() -> Option<PathBuf> {
    let mut candidates = vec![
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
    ];
    if let Some(home) = home() {
        candidates.push(home.join(".local/bin/codex"));
        candidates.push(home.join(".bun/bin/codex"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("codex")));
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Start the driver daemon if it isn't running; its socket.
fn ensure_daemon(driver: &Path) -> Result<PathBuf, String> {
    let mut daemon = DAEMON.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(d) = daemon.as_mut() {
        if matches!(d.child.try_wait(), Ok(None)) && d.socket.exists() {
            return Ok(d.socket.clone());
        }
        let _ = d.child.kill();
    }
    let socket = std::env::temp_dir().join(format!("handy-cua-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    // Spawned directly (not through `open`), so it stays in Handy's
    // responsibility chain and uses Handy's permissions.
    let child = Command::new(driver)
        .args(["serve", "--embedded", "--socket"])
        .arg(&socket)
        .envs(driver_env())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Couldn't start Cua Driver: {e}"))?;
    let started = Instant::now();
    while !socket.exists() {
        if started.elapsed() > Duration::from_secs(10) {
            return Err("Cua Driver didn't start".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    info!("Cua Driver started in {:?}", started.elapsed());
    *daemon = Some(Daemon {
        child,
        socket: socket.clone(),
    });
    Ok(socket)
}

/// Stop the driver daemon (on quit).
pub fn stop() {
    let mut daemon = DAEMON.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(mut d) = daemon.take() {
        let _ = d.child.kill();
        let _ = d.child.wait();
        let _ = std::fs::remove_file(&d.socket);
    }
}

fn instructions(task: &str) -> String {
    format!(
        r#"You are Felix, a voice assistant acting on the user's Mac. They asked:
<<<
{task}
>>>

Do it with the `cua` tools, which work apps in the background through accessibility:
- Open apps with launch_app (bundle_id preferred). To open a link or file in an app, pass it in `urls`.
- Read a window with get_window_state, then act on elements by element_index (click, type_text, set_value, press_key, hotkey, invoke_menu). Prefer element indices over coordinates.
- Your shell is sandboxed: it can read files and write in the working folder, but it can't open apps. Use the cua tools for anything on screen.
Take the most direct path; don't explore beyond what the task needs.

Don't do anything that can't be undone or that reaches other people (sending a message or email, posting, deleting, buying or paying, changing account or security settings) unless they asked for exactly that. Felix asks the user to confirm those steps and to allow each app the first time; if a tool call comes back refused, don't try another way around it: stop and say what's left for them.

End with one or two short sentences for the user: what you did, or what's left for them. No Markdown."#
    )
}

/// Arguments for `codex exec`: Cua's MCP proxy, behind Felix's gate, as the
/// only tool server; the user's own Codex config ignored (its MCP servers and
/// settings don't apply).
fn codex_args(
    felix: &Path,
    gate: &Path,
    driver: &Path,
    socket: &Path,
    out: &Path,
    task: &str,
) -> Vec<String> {
    let toml_str = |s: &str| format!("{s:?}");
    let env = driver_env()
        .iter()
        .map(|(k, v)| format!("{k}={}", toml_str(v)))
        .collect::<Vec<_>>()
        .join(",");
    vec![
        "exec".into(),
        "--skip-git-repo-check".into(),
        "--ephemeral".into(),
        "--ignore-user-config".into(),
        "--sandbox".into(),
        "workspace-write".into(),
        "-c".into(),
        "model_reasoning_effort=\"low\"".into(),
        "-c".into(),
        format!(
            "mcp_servers.cua.command={}",
            toml_str(&felix.to_string_lossy())
        ),
        "-c".into(),
        format!(
            "mcp_servers.cua.args=[\"--cua-gate\",{},{},{}]",
            toml_str(&gate.to_string_lossy()),
            toml_str(&driver.to_string_lossy()),
            toml_str(&socket.to_string_lossy())
        ),
        "-c".into(),
        format!("mcp_servers.cua.env={{{env}}}"),
        "-o".into(),
        out.to_string_lossy().into_owned(),
        instructions(task),
    ]
}

/// Carry out a computer task; a closing message for the user. With `fast`
/// and a known app, Simple Jev drives first (see `agent_decide`); Codex
/// takes over whatever it can't finish. Blocking; run it off the main thread.
pub fn run_task(task: &str, app: &str, content: &str, fast: bool) -> Result<String, String> {
    let driver = driver_path().ok_or("Cua Driver isn't installed")?;
    let socket = ensure_daemon(&driver)?;
    let mut task = task.to_string();
    if fast && !app.trim().is_empty() {
        let started = Instant::now();
        let outcome = crate::agent_decide::run(&driver, &socket, &task, app.trim(), content.trim());
        info!("Fast path finished in {:?}: {outcome:?}", started.elapsed());
        match outcome {
            crate::agent_decide::Outcome::Done(reply)
            | crate::agent_decide::Outcome::Handoff(reply) => return Ok(reply),
            _ if crate::agent_run::stopped() => return Err("Stopped".into()),
            crate::agent_decide::Outcome::GaveUp { done, .. } if !done.is_empty() => {
                task = format!(
                    "{task}\n\nAlready done in {app}, don't repeat it: {}. Carry on from there.",
                    done.join("; ")
                );
            }
            crate::agent_decide::Outcome::GaveUp { .. } => {}
        }
    }
    let task = task.as_str();
    let codex = codex_path().ok_or("Codex CLI isn't installed (brew install --cask codex)")?;
    let workdir = crate::agent_skills::projects_dir()
        .filter(|d| d.is_dir())
        .or_else(home)
        .ok_or("No home folder")?;
    let out = std::env::temp_dir().join(format!("handy-felix-{}.txt", uuid::Uuid::new_v4()));
    let log = std::env::temp_dir().join("handy-felix-task.log");
    let log_file = std::fs::File::create(&log).map_err(|e| e.to_string())?;

    let felix = std::env::current_exe().map_err(|e| e.to_string())?;
    let gate = crate::agent_run::gate_socket()?;
    let started = Instant::now();
    let mut child = Command::new(&codex)
        .args(codex_args(&felix, &gate, &driver, &socket, &out, task))
        .current_dir(&workdir)
        .stdin(Stdio::null())
        .stdout(log_file.try_clone().map_err(|e| e.to_string())?)
        .stderr(log_file)
        .spawn()
        .map_err(|e| format!("Couldn't start Codex: {e}"))?;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if crate::agent_run::stopped() => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Stopped".into());
            }
            Ok(None) if started.elapsed() > TASK_TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Stopped after 10 minutes".into());
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => return Err(e.to_string()),
        }
    };
    info!("Codex task finished in {:?} ({status})", started.elapsed());
    let reply = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_file(&out);
    let reply = reply.trim().to_string();
    if !status.success() || reply.is_empty() {
        warn!("Codex task failed; see {}", log.display());
        return Err(if reply.is_empty() {
            "Codex stopped without an answer".into()
        } else {
            reply
        });
    }
    debug!("Codex: {reply}");
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_gets_cua_through_felixs_gate_as_its_only_tool_server() {
        let args = codex_args(
            Path::new("/Apps/Handy.app/Contents/MacOS/handy"),
            Path::new("/tmp/handy-gate-1.sock"),
            Path::new("/Apps/Handy.app/Contents/MacOS/cua-driver"),
            Path::new("/tmp/handy-cua-1.sock"),
            Path::new("/tmp/out.txt"),
            "Open Notes",
        );
        let joined = args.join(" ");
        assert!(joined.contains("--ignore-user-config"));
        assert!(joined.contains("--sandbox workspace-write"));
        assert!(
            joined.contains(r#"mcp_servers.cua.command="/Apps/Handy.app/Contents/MacOS/handy""#)
        );
        assert!(joined.contains(
            r#"mcp_servers.cua.args=["--cua-gate","/tmp/handy-gate-1.sock","/Apps/Handy.app/Contents/MacOS/cua-driver","/tmp/handy-cua-1.sock"]"#
        ));
        assert!(joined.contains(r#"CUA_DRIVER_EMBEDDED="1""#));
        assert!(joined.contains(r#"CUA_TELEMETRY_ENABLED="false""#));
        assert!(args.last().unwrap().contains("Open Notes"));
    }
}
