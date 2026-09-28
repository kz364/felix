//! The rules file (`rules.toml`) from the Vocabulary page: open it, have a
//! remote model propose a fix for a mistranscription, save the fix.

use crate::managers::history::HistoryManager;
use crate::rules::Proposal;
use crate::settings::get_settings;
use std::sync::Arc;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

#[tauri::command]
#[specta::specta]
pub fn rules_file_path() -> Option<String> {
    crate::rules::path().map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
#[specta::specta]
pub fn open_rules_file(app: AppHandle) -> Result<(), String> {
    let path = crate::rules::path().ok_or("The rules file isn't set up")?;
    app.opener()
        .open_path(path.to_string_lossy(), None::<String>)
        .map_err(|e| e.to_string())
}

/// The last few dictations, newest first, with their model and recording.
pub async fn recent_dictations(history: &HistoryManager) -> Vec<crate::rules::Recent> {
    history
        .get_history_entries(None, Some(5))
        .await
        .map(|page| {
            page.entries
                .into_iter()
                .map(|e| crate::rules::Recent {
                    pasted: e
                        .post_processed_text
                        .clone()
                        .unwrap_or_else(|| e.transcription_text.clone()),
                    transcribed: e.transcription_text,
                    model: e.transcription_model,
                    audio_path: e
                        .has_audio
                        .then(|| history.recordings_dir().join(&e.file_name)),
                    file_name: e.file_name,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// What came of a typed report: the proposal, and whether it was applied.
#[derive(serde::Serialize, specta::Type)]
pub struct ReportOutcome {
    pub proposal: Proposal,
    pub applied: bool,
}

/// Fix a typed report the way a spoken one is fixed: propose rules and, if
/// they're safe (they parse, change something and pass every test), apply
/// them straight away so the user can move on; Undo takes them back.
/// Anything else comes back unapplied for the user to look at. Runs to the
/// end even if the page that asked is closed.
#[tauri::command]
#[specta::specta]
pub async fn report_mistake(
    app: AppHandle,
    history_manager: State<'_, Arc<HistoryManager>>,
    report: String,
) -> Result<ReportOutcome, String> {
    if report.trim().is_empty() {
        return Err("Describe what came out wrong".into());
    }
    let recent = recent_dictations(&history_manager).await;
    let settings = get_settings(&app);
    let proposal = crate::rules::propose(&settings, &report, &recent, "typed").await?;
    let applied = crate::rules::safe_to_apply(&proposal)
        && match crate::rules::save(&proposal.rules) {
            Ok(()) => {
                crate::rules::log_applied(Some(&proposal.id), "applied");
                true
            }
            Err(e) => {
                log::warn!("Couldn't apply the reported fix: {e}");
                false
            }
        };
    if applied {
        let _ = tauri::Emitter::emit(&app, "rules-changed", ());
    }
    Ok(ReportOutcome { proposal, applied })
}

/// Save a new rules file; it's used from the next dictation.
#[tauri::command]
#[specta::specta]
pub fn save_rules(rules: String, report_id: Option<String>) -> Result<(), String> {
    crate::rules::save(&rules)?;
    crate::rules::log_applied(report_id.as_deref(), "applied");
    Ok(())
}

/// Go back to the rules file from before the last change.
#[tauri::command]
#[specta::specta]
pub fn undo_rules() -> Result<(), String> {
    crate::rules::undo()
}

/// What the rules file holds, for the Vocabulary page.
#[tauri::command]
#[specta::specta]
pub fn learned_rules() -> crate::rules::Learned {
    crate::rules::learned()
}

/// Save a copy of the rules for another app: `.json` gets JSON (with the
/// words taught by voice), anything else the TOML file as it is.
#[tauri::command]
#[specta::specta]
pub fn export_rules(app: AppHandle, path: String) -> Result<(), String> {
    let text = if path.to_lowercase().ends_with(".json") {
        crate::rules::export_json(&get_settings(&app))?
    } else {
        let file = crate::rules::path().ok_or("The rules file isn't set up")?;
        std::fs::read_to_string(file).map_err(|e| e.to_string())?
    };
    std::fs::write(&path, text).map_err(|e| e.to_string())
}

/// Add a word to the vocabulary, e.g. before teaching it.
#[tauri::command]
#[specta::specta]
pub fn add_vocabulary_word(app: AppHandle, word: String) -> Result<(), String> {
    crate::rules::add_word(&word)?;
    let _ = tauri::Emitter::emit(&app, "rules-changed", ());
    Ok(())
}

/// Remove one learned entry: `kind` is "word", "correction" or "soundalike".
#[tauri::command]
#[specta::specta]
pub fn forget_rule(kind: String, index: u32) -> Result<(), String> {
    crate::rules::forget(&kind, index as usize)
}

/// Past mistake reports, newest first.
#[tauri::command]
#[specta::specta]
pub fn mistake_reports() -> Vec<crate::rules::Report> {
    crate::rules::reports()
}

/// Delete a report and its audio (the rules it added stay).
#[tauri::command]
#[specta::specta]
pub fn forget_mistake_report(id: String) -> Result<(), String> {
    crate::rules::forget_report(&id)
}
