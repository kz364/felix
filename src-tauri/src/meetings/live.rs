//! The transcript while the meeting is still going, so "What did I miss?"
//! and "Suggest a question" have something to go on, sign-offs can end the
//! recording, and the panel can show it. With a cloud transcriber it goes
//! to the cloud; otherwise the dictation model does it, only while
//! dictation isn't using it.
//!
//! Every [`EVERY`] each track's new audio goes through the same VAD the
//! final pass uses ([`pipeline::Listener`]), so the chunks it plans are the
//! final pass's own; each chunk is transcribed once no more speech can join
//! it. Their text is kept (`ahead.json`) and the final pass uses it for
//! every chunk that comes out the same, so after the call only chunks split
//! between speakers (and the mic's, where the call echoed into it) are
//! transcribed again. Kept in `live.json`, deleted with it.
//!
//! With the speaker model, every [`VOICES_EVERY`] the voices so far are told
//! apart the way the final pass does, from the model's answers kept from
//! the rounds before ([`diarize::FingerprintCache`]), and the system
//! track's chunks are cut where the speaker changes before they're
//! transcribed, so most come out as the final pass cuts them. When the
//! recording stops, what was heard (the VAD's state and the speaker
//! models' answers) is handed to the final pass ([`take_heard`]), which
//! carries on from there instead of starting over.

use super::diarize;
use super::pipeline;
use super::remote::Remote;
use super::track::SAMPLE_RATE;
use super::transcript::{self, Chunk, Segment, Source};
use super::MeetingMode;
use crate::managers::audio::AudioRecordingManager;
use crate::managers::transcription::TranscriptionManager;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

pub const FILE: &str = "live.json";
/// How often new audio is looked at.
const EVERY: Duration = Duration::from_secs(20);
/// Chunks per track each round, so a round can catch up after a busy one.
const PIECES_PER_ROUND: usize = 6;
/// New audio read at a time.
const READ_SECS: u64 = 60;
/// Give up after this many failures in a row.
const MAX_FAILURES: u32 = 3;
/// How often the voices so far are told apart.
const VOICES_EVERY: Duration = Duration::from_secs(60);
/// How long the final pass waits for the live pass to hand over.
const HANDOVER_WAIT: Duration = Duration::from_secs(30);

/// What the live pass heard of one track, for the final pass to carry on
/// from: the same audio gives the same answers.
pub struct Heard {
    pub source: Source,
    /// Whether the VAD heard it through the in-person gain.
    pub live_gain: bool,
    pub listener: pipeline::Listener,
    /// Samples the listener has heard.
    pub heard: u64,
    pub cache: diarize::FingerprintCache,
}

/// By meeting folder: `None` while the live pass runs, then what it heard.
static HEARD: Lazy<Mutex<HashMap<PathBuf, Option<Vec<Heard>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// What the live pass heard of the meeting in `dir`, once it has stopped
/// (waiting a little for it). Empty if it didn't run in this session.
pub fn take_heard(dir: &Path) -> Vec<Heard> {
    let since = Instant::now();
    loop {
        {
            let mut heard = HEARD.lock().unwrap_or_else(|e| e.into_inner());
            match heard.get(dir) {
                None => return Vec::new(),
                Some(Some(_)) => return heard.remove(dir).flatten().unwrap_or_default(),
                Some(None) if since.elapsed() > HANDOVER_WAIT => {
                    log::warn!("The live transcript didn't stop in time; starting over");
                    return Vec::new();
                }
                Some(None) => {}
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Chunks transcribed while recording, for the final pass to reuse.
pub const AHEAD_FILE: &str = "ahead.json";

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Ahead {
    /// What transcribed them, as the final pass names it: text from another
    /// engine isn't used.
    pub engine: String,
    /// By [`chunk_key`].
    pub texts: BTreeMap<String, String>,
}

pub fn chunk_key(source: Source, c: Chunk) -> String {
    format!("{}-{}-{}", source.key(), c.start_ms, c.end_ms)
}

fn parse_key(key: &str) -> Option<(Source, Chunk)> {
    let mut parts = key.rsplitn(3, '-');
    let end_ms = parts.next()?.parse().ok()?;
    let start_ms = parts.next()?.parse().ok()?;
    let source = match parts.next()? {
        "mic" => Source::Mic,
        "system" => Source::System,
        _ => return None,
    };
    Some((source, Chunk { start_ms, end_ms }))
}

/// Chunks tidied while recording, by [`chunk_key`].
pub const AHEAD_CLEANED_FILE: &str = "ahead_cleaned.json";
/// How often the chunks transcribed so far are tidied.
const CLEAN_EVERY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TidiedChunk {
    /// The chunk's text as tidied from (with the word corrections).
    pub raw: String,
    pub text: String,
}

/// While recording: tidy what's been transcribed, a minute's worth at a
/// time, so after the call only paragraphs with chunks transcribed again
/// need the model. Paragraphs are only known once the voices are told
/// apart, so it tidies chunk by chunk, each with the ones around it.
fn spawn_cleanup(app: &AppHandle, dir: &Path, mode: MeetingMode, stop: Arc<AtomicBool>) {
    let settings = crate::rules::with_rules(crate::settings::get_settings(app));
    if !settings.meeting_cleanup {
        return;
    }
    let Ok(llm) = super::llm::Llm::from_settings(&settings) else {
        return;
    };
    let dir = dir.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("meeting-live-cleanup".into())
        .spawn(move || {
            let mut tidied: BTreeMap<String, TidiedChunk> =
                super::summary::load_json(&dir, AHEAD_CLEANED_FILE).unwrap_or_default();
            let label = move |p: &transcript::Paragraph| {
                match (mode, p.source) {
                    (MeetingMode::Call, Source::Mic) => "Me",
                    (MeetingMode::Call, Source::System) => "Them",
                    _ => "Speaker",
                }
                .to_string()
            };
            loop {
                for _ in 0..CLEAN_EVERY.as_secs() {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
                let ahead: Ahead = super::summary::load_json(&dir, AHEAD_FILE).unwrap_or_default();
                let mut chunks: Vec<(String, transcript::Paragraph)> = ahead
                    .texts
                    .iter()
                    .filter(|(_, text)| !text.trim().is_empty())
                    .filter_map(|(key, text)| {
                        let (source, c) = parse_key(key)?;
                        let raw = super::summary::corrected(text, &settings);
                        (tidied.get(key).map(|t| &t.raw) != Some(&raw)).then(|| {
                            (
                                key.clone(),
                                transcript::Paragraph {
                                    source,
                                    start_ms: c.start_ms,
                                    end_ms: c.end_ms,
                                    text: raw,
                                    raw: None,
                                    speaker: None,
                                },
                            )
                        })
                    })
                    .collect();
                if chunks.is_empty() {
                    continue;
                }
                chunks.sort_by_key(|(_, p)| (p.start_ms, p.source == Source::System));
                let paragraphs: Vec<transcript::Paragraph> =
                    chunks.iter().map(|(_, p)| p.clone()).collect();
                let mut cleaned = super::summary::Cleaned::default();
                let error = tauri::async_runtime::block_on(super::summary::clean(
                    &llm,
                    &paragraphs,
                    &label,
                    &settings.custom_words,
                    &settings.soundalikes,
                    &mut cleaned,
                    |_, _| {},
                ));
                if let Some(e) = error {
                    log::warn!("Couldn't tidy the meeting so far: {e}");
                }
                for (key, p) in chunks {
                    if let Some(text) = cleaned.texts.get(&super::summary::paragraph_key(&p)) {
                        tidied.insert(
                            key,
                            TidiedChunk {
                                raw: p.text,
                                text: text.clone(),
                            },
                        );
                    }
                }
                if let Err(e) = super::summary::save_json(&dir, AHEAD_CLEANED_FILE, &tidied) {
                    log::warn!("{e}");
                }
            }
        });
}

/// After the call: paragraphs made only of chunks tidied while recording
/// (with the same text) take that tidied text. Returns how many.
pub fn reuse_cleaned(
    dir: &Path,
    segments: &[Segment],
    paragraphs: &[transcript::Paragraph],
    cleaned: &mut super::summary::Cleaned,
) -> usize {
    let tidied: BTreeMap<String, TidiedChunk> =
        super::summary::load_json(dir, AHEAD_CLEANED_FILE).unwrap_or_default();
    if tidied.is_empty() {
        return 0;
    }
    let mut reused = 0;
    for p in paragraphs {
        let key = super::summary::paragraph_key(p);
        if cleaned.texts.contains_key(&key) {
            continue;
        }
        let mut parts: Vec<&Segment> = segments
            .iter()
            .filter(|s| s.source == p.source && !s.echo && !s.text.trim().is_empty())
            .filter(|s| s.start_ms >= p.start_ms && s.end_ms <= p.end_ms)
            .collect();
        parts.sort_by_key(|s| s.start_ms);
        let texts: Option<Vec<&str>> = parts
            .iter()
            .map(|s| {
                let t = tidied.get(&chunk_key(
                    s.source,
                    Chunk {
                        start_ms: s.start_ms,
                        end_ms: s.end_ms,
                    },
                ))?;
                (t.raw == s.text).then_some(t.text.as_str())
            })
            .collect();
        let Some(texts) = texts.filter(|t| !t.is_empty()) else {
            continue;
        };
        let text = texts
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if super::summary::accept_cleaned(&p.text, &text) {
            cleaned.texts.insert(key, text);
            reused += 1;
        }
    }
    reused
}

/// Phrases that end a call.
const SIGN_OFFS: &[&str] = &[
    "thanks everyone",
    "thank you everyone",
    "thanks all",
    "thank you all",
    "thanks for joining",
    "thank you for joining",
    "talk to you later",
    "talk soon",
    "see you next week",
    "see you tomorrow",
    "have a good one",
    "have a great day",
    "have a good day",
    "have a good weekend",
    "bye everyone",
    "bye bye",
    "bye all",
];
/// Words said after a sign-off that mean the meeting carried on.
const CARRIES_ON_WORDS: usize = 8;

/// The latest sign-off in the meeting being recorded: (meeting id, ms).
static SIGN_OFF: Lazy<Mutex<Option<(String, u64)>>> = Lazy::new(|| Mutex::new(None));

/// When someone last said goodbye in this meeting, if nobody carried on since.
pub fn signed_off_at(id: &str) -> Option<u64> {
    SIGN_OFF
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .filter(|(m, _)| m == id)
        .map(|(_, at)| *at)
}

pub fn is_sign_off(text: &str) -> bool {
    let text: String = text
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '\'' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    SIGN_OFFS.iter().any(|p| text.contains(p))
}

fn note_sign_offs(id: &str, segment: &Segment) {
    let mut sign_off = SIGN_OFF.lock().unwrap_or_else(|e| e.into_inner());
    if is_sign_off(&segment.text) {
        *sign_off = Some((id.to_string(), segment.end_ms));
    } else if segment.text.split_whitespace().count() >= CARRIES_ON_WORDS
        && sign_off
            .as_ref()
            .is_some_and(|(m, at)| m == id && segment.start_ms >= *at)
    {
        *sign_off = None;
    }
}

pub fn load(dir: &Path) -> Vec<Segment> {
    super::summary::load_json(dir, FILE).unwrap_or_default()
}

/// Samples `from..` of a WAV that's still being written (up to its last
/// flush), at most `max`.
fn read_from(path: &Path, from: u64, max: u64) -> Option<Vec<f32>> {
    let mut reader = hound::WavReader::open(path).ok()?;
    let len = reader.duration() as u64;
    if len <= from {
        return Some(Vec::new());
    }
    reader.seek(from as u32).ok()?;
    Some(
        reader
            .samples::<i16>()
            .take((len - from).min(max) as usize)
            .filter_map(Result::ok)
            .map(|s| s as f32 / i16::MAX as f32)
            .collect(),
    )
}

/// The system track this far behind the mic has stopped.
const STALLED_SECS: usize = 5;

/// The mic with the call's echo cancelled as it's heard (on speakers).
struct Cleaner {
    canceller: super::aec::Canceller,
    mic: PathBuf,
    system: PathBuf,
    /// Samples of the mic (and system) track cancelled so far.
    done: u64,
    out: hound::WavWriter<std::io::BufWriter<std::fs::File>>,
}

impl Cleaner {
    fn new(dir: &Path) -> Result<Self, String> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        Ok(Cleaner {
            canceller: super::aec::Canceller::new()?,
            mic: dir.join(Source::Mic.file()),
            system: dir.join(Source::System.file()),
            done: 0,
            out: hound::WavWriter::create(dir.join(super::aec::LIVE_FILE), spec)
                .map_err(|e| e.to_string())?,
        })
    }

    /// Cancel what both tracks have recorded since; returns the cleaned mic.
    fn next(&mut self) -> Result<Vec<f32>, String> {
        let max = READ_SECS * SAMPLE_RATE as u64;
        let mic = read_from(&self.mic, self.done, max).unwrap_or_default();
        let mut system = read_from(&self.system, self.done, max).unwrap_or_default();
        // The call's track stalled (its capture failed): carry on without it.
        if mic.len() > system.len() + STALLED_SECS * SAMPLE_RATE as usize {
            system.resize(mic.len(), 0.0);
        }
        let frame = super::aec::Canceller::FRAME;
        let n = mic.len().min(system.len()) / frame * frame;
        let mut out = Vec::with_capacity(n);
        for at in (0..n).step_by(frame) {
            out.extend(
                self.canceller
                    .process(&mic[at..at + frame], &system[at..at + frame])?,
            );
        }
        for &s in &out {
            self.out
                .write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)
                .map_err(|e| e.to_string())?;
        }
        self.out.flush().map_err(|e| e.to_string())?;
        self.done += n as u64;
        Ok(out)
    }
}

struct Track {
    source: Source,
    /// What's transcribed: the track, or the mic cleaned as it's heard.
    path: PathBuf,
    cleaner: Option<Cleaner>,
    live_gain: bool,
    /// Hears the track as the final pass will.
    listener: pipeline::Listener,
    /// Samples heard so far.
    heard: u64,
    cache: diarize::FingerprintCache,
    /// Who speaks each frame so far (the system track's), once told apart.
    labels: Option<Vec<Option<u32>>>,
    /// How each chunk was cut when it was first ready, by [`chunk_key`]:
    /// kept, so a chunk isn't transcribed again each time the voices are
    /// told apart a little differently.
    cuts: HashMap<String, Vec<Chunk>>,
}

/// Tell the voices heard so far apart, as the final pass will. Only the
/// system track's are used now (its chunks are reused as cut); the mic's
/// answers are kept for the final pass.
fn tell_voices(dir: &Path, track: &mut Track, model: &Path) -> Result<(), String> {
    let samples = read_from(&track.path, 0, track.heard)
        .ok_or_else(|| format!("Couldn't read {}", track.path.display()))?;
    let speech = &track.listener.analysis.speech;
    if track.source == Source::Mic {
        let seg = super::segment::model_beside(model);
        diarize::fingerprint_cached(&samples, speech, model, seg.as_deref(), &mut track.cache)?;
        return Ok(());
    }
    let hints = super::speakers::name_hints(dir, speech.len());
    let voices = diarize::speakers_cached(&samples, speech, model, None, &hints, &mut track.cache)?;
    track.labels = Some(voices.labels);
    Ok(())
}

/// What makes the live transcript.
enum Engine {
    Remote(Remote),
    /// The dictation model, when dictation isn't using it, held to the
    /// meeting's language if it has just one.
    /// The dictation model, in the meeting's language, with its own words.
    Local(AppHandle, Option<String>, Vec<String>),
    /// A model loaded for meetings, set aside while dictation runs.
    Own {
        app: AppHandle,
        session: std::sync::Mutex<transcribe_cpp::Session>,
        settings: Box<crate::settings::AppSettings>,
        language: Option<String>,
    },
}

/// Load the meeting model for the live pass.
fn own_engine(
    app: &AppHandle,
    settings: &crate::settings::AppSettings,
    model_id: &str,
) -> Result<Engine, String> {
    let path = app
        .try_state::<Arc<crate::managers::model::ModelManager>>()
        .ok_or("Models aren't available")?
        .get_model_path(model_id)
        .map_err(|e| e.to_string())?;
    let model = transcribe_cpp::Model::load(&path).map_err(|e| e.to_string())?;
    let session = model.session().map_err(|e| e.to_string())?;
    Ok(Engine::Own {
        app: app.clone(),
        session: std::sync::Mutex::new(session),
        settings: Box::new(settings.clone()),
        language: super::language::pinned(&settings.meeting_languages).map(str::to_string),
    })
}

/// The model couldn't take a piece this round; try it again next round.
struct Busy;

impl Engine {
    fn transcribe(&self, audio: &[f32]) -> Result<Result<String, String>, Busy> {
        match self {
            Engine::Remote(remote) => {
                Ok(tauri::async_runtime::block_on(remote.transcribe(audio))
                    .map_err(|e| e.to_string()))
            }
            Engine::Own {
                app,
                session,
                settings,
                language,
            } => {
                let recording = app
                    .try_state::<Arc<AudioRecordingManager>>()
                    .is_some_and(|a| a.is_recording());
                let dictating = app
                    .try_state::<Arc<TranscriptionManager>>()
                    .is_some_and(|tm| tm.is_dictation_busy());
                if recording || dictating {
                    return Err(Busy);
                }
                let mut session = session.lock().unwrap_or_else(|e| e.into_inner());
                Ok(crate::managers::transcription::transcribe_meeting_chunk(
                    settings,
                    &mut session,
                    audio,
                    language.as_deref(),
                )
                .map_err(|e| e.to_string()))
            }
            Engine::Local(app, language, words) => {
                let recording = app
                    .try_state::<Arc<AudioRecordingManager>>()
                    .is_some_and(|a| a.is_recording());
                let Some(tm) = app.try_state::<Arc<TranscriptionManager>>() else {
                    return Err(Busy);
                };
                if recording || !tm.is_idle_for_meeting() {
                    if !recording && !tm.is_model_loaded() {
                        tm.initiate_model_load();
                    }
                    return Err(Busy);
                }
                Ok(tm
                    .transcribe_for_meeting(audio.to_vec(), language.as_deref(), words)
                    .map_err(|e| e.to_string()))
            }
        }
    }
}

/// While recording: keep `live.json` up to date until `stop` is set.
pub fn spawn(app: &AppHandle, dir: &Path, mode: MeetingMode, stop: Arc<AtomicBool>) {
    let mut settings = crate::rules::with_rules(crate::settings::get_settings(app));
    // The remote engine, or the local model to use; a model of the meeting's
    // own is loaded on the live thread, not here.
    let (remote, local_model) = match Remote::from_settings(&settings) {
        Ok(Some(remote)) => (
            Some(remote.for_languages(&settings.meeting_languages)),
            None,
        ),
        _ => {
            let models = app
                .try_state::<Arc<crate::managers::model::ModelManager>>()
                .map(|m| m.get_available_models())
                .unwrap_or_default();
            (None, Some(super::local_model(&settings, &models)))
        }
    };
    let vad = match app.path().resolve(
        "resources/models/silero_vad_v4.onnx",
        tauri::path::BaseDirectory::Resource,
    ) {
        Ok(p) => p,
        Err(e) => {
            log::warn!("No live transcript: couldn't find the VAD model: {e}");
            return;
        }
    };
    // As the final pass hears the mic in person (see `pipeline::run`).
    let lift_mic = mode == MeetingMode::InPerson && settings.meeting_auto_gain;
    // On a call through the speakers, the mic hears the call again: cancel
    // it as it's heard, as the final pass does.
    let clean_mic = mode == MeetingMode::Call
        && !super::manager::read_info(dir)
            .and_then(|i| i.output_device)
            .is_some_and(|o| super::echo::is_headphones(&o));
    // Told apart only with the model already there (the final pass fetches it).
    let speaker_model = settings
        .meeting_diarize
        .then(|| crate::portable::app_data_dir(app).ok())
        .flatten()
        .map(|d| d.join("models").join(diarize::MODEL_FILE))
        .filter(|p| p.is_file());
    let id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    spawn_cleanup(app, dir, mode, stop.clone());
    let dir = dir.to_path_buf();
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("meeting-live".into())
        .spawn(move || {
            HEARD
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(dir.clone(), None);
            // The invited people's names, for this meeting only.
            let now = chrono::Utc::now().timestamp_millis();
            let started = super::manager::read_info(&dir).map_or(now, |i| i.started_at);
            let words =
                super::calendar::name_words(&dir, started, now.max(started) + 30 * 60 * 1000);
            crate::managers::transcription::add_words(&mut settings, &words);
            let language = super::language::pinned(&settings.meeting_languages).map(str::to_string);
            let shared = || {
                (
                    Engine::Local(app.clone(), language.clone(), words.clone()),
                    super::local_engine(&settings.selected_model),
                )
            };
            let (engine, engine_name) = match (remote, local_model) {
                (Some(remote), _) => {
                    let name = format!("{} {}", remote.name, remote.model);
                    (Engine::Remote(remote), name)
                }
                (None, Some(model)) if model != settings.selected_model => {
                    match own_engine(&app, &settings, &model) {
                        Ok(engine) => (engine, super::local_engine(&model)),
                        Err(e) => {
                            log::warn!(
                                "Couldn't load the meeting model {model}, using dictation's: {e}"
                            );
                            shared()
                        }
                    }
                }
                _ => shared(),
            };
            let tracks = run(
                &app,
                &dir,
                &id,
                mode,
                &vad,
                lift_mic,
                clean_mic,
                speaker_model.as_deref(),
                engine,
                engine_name,
                &stop,
            );
            let heard = tracks
                .into_iter()
                .map(|t| Heard {
                    source: t.source,
                    live_gain: t.live_gain,
                    listener: t.listener,
                    heard: t.heard,
                    cache: t.cache,
                })
                .collect();
            HEARD
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(dir.clone(), Some(heard));
        });
}

/// The live pass, until `stop` is set. Returns what it heard.
#[allow(clippy::too_many_arguments)]
fn run(
    app: &AppHandle,
    dir: &Path,
    id: &str,
    mode: MeetingMode,
    vad: &Path,
    lift_mic: bool,
    clean_mic: bool,
    speaker_model: Option<&Path>,
    engine: Engine,
    engine_name: String,
    stop: &AtomicBool,
) -> Vec<Track> {
    let mut segments = load(dir);
    // A resumed meeting keeps what was done, if done the same way.
    let mut ahead: Ahead = super::summary::load_json(dir, AHEAD_FILE)
        .filter(|a: &Ahead| a.engine == engine_name)
        .unwrap_or(Ahead {
            engine: engine_name.clone(),
            texts: BTreeMap::new(),
        });
    let mut tracks: Vec<Track> = Vec::new();
    for source in [Source::Mic, Source::System] {
        if mode != MeetingMode::Call && source == Source::System {
            continue;
        }
        let live_gain = lift_mic && source == Source::Mic;
        match pipeline::Listener::new(vad, live_gain) {
            Ok(listener) => {
                let cleaner = (clean_mic && source == Source::Mic)
                    .then(|| {
                        Cleaner::new(dir)
                            .inspect_err(|e| log::warn!("Live echo cancelling: {e}"))
                            .ok()
                    })
                    .flatten();
                let path = match &cleaner {
                    Some(_) => dir.join(super::aec::LIVE_FILE),
                    None => dir.join(source.file()),
                };
                tracks.push(Track {
                    source,
                    path,
                    cleaner,
                    live_gain,
                    listener,
                    heard: 0,
                    cache: diarize::FingerprintCache::default(),
                    labels: None,
                    cuts: HashMap::new(),
                })
            }
            Err(e) => log::warn!("No live transcript for the {source:?} track: {e}"),
        }
    }
    let mut failures = 0;
    let mut voices_at: Option<Instant> = None;
    let mut voices_failed = false;
    let wait = |stop: &AtomicBool| {
        for _ in 0..EVERY.as_secs() {
            if stop.load(Ordering::Acquire) {
                return false;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        true
    };
    while wait(stop) && failures < MAX_FAILURES {
        let mut added = false;
        // The voices so far, now and then.
        let tell = speaker_model
            .filter(|_| !voices_failed && voices_at.is_none_or(|at| at.elapsed() >= VOICES_EVERY));
        for track in &mut tracks {
            // Hear what's new.
            while let Some(audio) = match &mut track.cleaner {
                Some(c) => c
                    .next()
                    .inspect_err(|e| log::warn!("Live echo cancelling: {e}"))
                    .ok(),
                None => read_from(&track.path, track.heard, READ_SECS * SAMPLE_RATE as u64),
            } {
                if audio.is_empty() || stop.load(Ordering::Acquire) {
                    break;
                }
                track.heard += audio.len() as u64;
                if let Err(e) = track.listener.push(&audio) {
                    log::warn!("Live transcript: {e}");
                    break;
                }
            }
            if let Some(model) = tell.filter(|_| !stop.load(Ordering::Acquire)) {
                let started = Instant::now();
                match tell_voices(dir, track, model) {
                    Ok(()) => log::debug!(
                        "Live: told the {:?} voices apart in {:.1?}",
                        track.source,
                        started.elapsed()
                    ),
                    Err(e) => {
                        log::warn!("Live: couldn't tell the voices apart: {e}");
                        voices_failed = true;
                        track.labels = None;
                    }
                }
            }
            // Chunks no more speech can join, cut where the speaker
            // changes once the voices are told apart that far.
            let speech = &track.listener.analysis.speech;
            let heard_ms = speech.len() as u64 * transcript::FRAME_MS;
            let by_voice =
                track.source == Source::System && speaker_model.is_some() && !voices_failed;
            let labels = &track.labels;
            let cuts = &mut track.cuts;
            let ready: Vec<Chunk> = transcript::plan_chunks(speech)
                .into_iter()
                .filter(|c| c.end_ms + transcript::PAUSE_MS <= heard_ms)
                .flat_map(|c| {
                    let key = chunk_key(track.source, c);
                    if let Some(pieces) = cuts.get(&key) {
                        return pieces.clone();
                    }
                    let pieces = match (labels, by_voice) {
                        (_, false) => vec![c],
                        (Some(labels), true)
                            if (c.end_ms / transcript::FRAME_MS) as usize <= labels.len() =>
                        {
                            diarize::split_by_speaker(c, labels)
                                .into_iter()
                                .map(|(c, _)| c)
                                .collect()
                        }
                        // Not told apart that far yet.
                        _ => return Vec::new(),
                    };
                    cuts.insert(key, pieces.clone());
                    pieces
                })
                .filter(|c| !ahead.texts.contains_key(&chunk_key(track.source, *c)))
                .take(PIECES_PER_ROUND)
                .collect();
            for chunk in ready {
                if stop.load(Ordering::Acquire) {
                    break;
                }
                let audio = match pipeline::read_chunk(&track.path, chunk, None) {
                    Ok(a) => a,
                    Err(e) => {
                        log::warn!("Live transcript: {e}");
                        break;
                    }
                };
                let Ok(result) = engine.transcribe(&audio) else {
                    break;
                };
                match result {
                    Ok(text) => {
                        failures = 0;
                        let text = text.trim().to_string();
                        ahead
                            .texts
                            .insert(chunk_key(track.source, chunk), text.clone());
                        added = true;
                        if text.is_empty() {
                            continue;
                        }
                        let segment = Segment {
                            source: track.source,
                            start_ms: chunk.start_ms,
                            end_ms: chunk.end_ms,
                            text,
                            echo: false,
                            speaker: None,
                        };
                        note_sign_offs(id, &segment);
                        // The same speech cut differently before.
                        segments.retain(|s| {
                            s.source != segment.source
                                || s.end_ms <= segment.start_ms
                                || s.start_ms >= segment.end_ms
                        });
                        segments.push(segment);
                    }
                    Err(e) => {
                        failures += 1;
                        log::warn!("Live transcript: {e}");
                        break;
                    }
                }
            }
        }
        if tell.is_some() {
            voices_at = Some(Instant::now());
        }
        if failures >= MAX_FAILURES {
            log::warn!("Live transcript stopped after {failures} failures");
        }
        if added {
            if let Err(e) = super::summary::save_json(dir, AHEAD_FILE, &ahead) {
                log::warn!("Couldn't save the transcript so far: {e}");
            }
            segments.sort_by_key(|s| s.start_ms);
            transcript::mark_echo(&mut segments);
            if let Err(e) = super::summary::save_json(dir, FILE, &segments) {
                log::warn!("Couldn't save the live transcript: {e}");
            }
            let _ = app.emit("meeting-live-transcript", id);
        }
    }
    tracks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_offs_are_recognised() {
        assert!(is_sign_off("Great, thanks everyone!"));
        assert!(is_sign_off("OK. Talk to you later."));
        assert!(is_sign_off("Bye-bye."));
        assert!(!is_sign_off("Thanks, that's everything on pricing."));
    }

    #[test]
    fn a_sign_off_is_forgotten_when_the_meeting_carries_on() {
        let seg = |start_ms, text: &str| Segment {
            source: Source::System,
            start_ms,
            end_ms: start_ms + 2_000,
            text: text.into(),
            echo: false,
            speaker: None,
        };
        note_sign_offs("m-test", &seg(1_000, "Thanks everyone, see you."));
        assert_eq!(signed_off_at("m-test"), Some(3_000));
        note_sign_offs(
            "m-test",
            &seg(4_000, "Oh wait, one more thing about the launch date"),
        );
        assert_eq!(signed_off_at("m-test"), None);
    }

    #[test]
    fn chunk_keys_read_back() {
        let c = Chunk {
            start_ms: 1_200,
            end_ms: 9_800,
        };
        assert_eq!(
            parse_key(&chunk_key(Source::System, c)),
            Some((Source::System, c))
        );
        assert_eq!(
            parse_key(&chunk_key(Source::Mic, c)),
            Some((Source::Mic, c))
        );
    }

    #[test]
    fn a_paragraph_of_tidied_chunks_takes_their_text() {
        let dir = std::env::temp_dir().join(format!("felix-ahead-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let seg = |start_ms, end_ms, text: &str| Segment {
            source: Source::System,
            start_ms,
            end_ms,
            text: text.into(),
            echo: false,
            speaker: Some(100),
        };
        // The second chunk was split between speakers and transcribed again.
        let segments = vec![
            seg(0, 4_000, "um so the plan is"),
            seg(4_000, 8_000, "to ship it friday"),
            seg(9_000, 12_000, "sounds good"),
        ];
        let tidy = |raw: &str, text: &str| TidiedChunk {
            raw: raw.into(),
            text: text.into(),
        };
        let tidied = BTreeMap::from([
            (
                "system-0-4000".to_string(),
                tidy("um so the plan is", "So the plan is"),
            ),
            (
                "system-4000-8000".to_string(),
                tidy("to ship it friday", "to ship it Friday."),
            ),
            (
                "system-9000-12000".to_string(),
                tidy("sound good", "Sounds good."),
            ),
        ]);
        super::super::summary::save_json(&dir, AHEAD_CLEANED_FILE, &tidied).unwrap();
        let para = |start_ms, end_ms, text: &str| transcript::Paragraph {
            source: Source::System,
            start_ms,
            end_ms,
            text: text.into(),
            raw: None,
            speaker: Some(100),
        };
        let paragraphs = vec![
            para(0, 8_000, "um so the plan is to ship it friday"),
            para(9_000, 12_000, "sounds good"),
        ];
        let mut cleaned = super::super::summary::Cleaned::default();
        assert_eq!(reuse_cleaned(&dir, &segments, &paragraphs, &mut cleaned), 1);
        assert_eq!(
            cleaned.texts.get("system-0").map(String::as_str),
            Some("So the plan is to ship it Friday.")
        );
        // Its text changed since (the raw differs): left for the final pass.
        assert!(!cleaned.texts.contains_key("system-9000"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
