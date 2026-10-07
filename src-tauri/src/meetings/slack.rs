//! Meeting notes sent to Slack through a Workflow Builder webhook, for the
//! user's agent (or the user) to act on the action items. The workflow
//! takes one text variable, `text`, and posts it where the user chose (a DM
//! to their agent): no Slack app or admin needed.

use super::manager::{meta_line, read_info, MeetingInfo};
use super::summary::{self, ActionItem, Summary};
use super::transcript::timestamp;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

/// Workflow webhooks are on this host; anything else isn't one.
const WEBHOOK_PREFIX: &str = "https://hooks.slack.com/";

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

/// The message: the whole of the notes, in the order Felix shows them.
pub fn message(user: Option<&str>, title: &str, meta: &str, s: &Summary) -> String {
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

fn webhook(app: &AppHandle) -> Option<String> {
    let url = crate::settings::get_settings(app).slack_webhook;
    let url = url.trim();
    url.starts_with(WEBHOOK_PREFIX).then(|| url.to_string())
}

async fn send(app: &AppHandle, id: &str) -> Result<(), String> {
    let url =
        webhook(app).ok_or("Paste the Slack workflow's webhook link in the meeting settings")?;
    let dir = super::manager::meeting_dir(app, id)?;
    let info: MeetingInfo = read_info(&dir).ok_or("The meeting isn't there any more")?;
    let notes: Summary =
        summary::load_json(&dir, summary::SUMMARY_FILE).ok_or("The meeting has no notes yet")?;
    let title = info.title.clone().unwrap_or_else(|| notes.title.clone());
    let user = crate::rules::user_name(&crate::settings::get_settings(app));
    let text = message(user.as_deref(), &title, &meta_line(&info), &notes);
    let result = async {
        let response = reqwest::Client::new()
            .post(&url)
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
    .await;
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

/// After the notes are written, if sending is on.
pub fn send_in_background(app: &AppHandle, id: &str) {
    if !crate::settings::get_settings(app).slack_send || webhook(app).is_none() {
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
        let text = message(Some("Sam Rivera"), "Finance sync", "Wed 7 Oct · 14:00", &s);
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
        let text = message(None, "Chat", "Today", &Summary::default());
        assert!(text.starts_with("My meeting notes from Felix: Chat"));
        assert!(text.ends_with("Action items:\n• None"));
    }
}
