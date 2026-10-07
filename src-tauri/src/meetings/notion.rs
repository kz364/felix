//! Saving meetings to Notion, in a "Felix meetings" database under a page
//! the user picked. That page is private to them (a page made under a
//! private page is private too), so a meeting is only ever seen by others
//! when the user shares it ([`share`]). One page per meeting; summarising
//! again rewrites the same page.
//!
//! The integration token is a secret: it only goes in the
//! `Authorization` header and never in logs or error messages.

use super::manager::{read_info, MeetingInfo, MeetingManager};
use super::summary::{self, ActionItem, Summary};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;
use tauri::{AppHandle, Manager};

const API: &str = "https://api.notion.com/v1";
/// What databases and pages are written with.
const VERSION: &str = "2022-06-28";
/// Moving a page needs a newer version than the rest uses.
const MOVE_VERSION: &str = "2026-03-11";
/// Most blocks Notion takes in one request.
const MAX_CHILDREN: usize = 100;
/// Longest text in one piece of rich text (UTF-16 units).
const MAX_TEXT: usize = 2000;
/// Longest name of a multi-select option.
const MAX_TAG: usize = 100;
/// Notion allows about three requests a second.
const PACE: Duration = Duration::from_millis(350);
const DATABASE_TITLE: &str = "Felix meetings";
/// Where the database's id is kept, so it's made once.
const STATE_FILE: &str = "notion.json";

/// The database Felix made, and the page it's under.
#[derive(Debug, Default, Serialize, Deserialize)]
struct State {
    parent_id: String,
    database_id: String,
}

// --- Building what Notion is sent -------------------------------------

/// The 32 hex digits of a Notion page link or id, written with dashes.
/// Links end in the id ("…/Team-notes-1429989fe8ac4effbc8f57f56486db54?pvs=4").
pub fn parse_id(input: &str) -> Option<String> {
    let path = input.trim().split(['?', '#']).next()?;
    let last = path.trim_end_matches('/').rsplit('/').next()?;
    let digits: String = last.chars().filter(|c| *c != '-').collect();
    let id = digits.get(digits.len().checked_sub(32)?..)?;
    if !id.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let id = id.to_lowercase();
    Some(format!(
        "{}-{}-{}-{}-{}",
        &id[..8],
        &id[8..12],
        &id[12..16],
        &id[16..20],
        &id[20..]
    ))
}

/// Text as rich text, in pieces Notion accepts.
pub fn rich_text(text: &str) -> Vec<Value> {
    let mut pieces = Vec::new();
    let mut current = String::new();
    let mut units = 0;
    for c in text.chars() {
        if units + c.len_utf16() > MAX_TEXT {
            pieces.push(std::mem::take(&mut current));
            units = 0;
        }
        current.push(c);
        units += c.len_utf16();
    }
    if !current.is_empty() {
        pieces.push(current);
    }
    pieces
        .into_iter()
        .map(|p| json!({ "type": "text", "text": { "content": p } }))
        .collect()
}

fn block(kind: &str, text: &str) -> Value {
    json!({ "object": "block", "type": kind, kind: { "rich_text": rich_text(text) } })
}

fn to_do(text: &str) -> Value {
    json!({
        "object": "block",
        "type": "to_do",
        "to_do": { "rich_text": rich_text(text), "checked": false },
    })
}

/// An action item as one line: others' items start with their owner.
fn item_line(a: &ActionItem, with_owner: bool) -> String {
    let mut line = String::new();
    if with_owner {
        line.push_str(a.owner.trim());
        line.push_str(": ");
    }
    line.push_str(a.task.trim());
    if !a.due.trim().is_empty() {
        line.push_str(&format!(" (due {})", a.due.trim()));
    }
    if a.tentative {
        line.push_str(" (tentative)");
    }
    line
}

/// The action items under their headings: the user's ("Me"), other
/// people's, and ones nobody took on. Empty groups are left out.
pub fn action_groups(items: &[ActionItem]) -> Vec<(&'static str, Vec<String>)> {
    let mine = |a: &ActionItem| a.owner.trim().eq_ignore_ascii_case("me");
    let mut groups = vec![
        (
            "Your action items",
            items
                .iter()
                .filter(|a| mine(a))
                .map(|a| item_line(a, false))
                .collect::<Vec<_>>(),
        ),
        (
            "Others' action items",
            items
                .iter()
                .filter(|a| !mine(a) && !a.owner.trim().is_empty())
                .map(|a| item_line(a, true))
                .collect(),
        ),
        (
            "No owner yet",
            items
                .iter()
                .filter(|a| a.owner.trim().is_empty())
                .map(|a| item_line(a, false))
                .collect(),
        ),
    ];
    groups.retain(|(_, lines)| !lines.is_empty());
    groups
}

/// The page's body but for the transcript: overview, key points,
/// decisions and action items.
pub fn summary_blocks(summary: &Summary) -> Vec<Value> {
    let mut out = Vec::new();
    if !summary.overview.trim().is_empty() {
        out.push(block("paragraph", summary.overview.trim()));
    }
    for (heading, list) in [
        ("Key points", &summary.key_points),
        ("Decisions", &summary.decisions),
    ] {
        if list.iter().all(|i| i.trim().is_empty()) {
            continue;
        }
        out.push(block("heading_2", heading));
        out.extend(
            list.iter()
                .filter(|i| !i.trim().is_empty())
                .map(|i| block("bulleted_list_item", i.trim())),
        );
    }
    let groups = action_groups(&summary.action_items);
    if !groups.is_empty() {
        out.push(block("heading_2", "Action items"));
        for (heading, lines) in groups {
            out.push(block("heading_3", heading));
            out.extend(lines.iter().map(|l| to_do(l)));
        }
    }
    out
}

/// The "Transcript" toggle holding the first lines (a request takes
/// only so many blocks), and the paragraphs for the rest, to be added to
/// the toggle afterwards.
pub fn transcript_toggle(lines: &[String]) -> (Value, Vec<Value>) {
    let mut paragraphs: Vec<Value> = lines.iter().map(|l| block("paragraph", l)).collect();
    let rest = paragraphs.split_off(paragraphs.len().min(MAX_CHILDREN));
    let toggle = json!({
        "object": "block",
        "type": "toggle",
        "toggle": { "rich_text": rich_text("Transcript"), "children": paragraphs },
    });
    (toggle, rest)
}

/// A name as a multi-select option: those can't hold commas.
fn tag(name: &str) -> String {
    name.replace(',', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_TAG)
        .collect()
}

/// The page's columns in the "Felix meetings" database.
pub fn page_properties(title: &str, start: &str, people: &[String], shared: bool) -> Value {
    let people: Vec<Value> = people
        .iter()
        .map(|p| tag(p))
        .filter(|p| !p.is_empty())
        .map(|p| json!({ "name": p }))
        .collect();
    json!({
        "Name": { "title": rich_text(title) },
        "Date": { "date": { "start": start } },
        "People": { "multi_select": people },
        "Shared": { "checkbox": shared },
    })
}

fn database_body(parent_id: &str) -> Value {
    json!({
        "parent": { "type": "page_id", "page_id": parent_id },
        "title": rich_text(DATABASE_TITLE),
        "properties": {
            "Name": { "title": {} },
            "Date": { "date": {} },
            "People": { "multi_select": {} },
            "Shared": { "checkbox": {} },
        },
    })
}

// --- Talking to Notion ------------------------------------------------

/// What Notion answered when it refused.
#[derive(Debug)]
struct ApiError {
    status: u16,
    code: String,
    message: String,
}

impl ApiError {
    fn plain(message: impl Into<String>) -> Self {
        Self {
            status: 0,
            code: String::new(),
            message: message.into(),
        }
    }

    /// The page or database isn't there, or is in the bin.
    fn gone(&self) -> bool {
        self.code == "object_not_found"
            || (self.code == "validation_error" && self.message.contains("archived"))
    }

    /// In words the user can act on.
    fn describe(&self) -> String {
        match (self.status, self.code.as_str()) {
            (401, _) => "Notion didn't accept the token. Paste the integration's secret again.".into(),
            (404, _) | (_, "object_not_found") => "Notion can't find that page. Connect your integration to it: open the page, then ⋯ menu → Connections.".into(),
            (403, _) => "The integration isn't allowed to do that. Connect it to the page (⋯ menu → Connections) and give it permission to edit.".into(),
            (0, _) => self.message.clone(),
            _ => format!("Notion said: {}", self.message),
        }
    }
}

fn http() -> &'static reqwest::Client {
    static CLIENT: once_cell::sync::Lazy<reqwest::Client> = once_cell::sync::Lazy::new(|| {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .build()
            .unwrap_or_default()
    });
    &CLIENT
}

struct Notion {
    token: String,
}

impl Notion {
    async fn call(
        &self,
        method: reqwest::Method,
        path: &str,
        version: &str,
        body: Option<&Value>,
    ) -> Result<Value, ApiError> {
        for attempt in 0..4 {
            let mut request = http()
                .request(method.clone(), format!("{API}{path}"))
                .bearer_auth(&self.token)
                .header("Notion-Version", version);
            if let Some(b) = body {
                request = request.json(b);
            }
            // The error is the network's, which never holds the token.
            let response = request.send().await.map_err(|e| {
                ApiError::plain(format!("Couldn't reach Notion: {}", e.without_url()))
            })?;
            let status = response.status();
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let value: Value = response.json().await.unwrap_or(Value::Null);
            tokio::time::sleep(PACE).await;
            if status.is_success() {
                return Ok(value);
            }
            if status.as_u16() == 429 && attempt < 3 {
                tokio::time::sleep(Duration::from_secs(retry_after.unwrap_or(2).min(30))).await;
                continue;
            }
            return Err(ApiError {
                status: status.as_u16(),
                code: value["code"].as_str().unwrap_or_default().to_string(),
                message: value["message"].as_str().unwrap_or_default().to_string(),
            });
        }
        Err(ApiError::plain("Notion is busy; try again in a minute"))
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value, ApiError> {
        self.call(reqwest::Method::POST, path, VERSION, Some(body))
            .await
    }

    async fn patch(&self, path: &str, body: &Value) -> Result<Value, ApiError> {
        self.call(reqwest::Method::PATCH, path, VERSION, Some(body))
            .await
    }

    /// Add blocks at the end of a page or block.
    async fn append(&self, id: &str, blocks: &[Value]) -> Result<Value, ApiError> {
        self.patch(
            &format!("/blocks/{id}/children"),
            &json!({ "children": blocks }),
        )
        .await
    }

    /// Everything under a page, rewritten: the summary, then the
    /// transcript in its toggle.
    async fn write_body(
        &self,
        page_id: &str,
        sections: &[Value],
        lines: &[String],
    ) -> Result<(), ApiError> {
        for part in sections.chunks(MAX_CHILDREN) {
            self.append(page_id, part).await?;
        }
        if lines.is_empty() {
            return Ok(());
        }
        let (toggle, rest) = transcript_toggle(lines);
        let added = self.append(page_id, &[toggle]).await?;
        if let Some(toggle_id) = added["results"][0]["id"].as_str() {
            for part in rest.chunks(MAX_CHILDREN) {
                self.append(toggle_id, part).await?;
            }
        }
        Ok(())
    }

    /// Remove what's on a page, to write it again.
    async fn clear(&self, page_id: &str) -> Result<(), ApiError> {
        let mut cursor: Option<String> = None;
        loop {
            let mut path = format!("/blocks/{page_id}/children?page_size=100");
            if let Some(c) = &cursor {
                path.push_str(&format!("&start_cursor={c}"));
            }
            let page = self
                .call(reqwest::Method::GET, &path, VERSION, None)
                .await?;
            for b in page["results"].as_array().into_iter().flatten() {
                if let Some(id) = b["id"].as_str() {
                    self.call(
                        reqwest::Method::DELETE,
                        &format!("/blocks/{id}"),
                        VERSION,
                        None,
                    )
                    .await?;
                }
            }
            match page["next_cursor"].as_str() {
                Some(c) if page["has_more"].as_bool() == Some(true) => cursor = Some(c.to_string()),
                _ => return Ok(()),
            }
        }
    }
}

/// One sync or share at a time: Notion's rate limit is shared, and the
/// database must be made only once.
static BUSY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn state_path(app: &AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(crate::portable::app_data_dir(app)
        .map_err(|e| e.to_string())?
        .join(STATE_FILE))
}

fn load_state(app: &AppHandle) -> Option<State> {
    serde_json::from_slice(&std::fs::read(state_path(app).ok()?).ok()?).ok()
}

fn save_state(app: &AppHandle, state: &State) {
    let Ok(path) = state_path(app) else { return };
    if let Ok(json) = serde_json::to_vec_pretty(state) {
        if let Err(e) = std::fs::write(&path, json) {
            log::warn!("Couldn't save {}: {e}", path.display());
        }
    }
}

/// The "Felix meetings" database under `parent_id`, made the first time.
async fn ensure_database(
    app: &AppHandle,
    nt: &Notion,
    parent_id: &str,
    fresh: bool,
) -> Result<String, ApiError> {
    if let Some(s) = load_state(app).filter(|s| !fresh && s.parent_id == parent_id) {
        return Ok(s.database_id);
    }
    let made = nt.post("/databases", &database_body(parent_id)).await?;
    let database_id = made["id"]
        .as_str()
        .ok_or_else(|| ApiError::plain("Notion didn't say where it put the database"))?
        .to_string();
    log::info!("Made the \"{DATABASE_TITLE}\" database in Notion");
    save_state(
        app,
        &State {
            parent_id: parent_id.to_string(),
            database_id: database_id.clone(),
        },
    );
    Ok(database_id)
}

fn settings_for_sync(app: &AppHandle) -> Result<(Notion, String), String> {
    let s = crate::settings::get_settings(app);
    if s.notion_token.trim().is_empty() {
        return Err("Paste your Notion integration token in the meeting settings".into());
    }
    let parent = parse_id(&s.notion_parent)
        .ok_or("Paste the link of the private Notion page to save meetings under")?;
    Ok((
        Notion {
            token: s.notion_token.trim().to_string(),
        },
        parent,
    ))
}

/// What a meeting's page is made of.
struct Content {
    title: String,
    start: String,
    people: Vec<String>,
    sections: Vec<Value>,
    lines: Vec<String>,
}

fn content_of(
    app: &AppHandle,
    info: &MeetingInfo,
    dir: &std::path::Path,
) -> Result<Content, String> {
    let summary: Summary = summary::load_json(dir, summary::SUMMARY_FILE)
        .ok_or("This meeting has no summary to save yet")?;
    let paragraphs = super::pipeline::load(dir)
        .map(|t| super::manager::paragraphs_of(dir, &t))
        .unwrap_or_default();
    let me = crate::rules::user_name(&crate::settings::get_settings(app));
    let people = info
        .participants(&paragraphs)
        .into_iter()
        .map(|p| match (&me, p.as_str()) {
            (Some(me), "Me") => me.clone(),
            _ => p,
        })
        .collect();
    let title = if info.title.as_deref().is_some_and(|t| !t.trim().is_empty()) {
        info.display_title()
    } else if !summary.title.trim().is_empty() {
        summary.title.clone()
    } else {
        info.display_title()
    };
    let start = chrono::DateTime::from_timestamp_millis(info.started_at)
        .unwrap_or_default()
        .with_timezone(&chrono::Local)
        .to_rfc3339();
    Ok(Content {
        title,
        start,
        people,
        sections: summary_blocks(&summary),
        lines: super::jobs::transcript_lines(info, &paragraphs),
    })
}

/// A page's id and link from what Notion returned.
fn page_ref(page: &Value) -> Option<(String, String)> {
    Some((
        page["id"].as_str()?.to_string(),
        page["url"].as_str().unwrap_or_default().to_string(),
    ))
}

async fn sync_inner(app: &AppHandle, id: &str) -> Result<(), String> {
    let (nt, parent) = settings_for_sync(app)?;
    let dir = super::manager::meeting_dir(app, id)?;
    let info = read_info(&dir).ok_or("The meeting isn't there any more")?;
    let content = content_of(app, &info, &dir)?;
    let _turn = BUSY.lock().await;
    let ApiOutcome { page_id, url } = write_page(app, &nt, &parent, &info, &content)
        .await
        .map_err(|e| e.describe())?;
    if let Some(m) = app.try_state::<Arc<MeetingManager>>() {
        m.update_info(id, |i| {
            i.notion_page_id = Some(page_id);
            i.notion_url = Some(url).filter(|u| !u.is_empty());
            i.notion_error = None;
        });
    }
    log::info!("Meeting {id} saved to Notion");
    Ok(())
}

struct ApiOutcome {
    page_id: String,
    url: String,
}

/// Make the meeting's page, or rewrite the one it has.
async fn write_page(
    app: &AppHandle,
    nt: &Notion,
    parent: &str,
    info: &MeetingInfo,
    c: &Content,
) -> Result<ApiOutcome, ApiError> {
    if let Some(page_id) = &info.notion_page_id {
        // A page moved to the team's page has other columns; only its body
        // is written again.
        let updated = if info.notion_shared {
            Ok(Value::Null)
        } else {
            nt.patch(
                &format!("/pages/{page_id}"),
                &json!({ "properties": page_properties(&c.title, &c.start, &c.people, false) }),
            )
            .await
        };
        match updated {
            Ok(page) => {
                nt.clear(page_id).await?;
                nt.write_body(page_id, &c.sections, &c.lines).await?;
                let url = page_ref(&page)
                    .map(|(_, u)| u)
                    .unwrap_or_else(|| info.notion_url.clone().unwrap_or_default());
                return Ok(ApiOutcome {
                    page_id: page_id.clone(),
                    url,
                });
            }
            // Deleted in Notion since: make it again.
            Err(e) if e.gone() => log::info!("The Notion page is gone; making it again"),
            Err(e) => return Err(e),
        }
    }
    let create = |database_id: String| async move {
        nt.post(
            "/pages",
            &json!({
                "parent": { "database_id": database_id },
                "properties": page_properties(&c.title, &c.start, &c.people, false),
            }),
        )
        .await
    };
    let database_id = ensure_database(app, nt, parent, false).await?;
    let page = match create(database_id).await {
        // The database was deleted in Notion: make it again.
        Err(e) if e.gone() => {
            let fresh = ensure_database(app, nt, parent, true).await?;
            create(fresh).await?
        }
        other => other?,
    };
    let (page_id, url) = page_ref(&page)
        .ok_or_else(|| ApiError::plain("Notion didn't say where it put the page"))?;
    nt.write_body(&page_id, &c.sections, &c.lines).await?;
    Ok(ApiOutcome { page_id, url })
}

/// Save a summarised meeting to Notion, and say why not on the meeting
/// when it fails.
pub async fn sync(app: &AppHandle, id: &str) -> Result<(), String> {
    let result = sync_inner(app, id).await;
    if let Err(e) = &result {
        if let Some(m) = app.try_state::<Arc<MeetingManager>>() {
            m.update_info(id, |i| i.notion_error = Some(e.clone()));
        }
    }
    result
}

/// After a summary is saved: save the meeting too, if the user turned that
/// on and gave Notion what it needs. Failures only go to the log and the
/// meeting's page.
pub fn sync_in_background(app: &AppHandle, id: &str) {
    let s = crate::settings::get_settings(app);
    if !s.notion_sync || s.notion_token.trim().is_empty() || parse_id(&s.notion_parent).is_none() {
        return;
    }
    let (app, id) = (app.clone(), id.to_string());
    tauri::async_runtime::spawn(async move {
        if let Err(e) = sync(&app, &id).await {
            log::warn!("Meeting {id} couldn't be saved to Notion: {e}");
        }
    });
}

async fn move_page(nt: &Notion, page_id: &str, parent: Value) -> Result<Value, ApiError> {
    nt.call(
        reqwest::Method::POST,
        &format!("/pages/{page_id}/move"),
        MOVE_VERSION,
        Some(&json!({ "parent": parent })),
    )
    .await
}

/// Move a meeting's page to the team's page and tick Shared.
///
/// Notion can't give particular people access to a page, so sharing is
/// moving it where the team already has access. The API has a move
/// (`POST /v1/pages/{id}/move`, from version 2026-03-11), so the page keeps
/// its id and link. A team page is the parent as a page; a team database
/// is moved into by its data source, found by asking for the database.
/// Shared is ticked first: a page that has moved no longer has that column.
async fn share_inner(app: &AppHandle, id: &str) -> Result<(), String> {
    let (nt, _) = settings_for_sync(app)?;
    let target = parse_id(&crate::settings::get_settings(app).notion_share_parent)
        .ok_or("Paste the link of the team's Notion page in the meeting settings")?;
    let dir = super::manager::meeting_dir(app, id)?;
    let info = read_info(&dir).ok_or("The meeting isn't there any more")?;
    let page_id = info
        .notion_page_id
        .clone()
        .ok_or("Save the meeting to Notion first")?;
    // Re-read, in case the invite was saved before addresses were kept.
    let end = info.ended_at.unwrap_or(info.started_at + 60 * 60 * 1000);
    let invite = super::calendar::for_meeting(&dir, info.started_at, end);
    if !super::calendar::can_share(invite.as_ref()) {
        return Err(
            "Only meetings where everyone invited is from your organisation can be shared".into(),
        );
    }
    let _turn = BUSY.lock().await;
    nt.patch(
        &format!("/pages/{page_id}"),
        &json!({ "properties": { "Shared": { "checkbox": true } } }),
    )
    .await
    .map_err(|e| e.describe())?;
    let moved = match move_page(
        &nt,
        &page_id,
        json!({ "type": "page_id", "page_id": target }),
    )
    .await
    {
        Ok(p) => p,
        Err(e) if e.status == 400 => {
            // Not a page: a database, moved into by its data source.
            let db = nt
                .call(
                    reqwest::Method::GET,
                    &format!("/databases/{target}"),
                    MOVE_VERSION,
                    None,
                )
                .await
                .map_err(|_| e.describe())?;
            let source = db["data_sources"][0]["id"]
                .as_str()
                .ok_or_else(|| e.describe())?
                .to_string();
            move_page(
                &nt,
                &page_id,
                json!({ "type": "data_source_id", "data_source_id": source }),
            )
            .await
            .map_err(|e| e.describe())?
        }
        Err(e) => return Err(e.describe()),
    };
    let url = page_ref(&moved).map(|(_, u)| u).filter(|u| !u.is_empty());
    if let Some(m) = app.try_state::<Arc<MeetingManager>>() {
        m.update_info(id, |i| {
            i.notion_shared = true;
            i.notion_error = None;
            if url.is_some() {
                i.notion_url = url;
            }
        });
    }
    log::info!("Meeting {id} moved to the team's Notion page");
    Ok(())
}

pub async fn share(app: &AppHandle, id: &str) -> Result<(), String> {
    let result = share_inner(app, id).await;
    if let Err(e) = &result {
        if let Some(m) = app.try_state::<Arc<MeetingManager>>() {
            m.update_info(id, |i| i.notion_error = Some(e.clone()));
        }
    }
    result
}

/// Save (or save again) a meeting's notes to Notion: for a first upload of
/// an older meeting, or to retry after an error.
#[tauri::command]
#[specta::specta]
pub async fn sync_meeting_to_notion(app: AppHandle, id: String) -> Result<(), String> {
    sync(&app, &id).await
}

/// Move a meeting to the team's Notion page, so the people who were in it
/// can see it. Only meetings with an invite of colleagues only.
#[tauri::command]
#[specta::specta]
pub async fn share_meeting_in_notion(app: AppHandle, id: String) -> Result<(), String> {
    share(&app, &id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(task: &str, owner: &str, due: &str, tentative: bool) -> ActionItem {
        ActionItem {
            task: task.into(),
            owner: owner.into(),
            due: due.into(),
            tentative,
            ..Default::default()
        }
    }

    #[test]
    fn a_page_link_or_id_gives_the_32_digit_id() {
        let want = Some("1429989f-e8ac-4eff-bc8f-57f56486db54".to_string());
        assert_eq!(parse_id("1429989fe8ac4effbc8f57f56486db54"), want);
        assert_eq!(parse_id("1429989f-e8ac-4eff-bc8f-57f56486db54"), want);
        assert_eq!(
            parse_id(
                "https://www.notion.so/acme/Team-notes-1429989fe8ac4effbc8f57f56486db54?pvs=4"
            ),
            want
        );
        assert_eq!(
            parse_id("https://www.notion.so/1429989FE8AC4EFFBC8F57F56486DB54/"),
            want
        );
        assert_eq!(parse_id("https://www.notion.so/Team-notes"), None);
        assert_eq!(parse_id(""), None);
    }

    #[test]
    fn long_text_is_split_into_pieces_notion_takes() {
        let text = "a".repeat(4500);
        let pieces = rich_text(&text);
        let lens: Vec<usize> = pieces
            .iter()
            .map(|p| p["text"]["content"].as_str().unwrap().len())
            .collect();
        assert_eq!(lens, [2000, 2000, 500]);
        assert!(rich_text("").is_empty());
        // Counted the way Notion counts: an emoji is two units.
        let emoji = "😀".repeat(1001);
        let pieces = rich_text(&emoji);
        assert_eq!(pieces.len(), 2);
        assert_eq!(
            pieces[0]["text"]["content"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            1000
        );
    }

    #[test]
    fn action_items_are_grouped_by_who_has_them() {
        let items = [
            item("Send the deck", "Me", "Friday", false),
            item("Book the room", "Sam Rivera", "", true),
            item("Pick a date", "", "", false),
            item("Reply to Pat", "me", "", true),
        ];
        let groups = action_groups(&items);
        assert_eq!(
            groups,
            vec![
                (
                    "Your action items",
                    vec![
                        "Send the deck (due Friday)".to_string(),
                        "Reply to Pat (tentative)".to_string()
                    ]
                ),
                (
                    "Others' action items",
                    vec!["Sam Rivera: Book the room (tentative)".to_string()]
                ),
                ("No owner yet", vec!["Pick a date".to_string()]),
            ]
        );
        assert!(action_groups(&[]).is_empty());
    }

    #[test]
    fn the_summary_becomes_headed_lists_and_to_dos() {
        let summary = Summary {
            overview: "We agreed the launch.".into(),
            key_points: vec!["Launch in May".into()],
            decisions: vec![],
            action_items: vec![item("Send the deck", "Me", "", false)],
            ..Default::default()
        };
        let blocks = summary_blocks(&summary);
        let kinds: Vec<&str> = blocks.iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            [
                "paragraph",
                "heading_2",
                "bulleted_list_item",
                "heading_2",
                "heading_3",
                "to_do"
            ]
        );
    }

    #[test]
    fn a_long_transcript_fills_the_toggle_then_carries_on() {
        let lines: Vec<String> = (0..250).map(|i| format!("[0:{i:02}] Me: hi")).collect();
        let (toggle, rest) = transcript_toggle(&lines);
        assert_eq!(toggle["type"], "toggle");
        assert_eq!(toggle["toggle"]["children"].as_array().unwrap().len(), 100);
        assert_eq!(rest.len(), 150);
        assert_eq!(rest.chunks(MAX_CHILDREN).count(), 2);
        let (short, none) = transcript_toggle(&lines[..3]);
        assert_eq!(short["toggle"]["children"].as_array().unwrap().len(), 3);
        assert!(none.is_empty());
    }

    #[test]
    fn the_pages_columns_match_the_database() {
        let props = page_properties(
            "HPE call",
            "2026-10-07T10:00:00+02:00",
            &["Sam, Rivera".to_string(), "".to_string()],
            false,
        );
        assert_eq!(props["Name"]["title"][0]["text"]["content"], "HPE call");
        assert_eq!(props["Date"]["date"]["start"], "2026-10-07T10:00:00+02:00");
        assert_eq!(props["People"]["multi_select"].as_array().unwrap().len(), 1);
        assert_eq!(props["People"]["multi_select"][0]["name"], "Sam Rivera");
        assert_eq!(props["Shared"]["checkbox"], false);
        let db = database_body("1429989f-e8ac-4eff-bc8f-57f56486db54");
        for column in ["Name", "Date", "People", "Shared"] {
            assert!(db["properties"].get(column).is_some());
        }
    }

    #[test]
    fn refusals_are_put_in_words_without_the_token() {
        let e = ApiError {
            status: 404,
            code: "object_not_found".into(),
            message: "Could not find page".into(),
        };
        assert!(e.gone());
        assert!(e.describe().contains("Connections"));
        let e = ApiError {
            status: 401,
            code: "unauthorized".into(),
            message: "API token is invalid.".into(),
        };
        assert!(!e.gone());
        assert!(e.describe().contains("token"));
    }
}
