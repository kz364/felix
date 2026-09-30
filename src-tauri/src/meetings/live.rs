//! A rough transcript while the meeting is still going, so "What did I
//! miss?" and "Suggest a question" have something to go on, sign-offs can
//! end the recording, and the panel can show it. With a cloud transcriber
//! it goes to the cloud; otherwise the dictation model does it in short
//! pieces, only while dictation isn't using it.
//!
//! Every [`EVERY`] the new audio on each track is cut at a quiet moment and
//! sent off whole; no VAD, since the proper transcript is made from scratch
//! once the recording stops. Kept in `live.json`, deleted with it.

use super::remote::Remote;
use super::track::SAMPLE_RATE;
use super::transcript::{self, Segment, Source};
use super::MeetingMode;
use crate::managers::audio::AudioRecordingManager;
use crate::managers::transcription::TranscriptionManager;
use once_cell::sync::Lazy;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

pub const FILE: &str = "live.json";
/// How often new audio is sent.
const EVERY: Duration = Duration::from_secs(20);
/// Less new audio than this waits for the next round.
const MIN_NEW_SECS: u64 = 5;
/// One request carries at most this much.
const MAX_CHUNK_SECS: u64 = 60;
/// A local piece is shorter, so dictation that starts meanwhile never waits
/// long for the model.
const MAX_LOCAL_CHUNK_SECS: u64 = 15;
/// Pieces per track each round, so a round can catch up after a busy one.
const PIECES_PER_ROUND: usize = 4;
/// The cut goes at the quietest moment in this last stretch.
const CUT_SEARCH_SECS: u64 = 2;
const FRAME: usize = 480;
/// A frame louder than this has sound (as in capture).
const SOUND_RMS: f32 = 0.01;
/// Fewer loud frames than this (about 0.3 s) isn't worth sending.
const MIN_SOUND_FRAMES: usize = 10;
/// Give up after this many failures in a row.
const MAX_FAILURES: u32 = 3;

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

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// Where to cut `audio` so a word isn't split: the quietest frame in its
/// last [`CUT_SEARCH_SECS`], or the end if it's all loud.
pub fn cut_point(audio: &[f32]) -> usize {
    let search = (CUT_SEARCH_SECS * SAMPLE_RATE as u64) as usize;
    let from = audio.len().saturating_sub(search) / FRAME * FRAME;
    let mut best = (audio.len(), f32::MAX);
    let mut at = from;
    while at + FRAME <= audio.len() {
        let level = rms(&audio[at..at + FRAME]);
        if level < best.1 {
            best = (at + FRAME / 2, level);
        }
        at += FRAME;
    }
    if best.1 < SOUND_RMS {
        best.0
    } else {
        audio.len()
    }
}

fn has_sound(audio: &[f32]) -> bool {
    audio.chunks(FRAME).filter(|f| rms(f) > SOUND_RMS).count() >= MIN_SOUND_FRAMES
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
    /// Samples already sent (or skipped as quiet).
    done: u64,
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
    fn max_chunk_secs(&self) -> u64 {
        match self {
            Engine::Remote(_) => MAX_CHUNK_SECS,
            Engine::Local(_) => MAX_LOCAL_CHUNK_SECS,
        }
    }

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
    let engine = match Remote::from_settings(&settings) {
        Ok(Some(remote)) => Engine::Remote(remote.for_languages(&settings.meeting_languages)),
        _ => Engine::Local(app.clone()),
    };
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
            let mut tracks: Vec<Track> = [Source::Mic, Source::System]
                .into_iter()
                .filter(|s| mode == MeetingMode::Call || *s == Source::Mic)
                .map(|source| Track {
                    source,
                    path: dir.join(source.file()),
                    // Carry on after what a resumed meeting already has.
                    done: segments
                        .iter()
                        .filter(|s| s.source == source)
                        .map(|s| s.end_ms * SAMPLE_RATE as u64 / 1000)
                        .max()
                        .unwrap_or(0),
                })
                .collect();
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
                    // A few pieces a round, so a local model keeps up.
                    for _ in 0..PIECES_PER_ROUND {
                        if stop.load(Ordering::Acquire) {
                            break;
                        }
                        let max = engine.max_chunk_secs() * SAMPLE_RATE as u64;
                        let Some(audio) = read_from(&track.path, track.done, max) else {
                            break;
                        };
                        if (audio.len() as u64) < MIN_NEW_SECS * SAMPLE_RATE as u64 {
                            break;
                        }
                        let cut = cut_point(&audio);
                        let audio = &audio[..cut];
                        let start = track.done;
                        track.done += cut as u64;
                        if !has_sound(audio) {
                            continue;
                        }
                        let Ok(result) = engine.transcribe(audio) else {
                            track.done = start;
                            break;
                        };
                        match result {
                            Ok(text) if !text.trim().is_empty() => {
                                failures = 0;
                                let segment = Segment {
                                    source: track.source,
                                    start_ms: start * 1000 / SAMPLE_RATE as u64,
                                    end_ms: track.done * 1000 / SAMPLE_RATE as u64,
                                    text,
                                    echo: false,
                                    speaker: None,
                                };
                                note_sign_offs(&id, &segment);
                                segments.push(segment);
                                added = true;
                            }
                            Ok(_) => failures = 0,
                            Err(e) => {
                                failures += 1;
                                log::warn!("Live transcript: {e}");
                                // Try this stretch again next round.
                                track.done = start;
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

    #[test]
    fn cuts_land_in_a_pause() {
        let rate = SAMPLE_RATE as usize;
        let mut audio = vec![0.2_f32; rate * 6];
        // A pause 1 s before the end.
        for s in &mut audio[rate * 5 - 800..rate * 5 + 800] {
            *s = 0.0;
        }
        let cut = cut_point(&audio);
        assert!((rate * 5 - 800..=rate * 5 + 800).contains(&cut), "{cut}");
        // All loud: cut at the end.
        assert_eq!(cut_point(&vec![0.2_f32; rate * 6]), rate * 6);
    }
}
