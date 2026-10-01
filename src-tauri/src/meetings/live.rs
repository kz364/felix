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

use super::pipeline;
use super::remote::Remote;
use super::track::SAMPLE_RATE;
use super::transcript::{self, Chunk, Segment, Source};
use super::MeetingMode;
use crate::managers::audio::AudioRecordingManager;
use crate::managers::transcription::TranscriptionManager;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
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

struct Track {
    source: Source,
    path: PathBuf,
    /// Hears the track as the final pass will.
    listener: pipeline::Listener,
    /// Samples heard so far.
    heard: u64,
}

/// What makes the live transcript.
enum Engine {
    Remote(Remote),
    /// The dictation model, when dictation isn't using it.
    Local(AppHandle),
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
            Engine::Local(app) => {
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
                    .transcribe_for_meeting(audio.to_vec())
                    .map_err(|e| e.to_string()))
            }
        }
    }
}

/// While recording: keep `live.json` up to date until `stop` is set.
pub fn spawn(app: &AppHandle, dir: &Path, mode: MeetingMode, stop: Arc<AtomicBool>) {
    let settings = crate::rules::with_rules(crate::settings::get_settings(app));
    let (engine, engine_name) = match Remote::from_settings(&settings) {
        Ok(Some(remote)) => {
            let remote = remote.for_languages(&settings.meeting_languages);
            let name = format!("{} {}", remote.name, remote.model);
            (Engine::Remote(remote), name)
        }
        _ => (
            Engine::Local(app.clone()),
            format!("local {}", settings.selected_model),
        ),
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
    let id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = dir.to_path_buf();
    let app = app.clone();
    let _ = std::thread::Builder::new()
        .name("meeting-live".into())
        .spawn(move || {
            let mut segments = load(&dir);
            // A resumed meeting keeps what was done, if done the same way.
            let mut ahead: Ahead = super::summary::load_json(&dir, AHEAD_FILE)
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
                match pipeline::Listener::new(&vad, lift_mic && source == Source::Mic) {
                    Ok(listener) => tracks.push(Track {
                        source,
                        path: dir.join(source.file()),
                        listener,
                        heard: 0,
                    }),
                    Err(e) => log::warn!("No live transcript for the {source:?} track: {e}"),
                }
            }
            let mut failures = 0;
            let wait = |stop: &AtomicBool| {
                for _ in 0..EVERY.as_secs() {
                    if stop.load(Ordering::Acquire) {
                        return false;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
                true
            };
            while wait(&stop) {
                let mut added = false;
                for track in &mut tracks {
                    // Hear what's new.
                    while let Some(audio) =
                        read_from(&track.path, track.heard, READ_SECS * SAMPLE_RATE as u64)
                    {
                        if audio.is_empty() || stop.load(Ordering::Acquire) {
                            break;
                        }
                        track.heard += audio.len() as u64;
                        if let Err(e) = track.listener.push(&audio) {
                            log::warn!("Live transcript: {e}");
                            break;
                        }
                    }
                    // Chunks no more speech can join.
                    let speech = &track.listener.analysis.speech;
                    let heard_ms = speech.len() as u64 * transcript::FRAME_MS;
                    let ready: Vec<Chunk> = transcript::plan_chunks(speech)
                        .into_iter()
                        .filter(|c| c.end_ms + transcript::PAUSE_MS <= heard_ms)
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
                                note_sign_offs(&id, &segment);
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
                if failures >= MAX_FAILURES {
                    log::warn!("Live transcript stopped after {failures} failures");
                    return;
                }
                if added {
                    if let Err(e) = super::summary::save_json(&dir, AHEAD_FILE, &ahead) {
                        log::warn!("Couldn't save the transcript so far: {e}");
                    }
                    segments.sort_by_key(|s| s.start_ms);
                    transcript::mark_echo(&mut segments);
                    if let Err(e) = super::summary::save_json(&dir, FILE, &segments) {
                        log::warn!("Couldn't save the live transcript: {e}");
                    }
                    let _ = app.emit("meeting-live-transcript", &id);
                }
            }
        });
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
}
