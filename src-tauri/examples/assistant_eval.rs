//! Run assistant requests against ChatGPT with made-up text fields.
//!
//!     cargo run --example assistant_eval -- <cases.json> <model> <effort> [notes]
//!
//! cases.json: [{"id", "app", "field", "dictation"}], where "field" marks the
//! cursor with `|` or a selection with `[[...]]`, and is null when unreadable.

#[cfg(target_os = "macos")]
fn main() {
    use handy_app_lib::assistant::{ask, Assistant, Destination};
    use handy_app_lib::text_field::FieldSnapshot;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(&std::fs::read_to_string(&args[0]).expect("cases file"))
            .expect("cases JSON");
    let assistant = Assistant {
        name: "Felix",
        notes: args.get(3).map_or("", String::as_str),
        model: &args[1],
        effort: &args[2],
        agent_actions: std::env::var("FELIX_AGENT").map_or(true, |v| v != "0"),
    };
    tauri::async_runtime::block_on(async {
        for case in cases {
            let field = case["field"].as_str().map(|marked| {
                let (text, selection) = if let (Some(a), Some(b)) =
                    (marked.find("[["), marked.find("]]"))
                {
                    let text = format!("{}{}{}", &marked[..a], &marked[a + 2..b], &marked[b + 2..]);
                    let start = marked[..a].encode_utf16().count();
                    let len = marked[a + 2..b].encode_utf16().count();
                    (text, (start, len))
                } else {
                    let at = marked.find('|').unwrap_or(marked.len());
                    let text = marked.replacen('|', "", 1);
                    (text, (marked[..at].encode_utf16().count(), 0))
                };
                FieldSnapshot {
                    pid: 0,
                    role: "AXTextArea".into(),
                    value: text.encode_utf16().collect(),
                    selection: Some(selection),
                }
            });
            let destination = Destination {
                app_name: case["app"].as_str(),
                url_host: None,
                field: field.as_ref(),
            };
            let started = std::time::Instant::now();
            let result = ask(
                &assistant,
                case["dictation"].as_str().unwrap_or(""),
                &destination,
            )
            .await;
            let secs = started.elapsed().as_secs_f32();
            let line = match result {
                Ok(edit) => {
                    use handy_app_lib::assistant::Placement;
                    let (project, app, content) = match &edit.placement {
                        Placement::ClaudeSession { project } => (Some(project.clone()), None, None),
                        Placement::ComputerTask { app, content } => {
                            (None, Some(app.clone()), Some(content.clone()))
                        }
                        _ => (None, None, None),
                    };
                    serde_json::json!({"id": case["id"], "secs": secs, "placement": edit.placement_kind(), "project": project, "app": app, "content": content, "text": edit.text})
                }
                Err(e) => serde_json::json!({"id": case["id"], "secs": secs, "error": e}),
            };
            println!("{line}");
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn main() {}
