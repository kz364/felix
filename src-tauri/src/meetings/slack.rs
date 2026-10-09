//! Meeting notes sent to a Slack channel by the user's own Slack app, for
//! their agent (or the user) to act on: a message saying whose notes they
//! are, with the summary and the transcript attached as Markdown files.
//!
//! The app is one the user makes from [`APP_MANIFEST`] and installs in
//! their workspace; Felix keeps its bot token. Files go up the way Slack
//! asks now: an upload URL per file, the bytes to it, then one call that
//! shares them in the channel with the message.
//!
//! Until that's set up, a Workflow Builder webhook still works as before:
//! the workflow takes one text variable, `text` (the whole notes), and
//! posts it where the user chose.

use super::manager::{meta_line, read_info, MeetingInfo};
use super::summary::{self, ActionItem, Summary};
use super::transcript::timestamp;
use serde_json::{json, Value};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

const API: &str = "https://slack.com/api";
/// Workflow webhooks are on this host; anything else isn't one.
const WEBHOOK_PREFIX: &str = "https://hooks.slack.com/";
const TIMEOUT: Duration = Duration::from_secs(60);

/// The Slack app to make (Slack → Your apps → Create New App → From a
/// manifest). Reading channels finds one by name; joining posts to a
/// public one without inviting the app first.
pub const APP_MANIFEST: &str = r#"display_information:
  name: Felix Meetings
  description: Posts your meeting notes from Felix.
features:
  bot_user:
    display_name: Felix Meetings
    always_online: false
oauth_config:
  scopes:
    bot:
      - files:write
      - chat:write
      - channels:read
      - groups:read
      - channels:join
settings:
  org_deploy_enabled: false
  socket_mode_enabled: false
  token_rotation_enabled: false
"#;

fn item_line(a: &ActionItem, with_owner: bool) -> String {
    let mut line = String::from("• ");
    if with_owner {
        line.push_str(&format!("{}: ", a.owner));
    }
    line.push_str(&a.task);
    if !a.due.is_empty() {
        line.push_str(&format!(" (due {})", a.due));
    }
    if a.tentative {
        line.push_str(" (tentative)");
    }
    if let Some(ms) = a.at_ms {
        line.push_str(&format!(" [{}]", timestamp(ms)));
    }
    line
}

/// Whose notes these are, so the agent reading them knows they're the
/// user's: "Kaspar's", from the first word of their name.
fn whose(user: Option<&str>) -> String {
    match user.and_then(|n| n.split_whitespace().next()) {
        Some(first) => format!("{first}'s"),
        None => "My".to_string(),
    }
}

/// The message the files come with.
pub fn message(user: Option<&str>, title: &str, meta: &str) -> String {
    format!(
        "{} meeting notes from Felix: {title}\n{meta}\nSummary and transcript attached.",
        whose(user)
    )
}

/// The webhook's message: the whole of the notes, in the order Felix
/// shows them.
pub fn webhook_text(user: Option<&str>, title: &str, meta: &str, s: &Summary) -> String {
    let mut out = format!(
        "{} meeting notes from Felix: {title}\n{meta}\n",
        whose(user)
    );
    if !s.overview.is_empty() {
        out.push_str(&format!("\n{}\n", s.overview));
    }
    let mut section = |heading: &str, lines: Vec<String>| {
        if !lines.is_empty() {
            out.push_str(&format!("\n{heading}\n{}\n", lines.join("\n")));
        }
    };
    let bullets = |v: &[String]| v.iter().map(|p| format!("• {p}")).collect();
    section("Key points:", bullets(&s.key_points));
    section("Decisions:", bullets(&s.decisions));
    let items = |pick: &dyn Fn(&ActionItem) -> bool, with_owner: bool| {
        s.action_items
            .iter()
            .filter(|a| pick(a))
            .map(|a| item_line(a, with_owner))
            .collect::<Vec<_>>()
    };
    section("My action items:", items(&|a| a.owner == "Me", false));
    section(
        "Others' action items:",
        items(&|a| !a.owner.is_empty() && a.owner != "Me", true),
    );
    section("No owner yet:", items(&|a| a.owner.is_empty(), false));
    if s.action_items.is_empty() {
        section("Action items:", vec!["• None".into()]);
    }
    out.trim_end().to_string()
}

/// The attachments' names: "2026-10-08 Revenue sync - summary.md".
fn file_name(id: &str, title: &str, what: &str) -> String {
    let title: String = title
        .chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) { '-' } else { c })
        .collect();
    format!(
        "{} {} - {what}.md",
        id.get(..10).unwrap_or(id),
        title.trim()
    )
}

/// A channel id as Slack gives them ("C0123ABCD"), not a name.
fn is_channel_id(s: &str) -> bool {
    s.len() >= 9
        && matches!(s.as_bytes()[0], b'C' | b'G')
        && s.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// What a failed Slack call means for the user.
fn explain(method: &str, error: &str) -> String {
    match error {
        "invalid_auth" | "not_authed" | "token_revoked" | "account_inactive" => {
            "Slack didn't accept the bot token; copy it again from the app's OAuth & Permissions page".into()
        }
        "missing_scope" => {
            "The Slack app is missing a permission; make it again from Felix's manifest and reinstall it".into()
        }
        "not_in_channel" | "channel_not_found" => {
            "The Slack app isn't in that channel; in Slack, type /invite @Felix Meetings there".into()
        }
        _ => format!("Slack's {method} said {error}"),
    }
}

struct Slack {
    client: reqwest::Client,
    token: String,
}

impl Slack {
    async fn call(&self, method: &str, request: reqwest::RequestBuilder) -> Result<Value, String> {
        let response = request
            .bearer_auth(&self.token)
            .timeout(TIMEOUT)
            .send()
            .await
            .map_err(|e| format!("Couldn't reach Slack: {e}"))?;
        let v: Value = response
            .json()
            .await
            .map_err(|e| format!("Slack's {method} answered oddly: {e}"))?;
        if v["ok"].as_bool() == Some(true) {
            Ok(v)
        } else {
            Err(explain(
                method,
                v["error"].as_str().unwrap_or("unknown_error"),
            ))
        }
    }

    async fn get(&self, method: &str, query: &[(&str, &str)]) -> Result<Value, String> {
        let request = self.client.get(format!("{API}/{method}")).query(query);
        self.call(method, request).await
    }

    async fn post_form(&self, method: &str, form: &[(&str, &str)]) -> Result<Value, String> {
        let request = self.client.post(format!("{API}/{method}")).form(form);
        self.call(method, request).await
    }

    /// The channel's id, looking a name up among the channels the app can
    /// see.
    async fn channel_id(&self, channel: &str) -> Result<String, String> {
        let channel = channel.trim().trim_start_matches('#');
        if is_channel_id(channel) {
            return Ok(channel.to_string());
        }
        let mut cursor = String::new();
        loop {
            let page = self
                .get(
                    "conversations.list",
                    &[
                        ("types", "public_channel,private_channel"),
                        ("exclude_archived", "true"),
                        ("limit", "1000"),
                        ("cursor", &cursor),
                    ],
                )
                .await?;
            let found = page["channels"].as_array().and_then(|list| {
                list.iter()
                    .find(|c| c["name"].as_str() == Some(channel))
                    .and_then(|c| c["id"].as_str())
            });
            if let Some(id) = found {
                return Ok(id.to_string());
            }
            cursor = page["response_metadata"]["next_cursor"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            if cursor.is_empty() {
                return Err(format!(
                    "No Slack channel called #{channel}; for a private one, invite the app first (/invite @Felix Meetings)"
                ));
            }
        }
    }

    /// Upload one file; its id, for sharing.
    async fn upload(&self, name: &str, text: &str) -> Result<String, String> {
        let length = text.len().to_string();
        let v = self
            .post_form(
                "files.getUploadURLExternal",
                &[("filename", name), ("length", &length)],
            )
            .await?;
        let (Some(url), Some(id)) = (v["upload_url"].as_str(), v["file_id"].as_str()) else {
            return Err("Slack gave no upload link".into());
        };
        let response = self
            .client
            .post(url)
            .timeout(TIMEOUT)
            .body(text.to_string())
            .send()
            .await
            .map_err(|e| format!("Couldn't upload {name} to Slack: {e}"))?;
        if !response.status().is_success() {
            return Err(format!("Slack didn't take {name} ({})", response.status()));
        }
        Ok(id.to_string())
    }
}

fn webhook(app: &AppHandle) -> Option<String> {
    let url = crate::settings::get_settings(app).slack_webhook;
    let url = url.trim();
    url.starts_with(WEBHOOK_PREFIX).then(|| url.to_string())
}

/// Through a Workflow Builder webhook: one text variable, `text`.
async fn send_to_webhook(url: &str, text: &str) -> Result<(), String> {
    let response = reqwest::Client::new()
        .post(url)
        .timeout(std::time::Duration::from_secs(20))
        .json(&json!({ "text": text }))
        .send()
        .await
        .map_err(|e| format!("Couldn't reach Slack: {e}"))?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().await.unwrap_or_default();
    Err(format!("Slack said {status}: {}", body.trim()))
}

/// Whether the Slack app is set up (it's used over the webhook when it is).
fn app_ready(settings: &crate::settings::AppSettings) -> bool {
    !settings.slack_token.trim().is_empty() && !settings.slack_channel.trim().is_empty()
}

async fn send(app: &AppHandle, id: &str) -> Result<(), String> {
    let settings = crate::settings::get_settings(app);
    let webhook = webhook(app);
    if !app_ready(&settings) && webhook.is_none() {
        return Err(
            "Add the Slack app's bot token and a channel (or a workflow link) in the meeting settings"
                .into(),
        );
    }
    let dir = super::manager::meeting_dir(app, id)?;
    let info: MeetingInfo = read_info(&dir).ok_or("The meeting isn't there any more")?;
    let notes: Summary =
        summary::load_json(&dir, summary::SUMMARY_FILE).ok_or("The meeting has no notes yet")?;
    let title = info.title.clone().unwrap_or_else(|| notes.title.clone());
    let user = crate::rules::user_name(&settings);
    let meta = meta_line(&info);
    let result = match webhook.filter(|_| !app_ready(&settings)) {
        Some(url) => {
            send_to_webhook(&url, &webhook_text(user.as_deref(), &title, &meta, &notes)).await
        }
        None => {
            let text = message(user.as_deref(), &title, &meta);
            let summary_md = super::manager::markdown(&dir, true, false)?;
            let transcript_md = super::manager::markdown(&dir, false, true)?;
            let token = settings.slack_token.trim().to_string();
            let channel = settings.slack_channel.trim().to_string();
            send_files(
                token,
                &channel,
                id,
                &title,
                &text,
                &summary_md,
                &transcript_md,
            )
            .await
        }
    };
    let manager = app.state::<std::sync::Arc<super::MeetingManager>>();
    manager.update_info(id, |i| match &result {
        Ok(()) => {
            i.slack_sent_at = Some(chrono::Utc::now().timestamp_millis());
            i.slack_error = None;
        }
        Err(e) => i.slack_error = Some(e.clone()),
    });
    let _ = app.emit("meetings-changed", ());
    result
}

/// Through the Slack app: the message with the summary and transcript.
async fn send_files(
    token: String,
    channel: &str,
    id: &str,
    title: &str,
    text: &str,
    summary_md: &str,
    transcript_md: &str,
) -> Result<(), String> {
    {
        let slack = Slack {
            client: reqwest::Client::new(),
            token,
        };
        let channel = slack.channel_id(channel).await?;
        // Joining a public channel saves inviting the app; a private one
        // says no, and the app has to be invited instead.
        let _ = slack
            .post_form("conversations.join", &[("channel", &channel)])
            .await;
        let mut files = Vec::new();
        for (what, md) in [("summary", summary_md), ("transcript", transcript_md)] {
            let name = file_name(id, title, what);
            let file = slack.upload(&name, md).await?;
            files.push(json!({ "id": file, "title": name }));
        }
        let request = slack
            .client
            .post(format!("{API}/files.completeUploadExternal"))
            .json(&json!({
                "files": files,
                "channel_id": channel,
                "initial_comment": text,
            }));
        slack.call("files.completeUploadExternal", request).await?;
        Ok(())
    }
}

/// After the notes are written, if sending is on.
pub fn send_in_background(app: &AppHandle, id: &str) {
    let settings = crate::settings::get_settings(app);
    if !settings.slack_send || (!app_ready(&settings) && webhook(app).is_none()) {
        return;
    }
    let (app, id) = (app.clone(), id.to_string());
    tauri::async_runtime::spawn(async move {
        if let Err(e) = send(&app, &id).await {
            log::warn!("Meeting {id} couldn't be sent to Slack: {e}");
        }
    });
}

/// Send (or send again) a meeting's notes to Slack.
#[tauri::command]
#[specta::specta]
pub async fn send_meeting_to_slack(app: AppHandle, id: String) -> Result<(), String> {
    send(&app, &id).await
}

/// The manifest to make the Slack app from.
#[tauri::command]
#[specta::specta]
pub fn slack_app_manifest() -> String {
    APP_MANIFEST.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(task: &str, owner: &str, due: &str) -> ActionItem {
        ActionItem {
            task: task.into(),
            owner: owner.into(),
            due: due.into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_message_is_the_users_whole_notes_under_their_name() {
        let s = Summary {
            overview: "Agreed the revenue model.".into(),
            key_points: vec!["Three revenue drivers".into()],
            action_items: vec![
                item("Send Sam the deck", "Sam Rivera", ""),
                ActionItem {
                    at_ms: Some(65_000),
                    tentative: true,
                    ..item("Update the feeder model", "Me", "by Friday")
                },
                item("Book a follow-up", "", ""),
            ],
            ..Default::default()
        };
        let text = webhook_text(Some("Sam Rivera"), "Finance sync", "Wed 7 Oct · 14:00", &s);
        assert_eq!(
            text,
            "Sam's meeting notes from Felix: Finance sync\nWed 7 Oct · 14:00\n\nAgreed the revenue model.\n\n\
Key points:\n• Three revenue drivers\n\n\
My action items:\n• Update the feeder model (due by Friday) (tentative) [1:05]\n\n\
Others' action items:\n• Sam Rivera: Send Sam the deck\n\n\
No owner yet:\n• Book a follow-up"
        );
    }

    #[test]
    fn a_meeting_without_action_items_says_so() {
        let text = webhook_text(None, "Chat", "Today", &Summary::default());
        assert!(text.starts_with("My meeting notes from Felix: Chat"));
        assert!(text.ends_with("Action items:\n• None"));
    }

    #[test]
    fn the_message_says_whose_notes_they_are() {
        assert_eq!(
            message(Some("Sam Rivera"), "Finance sync", "Wed 7 Oct · 14:00"),
            "Sam's meeting notes from Felix: Finance sync\nWed 7 Oct · 14:00\nSummary and transcript attached."
        );
        assert!(message(None, "Chat", "Today").starts_with("My meeting notes from Felix: Chat"));
    }

    #[test]
    fn files_are_named_after_the_day_and_title() {
        assert_eq!(
            file_name("2026-10-08_14-00-24", "Revenue: Q4/Q1", "summary"),
            "2026-10-08 Revenue- Q4-Q1 - summary.md"
        );
    }

    #[test]
    fn channels_are_told_from_names() {
        assert!(is_channel_id("C0123ABCD"));
        assert!(is_channel_id("G01ABCDEFGH"));
        assert!(!is_channel_id("meeting-notes"));
        assert!(!is_channel_id("Cats"));
    }

    #[test]
    fn the_manifest_asks_for_what_sending_needs() {
        for scope in [
            "files:write",
            "channels:read",
            "groups:read",
            "channels:join",
        ] {
            assert!(APP_MANIFEST.contains(scope), "{scope}");
        }
        assert!(explain("x", "not_in_channel").contains("/invite"));
    }
}
