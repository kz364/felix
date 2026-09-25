//! The app side of meeting recording: one meeting at a time, started and
//! stopped from the Meetings page, the tray or a shortcut. Each meeting is a
//! folder under the app data dir (`meetings/<id>/`) with its tracks and a
//! `meeting.json`. While recording, the menu bar shows the elapsed time.
//! When a recording stops it's queued for transcription and then a summary,
//! which run in the background one meeting at a time (see [`super::jobs`]).
//! The user's own notes are kept next to it in `notes.md`.

use super::capture::{MeetingMode, Recording};
use super::jobs::Job;
use super::pipeline;
use super::summary::{self, Summary};
use super::track::TrackSummary;
use super::transcript::{self, Paragraph, Source};
use crate::managers::audio::AudioRecordingManager;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

const META_FILE: &str = "meeting.json";
pub(super) const NOTES_FILE: &str = "notes.md";

/// Where a background step (the summary) has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    Running,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptStatus {
    Queued,
    Transcribing,
    Done,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingStatus {
    Recording,
    Recorded,
    /// Handy quit or crashed while recording; the audio up to a few seconds
    /// before is kept.
    Interrupted,
}

/// A meeting's `meeting.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct MeetingInfo {
    pub id: String,
    pub mode: MeetingMode,
    pub status: MeetingStatus,
    /// Unix milliseconds.
    pub started_at: i64,
    pub ended_at: Option<i64>,
    pub mic: String,
    /// Why system audio wasn't recorded on a call.
    pub system_error: Option<String>,
    pub tracks: Vec<TrackSummary>,
    /// Set by the user; otherwise the page names it by mode and time.
    #[serde(default)]
    pub title: Option<String>,
    /// `None` for meetings recorded before transcription existed.
    #[serde(default)]
    pub transcript: Option<TranscriptStatus>,
    #[serde(default)]
    pub transcript_error: Option<String>,
    /// The title came from the summary, so a new summary may replace it.
    #[serde(default)]
    pub title_is_auto: bool,
    /// Summary and transcript cleanup.
    #[serde(default)]
    pub summary: Option<JobStatus>,
    #[serde(default)]
    pub summary_error: Option<String>,
    /// Names the user gave to told-apart voices (in person), by number.
    #[serde(default)]
    pub speakers: BTreeMap<u32, String>,
}

impl MeetingInfo {
    /// Who a paragraph is, for the summary and the Markdown: "Me"/"Them" on
    /// a call, the speaker's name or number in person, or nobody.
    pub fn speaker_label(&self, p: &Paragraph) -> Option<String> {
        match (self.mode, p.source, p.speaker) {
            (MeetingMode::Call, Source::Mic, _) => Some("Me".into()),
            (MeetingMode::Call, Source::System, _) => Some("Them".into()),
            (_, _, Some(n)) => Some(
                self.speakers
                    .get(&n)
                    .cloned()
                    .unwrap_or_else(|| format!("Speaker {}", n + 1)),
            ),
            _ => None,
        }
    }

    pub fn display_title(&self) -> String {
        self.title.clone().unwrap_or_else(|| {
            match self.mode {
                MeetingMode::Call => "Call",
                MeetingMode::InPerson => "In-person meeting",
            }
            .to_string()
        })
    }
}

/// What the background work is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Transcribing,
    Identifying,
    CleaningUp,
    Summarizing,
}

/// How far the running background work has got.
#[derive(Debug, Clone, Serialize, Type)]
pub struct TranscribeProgress {
    pub id: String,
    pub stage: Stage,
    pub done: u32,
    pub total: u32,
}

/// What the Meetings page shows about the current recording and transcription.
#[derive(Debug, Clone, Serialize, Type)]
pub struct MeetingState {
    pub recording: Option<MeetingInfo>,
    pub elapsed_ms: u64,
    pub transcribing: Option<TranscribeProgress>,
}

/// A meeting's transcript as the page shows it.
#[derive(Debug, Clone, Serialize, Type)]
pub struct MeetingTranscript {
    /// False while it's still being transcribed (what's there so far).
    pub complete: bool,
    pub paragraphs: Vec<Paragraph>,
}

struct Active {
    info: MeetingInfo,
    recording: Recording,
    ticker: Arc<AtomicBool>,
}

pub struct MeetingManager {
    pub(super) app: AppHandle,
    active: Mutex<Option<Active>>,
    /// Serialises read-modify-write of `meeting.json` files.
    meta_lock: Mutex<()>,
    /// Feeds jobs to the background worker, started on first use.
    pub(super) jobs: Mutex<Option<mpsc::Sender<Job>>>,
    /// Jobs queued or running, so none is queued twice.
    pub(super) queued: Mutex<HashSet<Job>>,
    pub(super) progress: Mutex<Option<TranscribeProgress>>,
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn write_info(dir: &Path, info: &MeetingInfo) {
    let path = dir.join(META_FILE);
    match serde_json::to_vec_pretty(info) {
        Ok(json) => {
            // Write then rename, so a crash never leaves half a file.
            let tmp = dir.join(format!("{META_FILE}.tmp"));
            if let Err(e) = std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, &path)) {
                log::error!("Couldn't save {}: {e}", path.display());
            }
        }
        Err(e) => log::error!("Couldn't encode meeting info: {e}"),
    }
}

pub(super) fn read_info(dir: &Path) -> Option<MeetingInfo> {
    serde_json::from_slice(&std::fs::read(dir.join(META_FILE)).ok()?).ok()
}

/// `m:ss`, or `h:mm:ss` from an hour.
pub fn format_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

impl MeetingManager {
    pub fn new(app: &AppHandle) -> Self {
        let manager = Self {
            app: app.clone(),
            active: Mutex::new(None),
            meta_lock: Mutex::new(()),
            jobs: Mutex::new(None),
            queued: Mutex::new(HashSet::new()),
            progress: Mutex::new(None),
        };
        manager.recover_interrupted();
        manager
    }

    pub(super) fn dir_of(&self, id: &str) -> Result<PathBuf, String> {
        Ok(self.meetings_dir()?.join(id))
    }

    /// Change a meeting's `meeting.json` and tell the page.
    pub(super) fn update_info(
        &self,
        id: &str,
        f: impl FnOnce(&mut MeetingInfo),
    ) -> Option<MeetingInfo> {
        let dir = self.dir_of(id).ok()?;
        let info = {
            let _lock = self.meta_lock.lock().unwrap_or_else(|e| e.into_inner());
            let mut info = read_info(&dir)?;
            f(&mut info);
            write_info(&dir, &info);
            info
        };
        let _ = self.app.emit("meetings-changed", ());
        Some(info)
    }

    pub fn meetings_dir(&self) -> Result<PathBuf, String> {
        crate::portable::app_data_dir(&self.app)
            .map(|d| d.join("meetings"))
            .map_err(|e| e.to_string())
    }

    /// Meetings left "recording" by a crash or force quit are marked
    /// interrupted; their WAVs are valid up to the last flush.
    fn recover_interrupted(&self) {
        for mut info in self.list() {
            if info.status != MeetingStatus::Recording {
                continue;
            }
            let Ok(dir) = self.meetings_dir().map(|d| d.join(&info.id)) else {
                continue;
            };
            info.status = MeetingStatus::Interrupted;
            info.tracks = ["mic.wav", "system.wav"]
                .iter()
                .filter_map(|file| {
                    let reader = hound::WavReader::open(dir.join(file)).ok()?;
                    Some(TrackSummary {
                        file: file.to_string(),
                        source: file.trim_end_matches(".wav").to_string(),
                        seconds: reader.duration() as f64 / reader.spec().sample_rate as f64,
                        peak_dbfs: 0.0,
                        padded_seconds: 0.0,
                    })
                })
                .collect();
            info.ended_at = info
                .tracks
                .iter()
                .map(|t| info.started_at + (t.seconds * 1000.0) as i64)
                .max();
            log::warn!("Meeting {} was interrupted; kept what was saved", info.id);
            if !info.tracks.is_empty() {
                // Transcribed once the app is up (see resume_transcriptions).
                info.transcript = Some(TranscriptStatus::Queued);
            }
            write_info(&dir, &info);
        }
    }

    /// All meetings on disk, newest first.
    pub fn list(&self) -> Vec<MeetingInfo> {
        let Ok(dir) = self.meetings_dir() else {
            return vec![];
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return vec![];
        };
        let mut meetings: Vec<MeetingInfo> = entries
            .flatten()
            .filter_map(|e| read_info(&e.path()))
            .collect();
        meetings.sort_by_key(|m| std::cmp::Reverse(m.started_at));
        meetings
    }

    pub fn is_recording(&self) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    pub fn state(&self) -> MeetingState {
        let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        MeetingState {
            recording: active.as_ref().map(|a| a.info.clone()),
            elapsed_ms: active
                .as_ref()
                .map_or(0, |a| a.recording.elapsed().as_millis() as u64),
            transcribing: self
                .progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }

    pub(super) fn changed(&self) {
        let _ = self.app.emit("meeting-state", self.state());
        crate::tray::sync_tray(&self.app);
    }

    pub fn start(&self, mode: MeetingMode) -> Result<MeetingInfo, String> {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(a) = active.as_ref() {
            return Ok(a.info.clone());
        }
        let started_at = now_ms();
        let id = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
        let dir = self.meetings_dir()?.join(&id);
        let mic = self
            .app
            .try_state::<Arc<AudioRecordingManager>>()
            .and_then(|m| m.meeting_microphone());
        let recording = Recording::start(&dir, mode, mic)?;
        let info = MeetingInfo {
            id,
            mode,
            status: MeetingStatus::Recording,
            started_at,
            ended_at: None,
            mic: recording.mic_label.clone(),
            system_error: recording.system_error.clone(),
            tracks: vec![],
            title: None,
            transcript: None,
            transcript_error: None,
            title_is_auto: false,
            summary: None,
            summary_error: None,
            speakers: BTreeMap::new(),
        };
        write_info(&dir, &info);
        log::info!("Meeting {} started ({:?})", info.id, mode);

        let mut settings = crate::settings::get_settings(&self.app);
        if settings.meeting_mode != mode {
            settings.meeting_mode = mode;
            crate::settings::write_settings(&self.app, settings);
        }

        let ticker = Arc::new(AtomicBool::new(false));
        spawn_menu_bar_timer(&self.app, ticker.clone());
        *active = Some(Active {
            info: info.clone(),
            recording,
            ticker,
        });
        drop(active);
        self.changed();
        Ok(info)
    }

    pub fn stop(&self) -> Result<Option<MeetingInfo>, String> {
        let Some(active) = self.active.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return Ok(None);
        };
        active.ticker.store(true, Ordering::Release);
        let dir = active.recording.dir.clone();
        // Picks up a title set while recording.
        let mut info = read_info(&dir).unwrap_or(active.info);
        let result = active.recording.stop();
        info.ended_at = Some(now_ms());
        info.status = MeetingStatus::Recorded;
        let outcome = match result {
            Ok(tracks) => {
                info.tracks = tracks;
                Ok(Some(info.clone()))
            }
            Err(e) => {
                info.status = MeetingStatus::Interrupted;
                Err(e)
            }
        };
        {
            let _lock = self.meta_lock.lock().unwrap_or_else(|e| e.into_inner());
            write_info(&dir, &info);
        }
        log::info!("Meeting {} stopped: {:?}", info.id, info.tracks);
        self.changed();
        if !info.tracks.is_empty() {
            self.queue(Job::Transcribe(info.id.clone()));
        }
        outcome
    }

    /// Start with the last mode, or stop.
    pub fn toggle(&self) -> Result<(), String> {
        if self.is_recording() {
            self.stop().map(|_| ())
        } else {
            let mode = crate::settings::get_settings(&self.app).meeting_mode;
            self.start(mode).map(|_| ())
        }
    }
}

/// Show "● 12:34" next to the tray icon until `stop` is set.
fn spawn_menu_bar_timer(app: &AppHandle, stop: Arc<AtomicBool>) {
    let app = app.clone();
    let started = std::time::Instant::now();
    std::thread::spawn(move || {
        let set_title = |title: Option<String>| {
            let app2 = app.clone();
            let _ = app.run_on_main_thread(move || {
                if let Some(tray) = app2.try_state::<tauri::tray::TrayIcon>() {
                    let _ = tray.set_title(title.as_deref());
                }
            });
        };
        while !stop.load(Ordering::Acquire) {
            set_title(Some(format!("● {}", format_elapsed(started.elapsed()))));
            std::thread::sleep(Duration::from_millis(500));
        }
        set_title(None);
    });
}

/// Stop a recording in progress when Handy quits, so the files are closed.
pub fn stop_on_exit(app: &AppHandle) {
    if let Some(m) = app.try_state::<Arc<MeetingManager>>() {
        if let Err(e) = m.stop() {
            log::error!("Couldn't stop the meeting on quit: {e}");
        }
    }
}

/// Start or stop from the tray or the shortcut, off the calling thread.
pub fn toggle_in_background(app: &AppHandle) {
    let app = app.clone();
    std::thread::spawn(move || {
        if let Some(m) = app.try_state::<Arc<MeetingManager>>() {
            if let Err(e) = m.toggle() {
                log::error!("Meeting: {e}");
                let _ = app.emit("meeting-error", e);
            }
        }
    });
}

#[tauri::command]
#[specta::specta]
pub async fn start_meeting(app: AppHandle, mode: MeetingMode) -> Result<MeetingInfo, String> {
    // Async so opening the devices doesn't block the main thread.
    app.state::<Arc<MeetingManager>>().start(mode)
}

#[tauri::command]
#[specta::specta]
pub async fn stop_meeting(app: AppHandle) -> Result<Option<MeetingInfo>, String> {
    app.state::<Arc<MeetingManager>>().stop()
}

#[tauri::command]
#[specta::specta]
pub fn get_meeting_state(app: AppHandle) -> MeetingState {
    app.state::<Arc<MeetingManager>>().state()
}

#[tauri::command]
#[specta::specta]
pub fn list_meetings(app: AppHandle) -> Vec<MeetingInfo> {
    app.state::<Arc<MeetingManager>>().list()
}

/// A meeting's folder, refusing ids that could point outside `meetings/`.
fn meeting_dir(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    // Ids are timestamps like 2026-09-24_14-32-05.
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("Invalid meeting".into());
    }
    Ok(app.state::<Arc<MeetingManager>>().meetings_dir()?.join(id))
}

/// Path of one of a meeting's tracks, for playback.
#[tauri::command]
#[specta::specta]
pub fn meeting_track_path(app: AppHandle, id: String, file: String) -> Result<String, String> {
    if file != "mic.wav" && file != "system.wav" {
        return Err("Invalid track".into());
    }
    let path = meeting_dir(&app, &id)?.join(file);
    if !path.is_file() {
        return Err("The recording isn't there any more".into());
    }
    Ok(path.to_string_lossy().into_owned())
}

/// The transcript so far, or `None` if there isn't one yet.
#[tauri::command]
#[specta::specta]
pub fn get_meeting_transcript(
    app: AppHandle,
    id: String,
) -> Result<Option<MeetingTranscript>, String> {
    let dir = meeting_dir(&app, &id)?;
    Ok(pipeline::load(&dir).map(|t| MeetingTranscript {
        complete: t.complete,
        paragraphs: paragraphs_of(&dir, &t),
    }))
}

/// A transcript's paragraphs, with the cleaned-up text where there is one.
pub(super) fn paragraphs_of(dir: &Path, t: &transcript::Transcript) -> Vec<Paragraph> {
    let cleaned: Option<summary::Cleaned> = summary::load_json(dir, summary::CLEANED_FILE);
    summary::apply_cleaned(transcript::paragraphs(&t.segments), cleaned.as_ref())
}

/// The summary, if one has been written.
#[tauri::command]
#[specta::specta]
pub fn get_meeting_summary(app: AppHandle, id: String) -> Result<Option<Summary>, String> {
    Ok(summary::load_json(
        &meeting_dir(&app, &id)?,
        summary::SUMMARY_FILE,
    ))
}

/// Write (or rewrite) the summary, with the notes as they are now.
#[tauri::command]
#[specta::specta]
pub fn summarize_meeting(app: AppHandle, id: String) -> Result<(), String> {
    let dir = meeting_dir(&app, &id)?;
    if !pipeline::load(&dir).is_some_and(|t| t.complete) {
        return Err("The meeting isn't transcribed yet".into());
    }
    app.state::<Arc<MeetingManager>>().queue(Job::Summarize(id));
    Ok(())
}

/// The whole meeting as Markdown: summary, notes and transcript.
#[tauri::command]
#[specta::specta]
pub fn meeting_markdown(app: AppHandle, id: String) -> Result<String, String> {
    let dir = meeting_dir(&app, &id)?;
    let info = read_info(&dir).ok_or("The meeting isn't there any more")?;
    let summary: Option<Summary> = summary::load_json(&dir, summary::SUMMARY_FILE);
    let notes = std::fs::read_to_string(dir.join(NOTES_FILE)).unwrap_or_default();
    let lines: Vec<String> = pipeline::load(&dir)
        .map(|t| paragraphs_of(&dir, &t))
        .unwrap_or_default()
        .iter()
        .map(|p| {
            let at = transcript::timestamp(p.start_ms);
            match info.speaker_label(p) {
                Some(who) => format!("**[{at}] {who}:** {}", p.text),
                None => format!("**[{at}]** {}", p.text),
            }
        })
        .collect();
    Ok(summary::to_markdown(
        &info.display_title(),
        &meta_line(&info),
        summary.as_ref(),
        &notes,
        &lines,
    ))
}

/// "Thu 24 Sep 2026, 14:32 · 42:10 · Me, Them".
pub(super) fn meta_line(info: &MeetingInfo) -> String {
    let started = chrono::DateTime::from_timestamp_millis(info.started_at)
        .map(|t| {
            t.with_timezone(&chrono::Local)
                .format("%a %-d %b %Y, %H:%M")
                .to_string()
        })
        .unwrap_or_default();
    let mut parts = vec![started];
    if let Some(end) = info.ended_at {
        parts.push(format_elapsed(Duration::from_millis(
            (end - info.started_at).max(0) as u64,
        )));
    }
    match info.mode {
        MeetingMode::Call => parts.push("Me, Them".into()),
        MeetingMode::InPerson if !info.speakers.is_empty() => parts.push(
            info.speakers
                .values()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        ),
        MeetingMode::InPerson => {}
    }
    parts.join(" · ")
}

/// Name a voice in an in-person meeting ("Speaker 2" → "Sam").
#[tauri::command]
#[specta::specta]
pub fn rename_meeting_speaker(
    app: AppHandle,
    id: String,
    speaker: u32,
    name: String,
) -> Result<(), String> {
    meeting_dir(&app, &id)?;
    let name = name.trim().to_string();
    app.state::<Arc<MeetingManager>>()
        .update_info(&id, |i| {
            if name.is_empty() {
                i.speakers.remove(&speaker);
            } else {
                i.speakers.insert(speaker, name);
            }
        })
        .map(|_| ())
        .ok_or_else(|| "The meeting isn't there any more".into())
}

/// Transcribe a meeting again from scratch, with the engine chosen now (to
/// compare engines, or after changing vocabulary). The summary is rewritten
/// afterwards; the user's notes are kept.
#[tauri::command]
#[specta::specta]
pub fn retranscribe_meeting(app: AppHandle, id: String) -> Result<(), String> {
    let dir = meeting_dir(&app, &id)?;
    let manager = app.state::<Arc<MeetingManager>>();
    if manager
        .progress
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|p| p.id == id)
    {
        return Err("This meeting is being processed; try again when it's done".into());
    }
    for file in [transcript::FILE, summary::CLEANED_FILE] {
        match std::fs::remove_file(dir.join(file)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Couldn't clear the old transcript: {e}")),
        }
    }
    manager.queue(Job::Transcribe(id));
    Ok(())
}

/// Transcribe a meeting that failed or was recorded before transcription.
#[tauri::command]
#[specta::specta]
pub fn transcribe_meeting(app: AppHandle, id: String) -> Result<(), String> {
    let dir = meeting_dir(&app, &id)?;
    if !dir.join(META_FILE).is_file() {
        return Err("The meeting isn't there any more".into());
    }
    app.state::<Arc<MeetingManager>>()
        .queue(Job::Transcribe(id));
    Ok(())
}

/// The user's notes on a meeting (Markdown), empty if none yet.
#[tauri::command]
#[specta::specta]
pub fn get_meeting_notes(app: AppHandle, id: String) -> Result<String, String> {
    match std::fs::read_to_string(meeting_dir(&app, &id)?.join(NOTES_FILE)) {
        Ok(notes) => Ok(notes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.to_string()),
    }
}

/// Save the user's notes; they're combined with the transcript when the
/// meeting is summarised.
#[tauri::command]
#[specta::specta]
pub fn save_meeting_notes(app: AppHandle, id: String, notes: String) -> Result<(), String> {
    let dir = meeting_dir(&app, &id)?;
    if !dir.is_dir() {
        return Err("The meeting isn't there any more".into());
    }
    let tmp = dir.join(format!("{NOTES_FILE}.tmp"));
    std::fs::write(&tmp, notes)
        .and_then(|_| std::fs::rename(&tmp, dir.join(NOTES_FILE)))
        .map_err(|e| format!("Couldn't save the notes: {e}"))
}

#[tauri::command]
#[specta::specta]
pub fn rename_meeting(app: AppHandle, id: String, title: String) -> Result<(), String> {
    meeting_dir(&app, &id)?;
    let manager = app.state::<Arc<MeetingManager>>();
    let title = title.trim();
    let title = (!title.is_empty()).then(|| title.to_string());
    let updated = manager.update_info(&id, |i| {
        i.title = title.clone();
        i.title_is_auto = false;
    });
    if updated.is_none() {
        return Err("The meeting isn't there any more".into());
    }
    // Keep the recording in progress in step, so stop() doesn't lose it.
    if let Some(a) = manager
        .active
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
        .filter(|a| a.info.id == id)
    {
        a.info.title = title;
    }
    manager.changed();
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn open_meeting_folder(app: AppHandle, id: Option<String>) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let dir = match id {
        Some(id) => meeting_dir(&app, &id)?,
        None => app.state::<Arc<MeetingManager>>().meetings_dir()?,
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<String>)
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_is_formatted_like_a_clock() {
        assert_eq!(format_elapsed(Duration::from_secs(5)), "0:05");
        assert_eq!(format_elapsed(Duration::from_secs(754)), "12:34");
        assert_eq!(format_elapsed(Duration::from_secs(3723)), "1:02:03");
    }
}

/// Meeting settings to change; fields left out stay as they are.
#[derive(Debug, Default, Deserialize, Type)]
pub struct MeetingSettingsUpdate {
    pub llm: Option<super::MeetingLlm>,
    pub cleanup: Option<bool>,
    pub summary_prompt: Option<String>,
    pub auto_gain: Option<bool>,
    pub input_boost_db: Option<f32>,
    pub transcriber: Option<super::MeetingTranscriber>,
    pub diarize: Option<bool>,
}

#[tauri::command]
#[specta::specta]
pub fn change_meeting_settings(
    app: AppHandle,
    update: MeetingSettingsUpdate,
) -> Result<(), String> {
    let mut settings = crate::settings::get_settings(&app);
    if let Some(v) = update.llm {
        settings.meeting_llm = v;
    }
    if let Some(v) = update.cleanup {
        settings.meeting_cleanup = v;
    }
    if let Some(v) = update.summary_prompt {
        settings.meeting_summary_prompt = v;
    }
    if let Some(v) = update.auto_gain {
        settings.meeting_auto_gain = v;
    }
    if let Some(v) = update.input_boost_db {
        settings.meeting_input_boost_db = v.clamp(0.0, 24.0);
    }
    if let Some(v) = update.transcriber {
        settings.meeting_transcriber = v;
    }
    if let Some(v) = update.diarize {
        settings.meeting_diarize = v;
    }
    crate::settings::write_settings(&app, settings);
    Ok(())
}

/// What the summary is asked for when the user hasn't written their own.
#[tauri::command]
#[specta::specta]
pub fn default_meeting_summary_prompt() -> String {
    summary::DEFAULT_SUMMARY_GUIDANCE.to_string()
}
