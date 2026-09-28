//! Dictating to a coding agent (Claude Code, Codex) in a terminal: file
//! names you say become the agent's file mentions, so "look at actions dot
//! rs" pastes as "look at @src/actions.rs".
//!
//! When recording starts in a terminal, the agent is looked up among the
//! terminal's processes, and the files of its project (from git) are read
//! in the background. At paste time only names that match exactly one file
//! are tagged.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Terminals an agent may run in.
const TERMINALS: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "com.mitchellh.ghostty",
    "dev.warp.Warp-Stable",
    "net.kovidgoyal.kitty",
    "io.alacritty",
    "org.alacritty",
    "com.github.wez.wezterm",
    "co.zeit.hyper",
];
/// Program names of the agents.
const AGENTS: &[&str] = &["claude", "codex"];
/// Packages the agents run from when installed with npm.
const AGENT_PACKAGES: &[&str] = &["@anthropic-ai/claude-code", "@openai/codex"];
/// Projects bigger than this aren't worth scanning for names.
const MAX_FILES: usize = 30_000;
/// How long a project's file list is reused.
const FILES_FRESH_FOR: Duration = Duration::from_secs(60);
/// Most words a spoken file name ("edit learning dot rs") spans.
const MAX_NAME_WORDS: usize = 3;

/// The project of the agent in front, once looked up.
static CURRENT: Lazy<Mutex<Option<Project>>> = Lazy::new(|| Mutex::new(None));
/// Project files by working directory, with when they were read.
type FileCache = HashMap<PathBuf, (Instant, Vec<String>)>;
static FILE_CACHE: Lazy<Mutex<FileCache>> = Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Clone, Debug)]
struct Project {
    /// The terminal the agent runs in.
    terminal_pid: i32,
    /// Paths relative to the agent's working directory.
    files: Vec<String>,
}

/// Recording started: if an agent runs in the terminal in front, read its
/// project's files in the background.
pub fn capture(bundle_id: Option<&str>, terminal_pid: Option<i32>) {
    *CURRENT.lock().unwrap() = None;
    let (Some(bundle_id), Some(terminal_pid)) = (bundle_id, terminal_pid) else {
        return;
    };
    if !TERMINALS.contains(&bundle_id) {
        return;
    }
    std::thread::spawn(move || {
        let Some(cwd) = agent_cwd(terminal_pid) else {
            return;
        };
        let Some(files) = project_files(&cwd) else {
            return;
        };
        log::debug!(
            "Coding agent in {}: {} project files",
            cwd.display(),
            files.len()
        );
        *CURRENT.lock().unwrap() = Some(Project {
            terminal_pid,
            files,
        });
    });
}

/// Tag the file names in a dictation pasted into the agent's terminal.
pub fn tag(text: &str, terminal_pid: Option<i32>) -> String {
    let Some(project) = CURRENT.lock().unwrap().clone() else {
        return text.to_string();
    };
    if Some(project.terminal_pid) != terminal_pid {
        return text.to_string();
    }
    let tagged = tag_files(text, &project.files);
    if tagged != text {
        log::info!("Tagged project files for the coding agent");
    }
    tagged
}

/// One process: pid, parent, command line.
struct Proc {
    pid: i32,
    ppid: i32,
    args: String,
}

fn processes() -> Vec<Proc> {
    let Ok(out) = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,args="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let pid = parts.next()?.parse().ok()?;
            let ppid = parts.next()?.parse().ok()?;
            Some(Proc {
                pid,
                ppid,
                args: parts.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}

fn is_agent(args: &str) -> bool {
    let program = args.split_whitespace().next().unwrap_or("");
    let name = program.rsplit('/').next().unwrap_or(program);
    AGENTS.contains(&name) || AGENT_PACKAGES.iter().any(|p| args.contains(p))
}

/// Agents running under `root`, outermost first (an agent's own helper
/// processes are not counted again).
fn agents_under(procs: &[Proc], root: i32) -> Vec<i32> {
    let mut children: HashMap<i32, Vec<&Proc>> = HashMap::new();
    for p in procs {
        children.entry(p.ppid).or_default().push(p);
    }
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(pid) = stack.pop() {
        for child in children.get(&pid).into_iter().flatten() {
            if is_agent(&child.args) {
                found.push(child.pid);
            } else {
                stack.push(child.pid);
            }
        }
    }
    found
}

fn cwd_of(pid: i32) -> Option<PathBuf> {
    let out = Command::new("/usr/sbin/lsof")
        .args(["-a", "-d", "cwd", "-p", &pid.to_string(), "-Fn"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|l| l.strip_prefix('n'))
        .map(PathBuf::from)
}

/// Working directory of the agent in the terminal. With several agents
/// open (tabs), only when they all work in the same place.
fn agent_cwd(terminal_pid: i32) -> Option<PathBuf> {
    let agents = agents_under(&processes(), terminal_pid);
    let mut cwds = agents.into_iter().filter_map(cwd_of);
    let first = cwds.next()?;
    cwds.all(|c| c == first).then_some(first)
}

fn project_files(cwd: &Path) -> Option<Vec<String>> {
    let mut cache = FILE_CACHE.lock().unwrap();
    if let Some((at, files)) = cache.get(cwd) {
        if at.elapsed() < FILES_FRESH_FOR {
            return Some(files.clone());
        }
    }
    drop(cache);
    let out = Command::new("/usr/bin/git")
        .arg("-C")
        .arg(cwd)
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect();
    if files.is_empty() || files.len() > MAX_FILES {
        return None;
    }
    cache = FILE_CACHE.lock().unwrap();
    cache.insert(cwd.to_path_buf(), (Instant::now(), files.clone()));
    Some(files)
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The one project file a name (or trailing part of a path) refers to.
fn unique_match<'a>(name: &str, files: &'a [String]) -> Option<&'a str> {
    let name = name.to_lowercase();
    let mut hits = files.iter().filter(|f| {
        let f = f.to_lowercase();
        f == name || f.ends_with(&format!("/{name}"))
    });
    let hit = hits.next()?;
    hits.next().is_none().then_some(hit.as_str())
}

fn extensions(files: &[String]) -> std::collections::HashSet<String> {
    files
        .iter()
        .filter_map(|f| basename(f).rsplit_once('.').map(|(_, e)| e.to_lowercase()))
        .collect()
}

/// Spoken file names ("edit learning dot rs") as written ones
/// ("edit_learning.rs"), when that names a project file.
fn join_spoken_names(text: &str, files: &[String]) -> String {
    let exts = extensions(files);
    let words: Vec<&str> = text.split(' ').collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let is_dot = words[i].eq_ignore_ascii_case("dot") && i + 1 < words.len();
        if is_dot {
            let (ext, trailing) = split_trailing(words[i + 1]);
            if exts.contains(&ext.to_lowercase()) {
                if let Some((taken, name)) = spoken_name(&out, ext, files) {
                    out.truncate(out.len() - taken);
                    out.push(format!("{name}{trailing}"));
                    i += 2;
                    continue;
                }
            }
        }
        out.push(words[i].to_string());
        i += 1;
    }
    out.join(" ")
}

/// The longest run of words before " dot ext" that names a project file:
/// how many words it took and the file's name.
fn spoken_name(before: &[String], ext: &str, files: &[String]) -> Option<(usize, String)> {
    for taken in (1..=MAX_NAME_WORDS.min(before.len())).rev() {
        let parts: Vec<String> = before[before.len() - taken..]
            .iter()
            .map(|w| w.to_lowercase())
            .collect();
        if parts
            .iter()
            .any(|w| w.is_empty() || !w.chars().all(|c| c.is_alphanumeric()))
        {
            continue;
        }
        for sep in ["_", "-", ""] {
            let candidate = format!("{}.{}", parts.join(sep), ext.to_lowercase());
            if let Some(file) = unique_match(&candidate, files) {
                return Some((taken, basename(file).to_string()));
            }
        }
    }
    None
}

/// A word and the punctuation after it.
fn split_trailing(word: &str) -> (&str, &str) {
    let end = word
        .trim_end_matches([',', '.', '?', '!', ':', ';', ')'])
        .len();
    word.split_at(end)
}

/// Turn file names that match exactly one project file into mentions.
pub(crate) fn tag_files(text: &str, files: &[String]) -> String {
    let joined = join_spoken_names(text, files);
    joined
        .split(' ')
        .map(|word| {
            let (name, trailing) = split_trailing(word);
            let (lead, name) =
                name.split_at(name.len() - name.trim_start_matches(['(', '"', '\'', '`']).len());
            let name_inner = name.trim_end_matches(['"', '\'', '`']);
            let looks_like_file = !name_inner.starts_with('@')
                && name_inner.contains('.')
                && name_inner.rsplit_once('.').is_some_and(|(stem, ext)| {
                    !stem.is_empty()
                        && !ext.is_empty()
                        && ext.chars().all(|c| c.is_ascii_alphanumeric())
                });
            match looks_like_file
                .then(|| unique_match(name_inner, files))
                .flatten()
            {
                Some(file) => format!("{lead}@{file}{}{trailing}", &name[name_inner.len()..]),
                None => word.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<String> {
        [
            "src/actions.rs",
            "src/edit_learning.rs",
            "src/managers/mod.rs",
            "src/shortcut/mod.rs",
            "src/overlay/RecordingOverlay.tsx",
            "package.json",
            "notes/read-me.md",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    #[test]
    fn tags_written_names() {
        assert_eq!(
            tag_files("Look at actions.rs, then package.json.", &files()),
            "Look at @src/actions.rs, then @package.json."
        );
        assert_eq!(
            tag_files("open recordingoverlay.tsx", &files()),
            "open @src/overlay/RecordingOverlay.tsx"
        );
    }

    #[test]
    fn tags_spoken_names() {
        assert_eq!(
            tag_files("fix edit learning dot rs please", &files()),
            "fix @src/edit_learning.rs please"
        );
        assert_eq!(
            tag_files("check read me dot md.", &files()),
            "check @notes/read-me.md."
        );
        assert_eq!(
            tag_files("Change actions dot RS", &files()),
            "Change @src/actions.rs"
        );
    }

    #[test]
    fn leaves_ambiguous_and_unknown_names() {
        assert_eq!(tag_files("edit mod.rs", &files()), "edit mod.rs");
        assert_eq!(
            tag_files("edit shortcut/mod.rs", &files()),
            "edit @src/shortcut/mod.rs"
        );
        assert_eq!(tag_files("see main.rs", &files()), "see main.rs");
        assert_eq!(tag_files("the dot is here", &files()), "the dot is here");
        assert_eq!(
            tag_files("already @src/actions.rs", &files()),
            "already @src/actions.rs"
        );
        assert_eq!(tag_files("it costs 3.50", &files()), "it costs 3.50");
    }

    #[test]
    fn finds_agents_in_the_process_tree() {
        let procs = vec![
            Proc {
                pid: 10,
                ppid: 1,
                args: "/Applications/Ghostty.app/Contents/MacOS/ghostty".into(),
            },
            Proc {
                pid: 11,
                ppid: 10,
                args: "/usr/bin/login -fp me".into(),
            },
            Proc {
                pid: 12,
                ppid: 11,
                args: "-zsh".into(),
            },
            Proc {
                pid: 13,
                ppid: 12,
                args: "/Users/me/.local/bin/claude".into(),
            },
            Proc {
                pid: 14,
                ppid: 13,
                args: "/Users/me/.local/bin/claude --helper".into(),
            },
            Proc {
                pid: 20,
                ppid: 12,
                args: "node /opt/homebrew/lib/node_modules/@openai/codex/bin/codex.js".into(),
            },
            Proc {
                pid: 30,
                ppid: 1,
                args: "/Users/me/.local/bin/claude".into(),
            },
        ];
        let mut found = agents_under(&procs, 10);
        found.sort();
        assert_eq!(found, vec![13, 20]);
    }
}
