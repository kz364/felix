//! Recording a meeting to disk: the mic and (on calls) the system audio as
//! separate tracks, `mic.wav` and `system.wav`, on one shared clock. Each
//! source fills a ring buffer from its audio thread; a writer thread per
//! track drains it, resamples to 16 kHz and appends to the WAV.

use super::track::{TrackSummary, TrackWriter, SAMPLE_RATE};
use crate::audio_toolkit::audio::FrameResampler;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How often the writers drain the rings.
const DRAIN_EVERY: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingMode {
    /// Mic + system audio.
    Call,
    /// Mic only.
    InPerson,
}

/// RMS of a 20 ms frame above which a track counts as having sound (about
/// -40 dBFS): speech on the call, or someone talking in the room.
const VOICE_RMS: f32 = 0.01;

/// When each track last had sound, in ms on the recording's clock. Read by
/// the watcher that offers to stop once a call has gone quiet.
#[derive(Default)]
pub struct Activity {
    mic: AtomicU64,
    system: AtomicU64,
    /// The loudest frame each track just had (RMS, as f32 bits), for the
    /// level shown while recording.
    mic_level: AtomicU32,
    system_level: AtomicU32,
}

impl Activity {
    fn slot(&self, name: &str) -> &AtomicU64 {
        if name == "system" {
            &self.system
        } else {
            &self.mic
        }
    }

    fn level_slot(&self, name: &str) -> &AtomicU32 {
        if name == "system" {
            &self.system_level
        } else {
            &self.mic_level
        }
    }

    /// Ms on the recording's clock when this track last had sound.
    pub fn last_sound_ms(&self, name: &str) -> u64 {
        self.slot(name).load(Ordering::Relaxed)
    }
}

/// Mono audio at `rate`, filled by a device's audio thread.
pub struct Source {
    pub rate: u32,
    pub consumer: rtrb::Consumer<f32>,
    pub label: String,
}

/// Something that keeps a source's device running until dropped.
trait Keepalive: Send {}
impl<T: Send> Keepalive for T {}

/// Set once when the recording stops: the time every track ends at.
type StopAt = Arc<OnceLock<Instant>>;

struct Track {
    writer: JoinHandle<Result<TrackSummary, String>>,
    keepalive: Box<dyn Keepalive>,
}

/// A meeting being recorded.
pub struct Recording {
    pub dir: PathBuf,
    pub mode: MeetingMode,
    pub mic_label: String,
    /// Why the system audio isn't being recorded on a call, if it isn't.
    pub system_error: Option<String>,
    pub activity: Arc<Activity>,
    t0: Instant,
    stop_at: StopAt,
    tracks: Vec<Track>,
}

struct WriterSetup {
    name: &'static str,
    t0: Instant,
    stop_at: StopAt,
    activity: Arc<Activity>,
    resume: bool,
}

fn spawn_writer(
    mut source: Source,
    path: PathBuf,
    setup: WriterSetup,
) -> Result<JoinHandle<Result<TrackSummary, String>>, String> {
    let WriterSetup {
        name,
        t0,
        stop_at,
        activity,
        resume,
    } = setup;
    let mut writer = if resume {
        TrackWriter::append(&path, name, t0)?
    } else {
        TrackWriter::create(&path, name, t0)?
    };
    std::thread::Builder::new()
        .name(format!("meeting-{name}"))
        .spawn(move || {
            let mut resampler = FrameResampler::new(
                source.rate as usize,
                SAMPLE_RATE as usize,
                Duration::from_millis(20),
            );
            let mut buf = Vec::with_capacity(source.rate as usize);
            loop {
                let stopping = stop_at.get().copied();
                buf.clear();
                let n = source.consumer.slots();
                if let Ok(chunk) = source.consumer.read_chunk(n) {
                    let (a, b) = chunk.as_slices();
                    buf.extend_from_slice(a);
                    buf.extend_from_slice(b);
                    chunk.commit_all();
                }
                let now = Instant::now();
                let incoming = buf.len() as u64 * SAMPLE_RATE as u64 / source.rate as u64;
                if incoming > 0 {
                    writer.align(incoming, now)?;
                }
                let mut result = Ok(());
                let mut loud = false;
                let mut peak = 0.0f32;
                resampler.push(&buf, |frame| {
                    let level = rms(frame);
                    peak = peak.max(level);
                    loud |= level > VOICE_RMS;
                    if result.is_ok() {
                        result = writer.write(frame, now);
                    }
                });
                activity
                    .level_slot(name)
                    .store(peak.to_bits(), Ordering::Relaxed);
                if loud {
                    activity
                        .slot(name)
                        .store(now.duration_since(t0).as_millis() as u64, Ordering::Relaxed);
                }
                if stopping.is_some() {
                    resampler.finish(|frame| {
                        if result.is_ok() {
                            result = writer.write(frame, now);
                        }
                    });
                }
                result?;
                if let Some(end) = stopping {
                    return writer.finish_at(end);
                }
                writer.keep_up(now)?;
                std::thread::sleep(DRAIN_EVERY);
            }
        })
        .map_err(|e| e.to_string())
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

impl Recording {
    /// Start recording into `dir` (created if needed). `mic` is the input
    /// device, `None` for the system default. On a call, failing to capture
    /// system audio doesn't stop the recording: it goes on with the mic, and
    /// `system_error` says why. With `resume`, the tracks already in `dir`
    /// are carried on rather than replaced, and the clock starts where they
    /// end.
    pub fn start(
        dir: &Path,
        mode: MeetingMode,
        mic: Option<cpal::Device>,
        resume: bool,
    ) -> Result<Self, String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
        let now = Instant::now();
        let already = if resume {
            super::track::duration_of(&dir.join("mic.wav")).unwrap_or_default()
        } else {
            Duration::ZERO
        };
        let t0 = now.checked_sub(already).unwrap_or(now);
        let stop_at = StopAt::default();
        let activity = Arc::new(Activity::default());
        // Nothing heard yet counts from the (re)start, not the meeting's start.
        let start_ms = already.as_millis() as u64;
        activity.mic.store(start_ms, Ordering::Relaxed);
        activity.system.store(start_ms, Ordering::Relaxed);
        let setup = |name| WriterSetup {
            name,
            t0,
            stop_at: stop_at.clone(),
            activity: activity.clone(),
            resume,
        };
        let mut tracks = Vec::new();

        let (source, stream) = super::mic::start(mic)?;
        let mic_label = source.label.clone();
        tracks.push(Track {
            writer: spawn_writer(source, dir.join("mic.wav"), setup("mic"))?,
            keepalive: Box::new(stream),
        });

        let mut system_error = None;
        if mode == MeetingMode::Call {
            match start_system() {
                Ok((source, keepalive)) => {
                    tracks.push(Track {
                        writer: spawn_writer(source, dir.join("system.wav"), setup("system"))?,
                        keepalive,
                    });
                }
                Err(e) => {
                    log::warn!("Recording the meeting without system audio: {e}");
                    system_error = Some(e);
                }
            }
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            mode,
            mic_label,
            system_error,
            activity,
            t0,
            stop_at,
            tracks,
        })
    }

    /// Length of the recording so far (including what came before a resume).
    pub fn elapsed(&self) -> Duration {
        self.t0.elapsed()
    }

    /// How long this track has had no sound.
    pub fn quiet_for(&self, track: &str) -> Duration {
        let now = self.elapsed().as_millis() as u64;
        Duration::from_millis(now.saturating_sub(self.activity.last_sound_ms(track)))
    }

    /// How loud it is right now, 0 to 1 (the louder track, on a dB scale
    /// from -50 dBFS), for the waves while recording.
    pub fn level(&self) -> f32 {
        let rms = ["mic", "system"]
            .iter()
            .map(|t| f32::from_bits(self.activity.level_slot(t).load(Ordering::Relaxed)))
            .fold(0.0f32, f32::max);
        let db = 20.0 * rms.max(1e-6).log10();
        ((db + 50.0) / 40.0).clamp(0.0, 1.0)
    }

    pub fn has_system_track(&self) -> bool {
        self.tracks.len() > 1
    }

    /// Stop the devices, write out what's buffered and close the files.
    pub fn stop(self) -> Result<Vec<TrackSummary>, String> {
        let end = Instant::now();
        // Devices first, so nothing arrives after the final drain.
        let writers: Vec<_> = self
            .tracks
            .into_iter()
            .map(|track| {
                drop(track.keepalive);
                track.writer
            })
            .collect();
        let _ = self.stop_at.set(end);
        let mut summaries = Vec::new();
        let mut first_error = None;
        for writer in writers {
            match writer.join() {
                Ok(Ok(summary)) => summaries.push(summary),
                Ok(Err(e)) => {
                    first_error.get_or_insert(e);
                }
                Err(_) => {
                    first_error.get_or_insert("A track writer crashed".into());
                }
            }
        }
        match first_error {
            Some(e) if summaries.is_empty() => Err(e),
            Some(e) => {
                log::error!("Meeting track failed: {e}");
                Ok(summaries)
            }
            None => Ok(summaries),
        }
    }
}

#[cfg(target_os = "macos")]
fn start_system() -> Result<(Source, Box<dyn Keepalive>), String> {
    let (source, tap) = super::system_audio::start()?;
    Ok((source, Box::new(tap)))
}

#[cfg(not(target_os = "macos"))]
fn start_system() -> Result<(Source, Box<dyn Keepalive>), String> {
    Err("System audio capture is only available on macOS".into())
}
