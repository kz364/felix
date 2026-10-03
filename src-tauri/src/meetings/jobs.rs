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
use super::transcript::{timestamp, Paragraph};
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
/// A chunk whose decode ran away (looping until the token cap) is split in
/// two and tried again, down to pieces this long (samples, 2 s); a piece
/// that still runs away is left out rather than failing the meeting.
const MIN_SPLIT_SAMPLES: usize = 32_000;
/// A frame for finding the quietest place to split (30 ms).
const SPLIT_FRAME: usize = 480;

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

    /// The speaker model, downloaded (39.6 MB) the first time it's needed,
    /// and the segmentation model (6 MB) next to it for turns and overlaps;
    /// without that one, speakers are still told apart, just less sharply.
    fn speaker_model(&self, id: &str) -> Result<std::path::PathBuf, String> {
        use super::diarize::{MODEL_BYTES, MODEL_FILE, MODEL_URL};
        let dir = crate::portable::app_data_dir(&self.app)
            .map_err(|e| e.to_string())?
            .join("models");
        let path = dir.join(MODEL_FILE);
        self.download(id, MODEL_URL, &path, MODEL_BYTES, "speaker model")?;
        let seg = dir
            .join(super::segment::MODEL_DIR)
            .join(super::segment::MODEL_FILE);
        if let Err(e) = self.download(
            id,
            super::segment::MODEL_URL,
            &seg,
            super::segment::MODEL_BYTES,
            "segmentation model",
        ) {
            log::warn!("{e}");
        }
        Ok(path)
    }

    /// Download `url` to `path` unless it's there with the right size.
    fn download(
        &self,
        id: &str,
        url: &str,
        path: &std::path::Path,
        bytes: u64,
        what: &str,
    ) -> Result<(), String> {
        use futures_util::StreamExt;
        use std::io::Write;
        if std::fs::metadata(path).is_ok_and(|m| m.len() == bytes) {
            return Ok(());
        }
        let dir = path.parent().ok_or("No models folder")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        log::info!("Downloading the {what} from {url}");
        let partial = path.with_extension("partial");
        tauri::async_runtime::block_on(async {
            let response = reqwest::get(url)
                .await
                .and_then(|r| r.error_for_status())
                .map_err(|e| format!("Couldn't download the {what}: {e}"))?;
            let mut file = std::fs::File::create(&partial).map_err(|e| e.to_string())?;
            let mut stream = response.bytes_stream();
            let mut got = 0usize;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| format!("The {what} download stopped: {e}"))?;
                file.write_all(&chunk).map_err(|e| e.to_string())?;
                got += chunk.len();
                self.progress_to(id, Stage::Identifying, got, bytes as usize);
            }
            file.flush().map_err(|e| e.to_string())?;
            if got as u64 != bytes {
                return Err(format!(
                    "The {what} download was {got} bytes, expected {bytes}"
                ));
            }
            Ok::<(), String>(())
        })
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&partial);
        })?;
        std::fs::rename(&partial, path).map_err(|e| e.to_string())
    }

    /// The user's word corrections, as dictation applies them: vocabulary
    /// spelling, taught words and replacements ("codecs" → "Codex").
    /// Sound-alikes are left to cleanup, which sees the conversation.
    /// Transcribed again: the names the user gave go to the new voices.
    fn carry_names(&self, id: &str) {
        let Ok(dir) = self.dir_of(id) else { return };
        let Some(t) = pipeline::load(&dir) else {
            return;
        };
        if let Some(names) = super::speakers::carry_names(&dir, &t) {
            log::info!("Meeting {id}: {} names carried over", names.len());
            self.update_info(id, |i| i.speakers = names);
        }
    }

    fn apply_corrections(&self, id: &str) {
        let Ok(dir) = self.dir_of(id) else { return };
        let Some(mut t) = pipeline::load(&dir) else {
            return;
        };
        let settings = crate::rules::with_rules(crate::settings::get_settings(&self.app));
        let mut changed = 0;
        for seg in &mut t.segments {
            let text = summary::corrected(&seg.text, &settings);
            if text != seg.text {
                seg.text = text;
                changed += 1;
            }
        }
        if changed > 0 {
            log::info!("Meeting {id}: corrected words in {changed} segments");
            if let Err(e) = pipeline::save(&dir, &t) {
                log::warn!("{e}");
            }
        }
    }

    /// Transcribe one meeting. True if it's done and can be summarised.
    fn transcribe(&self, id: &str) -> bool {
        match self.transcribe_inner(id) {
            Ok(()) => {
                log::info!("Meeting {id} transcribed");
                self.apply_corrections(id);
                self.carry_names(id);
                let names = self
                    .dir_of(id)
                    .map(|dir| super::speakers::apply(&dir))
                    .unwrap_or_default();
                self.update_info(id, |i| {
                    i.transcript = Some(TranscriptStatus::Done);
                    i.app_speakers = names;
                });
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
        // Resumed since it was queued: it's queued again when it stops.
        if super::manager::read_info(&dir)
            .is_none_or(|i| i.status == super::manager::MeetingStatus::Recording)
        {
            return Err(Stopped::Cancelled);
        }
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
        let speaker_model = if settings.meeting_diarize {
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
        // The user's voice on this mic, once they've recorded a print.
        let voiceprint = crate::portable::app_data_dir(&self.app)
            .ok()
            .and_then(|dir| super::voiceprint::load_or_nearest(&dir, &info.mic));
        // The languages it's in: chosen, found before, or listened for now.
        let models = self
            .app
            .try_state::<Arc<crate::managers::model::ModelManager>>()
            .map(|m| m.get_available_models())
            .unwrap_or_default();
        let languages = if !settings.meeting_languages.is_empty() {
            settings.meeting_languages.clone()
        } else if !info.languages.is_empty() {
            info.languages.clone()
        } else {
            self.languages_in(&dir, &models)
        };
        if !languages.is_empty() {
            log::info!("Meeting {id} is in {languages:?}");
            let found = languages.clone();
            self.update_info(id, |i| i.languages = found);
        }
        // A provider's API, if one is chosen: no need to share the local engine.
        if let Some(remote) = Remote::from_settings(&settings)? {
            let remote = remote.for_languages(&languages);
            let engine = format!("{} {}", remote.name, remote.model);
            log::info!("Transcribing meeting {id} with {engine}");
            return pipeline::run(
                &dir,
                info.mode,
                &vad,
                level,
                speaker_model.as_deref(),
                voiceprint.as_deref(),
                &engine,
                |audio| {
                    tauri::async_runtime::block_on(remote.transcribe(&audio))
                        .map_err(Stopped::Failed)
                },
                progress,
            )
            .map(|_| ());
        }

        // The dictation model doesn't know the meeting's languages: use a
        // downloaded one that does, loaded just for this meeting.
        match super::language::route(&models, &settings.selected_model, &languages) {
            super::language::Route::Other(model_id) => {
                return self.transcribe_with_own_model(
                    id,
                    &dir,
                    &model_id,
                    &languages,
                    info.mode,
                    &vad,
                    level,
                    speaker_model.as_deref(),
                    voiceprint.as_deref(),
                    progress,
                );
            }
            super::language::Route::NoModel => log::warn!(
                "No downloaded model knows all of {languages:?}; transcribing meeting {id} with {} anyway",
                settings.selected_model
            ),
            super::language::Route::Current => {}
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
            voiceprint.as_deref(),
            &engine,
            |audio| self.transcribe_chunk(&tm, audio),
            progress,
        );
        tm.set_meeting_job(false);
        tm.maybe_unload_immediately("meeting transcription");
        result.map(|_| ())
    }

    /// The languages heard in the recording, from a few stretches of each
    /// track, by the best downloaded model that can tell. Empty if none can.
    fn languages_in(
        &self,
        dir: &std::path::Path,
        models: &[crate::managers::model::ModelInfo],
    ) -> Vec<String> {
        let Some(detector) = super::language::detector(models) else {
            log::info!("No model to find a meeting's languages with");
            return vec![];
        };
        let Some(path) = self
            .app
            .try_state::<Arc<crate::managers::model::ModelManager>>()
            .and_then(|m| m.get_model_path(&detector.id).ok())
        else {
            return vec![];
        };
        let started = Instant::now();
        let result = self.without_dictation_model(|| -> Result<Vec<String>, String> {
            let model = transcribe_cpp::Model::load(&path).map_err(|e| e.to_string())?;
            let mut session = model.session().map_err(|e| e.to_string())?;
            let mut hits = Vec::new();
            for track in ["mic.wav", "system.wav"] {
                let Ok(audio) = crate::audio_toolkit::read_wav_samples(dir.join(track)) else {
                    continue;
                };
                for span in super::language::sample_spans(&audio) {
                    let t = session
                        .run(&audio[span], &transcribe_cpp::RunOptions::default())
                        .map_err(|e| e.to_string())?;
                    if let Some(l) = t.language.filter(|_| !t.text.trim().is_empty()) {
                        hits.push(l.split(['-', '_']).next().unwrap_or(&l).to_lowercase());
                    }
                }
            }
            Ok(super::language::languages_heard(&hits))
        });
        match result {
            Ok(languages) => {
                log::info!(
                    "Found {languages:?} in the meeting with {} in {:?}",
                    detector.id,
                    started.elapsed()
                );
                languages
            }
            Err(e) => {
                log::warn!("Couldn't find the meeting's languages: {e}");
                vec![]
            }
        }
    }

    /// Run `work`, which loads a model other than dictation's, with the
    /// dictation model out of memory so the two aren't loaded at once; it's
    /// loaded again afterwards. Left alone while dictation is using it, and
    /// dictation meanwhile loads it on demand as after an idle unload.
    fn without_dictation_model<T>(&self, work: impl FnOnce() -> T) -> T {
        let recording = self
            .app
            .try_state::<Arc<AudioRecordingManager>>()
            .is_some_and(|a| a.is_recording());
        let set_aside = self
            .app
            .try_state::<Arc<TranscriptionManager>>()
            .map(|tm| tm.inner().clone())
            .filter(|tm| !recording && tm.is_idle_for_meeting())
            .filter(|tm| match tm.unload_model() {
                Ok(()) => true,
                Err(e) => {
                    log::warn!("Couldn't unload the dictation model: {e}");
                    false
                }
            });
        if set_aside.is_some() {
            log::info!("Unloaded the dictation model while a meeting uses another");
        }
        let result = work();
        if let Some(tm) = set_aside {
            tm.initiate_model_load();
        }
        result
    }

    /// Transcribe with a model other than dictation's, loaded for this job.
    #[allow(clippy::too_many_arguments)]
    fn transcribe_with_own_model(
        &self,
        id: &str,
        dir: &std::path::Path,
        model_id: &str,
        languages: &[String],
        mode: super::MeetingMode,
        vad: &std::path::Path,
        level: Option<super::level::LevelSettings>,
        speaker_model: Option<&std::path::Path>,
        voiceprint: Option<&[f32]>,
        progress: impl FnMut(pipeline::Step),
    ) -> Result<(), Stopped> {
        let path = self
            .app
            .try_state::<Arc<crate::managers::model::ModelManager>>()
            .ok_or_else(|| "Models aren't available".to_string())?
            .get_model_path(model_id)
            .map_err(|e| e.to_string())?;
        self.without_dictation_model(|| {
            let model = transcribe_cpp::Model::load(&path)
                .map_err(|e| format!("Couldn't load {model_id}: {e}"))?;
            let mut session = model.session().map_err(|e| e.to_string())?;
            let options = transcribe_cpp::RunOptions {
                // One language is named; a mix is left to the model, chunk by chunk.
                language: (languages.len() == 1).then(|| languages[0].clone()),
                ..Default::default()
            };
            let engine = format!("local {model_id} {}", languages.join("+"));
            log::info!("Transcribing meeting {id} with {engine}");
            let mut run = |audio: &[f32]| {
                session
                    .run(audio, &options)
                    .map(|t| t.text)
                    .map_err(|e| e.to_string())
            };
            pipeline::run(
                dir,
                mode,
                vad,
                level,
                speaker_model,
                voiceprint,
                &engine,
                |audio| split_runaways(&mut run, &audio),
                progress,
            )
            .map(|_| ())
        })
    }

    /// Transcribe one chunk when dictation isn't using the model. Dictation
    /// always goes first: this waits while a recording is in progress.
    fn transcribe_chunk(
        &self,
        tm: &TranscriptionManager,
        audio: Vec<f32>,
    ) -> Result<String, Stopped> {
        match self.transcribe_once(tm, &audio) {
            Err(Stopped::Failed(e)) if ran_away(&e) => {
                if audio.len() < 2 * MIN_SPLIT_SAMPLES {
                    log::warn!(
                        "Leaving out {:.1} s of a meeting the model keeps looping on",
                        audio.len() as f32 / 16_000.0
                    );
                    return Ok(String::new());
                }
                let cut = quietest_split(&audio);
                log::warn!("Meeting chunk ran away; splitting it at {cut} and trying again");
                let first = self.transcribe_chunk(tm, audio[..cut].to_vec())?;
                let second = self.transcribe_chunk(tm, audio[cut..].to_vec())?;
                Ok(format!("{} {}", first.trim(), second.trim())
                    .trim()
                    .to_string())
            }
            other => other,
        }
    }

    fn transcribe_once(&self, tm: &TranscriptionManager, audio: &[f32]) -> Result<String, Stopped> {
        let mut last_error = String::new();
        for _ in 0..CHUNK_ATTEMPTS {
            self.wait_for_turn(tm)?;
            match tm.transcribe_for_meeting(audio.to_vec()) {
                Ok(text) => return Ok(text),
                // The same audio decodes the same way: no point retrying.
                Err(e) if ran_away(&e.to_string()) => return Err(Stopped::Failed(e.to_string())),
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
        // Who held the floor first: it can tell the voices apart again,
        // which the cleanup's paragraphs and the clues should start from.
        let updated = self.read_floor(id, &dir, &llm, &transcript, info).await;
        let info = updated.as_ref().unwrap_or(info);
        let transcript = match updated {
            Some(_) => pipeline::load(&dir).unwrap_or(transcript),
            None => transcript,
        };

        // Tidying the text and looking for clues to who's who are separate
        // requests to the model: both at once. The clues are found once per
        // transcript; then the names are worked out again with them.
        let cleanup = async {
            if !settings.meeting_cleanup {
                return Ok(());
            }
            let raw = super::transcript::fixed_paragraphs(
                &transcript.segments,
                &super::manager::speaker_fixes(&dir),
            );
            let mut cleaned: Cleaned =
                summary::load_json(&dir, summary::CLEANED_FILE).unwrap_or_default();
            let reused = super::live::reuse_cleaned(&dir, &transcript.segments, &raw, &mut cleaned);
            if reused > 0 {
                log::info!("Meeting {id}: {reused} paragraphs were tidied while recording");
            }
            let label = |p: &super::transcript::Paragraph| {
                info.speaker_label(p).unwrap_or_else(|| "Speaker".into())
            };
            let error = summary::clean(
                &llm,
                &raw,
                &label,
                &settings.custom_words,
                &settings.soundalikes,
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
            Ok::<(), String>(())
        };
        let clues = self.find_clues(id, &dir, &llm, &transcript, info);
        let (cleaned, clued) = futures_util::join!(cleanup, clues);
        cleaned?;
        let info = &clued.unwrap_or_else(|| info.clone());

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
        let about = about_meeting(info, &paragraphs_of(&dir, &transcript));
        let context = summary::SummaryContext {
            previous: self.previous_in_series(info),
            british: tauri_plugin_os::locale().is_some_and(|l| summary::spells_british(&l)),
        };
        self.progress_to(id, Stage::Summarizing, 0, 1);
        let summary = summary::summarize(
            &llm,
            &settings.meeting_summary_prompt,
            &about,
            &notes,
            &lines,
            &context,
            |done, total| self.progress_to(id, Stage::Summarizing, done, total),
        )
        .await?;
        summary::save_json(&dir, summary::SUMMARY_FILE, &summary)?;
        Ok(summary)
    }
}

impl MeetingManager {
    /// Find clues (if not found yet) and name voices again with them. The
    /// meeting as it is after, if anything changed.
    /// Name lines from the conversation ([`super::floor`]), once per
    /// transcript, and work the voices and names out again with them. The
    /// meeting as it now is, when that changed anything.
    async fn read_floor(
        &self,
        id: &str,
        dir: &std::path::Path,
        llm: &Llm,
        transcript: &super::transcript::Transcript,
        info: &MeetingInfo,
    ) -> Option<MeetingInfo> {
        if dir.join(super::floor::FILE).exists() {
            return None;
        }
        let careful = llm.careful();
        let label = |s: &super::transcript::Segment| {
            let p = Paragraph {
                source: s.source,
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                text: String::new(),
                raw: None,
                speaker: s.speaker,
            };
            info.speaker_label(&p).unwrap_or_else(|| "Speaker".into())
        };
        let end = info.ended_at.unwrap_or(info.started_at + 60 * 60 * 1000);
        let invite = super::calendar::for_meeting(dir, info.started_at, end).unwrap_or_default();
        let turns = match super::floor::find(
            careful.as_ref().unwrap_or(llm),
            &transcript.segments,
            &label,
            &invite.attendees,
            invite.me.as_deref(),
            summary::CLEANUP_EFFORT,
        )
        .await
        {
            Ok(t) => t,
            Err(e) => {
                log::warn!("Meeting {id}: couldn't read who held the floor: {e}");
                return None;
            }
        };
        log::info!(
            "Meeting {id}: {} lines named from the conversation",
            turns.len()
        );
        summary::save_json(dir, super::floor::FILE, &turns).ok()?;
        if turns.is_empty() {
            return None;
        }
        let names = super::speakers::apply(dir);
        self.update_info(id, |i| i.app_speakers = names);
        let _ = tauri::Emitter::emit(&self.app, "meetings-changed", ());
        super::manager::read_info(dir)
    }

    async fn find_clues(
        &self,
        id: &str,
        dir: &std::path::Path,
        llm: &Llm,
        transcript: &super::transcript::Transcript,
        info: &MeetingInfo,
    ) -> Option<MeetingInfo> {
        if dir.join(super::clues::FILE).exists() {
            return None;
        }
        let paragraphs = paragraphs_of(dir, transcript);
        let label = |p: &Paragraph| info.speaker_label(p).unwrap_or_else(|| "Speaker".into());
        let found = super::clues::find(llm, &paragraphs, &label).await;
        let (clues, turns) = match found {
            Ok(c) => c,
            Err(e) => {
                log::warn!("Meeting {id}: couldn't look for speaker clues: {e}");
                return None;
            }
        };
        log::info!(
            "Meeting {id}: {} speaker clues, {} turns given another speaker",
            clues.len(),
            turns.len()
        );
        summary::save_json(dir, super::clues::TURNS_FILE, &turns).ok()?;
        summary::save_json(dir, super::clues::FILE, &clues).ok()?;
        let names = super::speakers::apply(dir);
        if names == info.app_speakers {
            if !turns.is_empty() {
                let _ = tauri::Emitter::emit(&self.app, "meetings-changed", ());
            }
            return None;
        }
        self.update_info(id, |i| i.app_speakers = names.clone());
        let _ = tauri::Emitter::emit(&self.app, "meetings-changed", ());
        super::manager::read_info(dir)
    }

    /// The last summarised meeting the user gave the same title, as context
    /// for a recurring meeting.
    fn previous_in_series(&self, info: &MeetingInfo) -> Option<String> {
        let title = info.title.as_deref().filter(|_| !info.title_is_auto)?;
        let key = title.trim().to_lowercase();
        self.list().into_iter().find_map(|m| {
            let same = m.started_at < info.started_at
                && !m.title_is_auto
                && m.title
                    .as_deref()
                    .is_some_and(|t| t.trim().to_lowercase() == key);
            if !same {
                return None;
            }
            let dir = self.dir_of(&m.id).ok()?;
            let previous: Summary = summary::load_json(&dir, summary::SUMMARY_FILE)?;
            let when = chrono::DateTime::from_timestamp_millis(m.started_at)?
                .with_timezone(&chrono::Local)
                .format("%-d %B %Y")
                .to_string();
            Some(summary::previous_block(title, &when, &previous))
        })
    }
}

/// What the summariser (and the in-meeting questions) are told about the
/// meeting itself: when, who took part, and how to read "we".
pub(super) fn about_meeting(info: &MeetingInfo, paragraphs: &[Paragraph]) -> String {
    let has_speakers = paragraphs.iter().any(|p| p.speaker.is_some());
    let who = match info.mode {
        super::MeetingMode::Call if has_speakers => {
            "A video call. \"Me\" is the user (their mic). The other side (the Mac's audio) was told apart by voice or named by the call app (\"Them 1\", \"Them 2\", or a name); \"Speaker N\" is someone in the room with the user. Labels can be wrong."
        }
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
    let participants = info.participants(paragraphs);
    let block = if participants.is_empty() {
        String::new()
    } else {
        format!("\nParticipants: {}", participants.join(", "))
    };
    let we = match info.mode {
        super::MeetingMode::Call => {
            "\n\"We\" means the user's side (Me and anyone in the room with them), not the whole call. An action item is the user's only if they committed to it themselves; otherwise it belongs to whoever took it on."
        }
        super::MeetingMode::InPerson => {
            "\nAn action item is the user's only if they committed to it themselves; otherwise it belongs to whoever took it on."
        }
    };
    format!("{}\n{who}{title}{block}{we}", meta_line(info))
}

/// Transcribe a chunk; one the model runs away on is split at its quietest
/// point and tried again in halves.
pub fn split_runaways(
    run: &mut dyn FnMut(&[f32]) -> Result<String, String>,
    audio: &[f32],
) -> Result<String, Stopped> {
    match run(audio) {
        Ok(text) => Ok(text),
        Err(e) if ran_away(&e) => {
            if audio.len() < 2 * MIN_SPLIT_SAMPLES {
                log::warn!(
                    "Leaving out {:.1} s of a meeting the model keeps looping on",
                    audio.len() as f32 / 16_000.0
                );
                return Ok(String::new());
            }
            let cut = quietest_split(audio);
            let first = split_runaways(run, &audio[..cut])?;
            let second = split_runaways(run, &audio[cut..])?;
            Ok(format!("{} {}", first.trim(), second.trim())
                .trim()
                .to_string())
        }
        Err(e) => Err(Stopped::Failed(e)),
    }
}

/// The model decoded until its token cap (usually looping on a phrase).
fn ran_away(error: &str) -> bool {
    error.contains("output truncated")
}

/// Where to split a chunk: the quietest frame in its middle half, so no
/// word is cut.
fn quietest_split(audio: &[f32]) -> usize {
    let (from, to) = (audio.len() / 4, audio.len() * 3 / 4);
    let mut best = (audio.len() / 2, f32::MAX);
    let mut at = from;
    while at + SPLIT_FRAME <= to {
        let frame = &audio[at..at + SPLIT_FRAME];
        let energy = frame.iter().map(|v| v * v).sum::<f32>();
        if energy < best.1 {
            best = (at + SPLIT_FRAME / 2, energy);
        }
        at += SPLIT_FRAME;
    }
    best.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runaway_chunks_split_in_a_pause() {
        let mut audio = vec![0.3_f32; 16_000 * 10];
        // A pause at 6 s.
        for v in &mut audio[16_000 * 6 - 800..16_000 * 6 + 800] {
            *v = 0.0;
        }
        let cut = quietest_split(&audio);
        assert!(
            (16_000 * 6 - 800..=16_000 * 6 + 800).contains(&cut),
            "{cut}"
        );
        assert!(ran_away(
            "run: output truncated: decode hit the context/generation cap"
        ));
    }
}
