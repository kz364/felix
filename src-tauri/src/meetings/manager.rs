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
    /// Paused from the panel: the files are closed and it carries on with
    /// Resume. Transcribed once it's stopped.
    Paused,
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
    /// Where the Mac played sound when recording started; headphones mean
    /// the call can't echo into the mic.
    #[serde(default)]
    pub output_device: Option<String>,
    /// The call app that held the mic (bundle id), for speaker names.
    #[serde(default)]
    pub call_app: Option<String>,
    /// Offsets into the recording (ms) where it was resumed after a stop.
    #[serde(default)]
    pub resumed_at_ms: Vec<u64>,
    /// The languages it was transcribed as (chosen, or found in it).
    #[serde(default)]
    pub languages: Vec<String>,
    /// Names the call app showed for the other side's voices, by number;
    /// the user's own names in `speakers` win.
    #[serde(default)]
    pub app_speakers: BTreeMap<u32, String>,
    /// Whether it's a call was worked out rather than chosen (`detect`):
    /// it records like a call, and `mode` is settled when it stops.
    #[serde(default)]
    pub mode_auto: bool,
    /// The calendar event it was recorded as part of (EventKit's id), so
    /// the invite read for it is that event's, not another that overlaps.
    #[serde(default)]
    pub event_id: Option<String>,
    /// Its page in Notion, once saved there (see [`super::notion`]).
    #[serde(default)]
    pub notion_page_id: Option<String>,
    #[serde(default)]
    pub notion_url: Option<String>,
    /// Why the last save to Notion failed.
    #[serde(default)]
    pub notion_error: Option<String>,
    /// Moved to the team's page, where the people who were in it can see it.
    #[serde(default)]
    pub notion_shared: bool,
    /// Everyone on its invite is from the user's own organisation, so it can
    /// be shared. Worked out when meetings are listed, not saved.
    #[serde(default)]
    pub notion_shareable: bool,
    /// When its notes were last sent to Slack (Unix ms).
    #[serde(default)]
    pub slack_sent_at: Option<i64>,
    /// Why the last send to Slack failed.
    #[serde(default)]
    pub slack_error: Option<String>,
}

/// The name of a told-apart voice nobody named: the other side of a call
/// is numbered from [`super::pipeline::SYSTEM_SPEAKERS`].
pub fn default_speaker_name(n: u32) -> String {
    if n >= super::pipeline::SYSTEM_SPEAKERS {
        format!("Them {}", n - super::pipeline::SYSTEM_SPEAKERS + 1)
    } else {
        format!("Speaker {}", n + 1)
    }
}

impl MeetingInfo {
    /// Who a paragraph is, for the summary and the Markdown: a name the user
    /// (or the call app) gave, otherwise "Me"/"Them 2"/"Speaker 3".
    pub fn speaker_label(&self, p: &Paragraph) -> Option<String> {
        if let Some(name) = p
            .speaker
            .and_then(|n| self.speakers.get(&n).or_else(|| self.app_speakers.get(&n)))
        {
            return Some(name.clone());
        }
        match (self.mode, p.source, p.speaker) {
            (MeetingMode::Call, Source::Mic, None) => Some("Me".into()),
            (MeetingMode::Call, Source::System, None) => Some("Them".into()),
            (_, _, Some(super::diarize::ME)) => Some("Me".into()),
            (_, _, Some(n)) => Some(default_speaker_name(n)),
            _ => None,
        }
    }

    /// Everyone heard, as labelled in the transcript, in order of first word.
    pub fn participants(&self, paragraphs: &[Paragraph]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for p in paragraphs {
            if let Some(label) = self.speaker_label(p) {
                if !out.contains(&label) {
                    out.push(label);
                }
            }
        }
        out
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
    /// A meeting paused from the panel, waiting for Resume or Stop.
    pub paused: Option<MeetingInfo>,
    pub elapsed_ms: u64,
    pub transcribing: Option<TranscribeProgress>,
    /// "What did I miss?" and "Suggest a question" work (a cloud
    /// transcriber keeps a live transcript).
    pub live: bool,
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
    /// Stops the menu bar timer, the speaker-name reader and the live
    /// transcript.
    helpers: Arc<AtomicBool>,
    _awake: super::awake::KeepAwake,
}

pub struct MeetingManager {
    pub(super) app: AppHandle,
    active: Mutex<Option<Active>>,
    /// The meeting paused from the panel, if any.
    paused: Mutex<Option<MeetingInfo>>,
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

pub fn read_info(dir: &Path) -> Option<MeetingInfo> {
    serde_json::from_slice(&std::fs::read(dir.join(META_FILE)).ok()?).ok()
}

/// The longest track, in seconds.
fn recorded_seconds(info: &MeetingInfo) -> f64 {
    info.tracks.iter().map(|t| t.seconds).fold(0.0, f64::max)
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
            paused: Mutex::new(None),
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
            if info.status == MeetingStatus::Paused {
                // Handy quit while paused: the files were already closed.
                info.status = MeetingStatus::Recorded;
                if !info.tracks.is_empty() {
                    info.transcript = Some(TranscriptStatus::Queued);
                }
                if let Ok(dir) = self.dir_of(&info.id) {
                    write_info(&dir, &info);
                }
                continue;
            }
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
            .filter_map(|e| {
                let mut info = read_info(&e.path())?;
                info.notion_shareable = info.notion_page_id.is_some()
                    && !info.notion_shared
                    && super::calendar::can_share(super::calendar::load(&e.path()).as_ref());
                Some(info)
            })
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
        let paused = self
            .paused
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        MeetingState {
            recording: active.as_ref().map(|a| a.info.clone()),
            elapsed_ms: match (active.as_ref(), paused.as_ref()) {
                (Some(a), _) => a.recording.elapsed().as_millis() as u64,
                (None, Some(p)) => (recorded_seconds(p) * 1000.0) as u64,
                (None, None) => 0,
            },
            paused,
            transcribing: self
                .progress
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            live: active.is_some()
                && super::remote::Remote::from_settings(&crate::settings::get_settings(&self.app))
                    .is_ok_and(|r| r.is_some()),
        }
    }

    pub(super) fn changed(&self) {
        let _ = self.app.emit("meeting-state", self.state());
        crate::tray::sync_tray(&self.app);
        super::watch::apply_screen_share(&self.app);
    }

    /// Recording, or paused mid-meeting.
    pub fn in_meeting(&self) -> bool {
        self.is_recording()
            || self
                .paused
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
    }

    /// Start recording; `None` works out whether it's a call.
    pub fn start(&self, mode: Option<MeetingMode>) -> Result<MeetingInfo, String> {
        self.begin(mode, None)
    }

    /// Record a call the watcher noticed, reading names from its app.
    pub fn start_call(&self, bundle_id: &str) -> Result<MeetingInfo, String> {
        self.begin(Some(MeetingMode::Call), Some(bundle_id.to_string()))
    }

    fn begin(
        &self,
        chosen: Option<MeetingMode>,
        call_app: Option<String>,
    ) -> Result<MeetingInfo, String> {
        let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(a) = active.as_ref() {
            return Ok(a.info.clone());
        }
        drop(active);
        self.finish_paused();
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(a) = active.as_ref() {
            return Ok(a.info.clone());
        }
        let started_at = now_ms();
        let id = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
        let dir = self.meetings_dir()?.join(&id);
        // Not chosen: record like a call (both sides) and decide on stopping.
        let mode = chosen.unwrap_or(MeetingMode::Call);
        let call_app = match chosen {
            Some(_) => call_app,
            None => super::detect::start(&id).map(|a| a.bundle_id.to_string()),
        };
        let mic = self
            .app
            .try_state::<Arc<AudioRecordingManager>>()
            .and_then(|m| m.meeting_microphone());
        let recording = Recording::start(&dir, mode, mic, false)?;
        // Named after the calendar meeting it's of; the notes keep the name.
        let event = super::calendar::meeting_now();
        if let Some(e) = &event {
            log::info!("Meeting {id} is the calendar's \"{}\"", e.title);
        }
        let event_id = event.as_ref().map(|e| e.id.clone());
        let info = MeetingInfo {
            id,
            mode,
            status: MeetingStatus::Recording,
            started_at,
            ended_at: None,
            mic: recording.mic_label.clone(),
            system_error: recording.system_error.clone(),
            tracks: vec![],
            title: event.map(|e| e.title).filter(|t| !t.trim().is_empty()),
            transcript: None,
            transcript_error: None,
            title_is_auto: false,
            summary: None,
            summary_error: None,
            speakers: BTreeMap::new(),
            output_device: output_device_name(),
            call_app,
            resumed_at_ms: vec![],
            languages: vec![],
            app_speakers: BTreeMap::new(),
            mode_auto: chosen.is_none(),
            event_id,
            notion_page_id: None,
            notion_url: None,
            notion_error: None,
            notion_shared: false,
            notion_shareable: false,
            slack_sent_at: None,
            slack_error: None,
        };
        write_info(&dir, &info);
        if let Some(id) = &info.event_id {
            super::calendar::save_for_event(&dir, id);
        }
        log::info!("Meeting {} started ({:?})", info.id, mode);

        let mut settings = crate::settings::get_settings(&self.app);
        // Remember the choice for the shortcut and the tray, except the
        // watcher's "Record this call?", which isn't one.
        if info.call_app.is_none() || chosen.is_none() {
            let detect = chosen.is_none();
            let mode = chosen.unwrap_or(settings.meeting_mode);
            if settings.meeting_detect_mode != detect || settings.meeting_mode != mode {
                settings.meeting_detect_mode = detect;
                settings.meeting_mode = mode;
                crate::settings::write_settings(&self.app, settings);
            }
        }
        self.activate(&mut active, info.clone(), recording);
        drop(active);
        self.changed();
        crate::meeting_panel::show(&self.app);
        super::watch::started(&self.app, &info);
        Ok(info)
    }

    /// Carry on recording into a meeting that stopped (the Mac slept, the
    /// length limit, or the user stopped too soon). Its transcript is made
    /// again from the whole recording afterwards.
    pub fn resume(&self, id: &str) -> Result<MeetingInfo, String> {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if active.is_some() {
            return Err("Stop the current recording first".into());
        }
        let other_paused = self
            .paused
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|p| p.id != id);
        if other_paused {
            // Resuming another meeting finishes the paused one.
            drop(active);
            self.finish_paused();
            return self.resume(id);
        }
        if self.is_processing(id) {
            return Err("This meeting is being processed; try again when it's done".into());
        }
        let dir = self.dir_of(id)?;
        let mut info = read_info(&dir).ok_or("The meeting isn't there any more")?;
        let mic = self
            .app
            .try_state::<Arc<AudioRecordingManager>>()
            .and_then(|m| m.meeting_microphone());
        // Still to be worked out: record both sides again.
        if info.mode_auto {
            info.mode = MeetingMode::Call;
            super::detect::start(id);
        }
        let recording = Recording::start(&dir, info.mode, mic, true)?;
        super::speakers::keep_names(&dir, &info.speakers);
        for file in [
            transcript::FILE,
            summary::CLEANED_FILE,
            super::live::FILE,
            super::clues::FILE,
            super::clues::TURNS_FILE,
            super::floor::FILE,
        ] {
            let _ = std::fs::remove_file(dir.join(file));
        }
        info.resumed_at_ms
            .push(recording.elapsed().as_millis() as u64);
        info.status = MeetingStatus::Recording;
        info.ended_at = None;
        info.transcript = None;
        info.transcript_error = None;
        info.system_error = recording.system_error.clone();
        {
            let _lock = self.meta_lock.lock().unwrap_or_else(|e| e.into_inner());
            write_info(&dir, &info);
        }
        log::info!("Meeting {id} resumed");
        *self.paused.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.activate(&mut active, info.clone(), recording);
        drop(active);
        self.changed();
        crate::meeting_panel::show(&self.app);
        Ok(info)
    }

    /// Start the helpers that run while recording and make it the active one.
    fn activate(&self, active: &mut Option<Active>, info: MeetingInfo, recording: Recording) {
        let helpers = Arc::new(AtomicBool::new(false));
        spawn_menu_bar_timer(&self.app, helpers.clone(), recording.elapsed());
        if info.mode == MeetingMode::Call {
            super::active_speaker::spawn_watcher(
                &self.app,
                &recording.dir,
                info.call_app.clone(),
                helpers.clone(),
            );
        }
        super::live::spawn(&self.app, &recording.dir, info.mode, helpers.clone());
        *active = Some(Active {
            info,
            recording,
            helpers,
            _awake: super::awake::KeepAwake::new("Recording a meeting"),
        });
    }

    fn is_processing(&self, id: &str) -> bool {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .is_some_and(|p| p.id == id)
    }

    /// How loud the recording is right now (0 to 1), while one is going.
    pub fn level(&self) -> Option<f32> {
        self.with_recording(|r, _| r.level())
    }

    /// How far into the recording it is, while one is in progress.
    pub fn elapsed_ms(&self) -> Option<u64> {
        self.with_recording(|r, _| r.elapsed().as_millis() as u64)
    }

    /// The folder of the meeting being recorded and how far in it is.
    pub fn recording_at(&self) -> Option<(PathBuf, u64)> {
        self.with_recording(|r, _| (r.dir.clone(), r.elapsed().as_millis() as u64))
    }

    /// Look at the recording in progress.
    pub(super) fn with_recording<R>(
        &self,
        f: impl FnOnce(&Recording, &MeetingInfo) -> R,
    ) -> Option<R> {
        let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        active.as_ref().map(|a| f(&a.recording, &a.info))
    }

    pub fn stop(&self) -> Result<Option<MeetingInfo>, String> {
        if !self.is_recording() {
            return Ok(self.finish_paused());
        }
        let (info, outcome) = self.close_recording(MeetingStatus::Recorded)?;
        self.finish(&info);
        outcome
    }

    /// Close the files but keep the panel, to carry on with `resume_paused`.
    pub fn pause(&self) -> Result<(), String> {
        let (info, outcome) = self.close_recording(MeetingStatus::Paused)?;
        outcome?;
        log::info!("Meeting {} paused", info.id);
        *self.paused.lock().unwrap_or_else(|e| e.into_inner()) = Some(info);
        self.changed();
        Ok(())
    }

    /// Carry on with the paused meeting.
    pub fn resume_paused(&self) -> Result<MeetingInfo, String> {
        let id = self
            .paused
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|p| p.id.clone())
            .ok_or("Nothing is paused")?;
        self.resume(&id)
    }

    /// Stop the paused meeting for good: transcribe it and hide the panel.
    fn finish_paused(&self) -> Option<MeetingInfo> {
        let paused = self
            .paused
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()?;
        let info = self.update_info(&paused.id, |i| {
            if i.status == MeetingStatus::Paused {
                i.status = MeetingStatus::Recorded;
            }
        })?;
        log::info!("Meeting {} stopped while paused", info.id);
        self.finish(&info);
        Some(info)
    }

    /// After the last stop: hide the panel and transcribe.
    fn finish(&self, info: &MeetingInfo) {
        self.changed();
        crate::meeting_panel::hide(&self.app);
        if !info.tracks.is_empty() {
            self.queue(Job::Transcribe(info.id.clone()));
        }
        super::watch::stopped(&self.app, info);
    }

    /// Stop the recording in progress and save `meeting.json` with `status`
    /// (Interrupted if the files couldn't be closed).
    #[allow(clippy::type_complexity)]
    fn close_recording(
        &self,
        status: MeetingStatus,
    ) -> Result<(MeetingInfo, Result<Option<MeetingInfo>, String>), String> {
        let Some(active) = self.active.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return Err("Nothing is recording".into());
        };
        active.helpers.store(true, Ordering::Release);
        let dir = active.recording.dir.clone();
        // Picks up a title set while recording.
        let mut info = read_info(&dir).unwrap_or(active.info);
        let result = active.recording.stop();
        info.ended_at = Some(now_ms());
        info.status = status;
        let outcome = match result {
            Ok(tracks) => {
                if info.mode_auto {
                    let seconds = tracks.iter().map(|t| t.seconds).fold(0.0, f64::max);
                    info.mode = super::detect::finish(&info.id, &dir, seconds);
                }
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
        Ok((info, outcome))
    }

    /// Start with the last mode, or stop.
    pub fn toggle(&self) -> Result<(), String> {
        if self.is_recording() {
            self.stop().map(|_| ())
        } else if self
            .paused
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
        {
            self.resume_paused().map(|_| ())
        } else {
            let settings = crate::settings::get_settings(&self.app);
            let mode = (!settings.meeting_detect_mode).then_some(settings.meeting_mode);
            self.start(mode).map(|_| ())
        }
    }
}

/// How long the meeting has run, for the menu bar. Minutes only, so it can't
/// be mistaken for the clock beside it.
fn menu_bar_elapsed(d: Duration) -> String {
    let m = d.as_secs() / 60;
    if m >= 60 {
        format!("Rec {}h {:02}m", m / 60, m % 60)
    } else {
        format!("Rec {m}m")
    }
}

/// Show "Rec 12m" next to the tray icon until `stop` is set.
fn spawn_menu_bar_timer(app: &AppHandle, stop: Arc<AtomicBool>, already: Duration) {
    let app = app.clone();
    let started = std::time::Instant::now()
        .checked_sub(already)
        .unwrap_or_else(std::time::Instant::now);
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
            set_title(Some(menu_bar_elapsed(started.elapsed())));
            std::thread::sleep(Duration::from_millis(500));
        }
        // tray-icon ignores `None` on macOS, which left "Rec 14m" showing
        // after the meeting ended; an empty title clears it.
        set_title(Some(String::new()));
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
pub async fn start_meeting(
    app: AppHandle,
    mode: Option<MeetingMode>,
) -> Result<MeetingInfo, String> {
    // Async so opening the devices doesn't block the main thread.
    app.state::<Arc<MeetingManager>>().start(mode)
}

#[tauri::command]
#[specta::specta]
pub async fn stop_meeting(app: AppHandle) -> Result<Option<MeetingInfo>, String> {
    app.state::<Arc<MeetingManager>>().stop()
}

/// Pause from the panel; Resume carries on in the same meeting.
#[tauri::command]
#[specta::specta]
pub async fn pause_meeting(app: AppHandle) -> Result<(), String> {
    app.state::<Arc<MeetingManager>>().pause()
}

#[tauri::command]
#[specta::specta]
pub async fn resume_paused_meeting(app: AppHandle) -> Result<MeetingInfo, String> {
    app.state::<Arc<MeetingManager>>().resume_paused()
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
pub(super) fn meeting_dir(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
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

/// The rough transcript made while recording (`live.json`), for the panel.
#[tauri::command]
#[specta::specta]
pub fn get_live_transcript(app: AppHandle, id: String) -> Result<Vec<Paragraph>, String> {
    Ok(transcript::paragraphs(&super::live::load(&meeting_dir(
        &app, &id,
    )?)))
}

/// The user's own fixes to a transcript, by [`summary::paragraph_key`].
/// Kept apart from the transcript so they survive cleanup and transcribing
/// again.
pub(super) const EDITS_FILE: &str = "edits.json";

/// A transcript's paragraphs, with the cleaned-up text where there is one
/// and the user's edits over that.
pub fn paragraphs_of(dir: &Path, t: &transcript::Transcript) -> Vec<Paragraph> {
    let cleaned: Option<summary::Cleaned> = summary::load_json(dir, summary::CLEANED_FILE);
    let edits: BTreeMap<String, String> = summary::load_json(dir, EDITS_FILE).unwrap_or_default();
    apply_edits(
        summary::apply_cleaned(
            transcript::fixed_paragraphs(&t.segments, &speaker_fixes(dir)),
            cleaned.as_ref(),
        ),
        &edits,
    )
}

/// Who the user said spoke from a segment to the end of its paragraph, by
/// segment key (see [`transcript::fixed_paragraphs`]): at a paragraph's
/// start that's the paragraph, further in it splits it.
pub(super) const SPEAKER_FIXES_FILE: &str = "speaker_fixes.json";

pub fn user_speaker_fixes(dir: &Path) -> BTreeMap<String, u32> {
    summary::load_json(dir, SPEAKER_FIXES_FILE).unwrap_or_default()
}

/// The speakers the transcript shows: turns found from the conversation
/// (see [`super::clues`]), under the user's own fixes.
pub fn speaker_fixes(dir: &Path) -> BTreeMap<String, u32> {
    let mut fixes: BTreeMap<String, u32> =
        summary::load_json(dir, super::clues::TURNS_FILE).unwrap_or_default();
    fixes.extend(user_speaker_fixes(dir));
    fixes
}

/// A number for a new person heard on `source`: after every voice already
/// used there (the other side of a call from
/// [`super::pipeline::SYSTEM_SPEAKERS`]).
fn new_speaker(source: Source, used: impl Iterator<Item = u32>) -> u32 {
    let system = super::pipeline::SYSTEM_SPEAKERS;
    let used: Vec<u32> = used.filter(|&n| n != super::diarize::ME).collect();
    match source {
        Source::System => used
            .iter()
            .copied()
            .filter(|&n| n >= system)
            .max()
            .map_or(system, |n| n + 1),
        Source::Mic => used
            .iter()
            .copied()
            .filter(|&n| n < system)
            .max()
            .map_or(0, |n| n + 1),
    }
}

fn apply_edits(mut paragraphs: Vec<Paragraph>, edits: &BTreeMap<String, String>) -> Vec<Paragraph> {
    for p in &mut paragraphs {
        if let Some(text) = edits.get(&summary::paragraph_key(p)) {
            if *text != p.text {
                let before = std::mem::replace(&mut p.text, text.clone());
                p.raw.get_or_insert(before);
            }
        }
    }
    paragraphs
}

/// Fix a paragraph of the transcript. The fix is kept when the transcript
/// is cleaned up or made again, and names in it are learned like dictation
/// fixes.
#[tauri::command]
#[specta::specta]
pub async fn edit_meeting_paragraph(
    app: AppHandle,
    id: String,
    source: Source,
    start_ms: u64,
    text: String,
) -> Result<(), String> {
    let dir = meeting_dir(&app, &id)?;
    let t = pipeline::load(&dir).ok_or("The meeting isn't transcribed yet")?;
    let paragraphs = paragraphs_of(&dir, &t);
    let p = paragraphs
        .iter()
        .find(|p| p.source == source && p.start_ms == start_ms)
        .ok_or("That part of the transcript isn't there any more")?;
    let key = summary::paragraph_key(p);
    let before = p.text.clone();
    let text = text.trim().to_string();
    let mut edits: BTreeMap<String, String> =
        summary::load_json(&dir, EDITS_FILE).unwrap_or_default();
    if text.is_empty() || Some(&text) == p.raw.as_ref() {
        edits.remove(&key);
    } else {
        edits.insert(key, text.clone());
    }
    summary::save_json(&dir, EDITS_FILE, &edits)?;
    let _ = app.emit("meetings-changed", ());
    if !text.is_empty() && text != before && crate::settings::get_settings(&app).learn_from_edits {
        crate::edit_learning::learn_from(&app, &before, &text).await;
    }
    Ok(())
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

fn output_device_name() -> Option<String> {
    use cpal::traits::{DeviceTrait, HostTrait};
    cpal::default_host()
        .default_output_device()
        .and_then(|d| d.name().ok())
}

/// Every name given to a voice, the user's first.
fn named_speakers(info: &MeetingInfo) -> Vec<String> {
    let mut ids: Vec<&u32> = info.speakers.keys().collect();
    ids.extend(
        info.app_speakers
            .keys()
            .filter(|k| !info.speakers.contains_key(k)),
    );
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        let name = info.speakers.get(id).or_else(|| info.app_speakers.get(id));
        if let Some(name) = name.filter(|n| !out.contains(n)) {
            out.push(name.clone());
        }
    }
    out
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
    let recorded = info
        .tracks
        .iter()
        .map(|t| t.seconds)
        .fold(0.0_f64, f64::max);
    if recorded > 0.0 {
        parts.push(format_elapsed(Duration::from_secs_f64(recorded)));
    } else if let Some(end) = info.ended_at {
        parts.push(format_elapsed(Duration::from_millis(
            (end - info.started_at).max(0) as u64,
        )));
    }
    match info.mode {
        MeetingMode::Call if named_speakers(info).is_empty() => parts.push("Me, Them".into()),
        MeetingMode::Call => parts.push(
            std::iter::once("Me".to_string())
                .chain(named_speakers(info))
                .collect::<Vec<_>>()
                .join(", "),
        ),
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
    let dir = meeting_dir(&app, &id)?;
    let name = name.trim().to_string();
    let info = app
        .state::<Arc<MeetingManager>>()
        .update_info(&id, |i| {
            if name.is_empty() {
                i.speakers.remove(&speaker);
            } else {
                i.speakers.insert(speaker, name);
            }
        })
        .ok_or("The meeting isn't there any more")?;
    // The remembered voice learns the name for next time.
    super::remembered::apply(&dir, &info.speakers, &BTreeMap::new());
    Ok(())
}

/// Say who spoke one paragraph, leaving the rest of that voice as it is:
/// `speaker` is someone already in the meeting, or `name` a person (reused
/// if a voice already has that name, otherwise added). Setting it back to
/// what transcription found removes the fix. Returns the speaker's number.
#[tauri::command]
#[specta::specta]
pub fn set_paragraph_speaker(
    app: AppHandle,
    id: String,
    source: Source,
    start_ms: u64,
    speaker: Option<u32>,
    name: Option<String>,
) -> Result<u32, String> {
    let dir = meeting_dir(&app, &id)?;
    let t = pipeline::load(&dir).ok_or("The meeting isn't transcribed yet")?;
    let segment = t
        .segments
        .iter()
        .find(|s| s.source == source && s.start_ms == start_ms)
        .ok_or("That part of the transcript isn't there any more")?;
    let key = transcript::segment_key(segment);
    let mut fixes = user_speaker_fixes(&dir);
    // Who it is without this fix, and the paragraph it's in now.
    let others: BTreeMap<String, u32> = fixes
        .iter()
        .filter(|(k, _)| **k != key)
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    let paragraphs = transcript::fixed_paragraphs(&t.segments, &others);
    let found = paragraphs
        .iter()
        .rfind(|p| p.source == source && p.start_ms <= start_ms)
        .cloned()
        .ok_or("That part of the transcript isn't there any more")?;
    let name = name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let chosen = match (speaker, name) {
        (Some(s), _) => s,
        (None, Some(name)) => {
            let manager = app.state::<Arc<MeetingManager>>();
            let info = read_info(&dir).ok_or("The meeting isn't there any more")?;
            let existing = if name.eq_ignore_ascii_case("me") {
                Some(super::diarize::ME)
            } else {
                info.speakers
                    .iter()
                    .chain(
                        info.app_speakers
                            .iter()
                            .filter(|(k, _)| !info.speakers.contains_key(k)),
                    )
                    .find(|(_, n)| n.eq_ignore_ascii_case(&name))
                    .map(|(&k, _)| k)
            };
            match existing {
                Some(s) => s,
                None => {
                    let used = t
                        .segments
                        .iter()
                        .filter_map(|s| s.speaker)
                        .chain(info.speakers.keys().copied())
                        .chain(info.app_speakers.keys().copied())
                        .chain(fixes.values().copied());
                    let s = new_speaker(source, used);
                    manager
                        .update_info(&id, |i| {
                            i.speakers.insert(s, name);
                        })
                        .ok_or("The meeting isn't there any more")?;
                    s
                }
            }
        }
        (None, None) => return Err("Choose who spoke it".into()),
    };
    if found.speaker == Some(chosen) {
        fixes.remove(&key);
    } else {
        fixes.insert(key, chosen);
    }
    // Splitting a paragraph: its tidied and edited text covered both
    // parts, so both go back to the words as transcribed.
    if found.start_ms != start_ms {
        forget_text(&dir, &summary::paragraph_key(&found))?;
    }
    summary::save_json(&dir, SPEAKER_FIXES_FILE, &fixes)?;
    super::speakers::record(&dir, &t);
    relearn_voices(&dir);
    let _ = app.emit("meetings-changed", ());
    Ok(chosen)
}

/// Drop a paragraph's tidied and edited text.
fn forget_text(dir: &Path, key: &str) -> Result<(), String> {
    if let Some(mut c) = summary::load_json::<summary::Cleaned>(dir, summary::CLEANED_FILE) {
        if c.texts.remove(key).is_some() {
            summary::save_json(dir, summary::CLEANED_FILE, &c)?;
        }
    }
    let mut edits: BTreeMap<String, String> =
        summary::load_json(dir, EDITS_FILE).unwrap_or_default();
    if edits.remove(key).is_some() {
        summary::save_json(dir, EDITS_FILE, &edits)?;
    }
    Ok(())
}

/// One part of a paragraph as transcribed, for splitting it.
#[derive(Debug, Clone, Serialize, Type)]
pub struct ParagraphPart {
    pub start_ms: u64,
    pub text: String,
}

/// The transcribed pieces a paragraph is made of, to pick where to split it.
#[tauri::command]
#[specta::specta]
pub fn paragraph_parts(
    app: AppHandle,
    id: String,
    source: Source,
    start_ms: u64,
) -> Result<Vec<ParagraphPart>, String> {
    let dir = meeting_dir(&app, &id)?;
    let t = pipeline::load(&dir).ok_or("The meeting isn't transcribed yet")?;
    let paragraphs = transcript::fixed_paragraphs(&t.segments, &speaker_fixes(&dir));
    let p = paragraphs
        .iter()
        .find(|p| p.source == source && p.start_ms == start_ms)
        .ok_or("That part of the transcript isn't there any more")?;
    let next = paragraphs
        .iter()
        .filter(|q| q.source == source && q.start_ms > p.start_ms)
        .map(|q| q.start_ms)
        .min()
        .unwrap_or(u64::MAX);
    let mut parts: Vec<ParagraphPart> = t
        .segments
        .iter()
        .filter(|s| s.source == source && !s.echo && !s.text.trim().is_empty())
        .filter(|s| s.start_ms >= p.start_ms && s.start_ms < next && s.start_ms <= p.end_ms)
        .map(|s| ParagraphPart {
            start_ms: s.start_ms,
            text: s.text.trim().to_string(),
        })
        .collect();
    parts.sort_by_key(|x| x.start_ms);
    Ok(parts)
}

/// Two voices in a meeting are one person: every paragraph of `from` is
/// given to `into`, and their remembered voices become one.
#[tauri::command]
#[specta::specta]
pub fn merge_meeting_voices(
    app: AppHandle,
    id: String,
    from: u32,
    into: u32,
) -> Result<(), String> {
    let dir = meeting_dir(&app, &id)?;
    let t = pipeline::load(&dir).ok_or("The meeting isn't transcribed yet")?;
    let mut fixes = user_speaker_fixes(&dir);
    for p in transcript::fixed_paragraphs(&t.segments, &fixes) {
        if p.speaker == Some(from) {
            fixes.insert(summary::paragraph_key(&p), into);
        }
    }
    summary::save_json(&dir, SPEAKER_FIXES_FILE, &fixes)?;
    let links: BTreeMap<u32, u32> =
        summary::load_json(&dir, super::remembered::LINKS_FILE).unwrap_or_default();
    if let (Some(&keep), Some(&drop), Ok(data)) = (
        links.get(&into),
        links.get(&from),
        crate::portable::app_data_dir(&app),
    ) {
        let mut store = super::remembered::load_store(&data);
        if super::remembered::merge(&mut store, keep, drop).is_ok() {
            super::remembered::save_store(&data, &store)?;
        }
    }
    super::speakers::record(&dir, &t);
    relearn_voices(&dir);
    let _ = app.emit("meetings-changed", ());
    Ok(())
}

/// What the user has said about a call's voices for learning, and who a
/// 1-on-1 would be with (from the invite, else the one named call voice).
#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct MeetingVouch {
    pub vouch: super::remembered::Vouch,
    pub suggested: Option<String>,
    /// Seconds of clean speech each person has from this meeting.
    pub learned: BTreeMap<String, u64>,
}

#[tauri::command]
#[specta::specta]
pub fn meeting_vouch(app: AppHandle, id: String) -> Result<MeetingVouch, String> {
    let dir = meeting_dir(&app, &id)?;
    let data = crate::portable::app_data_dir(&app).map_err(|e| e.to_string())?;
    let info = read_info(&dir).ok_or("The meeting isn't there any more")?;
    let call_names: Vec<&String> = info
        .speakers
        .iter()
        .chain(&info.app_speakers)
        .filter(|(v, _)| **v >= pipeline::SYSTEM_SPEAKERS)
        .map(|(_, n)| n)
        .collect();
    let suggested = super::speakers::one_on_one_name(&dir).or_else(|| match call_names[..] {
        [one, ..] if call_names.iter().all(|n| n == &one) => Some(one.clone()),
        _ => None,
    });
    Ok(MeetingVouch {
        vouch: super::remembered::vouched(&data, &id),
        suggested,
        learned: super::remembered::learned_from(&data, &id),
    })
}

/// The user vouches for a call's voices (a 1-on-1 with someone, or every
/// call voice named right), or takes that back: what's learned from it is
/// made again, and the meeting's names worked out again with it. Returns
/// seconds of clean speech learned per person. Off the main thread: it
/// reads the meeting's voice windows (no model runs).
#[tauri::command]
#[specta::specta]
pub async fn vouch_meeting(
    app: AppHandle,
    id: String,
    vouch: super::remembered::Vouch,
) -> Result<BTreeMap<String, u64>, String> {
    let dir = meeting_dir(&app, &id)?;
    let data = crate::portable::app_data_dir(&app).map_err(|e| e.to_string())?;
    let manager = app.state::<Arc<MeetingManager>>().inner().clone();
    if manager.is_processing(&id) {
        return Err("Wait until this meeting has finished processing".into());
    }
    let info = read_info(&dir).ok_or("The meeting isn't there any more")?;
    let task_id = id.clone();
    let me = crate::rules::user_name(&crate::settings::get_settings(&app));
    let (names, data) = tauri::async_runtime::spawn_blocking(move || {
        super::remembered::set_vouch(&data, &task_id, info.started_at, &vouch)?;
        Ok::<_, String>((super::speakers::apply(&dir, me.as_deref()), data))
    })
    .await
    .map_err(|e| e.to_string())??;
    manager.update_info(&id, |i| i.app_speakers = names);
    let _ = app.emit("meetings-changed", ());
    Ok(super::remembered::learned_from(&data, &id))
}

/// The user said who spoke: the remembered voices (and the user's own
/// print) are made again from the windows each person actually spoke in.
fn relearn_voices(dir: &Path) {
    if let Some(info) = read_info(dir) {
        super::remembered::apply(dir, &info.speakers, &BTreeMap::new());
    }
}

/// Paragraphs whose speaker Felix isn't sure of, by paragraph key, with why:
/// "overlap" (two people at once), "close" (the voice was hard to tell from
/// another) or "guessed" (named from what was said or by elimination).
#[tauri::command]
#[specta::specta]
pub fn speaker_doubts(app: AppHandle, id: String) -> Result<BTreeMap<String, String>, String> {
    let dir = meeting_dir(&app, &id)?;
    Ok(summary::load_json(&dir, super::speakers::DOUBTS_FILE).unwrap_or_default())
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
    if let Some(info) = read_info(&dir) {
        super::speakers::keep_names(&dir, &info.speakers);
    }
    for file in [
        transcript::FILE,
        summary::CLEANED_FILE,
        super::clues::FILE,
        super::clues::TURNS_FILE,
        super::floor::FILE,
    ] {
        match std::fs::remove_file(dir.join(file)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Couldn't clear the old transcript: {e}")),
        }
    }
    manager.queue(Job::Transcribe(id));
    Ok(())
}

/// Carry on recording into a stopped meeting.
#[tauri::command]
#[specta::specta]
pub async fn resume_meeting(app: AppHandle, id: String) -> Result<MeetingInfo, String> {
    app.state::<Arc<MeetingManager>>().resume(&id)
}

/// Move a meeting's folder to the Trash.
#[tauri::command]
#[specta::specta]
pub fn delete_meeting(app: AppHandle, id: String) -> Result<(), String> {
    let dir = meeting_dir(&app, &id)?;
    let manager = app.state::<Arc<MeetingManager>>();
    if manager
        .with_recording(|_, info| info.id == id)
        .unwrap_or(false)
    {
        return Err("Stop the recording first".into());
    }
    if manager.is_processing(&id) {
        return Err("This meeting is being processed; try again when it's done".into());
    }
    if manager
        .paused
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|p| p.id == id)
    {
        return Err("Stop the recording first".into());
    }
    trash(&dir)?;
    log::info!("Meeting {id} moved to the Trash");
    let _ = app.emit("meetings-changed", ());
    Ok(())
}

#[cfg(target_os = "macos")]
fn trash(path: &Path) -> Result<(), String> {
    use objc2_foundation::{NSFileManager, NSString, NSURL};
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    NSFileManager::defaultManager()
        .trashItemAtURL_resultingItemURL_error(&url, None)
        .map_err(|e| format!("Couldn't move the meeting to the Trash: {e}"))
}

#[cfg(not(target_os = "macos"))]
fn trash(path: &Path) -> Result<(), String> {
    std::fs::remove_dir_all(path).map_err(|e| format!("Couldn't delete the meeting: {e}"))
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
    std::fs::write(&tmp, &notes)
        .and_then(|_| std::fs::rename(&tmp, dir.join(NOTES_FILE)))
        .map_err(|e| format!("Couldn't save the notes: {e}"))?;
    // The notes may be open in the side panel and the Meetings page at once.
    let _ = app.emit("meeting-notes-changed", MeetingNotes { id, notes });
    Ok(())
}

/// The notes of a meeting, when they're saved.
#[derive(Debug, Clone, Serialize, Type)]
pub struct MeetingNotes {
    pub id: String,
    pub notes: String,
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
    fn new_people_go_after_the_voices_used_on_their_track() {
        // New people go after the voices already used on their track.
        let used = [100, 101, 0, super::super::diarize::ME];
        assert_eq!(new_speaker(Source::System, used.into_iter()), 102);
        assert_eq!(new_speaker(Source::Mic, used.into_iter()), 1);
        assert_eq!(new_speaker(Source::System, [].into_iter()), 100);
    }

    #[test]
    fn elapsed_is_formatted_like_a_clock() {
        assert_eq!(format_elapsed(Duration::from_secs(5)), "0:05");
        assert_eq!(format_elapsed(Duration::from_secs(754)), "12:34");
        assert_eq!(format_elapsed(Duration::from_secs(3723)), "1:02:03");
    }

    #[test]
    fn the_menu_bar_shows_minutes_not_a_clock() {
        assert_eq!(menu_bar_elapsed(Duration::from_secs(5)), "Rec 0m");
        assert_eq!(menu_bar_elapsed(Duration::from_secs(1137)), "Rec 18m");
        assert_eq!(menu_bar_elapsed(Duration::from_secs(3723)), "Rec 1h 02m");
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
    /// A model id, or empty for the dictation model.
    pub model: Option<String>,
    /// How the user's name is written (see `AppSettings::user_name`).
    pub user_name: Option<String>,
    pub languages: Option<Vec<String>>,
    pub diarize: Option<bool>,
    pub detect_calls: Option<bool>,
    pub auto_stop: Option<bool>,
    pub max_hours: Option<u32>,
    pub hide_from_screen_share: Option<bool>,
    pub panel: Option<bool>,
    pub notion_sync: Option<bool>,
    /// The Notion integration secret.
    pub notion_token: Option<String>,
    pub notion_parent: Option<String>,
    pub notion_share_parent: Option<String>,
    pub slack_send: Option<bool>,
    /// The Slack workflow webhook link.
    pub slack_webhook: Option<String>,
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
    if let Some(v) = update.model {
        settings.meeting_model = v;
    }
    if let Some(v) = update.user_name {
        settings.user_name = v.trim().to_string();
    }
    if let Some(v) = update.languages {
        settings.meeting_languages = v;
    }
    if let Some(v) = update.diarize {
        settings.meeting_diarize = v;
    }
    if let Some(v) = update.detect_calls {
        settings.meeting_detect_calls = v;
    }
    if let Some(v) = update.auto_stop {
        settings.meeting_auto_stop = v;
    }
    if let Some(v) = update.max_hours {
        settings.meeting_max_hours = v.min(24);
    }
    if let Some(v) = update.panel {
        settings.meeting_panel = v;
    }
    if let Some(v) = update.notion_sync {
        settings.notion_sync = v;
    }
    if let Some(v) = update.notion_token {
        settings.notion_token = v.trim().to_string();
    }
    if let Some(v) = update.notion_parent {
        settings.notion_parent = v.trim().to_string();
    }
    if let Some(v) = update.notion_share_parent {
        settings.notion_share_parent = v.trim().to_string();
    }
    if let Some(v) = update.slack_send {
        settings.slack_send = v;
    }
    if let Some(v) = update.slack_webhook {
        settings.slack_webhook = v.trim().to_string();
    }
    let hide = update.hide_from_screen_share;
    if let Some(v) = hide {
        settings.hide_from_screen_share = v;
    }
    crate::settings::write_settings(&app, settings);
    if hide.is_some() {
        super::watch::apply_screen_share(&app);
    }
    Ok(())
}

/// What the summary is asked for when the user hasn't written their own.
#[tauri::command]
#[specta::specta]
pub fn default_meeting_summary_prompt() -> String {
    summary::DEFAULT_SUMMARY_GUIDANCE.to_string()
}
