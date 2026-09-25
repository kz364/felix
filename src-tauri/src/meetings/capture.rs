//! Recording a meeting to disk: the mic and (on calls) the system audio as
//! separate tracks, `mic.wav` and `system.wav`, on one shared clock. Each
//! source fills a ring buffer from its audio thread; a writer thread per
//! track drains it, resamples to 16 kHz and appends to the WAV.

use super::track::{TrackSummary, TrackWriter, SAMPLE_RATE};
use crate::audio_toolkit::audio::FrameResampler;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
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
    t0: Instant,
    stop_at: StopAt,
    tracks: Vec<Track>,
}

fn spawn_writer(
    mut source: Source,
    path: PathBuf,
    name: &'static str,
    t0: Instant,
    stop_at: StopAt,
) -> Result<JoinHandle<Result<TrackSummary, String>>, String> {
    let mut writer = TrackWriter::create(&path, name, t0)?;
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
                resampler.push(&buf, |frame| {
                    if result.is_ok() {
                        result = writer.write(frame, now);
                    }
                });
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

impl Recording {
    /// Start recording into `dir` (created if needed). `mic` is the input
    /// device, `None` for the system default. On a call, failing to capture
    /// system audio doesn't stop the recording: it goes on with the mic, and
    /// `system_error` says why.
    pub fn start(dir: &Path, mode: MeetingMode, mic: Option<cpal::Device>) -> Result<Self, String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Couldn't create {}: {e}", dir.display()))?;
        let t0 = Instant::now();
        let stop_at = StopAt::default();
        let mut tracks = Vec::new();

        let (source, stream) = super::mic::start(mic)?;
        let mic_label = source.label.clone();
        tracks.push(Track {
            writer: spawn_writer(source, dir.join("mic.wav"), "mic", t0, stop_at.clone())?,
            keepalive: Box::new(stream),
        });

        let mut system_error = None;
        if mode == MeetingMode::Call {
            match start_system() {
                Ok((source, keepalive)) => {
                    tracks.push(Track {
                        writer: spawn_writer(
                            source,
                            dir.join("system.wav"),
                            "system",
                            t0,
                            stop_at.clone(),
                        )?,
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
            t0,
            stop_at,
            tracks,
        })
    }

    pub fn elapsed(&self) -> Duration {
        self.t0.elapsed()
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
