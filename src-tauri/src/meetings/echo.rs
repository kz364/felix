//! Is the call coming back through the speakers into the mic, and with what
//! delay? Found by cross-correlating the two tracks (GCC-PHAT) window by
//! window: real echo shows up as a clear peak at the same lag again and
//! again (the speaker-to-mic delay), while a mic that only hears the user,
//! or headphones, never does.
//!
//! The level-based echo mask (`pipeline::echo_mask`) then only works where
//! this found echo, and looks back by the measured delay. With headphones
//! (or no echo at all) nothing is masked, so the user's quiet replies while
//! the other side talks are never taken for echo.

use rustfft::num_complex::Complex32;
use rustfft::FftPlanner;
use std::path::Path;

/// Analysis rate: speech correlates fine at 8 kHz and it halves the work.
const RATE: usize = 8_000;
/// Window length (about 2 s at 8 kHz), a power of two for the FFT.
const WINDOW: usize = 16_384;
/// Windows start every second.
const HOP: usize = RATE;
/// The mic lags what the Mac plays by at most this much.
const MAX_LAG_MS: usize = 500;
/// The call audio the browser extension sends reaches the system track
/// after the speakers have played it, so the mic can be ahead by this much.
const MAX_LEAD_MS: usize = 1_000;
/// A window counts only if both tracks have sound in it (RMS).
const ENERGY_FLOOR: f32 = 0.002;
/// GCC-PHAT peak above which a window is taken to contain echo. Unrelated
/// signals peak around 0.01–0.02 at this window length.
const SCORE_FLOOR: f32 = 0.05;
/// Echo windows must agree on the lag within this much.
const LAG_TOLERANCE_MS: f32 = 35.0;
/// Below this share of windows with echo, the meeting is treated as echo-free
/// (headphones, or a quiet room).
const MIN_ECHO_SHARE: f32 = 0.05;
/// VAD frame length used by the pipeline, in ms.
const FRAME_MS: usize = super::transcript::FRAME_MS as usize;

/// What the correlation found.
#[derive(Debug, Clone, PartialEq)]
pub struct EchoReport {
    /// The speaker-to-mic delay, when there's echo; negative when the
    /// system track has the sound after the mic (extension call audio).
    pub lag_ms: Option<f32>,
    /// Per pipeline frame: inside a window where echo was confirmed.
    pub frames: Vec<bool>,
    /// Share of windows (with sound on both tracks) that held echo.
    pub share: f32,
}

impl EchoReport {
    pub fn none(frames: usize) -> Self {
        Self {
            lag_ms: None,
            frames: vec![false; frames],
            share: 0.0,
        }
    }

    pub fn has_echo(&self) -> bool {
        self.lag_ms.is_some()
    }
}

/// Read a 16 kHz mono 16-bit WAV at [`RATE`].
fn read_decimated(wav: &Path) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(wav).map_err(|e| format!("{}: {e}", wav.display()))?;
    let mut out = Vec::with_capacity(reader.duration() as usize / 2 + 1);
    let mut pair = None;
    for s in reader.samples::<i16>() {
        let v = s.map_err(|e| e.to_string())? as f32 / i16::MAX as f32;
        match pair.take() {
            None => pair = Some(v),
            Some(a) => out.push(0.5 * (a + v)),
        }
    }
    Ok(out)
}

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

/// GCC-PHAT between two equal-length windows: the peak's lag (samples the
/// mic is behind; negative when it's ahead) and its height, over lags
/// -`max_lead`..=`max_lag`.
fn gcc_phat(
    planner: &mut FftPlanner<f32>,
    mic: &[f32],
    system: &[f32],
    max_lag: usize,
    max_lead: usize,
) -> (i64, f32) {
    let n = mic.len() * 2;
    let fft = planner.plan_fft_forward(n);
    let ifft = planner.plan_fft_inverse(n);
    let to_complex = |x: &[f32]| {
        let mut v: Vec<Complex32> = x.iter().map(|&s| Complex32::new(s, 0.0)).collect();
        v.resize(n, Complex32::new(0.0, 0.0));
        v
    };
    let mut a = to_complex(mic);
    let mut b = to_complex(system);
    fft.process(&mut a);
    fft.process(&mut b);
    let mut r: Vec<Complex32> = a
        .iter()
        .zip(&b)
        .map(|(x, y)| {
            let c = x * y.conj();
            let m = c.norm();
            if m > 1e-12 {
                c / m
            } else {
                Complex32::new(0.0, 0.0)
            }
        })
        .collect();
    ifft.process(&mut r);
    // Positive lags: the mic is behind the system audio. Negative ones wrap
    // around to the end.
    let mut best = (0, f32::MIN);
    let lags = (0..=max_lag as i64).chain(-(max_lead.min(n / 2 - 1) as i64)..0);
    for lag in lags {
        let score = r[lag.rem_euclid(n as i64) as usize].re / n as f32;
        if score > best.1 {
            best = (lag, score);
        }
    }
    best
}

/// Correlate two tracks sampled at [`RATE`]; `frames` is the length of
/// the pipeline's per-frame masks.
pub fn analyze_samples(mic: &[f32], system: &[f32], frames: usize) -> EchoReport {
    let len = mic.len().min(system.len());
    if len < WINDOW {
        return EchoReport::none(frames);
    }
    let max_lag = MAX_LAG_MS * RATE / 1000;
    let max_lead = MAX_LEAD_MS * RATE / 1000;
    let mut planner = FftPlanner::new();
    // (window start, lag in samples, score) for windows with sound on both.
    let mut windows = Vec::new();
    let mut start = 0;
    while start + WINDOW <= len {
        let (m, s) = (&mic[start..start + WINDOW], &system[start..start + WINDOW]);
        if rms(m) > ENERGY_FLOOR && rms(s) > ENERGY_FLOOR {
            let (lag, score) = gcc_phat(&mut planner, m, s, max_lag, max_lead);
            windows.push((start, lag, score));
        }
        start += HOP;
    }
    let peaks: Vec<f32> = windows
        .iter()
        .filter(|w| w.2 >= SCORE_FLOOR)
        .map(|w| w.1 as f32 * 1000.0 / RATE as f32)
        .collect();
    if windows.is_empty() || peaks.is_empty() {
        return EchoReport::none(frames);
    }
    let mut sorted = peaks.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let lag_ms = sorted[sorted.len() / 2];
    let confirmed: Vec<usize> = windows
        .iter()
        .filter(|w| {
            w.2 >= SCORE_FLOOR
                && (w.1 as f32 * 1000.0 / RATE as f32 - lag_ms).abs() <= LAG_TOLERANCE_MS
        })
        .map(|w| w.0)
        .collect();
    let share = confirmed.len() as f32 / windows.len() as f32;
    log::info!(
        "Echo: {:.0}% of {} windows at {lag_ms:.0} ms",
        share * 100.0,
        windows.len()
    );
    if share < MIN_ECHO_SHARE {
        return EchoReport {
            lag_ms: None,
            frames: vec![false; frames],
            share,
        };
    }
    let mut mask = vec![false; frames];
    let frame_samples = RATE * FRAME_MS / 1000;
    for start in confirmed {
        // A window vouches for itself and the half window either side, so
        // stretches between two echo windows are covered.
        let from = start.saturating_sub(WINDOW / 2) / frame_samples;
        let to = ((start + WINDOW + WINDOW / 2) / frame_samples).min(frames);
        for m in mask.iter_mut().take(to).skip(from) {
            *m = true;
        }
    }
    EchoReport {
        lag_ms: Some(lag_ms),
        frames: mask,
        share,
    }
}

/// Correlate a meeting's two tracks.
pub fn analyze(mic_wav: &Path, system_wav: &Path, frames: usize) -> Result<EchoReport, String> {
    let mic = read_decimated(mic_wav)?;
    let system = read_decimated(system_wav)?;
    Ok(analyze_samples(&mic, &system, frames))
}

/// Output devices that keep the call out of the mic.
const HEADPHONE_WORDS: &[&str] = &[
    "headphone",
    "headset",
    "earphone",
    "earbud",
    "buds",
    "airpods",
    "air pods",
    "earpods",
    "beats",
    "hands-free",
];

/// Whether an output device's name says it's headphones.
pub fn is_headphones(name: &str) -> bool {
    let name = name.to_lowercase();
    HEADPHONE_WORDS.iter().any(|w| name.contains(w))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Speech-like noise: bursts of pseudo-random noise with pauses.
    fn voice(seed: u32, seconds: usize) -> Vec<f32> {
        let mut x = seed.wrapping_mul(2_654_435_761).max(1);
        (0..seconds * RATE)
            .map(|i| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let noise = (x as f32 / u32::MAX as f32) - 0.5;
                let on = (i / (RATE / 3)) % 4 != 3;
                if on {
                    0.2 * noise
                } else {
                    0.0
                }
            })
            .collect()
    }

    fn frames(len: usize) -> usize {
        len * 1000 / RATE / FRAME_MS
    }

    #[test]
    fn echo_is_found_with_its_delay() {
        let system = voice(1, 30);
        let lag = 120 * RATE / 1000; // 120 ms
        let noise = voice(9, 30);
        let mic: Vec<f32> = (0..system.len())
            .map(|i| {
                let echo = if i >= lag { 0.1 * system[i - lag] } else { 0.0 };
                echo + 0.02 * noise[i]
            })
            .collect();
        let report = analyze_samples(&mic, &system, frames(mic.len()));
        let got = report.lag_ms.expect("echo found");
        assert!((got - 120.0).abs() < 2.0, "{got}");
        assert!(report.share > 0.8);
        assert!(report.frames.iter().filter(|&&f| f).count() > report.frames.len() / 2);
    }

    #[test]
    fn call_audio_that_arrives_after_the_mic_heard_it_is_echo_too() {
        // The extension's copy of the call lands 300 ms after the speakers
        // played it.
        let call = voice(1, 30);
        let late = 300 * RATE / 1000;
        let noise = voice(9, 30);
        let mic: Vec<f32> = (0..call.len())
            .map(|i| 0.1 * call[i] + 0.02 * noise[i])
            .collect();
        let system: Vec<f32> = (0..call.len())
            .map(|i| if i >= late { call[i - late] } else { 0.0 })
            .collect();
        let report = analyze_samples(&mic, &system, frames(mic.len()));
        let got = report.lag_ms.expect("echo found");
        assert!((got + 300.0).abs() < 2.0, "{got}");
    }

    #[test]
    fn a_mic_that_only_hears_the_user_has_no_echo() {
        let system = voice(1, 30);
        let mic = voice(2, 30);
        let report = analyze_samples(&mic, &system, frames(mic.len()));
        assert!(!report.has_echo(), "{report:?}");
        assert!(report.frames.iter().all(|&f| !f));
    }

    #[test]
    fn headphones_are_recognised_by_name() {
        assert!(is_headphones("Kaspar's AirPods Pro"));
        assert!(is_headphones("Jabra Evolve2 Headset"));
        assert!(!is_headphones("MacBook Pro Speakers"));
    }
}
