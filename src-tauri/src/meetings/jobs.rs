//! The background work after a meeting: transcribe it, then tidy the
//! transcript and write the summary. One job at a time on one worker thread,
//! so meetings never compete with each other (and dictation always goes
//! first, see [`MeetingManager::wait_for_turn`]). Each step saves as it goes
//! and its status lives in `meeting.json`, so a quit resumes where it was.

use super::llm::Llm;
use super::manager::{
    meta_line, paragraphs_of, JobStatus, MeetingInfo, MeetingManager, Stage, TranscribeProgress,
    TranscriptStatus, NOTES_FILE,
};
use super::pipeline::{self, Stopped};
use super::remote::Remote;
use super::summary::{self, Cleaned, Summary};
use super::transcript::timestamp;
use crate::managers::audio::AudioRecordingManager;
use crate::managers::transcription::TranscriptionManager;
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use tauri::Manager;

/// Longest the model may take to load before a transcription gives up.
const MODEL_LOAD_WAIT: Duration = Duration::from_secs(180);
/// How often a waiting transcription checks whether dictation is done.
const TURN_POLL: Duration = Duration::from_millis(250);
/// Attempts per chunk before the transcription is marked failed.
const CHUNK_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Job {
    /// Transcribe, then summarise.
    Transcribe(String),
    /// Clean up and summarise an already transcribed meeting.
    Summarize(String),
}

impl Job {
    fn id(&self) -> &str {
        match self {
            Job::Transcribe(id) | Job::Summarize(id) => id,
        }
    }
}

impl MeetingManager {
    /// Queue work that was waiting or running when Handy last quit.
    pub fn resume_transcriptions(&self) {
        for info in self.list() {
            if matches!(
                info.transcript,
                Some(TranscriptStatus::Queued | TranscriptStatus::Transcribing)
            ) {
                self.queue(Job::Transcribe(info.id));
            } else if matches!(info.summary, Some(JobStatus::Queued | JobStatus::Running)) {
                self.queue(Job::Summarize(info.id));
            }
        }
    }

    /// Queue a job, unless the same one is already waiting or running.
    pub fn queue(&self, job: Job) {
        if !self
            .queued
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(job.clone())
        {
            return;
        }
        self.update_info(job.id(), |i| match &job {
            Job::Transcribe(_) => {
                i.transcript = Some(TranscriptStatus::Queued);
                i.transcript_error = None;
            }
            Job::Summarize(_) => {
                i.summary = Some(JobStatus::Queued);
                i.summary_error = None;
            }
        });
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let tx = jobs.get_or_insert_with(|| {
            let (tx, rx) = mpsc::channel::<Job>();
            let app = self.app.clone();
            let spawned = std::thread::Builder::new()
                .name("meeting-jobs".into())
                .spawn(move || {
                    for job in rx {
                        if let Some(m) = app.try_state::<Arc<MeetingManager>>() {
                            m.run(&job);
                        }
                    }
                });
            if let Err(e) = spawned {
                log::error!("Couldn't start the meeting worker: {e}");
            }
            tx
        });
        if tx.send(job).is_err() {
            log::error!("The meeting worker is gone");
        }
    }

    pub(super) fn set_progress(&self, progress: Option<TranscribeProgress>) {
        *self.progress.lock().unwrap_or_else(|e| e.into_inner()) = progress;
        let _ = tauri::Emitter::emit(&self.app, "meeting-state", self.state());
    }

    fn progress_to(&self, id: &str, stage: Stage, done: usize, total: usize) {
        self.set_progress(Some(TranscribeProgress {
            id: id.to_string(),
            stage,
            done: done as u32,
            total: total as u32,
        }));
    }

    fn run(&self, job: &Job) {
        let id = job.id();
        let summarize = match job {
            Job::Transcribe(_) => self.transcribe(id),
            Job::Summarize(_) => true,
        };
        if summarize {
            self.summarize(id);
        }
        self.queued
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(job);
        self.set_progress(None);
    }

    /// The speaker model, downloaded (26.5 MB) the first time it's needed.
    fn speaker_model(&self, id: &str) -> Result<std::path::PathBuf, String> {
        use super::diarize::{MODEL_BYTES, MODEL_FILE, MODEL_URL};
        use futures_util::StreamExt;
        use std::io::Write;
        let dir = crate::portable::app_data_dir(&self.app)
            .map_err(|e| e.to_string())?
            .join("models");
        let path = dir.join(MODEL_FILE);
        if std::fs::metadata(&path).is_ok_and(|m| m.len() == MODEL_BYTES) {
            return Ok(path);
        }
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        log::info!("Downloading the speaker model from {MODEL_URL}");
        let partial = dir.join(format!("{MODEL_FILE}.partial"));
        tauri::async_runtime::block_on(async {
            let response = reqwest::get(MODEL_URL)
                .await
                .and_then(|r| r.error_for_status())
                .map_err(|e| format!("Couldn't download the speaker model: {e}"))?;
            let mut file = std::fs::File::create(&partial).map_err(|e| e.to_string())?;
            let mut stream = response.bytes_stream();
            let mut got = 0usize;
            while let Some(bytes) = stream.next().await {
                let bytes =
                    bytes.map_err(|e| format!("The speaker model download stopped: {e}"))?;
                file.write_all(&bytes).map_err(|e| e.to_string())?;
                got += bytes.len();
                self.progress_to(id, Stage::Identifying, got, MODEL_BYTES as usize);
            }
            file.flush().map_err(|e| e.to_string())?;
            if got as u64 != MODEL_BYTES {
                return Err(format!(
                    "The speaker model download was {got} bytes, expected {MODEL_BYTES}"
                ));
            }
            Ok::<(), String>(())
        })
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&partial);
        })?;
        std::fs::rename(&partial, &path).map_err(|e| e.to_string())?;
        Ok(path)
    }

    /// Transcribe one meeting. True if it's done and can be summarised.
    fn transcribe(&self, id: &str) -> bool {
        match self.transcribe_inner(id) {
            Ok(()) => {
                log::info!("Meeting {id} transcribed");
                self.update_info(id, |i| i.transcript = Some(TranscriptStatus::Done));
                true
            }
            Err(Stopped::Cancelled) => false,
            Err(Stopped::Failed(e)) => {
                log::error!("Meeting {id} couldn't be transcribed: {e}");
                self.update_info(id, |i| {
                    i.transcript = Some(TranscriptStatus::Failed);
                    i.transcript_error = Some(e);
                });
                false
            }
        }
    }

    fn transcribe_inner(&self, id: &str) -> Result<(), Stopped> {
        let dir = self.dir_of(id)?;
        let Some(info) =
            self.update_info(id, |i| i.transcript = Some(TranscriptStatus::Transcribing))
        else {
            return Err(Stopped::Cancelled);
        };
        let settings = crate::rules::with_rules(crate::settings::get_settings(&self.app));
        let level = Some(super::level::LevelSettings::new(
            settings.meeting_input_boost_db,
            settings.meeting_auto_gain,
        ));
        let progress = |step| match step {
            pipeline::Step::Identifying => self.progress_to(id, Stage::Identifying, 0, 0),
            pipeline::Step::Transcribing { done, total } => {
                self.progress_to(id, Stage::Transcribing, done, total)
            }
        };
        let speaker_model = if info.mode == super::MeetingMode::InPerson && settings.meeting_diarize
        {
            match self.speaker_model(id) {
                Ok(path) => Some(path),
                Err(e) => {
                    // Transcribe anyway, without speakers.
                    log::warn!("No speaker model, transcribing without speakers: {e}");
                    None
                }
            }
        } else {
            None
        };
        let vad = self
            .app
            .path()
            .resolve(
                "resources/models/silero_vad_v4.onnx",
                tauri::path::BaseDirectory::Resource,
            )
            .map_err(|e| format!("Couldn't find the VAD model: {e}"))?;
        // A provider's API, if one is chosen: no need to share the local engine.
        if let Some(remote) = Remote::from_settings(&settings)? {
            let engine = format!("{} {}", remote.name, remote.model);
            log::info!("Transcribing meeting {id} with {engine}");
            return pipeline::run(
                &dir,
                info.mode,
                &vad,
                level,
                speaker_model.as_deref(),
                &engine,
                |audio| {
                    tauri::async_runtime::block_on(remote.transcribe(&audio))
                        .map_err(Stopped::Failed)
                },
                progress,
            )
            .map(|_| ());
        }

        let tm = self
            .app
            .try_state::<Arc<TranscriptionManager>>()
            .ok_or_else(|| "Transcription isn't available".to_string())?
            .inner()
            .clone();
        let engine = format!("local {}", settings.selected_model);
        log::info!("Transcribing meeting {id} with {engine}");
        tm.set_meeting_job(true);
        let result = pipeline::run(
            &dir,
            info.mode,
            &vad,
            level,
            speaker_model.as_deref(),
            &engine,
            |audio| self.transcribe_chunk(&tm, audio),
            progress,
        );
        tm.set_meeting_job(false);
        tm.maybe_unload_immediately("meeting transcription");
        result.map(|_| ())
    }

    /// Transcribe one chunk when dictation isn't using the model. Dictation
    /// always goes first: this waits while a recording is in progress.
    fn transcribe_chunk(
        &self,
        tm: &TranscriptionManager,
        audio: Vec<f32>,
    ) -> Result<String, Stopped> {
        let mut last_error = String::new();
        for _ in 0..CHUNK_ATTEMPTS {
            self.wait_for_turn(tm)?;
            match tm.transcribe_for_meeting(audio.clone()) {
                Ok(text) => return Ok(text),
                Err(e) => {
                    log::warn!("Meeting chunk failed, retrying: {e}");
                    last_error = e.to_string();
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
        Err(Stopped::Failed(last_error))
    }

    fn wait_for_turn(&self, tm: &TranscriptionManager) -> Result<(), Stopped> {
        let recording = || {
            self.app
                .try_state::<Arc<AudioRecordingManager>>()
                .is_some_and(|a| a.is_recording())
        };
        let mut loading_since: Option<Instant> = None;
        loop {
            if !recording() {
                if tm.is_idle_for_meeting() {
                    return Ok(());
                }
                if !tm.is_model_loaded() {
                    let since = *loading_since.get_or_insert_with(Instant::now);
                    if since.elapsed() > MODEL_LOAD_WAIT {
                        return Err(Stopped::Failed(
                            "The transcription model didn't load".into(),
                        ));
                    }
                    tm.initiate_model_load();
                }
            }
            std::thread::sleep(TURN_POLL);
        }
    }

    /// Tidy the transcript and write the summary.
    fn summarize(&self, id: &str) {
        let Some(info) = self.update_info(id, |i| {
            i.summary = Some(JobStatus::Running);
            i.summary_error = None;
        }) else {
            return;
        };
        match tauri::async_runtime::block_on(self.summarize_inner(id, &info)) {
            Ok(summary) => {
                log::info!("Meeting {id} summarised by {}", summary.model);
                self.update_info(id, |i| {
                    i.summary = Some(JobStatus::Done);
                    if (i.title.is_none() || i.title_is_auto) && !summary.title.is_empty() {
                        i.title = Some(summary.title.clone());
                        i.title_is_auto = true;
                    }
                });
            }
            Err(e) => {
                log::error!("Meeting {id} couldn't be summarised: {e}");
                self.update_info(id, |i| {
                    i.summary = Some(JobStatus::Failed);
                    i.summary_error = Some(e);
                });
            }
        }
    }

    async fn summarize_inner(&self, id: &str, info: &MeetingInfo) -> Result<Summary, String> {
        let dir = self.dir_of(id)?;
        let settings = crate::rules::with_rules(crate::settings::get_settings(&self.app));
        let llm = Llm::from_settings(&settings)?;
        let transcript = pipeline::load(&dir)
            .filter(|t| t.complete)
            .ok_or("The meeting isn't transcribed yet")?;

        if settings.meeting_cleanup {
            let raw = super::transcript::paragraphs(&transcript.segments);
            let mut cleaned: Cleaned =
                summary::load_json(&dir, summary::CLEANED_FILE).unwrap_or_default();
            let label = |p: &super::transcript::Paragraph| {
                info.speaker_label(p).unwrap_or_else(|| "Speaker".into())
            };
            let error = summary::clean(
                &llm,
                &raw,
                &label,
                &settings.custom_words,
                &mut cleaned,
                |done, total| self.progress_to(id, Stage::CleaningUp, done, total),
            )
            .await;
            summary::save_json(&dir, summary::CLEANED_FILE, &cleaned)?;
            if let Some(e) = error {
                // The raw text stands in for what failed; go on to the summary.
                log::warn!("Meeting {id}: some of the transcript wasn't cleaned up: {e}");
            }
            let _ = tauri::Emitter::emit(&self.app, "meetings-changed", ());
        }

        let lines: Vec<String> = paragraphs_of(&dir, &transcript)
            .iter()
            .map(|p| {
                let at = timestamp(p.start_ms);
                match info.speaker_label(p) {
                    Some(who) => format!("[{at}] {who}: {}", p.text),
                    None => format!("[{at}] {}", p.text),
                }
            })
            .collect();
        if lines.is_empty() {
            return Err("No speech was found in the recording".into());
        }
        let notes = std::fs::read_to_string(dir.join(NOTES_FILE)).unwrap_or_default();
        let about = about_meeting(
            info,
            transcript.segments.iter().any(|s| s.speaker.is_some()),
        );
        self.progress_to(id, Stage::Summarizing, 0, 1);
        let summary = summary::summarize(
            &llm,
            &settings.meeting_summary_prompt,
            &about,
            &notes,
            &lines,
            |done, total| self.progress_to(id, Stage::Summarizing, done, total),
        )
        .await?;
        summary::save_json(&dir, summary::SUMMARY_FILE, &summary)?;
        Ok(summary)
    }
}

/// What the summariser is told about the meeting itself.
fn about_meeting(info: &MeetingInfo, has_speakers: bool) -> String {
    let who = match info.mode {
        super::MeetingMode::Call => {
            "A video call. \"Me\" is the user (their mic); \"Them\" is everyone else on the call (the Mac's audio), possibly several people."
        }
        super::MeetingMode::InPerson if has_speakers => {
            "An in-person meeting recorded on one mic; voices were told apart automatically (\"Speaker 1\", \"Speaker 2\", or a name the user gave), so labels can be wrong."
        }
        super::MeetingMode::InPerson => {
            "An in-person meeting recorded on one mic; speakers aren't labelled."
        }
    };
    let title = match (&info.title, info.title_is_auto) {
        (Some(t), false) => format!("\nThe user titled it: {t}"),
        _ => String::new(),
    };
    format!("{}\n{who}{title}", meta_line(info))
}
