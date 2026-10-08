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
/// How often a writer checks its device is still delivering.
const CHECK_EVERY: Duration = Duration::from_secs(2);
/// No samples for this long: the device died (unplugged, tap gone).
const STALLED_AFTER: Duration = Duration::from_secs(3);
/// Between attempts to reopen a device.
const REOPEN_EVERY: Duration = Duration::from_secs(5);
/// How much audio to count before trusting a measured sample rate.
const RATE_WINDOW: Duration = Duration::from_secs(4);

/// The rate a device is really delivering at, when it's clearly not the one
/// it declared: the nearest standard rate to `measured`, if that's more than
/// 15% off `declared`. The system audio tap reports 48 kHz even when the
/// output (AirPods on a call) runs at 24 kHz, which would record the call
/// at double speed with gaps.
fn actual_rate(declared: u32, measured: f64) -> Option<u32> {
    const RATES: [u32; 8] = [
        8_000, 11_025, 16_000, 22_050, 24_000, 32_000, 44_100, 48_000,
    ];
    if (measured / declared as f64 - 1.0).abs() <= 0.15 {
        return None;
    }
    let nearest = RATES.into_iter().min_by(|a, b| {
        (*a as f64 - measured)
            .abs()
            .total_cmp(&(*b as f64 - measured).abs())
    })?;
    ((nearest as f64 / measured - 1.0).abs() <= 0.1 && nearest != declared).then_some(nearest)
}

/// On a call, system audio silent this long gets its tap rebuilt, in case
/// the tap went dead while still delivering zeros.
const SYSTEM_SILENCE_LIMIT: Duration = Duration::from_secs(5 * 60);

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

type Opened = (Source, Box<dyn Keepalive>);

/// Where a track's audio comes from, reopened when it dies or moves.
trait Input: Send {
    fn open(&mut self) -> Result<Opened, String>;
    /// The device it should record has changed since it was opened (the
    /// system default moved to other hardware).
    fn moved(&mut self) -> bool {
        false
    }
    /// Reopen after this long without sound.
    fn silence_limit(&self) -> Option<Duration> {
        None
    }
    /// Gain to apply to its samples right now.
    fn gain(&mut self) -> f32 {
        1.0
    }
    /// Catch up with what changed around it, checked every [`CHECK_EVERY`].
    fn refresh(&mut self) {}
}

/// The meeting mic: the chosen device, or whatever the system default is.
struct MicInput {
    chosen: Option<cpal::Device>,
    opened: String,
    /// The system default when it was opened (it may be a dictation mic,
    /// which was skipped).
    default: Option<String>,
    /// The Mac's input volume for it when the meeting started, in dB.
    start_db: Option<f32>,
}

impl MicInput {
    fn volume_db(&self) -> Option<f32> {
        super::mic::input_volume_db(Some(self.opened.as_str()).filter(|o| !o.is_empty()))
    }
}

impl Input for MicInput {
    fn open(&mut self) -> Result<Opened, String> {
        self.default = super::mic::default_name();
        let (source, stream) = match super::mic::start(self.chosen.clone()) {
            Ok(opened) => opened,
            // The chosen mic went away mid-meeting: carry on with the default.
            Err(e) if self.chosen.is_some() && !self.opened.is_empty() => {
                log::warn!("Meeting mic couldn't reopen ({e}); using the default input");
                super::mic::start(None)?
            }
            Err(e) => return Err(e),
        };
        let reopened = !self.opened.is_empty();
        let device_changed = reopened && self.opened != source.label;
        self.opened = source.label.clone();
        if !reopened || device_changed {
            self.start_db = self.volume_db();
        }
        Ok((source, Box::new(stream)))
    }

    fn gain(&mut self) -> f32 {
        match (self.start_db, self.volume_db()) {
            (Some(start), Some(now)) => super::mic::undo_volume_change(start, now),
            _ => 1.0,
        }
    }

    fn moved(&mut self) -> bool {
        if self.chosen.is_some() {
            return false;
        }
        super::mic::default_name().is_some_and(|name| Some(name) != self.default)
    }
}

/// The call's audio: a tap on everything the Mac plays, but Safari.
#[cfg(target_os = "macos")]
struct SystemInput {
    output: Option<String>,
    tap: objc2_core_audio::AudioObjectID,
    left_out: Vec<objc2_core_audio::AudioObjectID>,
    /// The tap couldn't be changed in place to leave out what it should.
    stale: bool,
}

#[cfg(target_os = "macos")]
impl Input for SystemInput {
    fn open(&mut self) -> Result<Opened, String> {
        let (source, tap) = super::system_audio::start()?;
        self.output = super::system_audio::output_uid();
        self.tap = tap.tap;
        self.left_out = tap.left_out.clone();
        self.stale = false;
        Ok((source, Box::new(tap)))
    }

    fn moved(&mut self) -> bool {
        // The tap's aggregate device is built on the output it started with.
        self.stale || super::system_audio::output_uid() != self.output
    }

    fn refresh(&mut self) {
        // Safari opened (or a call moved into it) since the tap started.
        let now = super::system_audio::left_out_processes();
        if now == self.left_out {
            return;
        }
        match super::system_audio::leave_out(self.tap, &now) {
            Ok(()) => {
                log::info!("System audio tap now leaves out {} process(es)", now.len());
                self.left_out = now;
            }
            Err(e) => {
                log::warn!("{e}; rebuilding the tap");
                self.stale = true;
            }
        }
    }

    fn silence_limit(&self) -> Option<Duration> {
        Some(SYSTEM_SILENCE_LIMIT)
    }
}

/// Set once when the recording stops: the time every track ends at.
type StopAt = Arc<OnceLock<Instant>>;

struct Track {
    /// Owns the device too, and stops it before the last drain.
    writer: JoinHandle<Result<TrackSummary, String>>,
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
    mut input: Box<dyn Input>,
    (mut source, keepalive): Opened,
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
            let mut keepalive = Some(keepalive);
            let started = Instant::now();
            let (mut last_audio, mut last_loud) = (started, started);
            // Samples the device delivered since `counting_since`.
            let (mut counted, mut counting_since) = (0u64, started);
            let (mut last_check, mut last_open) = (started, started);
            let mut gain = input.gain();
            loop {
                let stopping = stop_at.get().copied();
                if stopping.is_some() {
                    // The device first, so nothing arrives after this drain.
                    drop(keepalive.take());
                }
                buf.clear();
                let n = source.consumer.slots();
                if let Ok(chunk) = source.consumer.read_chunk(n) {
                    let (a, b) = chunk.as_slices();
                    buf.extend_from_slice(a);
                    buf.extend_from_slice(b);
                    chunk.commit_all();
                }
                if gain != 1.0 {
                    for v in &mut buf {
                        *v *= gain;
                    }
                }
                let now = Instant::now();
                let incoming = buf.len() as u64 * SAMPLE_RATE as u64 / source.rate as u64;
                counted += buf.len() as u64;
                if incoming > 0 {
                    last_audio = now;
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
                    last_loud = now;
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
                let window = now.duration_since(counting_since);
                if window >= RATE_WINDOW {
                    let measured = counted as f64 / window.as_secs_f64();
                    // Only while audio flows: a stall is handled below.
                    if now.duration_since(last_audio) < STALLED_AFTER {
                        if let Some(rate) = actual_rate(source.rate, measured) {
                            log::warn!(
                                "Meeting {name} track: device says {} Hz but delivers about {measured:.0} Hz; recording at {rate} Hz",
                                source.rate
                            );
                            let mut flushed = Ok(());
                            resampler.finish(|frame| {
                                if flushed.is_ok() {
                                    flushed = writer.write(frame, now);
                                }
                            });
                            flushed?;
                            source.rate = rate;
                            resampler = FrameResampler::new(
                                rate as usize,
                                SAMPLE_RATE as usize,
                                Duration::from_millis(20),
                            );
                        }
                    }
                    (counted, counting_since) = (0, now);
                }
                if now.duration_since(last_check) >= CHECK_EVERY {
                    last_check = now;
                    input.refresh();
                    let new_gain = input.gain();
                    if (new_gain - gain).abs() > 0.01 {
                        log::info!(
                            "Meeting {name} track: input volume moved; gain now {:.1} dB",
                            20.0 * new_gain.log10()
                        );
                        gain = new_gain;
                    }
                    let silent = now.duration_since(last_loud.max(last_open));
                    let why = if now.duration_since(last_audio) >= STALLED_AFTER {
                        Some("no audio is arriving")
                    } else if input.moved() {
                        Some("the device changed")
                    } else if input.silence_limit().is_some_and(|limit| silent >= limit) {
                        Some("it has been silent")
                    } else {
                        None
                    };
                    if let Some(why) = why.filter(|_| now.duration_since(last_open) >= REOPEN_EVERY)
                    {
                        last_open = now;
                        log::warn!("Meeting {name} track: {why}; reopening the device");
                        drop(keepalive.take());
                        let mut flushed = Ok(());
                        resampler.finish(|frame| {
                            if flushed.is_ok() {
                                flushed = writer.write(frame, now);
                            }
                        });
                        flushed?;
                        match input.open() {
                            Ok((new_source, new_keepalive)) => {
                                log::info!("Meeting {name} track now on {}", new_source.label);
                                source = new_source;
                                keepalive = Some(new_keepalive);
                                resampler = FrameResampler::new(
                                    source.rate as usize,
                                    SAMPLE_RATE as usize,
                                    Duration::from_millis(20),
                                );
                                last_audio = Instant::now();
                                (counted, counting_since) = (0, last_audio);
                            }
                            // Gaps are padded with silence; try again shortly.
                            Err(e) => log::warn!("Meeting {name} track couldn't reopen: {e}"),
                        }
                    }
                }
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

        let mut mic = Box::new(MicInput {
            chosen: mic,
            opened: String::new(),
            default: None,
            start_db: None,
        });
        let opened = mic.open()?;
        let mic_label = opened.0.label.clone();
        tracks.push(Track {
            writer: spawn_writer(mic, opened, dir.join("mic.wav"), setup("mic"))?,
        });

        let mut system_error = None;
        if mode == MeetingMode::Call {
            match system_input() {
                Ok((input, opened)) => {
                    tracks.push(Track {
                        writer: spawn_writer(
                            input,
                            opened,
                            dir.join("system.wav"),
                            setup("system"),
                        )?,
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
        let writers: Vec<_> = self.tracks.into_iter().map(|t| t.writer).collect();
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
fn system_input() -> Result<(Box<dyn Input>, Opened), String> {
    let mut input = Box::new(SystemInput {
        output: None,
        tap: 0,
        left_out: Vec::new(),
        stale: false,
    });
    let opened = input.open()?;
    Ok((input, opened))
}

#[cfg(not(target_os = "macos"))]
fn system_input() -> Result<(Box<dyn Input>, Opened), String> {
    Err("System audio capture is only available on macOS".into())
}

#[cfg(test)]
mod tests {
    use super::actual_rate;

    #[test]
    fn a_device_running_slower_than_it_says_is_caught() {
        // AirPods on a call behind a tap that says 48 kHz.
        assert_eq!(actual_rate(48_000, 23_870.0), Some(24_000));
        assert_eq!(actual_rate(48_000, 16_100.0), Some(16_000));
        // Normal jitter is left alone.
        assert_eq!(actual_rate(48_000, 47_200.0), None);
        assert_eq!(actual_rate(24_000, 26_000.0), None);
        // Nothing near a standard rate: keep what it says.
        assert_eq!(actual_rate(48_000, 36_000.0), None);
    }
}
