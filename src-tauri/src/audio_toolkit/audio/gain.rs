//! Input gain applied to 16 kHz frames *before* VAD.
//!
//! Quiet input (whispering, a clip-on mic at chest height, a low-gain USB
//! interface) otherwise reaches Silero below its speech threshold and is
//! dropped before the ASR model ever sees it. Two stages run per frame:
//!
//! 1. A fixed, user-set gain in dB.
//! 2. An optional automatic gain control that tracks the speech level and
//!    raises it toward a target, bounded by a maximum boost and by a noise
//!    ceiling so room noise is never lifted into the speech range.
//!
//! A soft limiter keeps boosted peaks from clipping. The AGC state lives for
//! the life of the input stream, so a user who whispers every dictation gets
//! the right gain from the first syllable of the next one.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// Speech level the AGC steers toward (≈ -20 dBFS RMS).
const TARGET_RMS: f32 = 0.1;
/// Upper bound on automatic boost (+30 dB).
const MAX_AUTO_GAIN: f32 = 31.6;
/// Automatic gain never lowers the signal below unity; loud input is left to
/// the limiter so normal speech is untouched.
const MIN_AUTO_GAIN: f32 = 1.0;
/// Boosted noise floor must stay below this level (≈ -38 dBFS RMS).
const NOISE_CEILING_RMS: f32 = 0.0126;
/// A frame counts as speech when it sits this far above the noise floor (≈ +10 dB).
const SPEECH_OVER_NOISE: f32 = 3.16;
/// Absolute floor below which frames are treated as digital silence.
const SILENCE_RMS: f32 = 1e-5;
/// Where the soft limiter starts bending the waveform.
const LIMITER_KNEE: f32 = 0.8;

/// Settings shared between the settings UI and the capture thread.
#[derive(Debug)]
pub struct GainConfig {
    gain_db_bits: AtomicU32,
    auto_gain: AtomicBool,
}

impl GainConfig {
    pub fn new(gain_db: f32, auto_gain: bool) -> Arc<Self> {
        Arc::new(Self {
            gain_db_bits: AtomicU32::new(gain_db.to_bits()),
            auto_gain: AtomicBool::new(auto_gain),
        })
    }

    pub fn set_gain_db(&self, gain_db: f32) {
        self.gain_db_bits
            .store(gain_db.clamp(-20.0, 30.0).to_bits(), Ordering::Relaxed);
    }

    pub fn gain_db(&self) -> f32 {
        f32::from_bits(self.gain_db_bits.load(Ordering::Relaxed))
    }

    pub fn set_auto_gain(&self, enabled: bool) {
        self.auto_gain.store(enabled, Ordering::Relaxed);
    }

    pub fn auto_gain(&self) -> bool {
        self.auto_gain.load(Ordering::Relaxed)
    }
}

/// Per-stream gain state. Not `Sync`; owned by the capture thread.
pub struct InputGain {
    config: Arc<GainConfig>,
    noise_floor: f32,
    speech_level: Option<f32>,
    applied_gain: f32,
    scratch: Vec<f32>,
}

impl InputGain {
    pub fn new(config: Arc<GainConfig>) -> Self {
        Self {
            config,
            noise_floor: 1e-3,
            speech_level: None,
            applied_gain: 1.0,
            scratch: Vec::new(),
        }
    }

    /// Current total linear gain (fixed × automatic), for diagnostics.
    pub fn current_gain(&self) -> f32 {
        self.applied_gain
    }

    /// Return `frame` with gain applied. Borrow ends before the next call.
    pub fn process<'a>(&'a mut self, frame: &[f32]) -> &'a [f32] {
        let fixed = db_to_linear(self.config.gain_db());
        let auto_enabled = self.config.auto_gain();

        let target = if auto_enabled {
            let rms = rms(frame) * fixed;
            fixed * self.update_auto_gain(rms)
        } else {
            fixed
        };

        if (target - 1.0).abs() < 1e-3 && (self.applied_gain - 1.0).abs() < 1e-3 {
            self.applied_gain = 1.0;
            self.scratch.clear();
            self.scratch.extend_from_slice(frame);
            return &self.scratch;
        }

        // Ramp across the frame so gain changes never click.
        let start = self.applied_gain;
        let n = frame.len().max(1) as f32;
        self.scratch.clear();
        self.scratch.extend(frame.iter().enumerate().map(|(i, &s)| {
            let g = start + (target - start) * (i as f32 + 1.0) / n;
            soft_limit(s * g)
        }));
        self.applied_gain = target;
        &self.scratch
    }

    /// Update level trackers with one frame's RMS (after fixed gain) and
    /// return the automatic gain to apply.
    fn update_auto_gain(&mut self, rms: f32) -> f32 {
        if rms < SILENCE_RMS {
            return self.auto_gain_for_current_state();
        }

        // Noise floor: follows quiet frames quickly, creeps up slowly so
        // sustained speech doesn't get mistaken for noise.
        if rms < self.noise_floor {
            self.noise_floor = self.noise_floor * 0.9 + rms * 0.1;
        } else {
            self.noise_floor = self.noise_floor * 0.998 + rms * 0.002;
        }

        if rms > self.noise_floor * SPEECH_OVER_NOISE {
            self.speech_level = Some(match self.speech_level {
                None => rms,
                // Fast attack so a loud syllable is caught within a few frames,
                // slow release so gain doesn't pump between words.
                Some(level) if rms > level => level * 0.7 + rms * 0.3,
                Some(level) => level * 0.97 + rms * 0.03,
            });
        }

        self.auto_gain_for_current_state()
    }

    fn auto_gain_for_current_state(&self) -> f32 {
        let Some(level) = self.speech_level else {
            return 1.0;
        };
        let wanted = TARGET_RMS / level.max(SILENCE_RMS);
        let noise_cap = NOISE_CEILING_RMS / self.noise_floor.max(SILENCE_RMS);
        wanted.min(noise_cap).clamp(MIN_AUTO_GAIN, MAX_AUTO_GAIN)
    }
}

pub fn db_to_linear(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

fn rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt()
}

/// Linear below the knee, smoothly saturating to ±1.0 above it.
fn soft_limit(s: f32) -> f32 {
    let a = s.abs();
    if a <= LIMITER_KNEE {
        return s;
    }
    let headroom = 1.0 - LIMITER_KNEE;
    s.signum() * (LIMITER_KNEE + headroom * ((a - LIMITER_KNEE) / headroom).tanh())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amplitude: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| amplitude * (i as f32 * 2.0 * std::f32::consts::PI * 220.0 / 16_000.0).sin())
            .collect()
    }

    fn noise(amplitude: f32, len: usize, seed: &mut u32) -> Vec<f32> {
        (0..len)
            .map(|_| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                amplitude * ((*seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0)
            })
            .collect()
    }

    #[test]
    fn unity_when_disabled_and_zero_db() {
        let mut gain = InputGain::new(GainConfig::new(0.0, false));
        let frame = sine(0.2, 512);
        assert_eq!(gain.process(&frame), frame.as_slice());
    }

    #[test]
    fn fixed_gain_scales_signal() {
        let mut gain = InputGain::new(GainConfig::new(6.0, false));
        let frame = sine(0.05, 512);
        // Settle the ramp, then measure.
        gain.process(&frame);
        let out = gain.process(&frame).to_vec();
        let ratio = rms(&out) / rms(&frame);
        assert!((ratio - db_to_linear(6.0)).abs() < 0.02, "ratio {ratio}");
    }

    #[test]
    fn auto_gain_lifts_whisper_level_speech() {
        let mut gain = InputGain::new(GainConfig::new(0.0, true));
        let mut seed = 7;
        // Quiet room, then whisper-level speech (~-46 dBFS RMS).
        for _ in 0..40 {
            gain.process(&noise(0.0003, 512, &mut seed));
        }
        let whisper = sine(0.007, 512);
        let mut out = Vec::new();
        for _ in 0..40 {
            out = gain.process(&whisper).to_vec();
        }
        let boosted_db = 20.0 * (rms(&out) / rms(&whisper)).log10();
        assert!(boosted_db > 15.0, "boost only {boosted_db:.1} dB");
        assert!(gain.current_gain() <= MAX_AUTO_GAIN + 1e-3);
    }

    #[test]
    fn auto_gain_does_not_lift_noise_floor_past_ceiling() {
        let mut gain = InputGain::new(GainConfig::new(0.0, true));
        let mut seed = 11;
        // Noisy room (~-45 dBFS) with quiet speech on top.
        for _ in 0..60 {
            gain.process(&noise(0.01, 512, &mut seed));
        }
        for _ in 0..40 {
            let mut frame = noise(0.01, 512, &mut seed);
            for (s, t) in frame.iter_mut().zip(sine(0.05, 512)) {
                *s += t;
            }
            gain.process(&frame);
        }
        let floor_after = gain.noise_floor * gain.current_gain();
        assert!(
            floor_after <= NOISE_CEILING_RMS * 1.6,
            "floor {floor_after}"
        );
    }

    #[test]
    fn normal_speech_is_left_alone() {
        let mut gain = InputGain::new(GainConfig::new(0.0, true));
        let loud = sine(0.3, 512);
        for _ in 0..20 {
            gain.process(&loud);
        }
        assert!((gain.current_gain() - 1.0).abs() < 1e-3);
    }

    #[test]
    fn limiter_never_exceeds_full_scale() {
        let mut gain = InputGain::new(GainConfig::new(30.0, false));
        let frame = sine(0.5, 512);
        gain.process(&frame);
        assert!(gain.process(&frame).iter().all(|s| s.abs() <= 1.0));
    }
}
