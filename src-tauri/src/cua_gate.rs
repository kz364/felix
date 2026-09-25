//! The gate between Codex and Cua Driver: Codex's MCP server is Felix's own
//! executable in this mode (`handy --cua-gate <gate socket> <driver>
//! <driver socket>`), which starts the driver's MCP proxy and passes
//! messages both ways, except:
//! - `tools/list` only lists the tools Felix allows (`agent_run::ALLOWED_TOOLS`);
//! - every `tools/call` is first checked by the running Felix over its gate
//!   socket (`agent_run::check_call`), which shows it on the card and may ask
//!   the user; a refused call gets an error result instead of reaching the
//!   driver.
//!
//! It remembers element names from window readings, so Felix sees "Send",
//! not "element 14". If Felix can't be reached, every call is refused.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

/// Element names from the latest readings: (pid, index or ref) → name.
type Labels = Arc<Mutex<HashMap<(i64, String), String>>>;

/// Run the relay until Codex closes its end. Returns the exit code.
pub fn relay(args: &[String]) -> i32 {
    let [gate, driver, socket] = args else {
        eprintln!("usage: --cua-gate <gate socket> <driver> <driver socket>");
        return 2;
    };
    let child = Command::new(driver)
        .args(["mcp", "--embedded", "--socket", socket])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            eprintln!("cua-gate: couldn't start the driver: {e}");
            return 1;
        }
    };
    let mut to_driver = child.stdin.take().expect("piped stdin");
    let from_driver = child.stdout.take().expect("piped stdout");
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let labels: Labels = Arc::default();
    // Requests in flight: id → (tool, pid), to read labels from the answer
    // and to filter the tool list.
    let pending: Arc<Mutex<HashMap<String, (String, i64)>>> = Arc::default();

    let reader = {
        let (out, labels, pending) = (out.clone(), labels.clone(), pending.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(from_driver).lines().map_while(Result::ok) {
                let line = match serde_json::from_str::<Value>(&line) {
                    Ok(mut msg) => {
                        let id = msg["id"].to_string();
                        if let Some((tool, pid)) = pending.lock().unwrap().remove(&id) {
                            if tool == "tools/list" {
                                filter_tools(&mut msg);
                            } else {
                                remember_labels(&labels, pid, &msg["result"]);
                            }
                        }
                        msg.to_string()
                    }
                    Err(_) => line,
                };
                let mut out = out.lock().unwrap();
                let _ = writeln!(out, "{line}");
                let _ = out.flush();
            }
        })
    };

    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            let _ = writeln!(to_driver, "{line}");
            continue;
        };
        let id = msg["id"].clone();
        match msg["method"].as_str() {
            Some("tools/list") => {
                pending
                    .lock()
                    .unwrap()
                    .insert(id.to_string(), ("tools/list".into(), 0));
            }
            Some("tools/call") => {
                let tool = msg["params"]["name"].as_str().unwrap_or_default();
                let args = &msg["params"]["arguments"];
                let pid = args["pid"].as_i64().unwrap_or(0);
                let key = args["element_index"]
                    .as_i64()
                    .map(|i| i.to_string())
                    .or_else(|| args["ref"].as_str().map(str::to_string));
                let label = key.and_then(|k| labels.lock().unwrap().get(&(pid, k)).cloned());
                if let Err(message) = ask_felix(Path::new(gate), tool, args, label.as_deref()) {
                    let reply = json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {"content": [{"type": "text", "text": message}], "isError": true},
                    });
                    let mut out = out.lock().unwrap();
                    let _ = writeln!(out, "{reply}");
                    let _ = out.flush();
                    continue;
                }
                pending
                    .lock()
                    .unwrap()
                    .insert(id.to_string(), (tool.to_string(), pid));
            }
            _ => {}
        }
        if writeln!(to_driver, "{line}").is_err() {
            break;
        }
        let _ = to_driver.flush();
    }
    drop(to_driver);
    let _ = reader.join();
    let _ = child.kill();
    let _ = child.wait();
    0
}

/// Ask the running Felix whether this call may go ahead (blocks while the
/// user is asked). Unreachable means no.
fn ask_felix(gate: &Path, tool: &str, args: &Value, label: Option<&str>) -> Result<(), String> {
    let mut stream = std::os::unix::net::UnixStream::connect(gate)
        .map_err(|_| "Felix isn't running, so nothing can be done.".to_string())?;
    let request = json!({"tool": tool, "args": args, "label": label});
    writeln!(stream, "{request}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let reply: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    if reply["ok"] == true {
        Ok(())
    } else {
        Err(reply["message"]
            .as_str()
            .unwrap_or("Not allowed")
            .to_string())
    }
}

fn filter_tools(msg: &mut Value) {
    if let Some(tools) = msg["result"]["tools"].as_array_mut() {
        tools.retain(|t| {
            t["name"]
                .as_str()
                .is_some_and(|n| crate::agent_run::ALLOWED_TOOLS.contains(&n))
        });
    }
}

/// Keep the names of the elements a window reading returned, by index or ref.
fn remember_labels(labels: &Labels, pid: i64, result: &Value) {
    let content = result.get("structuredContent").cloned().or_else(|| {
        result["content"][0]["text"]
            .as_str()
            .and_then(|t| serde_json::from_str(t).ok())
    });
    let Some(content) = content else { return };
    let mut found = Vec::new();
    collect_labels(&content, &mut found);
    if found.is_empty() {
        return;
    }
    let mut labels = labels.lock().unwrap();
    labels.retain(|(p, _), _| *p != pid);
    for (key, name) in found {
        labels.insert((pid, key), name);
    }
}

fn collect_labels(value: &Value, found: &mut Vec<(String, String)>) {
    match value {
        Value::Object(map) => {
            let key = map
                .get("element_index")
                .and_then(Value::as_i64)
                .map(|i| i.to_string())
                .or_else(|| map.get("ref").and_then(Value::as_str).map(str::to_string));
            let name = ["label", "name", "title", "text"]
                .iter()
                .find_map(|k| map.get(*k).and_then(Value::as_str))
                .filter(|n| !n.trim().is_empty());
            if let (Some(key), Some(name)) = (key, name) {
                found.push((key, name.to_string()));
            }
            for v in map.values() {
                collect_labels(v, found);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| collect_labels(v, found)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remembers_element_names_from_readings() {
        let labels: Labels = Arc::default();
        let result = json!({"structuredContent": {"elements": [
            {"element_index": 3, "role": "AXButton", "label": "Send"},
            {"element_index": 4, "role": "AXGroup"},
            {"ref": "e12", "name": "Buy now"},
        ]}});
        remember_labels(&labels, 42, &result);
        let labels = labels.lock().unwrap();
        assert_eq!(
            labels.get(&(42, "3".into())).map(String::as_str),
            Some("Send")
        );
        assert_eq!(
            labels.get(&(42, "e12".into())).map(String::as_str),
            Some("Buy now")
        );
        assert!(!labels.contains_key(&(42, "4".into())));
    }

    #[test]
    fn hides_tools_felix_doesnt_allow() {
        let mut msg = json!({"result": {"tools": [
            {"name": "click"}, {"name": "set_config"}, {"name": "clipboard_read"}, {"name": "type_text"}
        ]}});
        filter_tools(&mut msg);
        let names: Vec<_> = msg["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["click", "type_text"]);
    }
}
