//! One track of a meeting recording: 16 kHz mono 16-bit WAV, written as the
//! audio arrives and kept valid on disk, placed on the meeting's shared clock
//! so the mic and system tracks line up.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const SAMPLE_RATE: u32 = 16_000;
/// How often the WAV header is rewritten, so a crash loses at most this much.
const FLUSH_EVERY: Duration = Duration::from_secs(2);
/// A track further behind the clock than this (a device that stalled or
/// dropped out) is padded with silence so later audio stays in place.
const MAX_LAG: Duration = Duration::from_millis(500);
/// Audio arriving this much later than where the track has got to (after a
/// quiet stretch: the system tap sends nothing while nothing plays) is moved
/// to where it belongs.
const RESYNC: Duration = Duration::from_millis(150);

/// What a finished track holds.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct TrackSummary {
    pub file: String,
    pub source: String,
    pub seconds: f64,
    /// Loudest sample in dBFS; very low means the track is silent (for the
    /// system track, usually a missing permission).
    pub peak_dbfs: f32,
    /// Seconds of silence added to keep the track on the clock.
    pub padded_seconds: f64,
}

pub struct TrackWriter {
    writer: hound::WavWriter<BufWriter<File>>,
    path: PathBuf,
    source: String,
    t0: Instant,
    written: u64,
    padded: u64,
    peak: f32,
    last_flush: Instant,
}

/// How long a finished (or flushed) track is.
pub fn duration_of(path: &Path) -> Option<Duration> {
    let reader = hound::WavReader::open(path).ok()?;
    Some(Duration::from_secs_f64(
        reader.duration() as f64 / reader.spec().sample_rate as f64,
    ))
}

fn samples_for(d: Duration) -> u64 {
    (d.as_secs_f64() * SAMPLE_RATE as f64) as u64
}

impl TrackWriter {
    pub fn create(path: &Path, source: &str, t0: Instant) -> Result<Self, String> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let writer = hound::WavWriter::create(path, spec)
            .map_err(|e| format!("Couldn't create {}: {e}", path.display()))?;
        Ok(Self::around(writer, path, source, t0, 0))
    }

    /// Carry on writing a track from an earlier recording of the meeting
    /// (or start it, if it isn't there). `t0` is where the resumed recording's
    /// clock starts, so it's placed after what's already there.
    pub fn append(path: &Path, source: &str, t0: Instant) -> Result<Self, String> {
        if !path.is_file() {
            return Self::create(path, source, t0);
        }
        let writer = hound::WavWriter::append(path)
            .map_err(|e| format!("Couldn't reopen {}: {e}", path.display()))?;
        let written = writer.len() as u64;
        Ok(Self::around(writer, path, source, t0, written))
    }

    fn around(
        writer: hound::WavWriter<BufWriter<File>>,
        path: &Path,
        source: &str,
        t0: Instant,
        written: u64,
    ) -> Self {
        Self {
            writer,
            path: path.to_path_buf(),
            source: source.to_string(),
            t0,
            written,
            padded: 0,
            peak: 0.0,
            last_flush: Instant::now(),
        }
    }

    fn pad(&mut self, samples: u64) -> Result<(), String> {
        for _ in 0..samples {
            self.writer.write_sample(0i16).map_err(|e| e.to_string())?;
        }
        self.written += samples;
        self.padded += samples;
        Ok(())
    }

    /// Before writing `incoming` samples that arrived at `now`: if they
    /// started well after where the track has got to, pad up to their start.
    /// This places the first audio (streams open at different speeds) and
    /// audio after a gap.
    pub fn align(&mut self, incoming: u64, now: Instant) -> Result<(), String> {
        let starts_at = samples_for(now.duration_since(self.t0)).saturating_sub(incoming);
        let threshold = if self.written == 0 {
            0
        } else {
            samples_for(RESYNC)
        };
        if starts_at > self.written + threshold {
            self.pad(starts_at - self.written)?;
        }
        Ok(())
    }

    /// Append audio at [`SAMPLE_RATE`].
    pub fn write(&mut self, samples: &[f32], now: Instant) -> Result<(), String> {
        for &s in samples {
            self.peak = self.peak.max(s.abs());
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            self.writer.write_sample(v).map_err(|e| e.to_string())?;
        }
        self.written += samples.len() as u64;
        self.maybe_flush(now)
    }

    /// Pad with silence if the track has fallen well behind the clock.
    pub fn keep_up(&mut self, now: Instant) -> Result<(), String> {
        let expected = samples_for(now.duration_since(self.t0));
        if expected > self.written + samples_for(MAX_LAG) {
            self.pad(expected - self.written)?;
        }
        self.maybe_flush(now)
    }

    fn maybe_flush(&mut self, now: Instant) -> Result<(), String> {
        if now.duration_since(self.last_flush) >= FLUSH_EVERY {
            // Rewrites the header, so the file is playable up to here.
            self.writer.flush().map_err(|e| e.to_string())?;
            self.last_flush = now;
        }
        Ok(())
    }

    /// Pad to the clock at `now` (so every track ends at the same time) and
    /// close the file.
    pub fn finish_at(mut self, now: Instant) -> Result<TrackSummary, String> {
        let expected = samples_for(now.duration_since(self.t0));
        if expected > self.written {
            self.pad(expected - self.written)?;
        }
        self.finish()
    }

    pub fn finish(self) -> Result<TrackSummary, String> {
        let summary = TrackSummary {
            file: self
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            source: self.source,
            seconds: self.written as f64 / SAMPLE_RATE as f64,
            peak_dbfs: 20.0 * self.peak.max(1e-6).log10(),
            padded_seconds: self.padded as f64 / SAMPLE_RATE as f64,
        };
        self.writer.finalize().map_err(|e| e.to_string())?;
        Ok(summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("handy-track-{}-{name}.wav", std::process::id()))
    }

    #[test]
    fn first_audio_is_placed_on_the_clock() {
        let path = temp("offset");
        let t0 = Instant::now();
        let mut w = TrackWriter::create(&path, "mic", t0).unwrap();
        // 100 ms of audio arriving 300 ms in: it starts at 200 ms.
        let at = t0 + Duration::from_millis(300);
        w.align(1600, at).unwrap();
        w.write(&[0.5; 1600], at).unwrap();
        let s = w.finish().unwrap();
        assert!((s.seconds - 0.3).abs() < 0.001);
        assert!((s.padded_seconds - 0.2).abs() < 0.001);
        let samples: Vec<i16> = hound::WavReader::open(&path)
            .unwrap()
            .samples()
            .map(Result::unwrap)
            .collect();
        assert_eq!(samples[3199], 0);
        assert!(samples[3200] > 16000);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn stalls_are_padded_and_the_file_stays_readable_before_finish() {
        let path = temp("stall");
        let t0 = Instant::now();
        let mut w = TrackWriter::create(&path, "system", t0).unwrap();
        w.align(1600, t0 + Duration::from_millis(100)).unwrap();
        w.write(&[0.1; 1600], t0 + Duration::from_millis(100))
            .unwrap();
        // Nothing for 3 s: padded up to the clock.
        w.keep_up(t0 + Duration::from_secs(3)).unwrap();
        // The header was flushed, so another reader sees the audio so far.
        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.duration(), 3 * SAMPLE_RATE);
        let s = w.finish().unwrap();
        assert!((s.seconds - 3.0).abs() < 0.001);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn audio_after_a_quiet_stretch_lands_where_it_belongs() {
        let path = temp("resync");
        let t0 = Instant::now();
        let mut w = TrackWriter::create(&path, "system", t0).unwrap();
        w.align(1600, t0 + Duration::from_millis(100)).unwrap();
        w.write(&[0.1; 1600], t0 + Duration::from_millis(100))
            .unwrap();
        // Nothing until 400 ms of audio arrives at 1.4 s: it started at 1.0 s.
        let at = t0 + Duration::from_millis(1400);
        w.align(6400, at).unwrap();
        w.write(&[0.1; 6400], at).unwrap();
        let s = w.finish().unwrap();
        assert!((s.seconds - 1.4).abs() < 0.001);
        assert!((s.padded_seconds - 0.9).abs() < 0.001);
        // Small jitter isn't padded.
        let mut w = TrackWriter::create(&path, "mic", t0).unwrap();
        w.align(1600, t0 + Duration::from_millis(100)).unwrap();
        w.write(&[0.1; 1600], t0).unwrap();
        w.align(800, t0 + Duration::from_millis(200)).unwrap();
        assert_eq!(w.finish().unwrap().padded_seconds, 0.0);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_resumed_track_carries_on_after_what_was_there() {
        let path = temp("append");
        let t0 = Instant::now();
        let mut w = TrackWriter::create(&path, "mic", t0).unwrap();
        w.write(&[0.1; 16_000], t0).unwrap();
        w.finish().unwrap();
        // Resumed later: its clock starts 1 s back, where the track ended.
        let now = Instant::now();
        let t0 = now - Duration::from_secs(1);
        let mut w = TrackWriter::append(&path, "mic", t0).unwrap();
        w.align(8_000, now + Duration::from_millis(500)).unwrap();
        w.write(&[0.2; 8_000], now + Duration::from_millis(500))
            .unwrap();
        let s = w.finish().unwrap();
        assert!((s.seconds - 1.5).abs() < 0.001, "{}", s.seconds);
        assert_eq!(duration_of(&path).unwrap().as_millis(), 1500);
        std::fs::remove_file(&path).unwrap();
    }
}
