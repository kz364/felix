//! The voice assistant ("Felix" by default). Saying its name anywhere in a
//! dictation hands the dictation, what's in the focused text field and a few
//! notes about the user to a ChatGPT model, which writes what should end up in
//! the field: the dictation with a request carried out ("my email is Felix put
//! my email here"), a new piece of writing ("Felix, reply saying I'm in"), or a
//! rewrite of the selection or the whole draft ("Felix, make this shorter").
//!
//! The model answers in JSON with the text and where it goes, so a request to
//! change the draft can replace the field rather than paste after it.

use crate::settings::AppSettings;
use crate::text_field::FieldSnapshot;
use serde::Deserialize;

/// Field text sent before and after the cursor. Longer fields are cut, and
/// then can't be replaced as a whole.
const MAX_BEFORE: usize = 6000;
const MAX_AFTER: usize = 2000;

/// Where the assistant's text goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// At the cursor, like a dictation.
    Insert,
    /// Over the selected text (the paste replaces it).
    ReplaceSelection,
    /// Over the whole field. Holds the field's text as read before the
    /// request, so the paste can check nothing changed in the meantime.
    ReplaceField(Vec<u16>),
    /// Nothing pasted: start a Claude Code session in the desktop app, in
    /// this project's folder, with the text as its first message.
    ClaudeSession { project: String },
    /// Nothing pasted: do the text, a task, on the computer (Codex + Cua).
    ComputerTask,
    /// Nothing pasted: the text reports a dictation that came out wrong;
    /// fix the rules file (see `rules`).
    ReportMistake,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub text: String,
    pub placement: Placement,
}

impl Edit {
    /// Placement without the field text, for logs.
    pub fn placement_kind(&self) -> &'static str {
        match self.placement {
            Placement::Insert => "insert",
            Placement::ReplaceSelection => "replace selection",
            Placement::ReplaceField(_) => "replace field",
            Placement::ClaudeSession { .. } => "claude session",
            Placement::ComputerTask => "computer task",
            Placement::ReportMistake => "report mistake",
        }
    }
}

/// Whether `text` says the assistant's name (a whole word, any case).
pub fn is_addressed(text: &str, name: &str) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }
    regex::Regex::new(&format!(r"(?i)\b{}\b", regex::escape(name)))
        .is_ok_and(|re| re.is_match(text))
}

/// The request types that act on the computer rather than the field, offered
/// only when the user turned them on.
fn agent_rules(name: &str) -> String {
    format!(
        r#"- They asked you to start a Claude Code session, thread or project ("{name}, open Claude Code and start a new project that…"): use "start_claude_session". "text" is the first message for that session, written as a clear task in their words; "project" is a short name for the project folder (the one they named, or a few words describing it). Only when they clearly ask for this.
- They asked you to do something on the computer other than writing into this field (open an app and do something in it, find or change something in an app): use "computer_task". "text" is the task as a clear instruction with every detail they gave. Only when they tell you, {name}, to do it now; a reminder or note they are writing ("remember to open Calendar") is dictation, not a task. Claude Code sessions use "start_claude_session".
"#
    )
}

fn instructions(name: &str, agent_actions: bool) -> String {
    let (agent_rules, agent_placements, project_rule) = if agent_actions {
        (
            agent_rules(name),
            "- \"start_claude_session\", \"computer_task\": nothing goes in the field; see above.\n",
            "\n\"project\" is empty unless you start a Claude session.\n",
        )
    } else {
        (String::new(), "", "")
    };
    format!(
        r#"You are {name}, a writing assistant built into a dictation app. The user dictated into a text field in some app, and somewhere in the dictation (start, middle or end) they said your name, "{name}". The rest of the dictation may be text they want written as they said it.

The request is the instruction said right before or right after your name ("{name}, put my email here", "make this shorter, {name}"). If the words next to your name could be an instruction to you, treat them as one, even if it sounds casual.

Work out what they want and write the text that should end up in the field:
- They dictated text and asked you to fill something in or add to it ("my email is {name} put my email here"): write their text with the request carried out.
- A request can come after a long stretch of dictation ("... {name}, go add my details"). It applies to everything they said before it: write all of that text with the request carried out, putting added details where they naturally belong.
- They asked you to write something (a reply, a message, a list, a summary): write it in their voice, fitting the app and what's already in the field.
- They selected text and asked for a change: rewrite the selection.
- They asked to change the draft already in the field (fix, shorten, rephrase, translate it): rewrite the whole field.
- They're telling you dictation got something wrong, to fix how a word or name is written, or that it should hear something differently ("{name}, it keeps writing cloud instead of Claude", "{name}, kubectl is spelled k-u-b-e-c-t-l", "{name}, that last one came out wrong, I said…"): use "report_mistake". "text" is their report in their own words, with every detail they gave, including how it was written and what they meant. This is about how dictation transcribes, not a request to edit the field.
{agent_rules}- There is no instruction next to your name (for example, your name alone at the end of an email): do nothing. Return the dictation exactly as they said it, without your name, with "insert_at_cursor". Don't continue, complete or polish the text, and don't touch the field.

Choose where the text goes:
- "report_mistake": nothing goes in the field; see above.
- "insert_at_cursor": at the cursor, between the text before and after it.
- "replace_selection": replaces the selected text. Only when there is a selection.
- "replace_field": replaces everything in the field. Only when the field is included in full.
{agent_placements}{project_rule}
Rules:
- "text" is exactly what goes in the field: no quotes around it, no preamble, no explanation, no comments to the user.
- Leave out your name and the request itself.
- Use details from "About the user" when asked for them. Never make up personal details such as emails, phone numbers or addresses; if one is missing, write a placeholder like [email].
- Fix obvious speech-recognition mistakes in the dictation.
- Match the language, tone and formatting of the field and the app. Use Markdown only if the field already does.
- When inserting mid-text, write only the new text, so it reads naturally between what's before and after the cursor."#
    )
}

/// The reply format; the agent placements and "project" only when they're on.
fn schema(agent_actions: bool) -> serde_json::Value {
    let mut placements = vec![
        "insert_at_cursor",
        "replace_selection",
        "replace_field",
        "report_mistake",
    ];
    let mut properties = serde_json::json!({"text": {"type": "string"}});
    let mut required = vec!["placement", "text"];
    if agent_actions {
        placements.extend(["start_claude_session", "computer_task"]);
        properties["project"] = serde_json::json!({"type": "string"});
        required.push("project");
    }
    properties["placement"] = serde_json::json!({"type": "string", "enum": placements});
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// Where the dictation is going, for the model.
pub struct Destination<'a> {
    pub app_name: Option<&'a str>,
    pub url_host: Option<&'a str>,
    pub field: Option<&'a FieldSnapshot>,
}

/// The request text, and whether the whole field is in it.
fn build_input(dictation: &str, notes: &str, destination: &Destination) -> (String, bool) {
    let mut input = String::new();
    match (destination.app_name, destination.url_host) {
        (Some(app), Some(host)) => input.push_str(&format!("App: {app} ({host})\n")),
        (Some(app), None) => input.push_str(&format!("App: {app}\n")),
        (None, Some(host)) => input.push_str(&format!("Website: {host}\n")),
        (None, None) => {}
    }
    let notes = notes.trim();
    input.push_str("\nAbout the user:\n");
    input.push_str(if notes.is_empty() { "(nothing)" } else { notes });
    input.push('\n');

    let mut whole_field = false;
    match destination.field {
        Some(field) => {
            let (loc, len) = field.selection.unwrap_or((field.value.len(), 0));
            let loc = loc.min(field.value.len());
            let end = (loc + len).min(field.value.len());
            let before = String::from_utf16_lossy(&field.value[..loc]);
            let selected = String::from_utf16_lossy(&field.value[loc..end]);
            let after = String::from_utf16_lossy(&field.value[end..]);
            let before_chars = before.chars().count();
            let after_chars = after.chars().count();
            whole_field = before_chars <= MAX_BEFORE && after_chars <= MAX_AFTER;
            let before: String = before
                .chars()
                .skip(before_chars.saturating_sub(MAX_BEFORE))
                .collect();
            let after: String = after.chars().take(MAX_AFTER).collect();

            if field.value.is_empty() {
                input.push_str("\nText field: empty.\n");
            } else {
                input.push_str(if whole_field {
                    "\nText field (in full):\n"
                } else {
                    "\nText field (cut to the part around the cursor; replace_field is not allowed):\n"
                });
                input.push_str("<<<\n");
                input.push_str(&before);
                if selected.is_empty() {
                    input.push_str("[CURSOR]");
                } else {
                    input.push_str("[SELECTION START]");
                    input.push_str(&selected);
                    input.push_str("[SELECTION END]");
                }
                input.push_str(&after);
                input.push_str("\n>>>\n");
            }
        }
        None => input.push_str("\nText field: couldn't be read; the text goes at the cursor.\n"),
    }

    input.push_str("\nDictation:\n<<<\n");
    input.push_str(dictation.trim());
    input.push_str("\n>>>\n");
    (input, whole_field)
}

#[derive(Deserialize)]
struct Reply {
    placement: String,
    text: String,
    #[serde(default)]
    project: String,
}

/// Turn the model's JSON into an edit, falling back to inserting when it asks
/// for a placement that isn't possible here.
fn parse_reply(
    reply: &str,
    field: Option<&FieldSnapshot>,
    whole_field: bool,
) -> Result<Edit, String> {
    let reply: Reply = serde_json::from_str(reply.trim())
        .map_err(|_| "The assistant's reply wasn't in the expected format".to_string())?;
    let text = reply.text.trim().to_string();
    let has_selection = field.is_some_and(|f| f.selection.is_some_and(|(_, len)| len > 0));
    let placement = match (reply.placement.as_str(), field) {
        ("computer_task", _) if !text.is_empty() => Placement::ComputerTask,
        ("report_mistake", _) if !text.is_empty() => Placement::ReportMistake,
        ("start_claude_session", _) if !text.is_empty() => Placement::ClaudeSession {
            project: reply.project.trim().to_string(),
        },
        ("replace_selection", _) if has_selection => Placement::ReplaceSelection,
        ("replace_field", Some(field)) if whole_field && !field.value.is_empty() => {
            Placement::ReplaceField(field.value.clone())
        }
        _ => Placement::Insert,
    };
    // An empty insert (the name said with no request) pastes nothing.
    Ok(Edit { text, placement })
}

/// Ask the model to carry out the dictation's request.
pub async fn run(
    settings: &AppSettings,
    dictation: &str,
    destination: &Destination<'_>,
) -> Result<Edit, String> {
    ask(
        &Assistant {
            name: &settings.assistant_name,
            notes: &settings.assistant_notes,
            model: &settings.assistant_model,
            effort: &settings.assistant_effort,
            agent_actions: settings.agent_actions_enabled,
        },
        dictation,
        destination,
    )
    .await
}

/// Who answers, and what it knows about the user.
pub struct Assistant<'a> {
    pub name: &'a str,
    pub notes: &'a str,
    pub model: &'a str,
    pub effort: &'a str,
    /// Offer the placements that act on the computer (Claude sessions,
    /// computer tasks), not just ones that write into the field.
    pub agent_actions: bool,
}

pub async fn ask(
    assistant: &Assistant<'_>,
    dictation: &str,
    destination: &Destination<'_>,
) -> Result<Edit, String> {
    #[cfg(target_os = "macos")]
    {
        if crate::chatgpt::signed_in_as().is_none() {
            return Err(format!(
                "Sign in with ChatGPT (Settings → Voice Control) to use {}",
                assistant.name
            ));
        }
        let (input, whole_field) = build_input(dictation, assistant.notes, destination);
        let instructions = instructions(assistant.name, assistant.agent_actions);
        let schema = schema(assistant.agent_actions);
        let reply = crate::chatgpt::complete(crate::chatgpt::Request {
            model: assistant.model,
            effort: assistant.effort,
            instructions: &instructions,
            input: &input,
            schema: Some(&schema),
        })
        .await?;
        let mut edit = parse_reply(&reply, destination.field, whole_field)?;
        if !assistant.agent_actions
            && matches!(
                edit.placement,
                Placement::ClaudeSession { .. } | Placement::ComputerTask
            )
        {
            edit.placement = Placement::Insert;
        }
        Ok(edit)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (assistant, dictation, destination);
        Err("The assistant is only available on macOS".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(text: &str, selection: (usize, usize)) -> FieldSnapshot {
        FieldSnapshot {
            pid: 1,
            role: "AXTextArea".into(),
            value: text.encode_utf16().collect(),
            selection: Some(selection),
        }
    }

    #[test]
    fn name_is_matched_as_a_whole_word_in_any_case() {
        assert!(is_addressed("my email is felix put it here", "Felix"));
        assert!(is_addressed("Felix, reply saying yes.", "Felix"));
        assert!(is_addressed("ask FELIX's opinion", "Felix"));
        assert!(!is_addressed("the Felixstowe office", "Felix"));
        assert!(!is_addressed("nothing to see", "Felix"));
        assert!(!is_addressed("anything", " "));
    }

    #[test]
    fn input_marks_the_cursor_or_selection() {
        let f = field("Hi Sam, thanks!", (7, 0));
        let dest = Destination {
            app_name: Some("Mail"),
            url_host: None,
            field: Some(&f),
        };
        let (input, whole) = build_input("Felix say I'll be late", "Name: Kas", &dest);
        assert!(whole);
        assert!(input.contains("App: Mail\n"));
        assert!(input.contains("Name: Kas"));
        assert!(input.contains("Hi Sam,[CURSOR] thanks!"));
        assert!(input.contains("Felix say I'll be late"));

        let f = field("Hi Sam, thanks!", (8, 6));
        let dest = Destination {
            app_name: None,
            url_host: None,
            field: Some(&f),
        };
        let (input, _) = build_input("Felix fancier", "", &dest);
        assert!(input.contains("Hi Sam, [SELECTION START]thanks[SELECTION END]!"));
        assert!(input.contains("About the user:\n(nothing)"));
    }

    #[test]
    fn long_fields_are_cut_and_cannot_be_replaced() {
        let long = "word ".repeat(2000);
        let f = field(&long, (long.encode_utf16().count(), 0));
        let dest = Destination {
            app_name: None,
            url_host: None,
            field: Some(&f),
        };
        let (input, whole) = build_input("Felix fix it", "", &dest);
        assert!(!whole);
        assert!(input.contains("replace_field is not allowed"));
        let edit = parse_reply(
            r#"{"placement":"replace_field","text":"Fixed."}"#,
            Some(&f),
            whole,
        )
        .unwrap();
        assert_eq!(edit.placement, Placement::Insert);
    }

    #[test]
    fn replies_map_to_placements() {
        let f = field("Draft text", (0, 5));
        let edit = parse_reply(
            r#"{"placement":"replace_selection","text":"Final"}"#,
            Some(&f),
            true,
        )
        .unwrap();
        assert_eq!(edit.placement, Placement::ReplaceSelection);

        let edit = parse_reply(
            r#"{"placement":"replace_field","text":"All new"}"#,
            Some(&f),
            true,
        )
        .unwrap();
        assert_eq!(
            edit.placement,
            Placement::ReplaceField("Draft text".encode_utf16().collect())
        );

        // No selection: a selection replacement becomes an insert.
        let caret = field("Draft text", (10, 0));
        let edit = parse_reply(
            r#"{"placement":"replace_selection","text":" more"}"#,
            Some(&caret),
            true,
        )
        .unwrap();
        assert_eq!(edit.placement, Placement::Insert);

        // Unreadable field: always an insert.
        let edit = parse_reply(r#"{"placement":"replace_field","text":"x"}"#, None, false).unwrap();
        assert_eq!(edit.placement, Placement::Insert);

        assert!(parse_reply("Sure! Here you go", Some(&f), true).is_err());
        let edit = parse_reply(
            r#"{"placement":"insert_at_cursor","text":"  "}"#,
            None,
            false,
        )
        .unwrap();
        assert_eq!(edit.text, "");

        let edit = parse_reply(
            r#"{"placement":"start_claude_session","text":"Build a recipe scraper","project":" Recipe Scraper "}"#,
            None,
            false,
        )
        .unwrap();
        assert_eq!(
            edit.placement,
            Placement::ClaudeSession {
                project: "Recipe Scraper".into()
            }
        );

        let edit = parse_reply(
            r#"{"placement":"computer_task","text":"Open Notes and start a shopping list","project":""}"#,
            None,
            false,
        )
        .unwrap();
        assert_eq!(edit.placement, Placement::ComputerTask);
        assert_eq!(edit.text, "Open Notes and start a shopping list");
    }

    #[test]
    fn agent_actions_are_only_offered_when_on() {
        let off = instructions("Felix", false);
        assert!(!off.contains("computer_task") && !off.contains("start_claude_session"));
        assert!(!off.contains("\"project\""));
        assert!(off.contains("\"replace_field\": replaces everything in the field. Only when the field is included in full.\n\nRules:"));
        let on = instructions("Felix", true);
        assert!(on.contains("computer_task") && on.contains("start_claude_session"));

        let schema_off = schema(false).to_string();
        assert!(!schema_off.contains("computer_task") && !schema_off.contains("project"));
        let schema_on = schema(true).to_string();
        assert!(schema_on.contains("computer_task") && schema_on.contains("project"));
    }

    #[test]
    fn mistake_reports_are_always_offered() {
        assert!(instructions("Felix", false).contains("report_mistake"));
        assert!(schema(false).to_string().contains("report_mistake"));
        let edit = parse_reply(
            r#"{"placement":"report_mistake","text":"it wrote cloud instead of Claude"}"#,
            None,
            false,
        )
        .unwrap();
        assert_eq!(edit.placement, Placement::ReportMistake);
    }
}
