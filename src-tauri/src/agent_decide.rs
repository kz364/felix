//! Felix's fast path for computer tasks: a classifier picks each step.
//!
//! Cua reads the app's accessibility tree, the actionable elements become
//! candidate actions ("press button 'New Note'", "type the text into
//! textarea 'Body'", "finished"), and Simple Jev (Featherless's classifier)
//! picks one, in about a second. A classifier can't write, so the text to
//! type comes from Felix's router.
//!
//! Guardrails are code, not prompt: Felix never presses anything that sends,
//! posts, deletes, buys or pays (it stops and hands over), types the text at
//! most once, and gives up when the same action changes nothing. Whatever it
//! can't finish goes to Codex (see `agent`).

use log::{debug, info};
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

/// Simple Jev's public demo: no key, 4 requests a second, short context.
const JEV_URL: &str = "https://simple-jev-demo-api.featherless.ai/v1/classifier";
const JEV_MODEL: &str = "featherless-ai/Qwen3.6-35B-A3B-classifier";
const MAX_STEPS: usize = 10;
/// Jev's limit for a choice question.
const MAX_CHOICES: usize = 50;
const LABEL_CHARS: usize = 40;
const FINISHED: &str = "finished: nothing left to do";
const NEXT_QUESTION: &str = "Which single action should be taken next toward the goal? Never add to or change existing, unrelated content: if the goal is to start or create something, create a new one first.";
const DONE_QUESTION: &str = "Has the goal already been fully achieved?";

static BLOCKED: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)\b(send|post|publish|share|reply|forward|delete|remove|erase|empty|trash|buy|purchase|pay|checkout|place order|transfer|submit|sign out|log ?out|unsubscribe)\b").unwrap()
});

const PRESSABLE: &[&str] = &[
    "AXButton",
    "AXMenuItem",
    "AXMenuBarItem",
    "AXCheckBox",
    "AXRadioButton",
    "AXPopUpButton",
    "AXMenuButton",
    "AXRow",
    "AXCell",
    "AXLink",
    "AXTab",
    "AXDisclosureTriangle",
];
const TYPEABLE: &[&str] = &["AXTextField", "AXTextArea", "AXSearchField", "AXComboBox"];

/// How a fast run ended.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// The goal is met; a line for the user.
    Done(String),
    /// Stopped before something only the user should do; a line for them.
    Handoff(String),
    /// Couldn't finish; what it did so far, for Codex to carry on from.
    GaveUp { reason: String, done: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
enum Step {
    Press(u64),
    Type(u64),
    Finish,
}

struct Driver<'a> {
    path: &'a Path,
    socket: &'a Path,
}

impl Driver<'_> {
    fn call(&self, tool: &str, args: Value) -> Result<Value, String> {
        let out = Command::new(self.path)
            .args(["call", tool, &args.to_string(), "--socket"])
            .arg(self.socket)
            .envs(crate::agent::driver_env())
            .output()
            .map_err(|e| format!("{tool}: {e}"))?;
        let value: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        let value = value.get("structuredContent").cloned().unwrap_or(value);
        if !out.status.success() || value.get("code").is_some() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("{tool}: {} {}", err.trim(), value));
        }
        Ok(value)
    }

    /// The app's pid and its main window, launching it if needed.
    fn open(&self, name: &str) -> Result<(u64, u64), String> {
        for attempt in 0..2 {
            let apps = self.call("list_apps", json!({}))?;
            let app = find_app(&apps, name).ok_or(format!("No app called {name}"))?;
            let pid = app["pid"].as_u64().filter(|_| app["running"] == true);
            if let Some(pid) = pid {
                let windows = self.call("list_windows", json!({"pid": pid}))?;
                if let Some(window) = main_window(&windows) {
                    return Ok((pid, window));
                }
            }
            if attempt == 0 {
                // Launching a running app reopens its window.
                let bundle = app["bundle_id"].as_str().unwrap_or_default();
                self.call("launch_app", json!({"bundle_id": bundle}))?;
                std::thread::sleep(Duration::from_millis(1200));
            }
        }
        Err(format!("{name} has no window to work in"))
    }
}

/// Match an app by name, ignoring case and invisible marks ("\u{200e}WhatsApp").
fn find_app<'a>(apps: &'a Value, name: &str) -> Option<&'a Value> {
    let norm = |s: &str| {
        s.chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect::<String>()
    };
    let want = norm(name.trim_end_matches(".app"));
    let apps = apps["apps"].as_array()?;
    let name_of = |a: &Value| norm(a["name"].as_str().unwrap_or_default());
    apps.iter().find(|a| name_of(a) == want).or_else(|| {
        apps.iter()
            .find(|a| !want.is_empty() && name_of(a).contains(&want))
    })
}

/// The app's real window: skip menu-bar-sized strips, prefer on screen, then size.
fn main_window(windows: &Value) -> Option<u64> {
    windows["windows"]
        .as_array()?
        .iter()
        .filter(|w| w["bounds"]["height"].as_f64() >= Some(150.0))
        .filter(|w| w["bounds"]["width"].as_f64() >= Some(200.0))
        .max_by_key(|w| {
            let area = w["bounds"]["height"].as_f64().unwrap_or(0.0)
                * w["bounds"]["width"].as_f64().unwrap_or(0.0);
            (w["is_on_screen"] == true, area as u64)
        })
        .and_then(|w| w["window_id"].as_u64())
}

fn describe(element: &Value) -> Option<String> {
    let role = element["role"].as_str()?;
    let label: String = element["label"]
        .as_str()?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(LABEL_CHARS)
        .collect();
    if label.is_empty() {
        return None;
    }
    let kind = role.trim_start_matches("AX").to_lowercase();
    Some(format!("{kind} '{label}'"))
}

/// The actions on offer: {candidate: (step, description for the classifier)}.
fn candidates(
    elements: &[Value],
    can_type: bool,
    goal: &str,
) -> BTreeMap<String, (Step, Option<String>)> {
    let mut out: Vec<(String, (Step, Option<String>))> = Vec::new();
    for e in elements {
        let (Some(name), Some(role), Some(idx)) =
            (describe(e), e["role"].as_str(), e["element_index"].as_u64())
        else {
            continue;
        };
        let presses = e["actions"]
            .as_array()
            .is_some_and(|a| a.iter().any(|a| a.as_str() == Some("AXPress")));
        if TYPEABLE.contains(&role) && can_type {
            let value: String = e["value"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(60)
                .collect();
            let desc = if value.is_empty() {
                "empty field".to_string()
            } else {
                format!("field currently holds: {value:?}")
            };
            out.push((
                format!("type the text into {name}"),
                (Step::Type(idx), Some(desc)),
            ));
        }
        if PRESSABLE.contains(&role) || presses {
            out.push((format!("press {name}"), (Step::Press(idx), None)));
        }
    }
    // First of each name wins; keep the ones sharing words with the goal.
    let mut seen = std::collections::HashSet::new();
    out.retain(|(k, _)| seen.insert(k.clone()));
    if out.len() > MAX_CHOICES - 1 {
        let words: std::collections::HashSet<String> = goal
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() > 2)
            .map(String::from)
            .collect();
        let overlap = |k: &str| {
            k.to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| words.contains(*w))
                .count()
        };
        out.sort_by_key(|(k, _)| std::cmp::Reverse(overlap(k)));
        out.truncate(MAX_CHOICES - 1);
    }
    let mut map: BTreeMap<_, _> = out.into_iter().collect();
    map.insert(FINISHED.to_string(), (Step::Finish, None));
    map
}

/// Ask Jev for the next action and whether the goal is met.
fn decide(
    state: &str,
    options: &BTreeMap<String, (Step, Option<String>)>,
) -> Result<(String, f64), String> {
    let criteria: serde_json::Map<String, Value> = options
        .iter()
        .map(|(k, (_, d))| (k.clone(), json!(d)))
        .collect();
    let body = json!({
        "model": JEV_MODEL,
        "state": state,
        "questions": {
            "next": {"type": "choice", "instructions": NEXT_QUESTION, "criteria": criteria},
            "done": {"type": "noul", "instructions": DONE_QUESTION},
        }
    });
    let reply: Value = tauri::async_runtime::block_on(async {
        let resp = reqwest::Client::new()
            .post(JEV_URL)
            .header("User-Agent", "felix/1.0")
            .timeout(Duration::from_secs(30))
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Simple Jev: {e}"))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!(
                "Simple Jev {status}: {}",
                text.chars().take(200).collect::<String>()
            ));
        }
        serde_json::from_str(&text).map_err(|e| e.to_string())
    })?;
    parse_decision(&reply, options)
}

fn parse_decision(
    reply: &Value,
    options: &BTreeMap<String, (Step, Option<String>)>,
) -> Result<(String, f64), String> {
    let choice = reply["answers"]["next"]["choice"]
        .as_str()
        .filter(|c| options.contains_key(*c))
        .ok_or("Simple Jev picked no known action")?;
    let done = reply["answers"]["done"]["noul"].as_f64().unwrap_or(0.0);
    Ok((choice.to_string(), done))
}

fn summary(done: &[String]) -> String {
    if done.is_empty() {
        return "It was already done.".into();
    }
    let mut s = done
        .iter()
        .map(|d| {
            d.replace("type the text into", "typed into")
                .replace("press ", "pressed ")
        })
        .collect::<Vec<_>>()
        .join(", then ");
    s[..1].make_ascii_uppercase();
    s + "."
}

/// Try the task with Jev. Blocking; run it off the main thread.
pub fn run(driver: &Path, socket: &Path, goal: &str, app: &str, content: &str) -> Outcome {
    let cua = Driver {
        path: driver,
        socket,
    };
    let give_up = |reason: String, done: &[String]| {
        info!("Fast path gave up: {reason}");
        Outcome::GaveUp {
            reason,
            done: done.to_vec(),
        }
    };
    let (pid, window) = match cua.open(app) {
        Ok(w) => w,
        Err(e) => return give_up(e, &[]),
    };
    let mut done: Vec<String> = Vec::new();
    let mut last: Option<(String, String)> = None;
    for step in 0..MAX_STEPS {
        let snap = match cua.call(
            "get_window_state",
            json!({"pid": pid, "window_id": window, "include_screenshot": false, "max_elements": 800}),
        ) {
            Ok(s) => s,
            Err(e) => return give_up(e, &done),
        };
        let elements = snap["elements"].as_array().cloned().unwrap_or_default();
        if elements.is_empty() {
            return give_up("the window's accessibility tree is empty".into(), &done);
        }
        let typed = done.iter().any(|d| d.starts_with("type "));
        let options = candidates(&elements, !content.is_empty() && !typed, goal);
        let fields = elements
            .iter()
            .filter(|e| TYPEABLE.contains(&e["role"].as_str().unwrap_or_default()))
            .filter_map(|e| {
                let value: String = e["value"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .take(60)
                    .collect();
                describe(e).map(|n| format!("{n} = {value:?}"))
            })
            .collect::<Vec<_>>()
            .join("; ");
        let text_line = if content.is_empty() {
            String::new()
        } else {
            let status = if typed {
                "already entered"
            } else {
                "not entered yet"
            };
            format!("Text to enter: {content:?} ({status})\n")
        };
        let state =
            format!(
            "Goal: {goal}\n{text_line}App: {app}, window: {}\nDone so far: {}\nFields: {fields}",
            snap["window_title"].as_str().unwrap_or_default(),
            if done.is_empty() { "nothing yet".into() } else { done.join("; ") },
        );
        let started = Instant::now();
        let (choice, finished) = match decide(&state, &options) {
            Ok(d) => d,
            Err(e) => return give_up(e, &done),
        };
        info!(
            "Fast step {}: {choice} (done {finished:.2}, {:?})",
            step + 1,
            started.elapsed()
        );
        let (action, _) = &options[&choice];
        if finished >= 0.7 || *action == Step::Finish {
            return Outcome::Done(summary(&done));
        }
        if matches!(action, Step::Press(_)) && BLOCKED.is_match(&choice) {
            let mut msg = summary(&done);
            if done.is_empty() {
                msg.clear();
            } else {
                msg.push(' ');
            }
            let target = choice.trim_start_matches("press ");
            return Outcome::Handoff(format!("{msg}The last step, {target}, is yours to do."));
        }
        // The same choice on an unchanged window: it isn't getting anywhere.
        let seen = (choice.clone(), Value::Array(elements.clone()).to_string());
        if last.as_ref() == Some(&seen) {
            return give_up(format!("stuck repeating {choice}"), &done);
        }
        last = Some(seen);
        let snapshot_id = snap["snapshot_id"].clone();
        let result = match action {
            Step::Press(idx) => cua.call(
                "click",
                json!({"pid": pid, "window_id": window, "snapshot_id": snapshot_id, "element_index": idx}),
            ),
            Step::Type(idx) => cua.call(
                "type_text",
                json!({"pid": pid, "window_id": window, "snapshot_id": snapshot_id, "element_index": idx, "text": content}),
            ),
            Step::Finish => unreachable!(),
        };
        if let Err(e) = result {
            return give_up(e, &done);
        }
        debug!("Fast step done: {choice}");
        done.push(choice);
        std::thread::sleep(Duration::from_millis(300));
    }
    give_up(format!("still going after {MAX_STEPS} steps"), &done)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(idx: u64, role: &str, label: &str) -> Value {
        json!({"element_index": idx, "role": role, "label": label})
    }

    #[test]
    fn finds_apps_despite_invisible_marks() {
        let apps = json!({"apps": [
            {"name": "\u{200e}WhatsApp", "pid": 1},
            {"name": "Notes", "pid": 2},
            {"name": "Notion Calendar", "pid": 3},
        ]});
        assert_eq!(find_app(&apps, "whatsapp").unwrap()["pid"], 1);
        assert_eq!(find_app(&apps, "Notes.app").unwrap()["pid"], 2);
        assert_eq!(find_app(&apps, "Notion").unwrap()["pid"], 3);
        assert!(find_app(&apps, "Safari").is_none());
    }

    #[test]
    fn picks_the_real_window_not_the_menu_strip() {
        let windows = json!({"windows": [
            {"window_id": 1, "is_on_screen": false, "bounds": {"width": 1728.0, "height": 33.0}},
            {"window_id": 2, "is_on_screen": false, "bounds": {"width": 900.0, "height": 600.0}},
            {"window_id": 3, "is_on_screen": true, "bounds": {"width": 500.0, "height": 400.0}},
        ]});
        assert_eq!(main_window(&windows), Some(3));
        assert_eq!(
            main_window(
                &json!({"windows": [{"window_id": 1, "bounds": {"width": 1728.0, "height": 33.0}}]})
            ),
            None
        );
    }

    #[test]
    fn offers_presses_typing_and_finishing() {
        let els = vec![
            el(0, "AXButton", "New Note"),
            el(1, "AXTextArea", "Body"),
            el(2, "AXButton", "New Note"),
            el(3, "AXStaticText", "Notes"),
            json!({"element_index": 4, "role": "AXImage", "label": "Pin", "actions": ["AXPress"]}),
        ];
        let c = candidates(&els, true, "start a packing list");
        let keys: Vec<_> = c.keys().cloned().collect();
        assert_eq!(
            keys,
            vec![
                FINISHED,
                "press button 'New Note'",
                "press image 'Pin'",
                "type the text into textarea 'Body'"
            ]
        );
        assert_eq!(c["press button 'New Note'"].0, Step::Press(0));
        assert!(!candidates(&els, false, "")
            .keys()
            .any(|k| k.starts_with("type")));
    }

    #[test]
    fn keeps_within_jevs_choice_limit_favouring_the_goal() {
        let mut els: Vec<Value> = (0..80)
            .map(|i| el(i, "AXButton", &format!("Button {i}")))
            .collect();
        els.push(el(99, "AXButton", "Checklist"));
        let c = candidates(&els, false, "make it a checklist");
        assert_eq!(c.len(), MAX_CHOICES);
        assert!(c.contains_key("press button 'Checklist'"));
    }

    #[test]
    fn never_presses_outward_or_irreversible_things() {
        for label in [
            "press button 'Send'",
            "press button 'Delete'",
            "press menuitem 'Empty Trash'",
            "press button 'Share'",
            "press button 'Buy Now'",
        ] {
            assert!(BLOCKED.is_match(label), "{label}");
        }
        for label in [
            "press button 'New Note'",
            "press button 'Checklist'",
            "press row 'Sender notes'",
        ] {
            assert!(!BLOCKED.is_match(label), "{label}");
        }
    }

    #[test]
    fn reads_jevs_answer() {
        let els = vec![el(0, "AXButton", "New Note")];
        let options = candidates(&els, false, "");
        let reply = json!({"answers": {"next": {"choice": "press button 'New Note'"}, "done": {"noul": 0.02}}});
        assert_eq!(
            parse_decision(&reply, &options).unwrap(),
            ("press button 'New Note'".into(), 0.02)
        );
        let bad =
            json!({"answers": {"next": {"choice": "press button 'Nope'"}, "done": {"noul": 0.1}}});
        assert!(parse_decision(&bad, &options).is_err());
    }

    #[test]
    fn summarises_what_it_did() {
        assert_eq!(
            summary(&[
                "press button 'New Note'".into(),
                "type the text into textarea 'Body'".into()
            ]),
            "Pressed button 'New Note', then typed into textarea 'Body'."
        );
    }
}
