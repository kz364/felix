//! Offline levelling for the mic track of an in-person meeting, applied to
//! each chunk before transcription. Nothing here touches dictation's gain.
//!
//! Voices across the table reach the MacBook mic far quieter than a person
//! dictating. The whole chunk is available, so each stretch of speech can be
//! brought to the target level on its own, looking ahead, with no pumping:
//!
//! 1. The user's fixed meeting boost.
//! 2. A high-pass at 80 Hz (table thumps, air conditioning rumble).
//! 3. Each speech stretch raised toward about -20 dBFS, by up to
//!    [`MAX_BOOST_DB`]. Pauses between them are raised no further than keeps
//!    the room noise below [`NOISE_CEILING`], so silence doesn't turn to hiss.
//! 4. A soft limiter for peaks.

use super::transcript::FRAME_MS;
use crate::audio_toolkit::audio::{db_to_linear, soft_limit};

/// Speech level to aim for (RMS, about -20 dBFS), as dictation's AGC does.
const TARGET_RMS: f32 = 0.1;
/// Most a speech stretch is raised. Higher than dictation's (+30 dB), since
/// far voices are quieter. To be confirmed with real recordings (see the spec).
pub const MAX_BOOST_DB: f32 = 36.0;
/// Room noise in pauses is kept below this RMS (about -45 dBFS).
const NOISE_CEILING: f32 = 0.0056;
/// Speech stretches closer than this are levelled together.
const JOIN_GAP_FRAMES: usize = 10;
/// Gain changes are ramped over this many samples (20 ms), so they never click.
const RAMP: usize = 320;
const SAMPLE_RATE: f32 = 16_000.0;
const FRAME_SAMPLES: usize = (16_000 * FRAME_MS / 1000) as usize;

#[derive(Debug, Clone, Copy)]
pub struct LevelSettings {
    /// Fixed gain in dB, applied first.
    pub boost_db: f32,
    /// Level each speech stretch (off: only the fixed boost and the high-pass).
    pub auto: bool,
    /// Most a speech stretch is raised, in dB ([`MAX_BOOST_DB`] by default).
    pub max_boost_db: f32,
}

impl LevelSettings {
    pub fn new(boost_db: f32, auto: bool) -> Self {
        Self {
            boost_db,
            auto,
            max_boost_db: MAX_BOOST_DB,
        }
    }
}

/// A second-order Butterworth high-pass, run forward over the samples.
fn high_pass(audio: &mut [f32], cutoff: f32) {
    let w = 2.0 * std::f32::consts::PI * cutoff / SAMPLE_RATE;
    let (sin, cos) = w.sin_cos();
    let alpha = sin / std::f32::consts::SQRT_2;
    let a0 = 1.0 + alpha;
    let b0 = (1.0 + cos) / 2.0 / a0;
    let b1 = -(1.0 + cos) / a0;
    let b2 = b0;
    let a1 = -2.0 * cos / a0;
    let a2 = (1.0 - alpha) / a0;
    let (mut x1, mut x2, mut y1, mut y2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    for s in audio.iter_mut() {
        let x = *s;
        let y = b0 * x + b1 * x1 + b2 * x2 - a1 * y1 - a2 * y2;
        (x2, x1, y2, y1) = (x1, x, y1, y);
        *s = y;
    }
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
}

/// Level `audio` (a chunk starting at `start_sample` in the track), using
/// the track's per-frame speech flags.
pub fn level(audio: &mut [f32], start_sample: usize, speech: &[bool], settings: LevelSettings) {
    let fixed = db_to_linear(settings.boost_db);
    if (fixed - 1.0).abs() > 1e-3 {
        for s in audio.iter_mut() {
            *s *= fixed;
        }
    }
    high_pass(audio, 80.0);
    if !settings.auto || audio.is_empty() {
        for s in audio.iter_mut() {
            *s = soft_limit(*s);
        }
        return;
    }

    // Frames of this chunk, and which are speech.
    let first_frame = start_sample / FRAME_SAMPLES;
    let len = audio.len();
    let frame_range = |f: usize| {
        let from = (f * FRAME_SAMPLES).saturating_sub(start_sample).min(len);
        let to = ((f + 1) * FRAME_SAMPLES)
            .saturating_sub(start_sample)
            .min(len);
        from..to
    };
    let frames = (start_sample + audio.len()).div_ceil(FRAME_SAMPLES) - first_frame;
    let is_speech = |i: usize| speech.get(first_frame + i).copied().unwrap_or(false);

    // Speech stretches, joined across short gaps.
    let mut stretches: Vec<(usize, usize)> = Vec::new();
    for i in (0..frames).filter(|&i| is_speech(i)) {
        match stretches.last_mut() {
            Some((_, end)) if i - *end <= JOIN_GAP_FRAMES => *end = i + 1,
            _ => stretches.push((i, i + 1)),
        }
    }

    // Room noise: the quietest half of the non-speech frames.
    let mut quiet: Vec<f32> = (0..frames)
        .filter(|&i| !is_speech(i))
        .map(|i| rms(&audio[frame_range(first_frame + i)]))
        .filter(|r| *r > 0.0)
        .collect();
    quiet.sort_by(|a, b| a.total_cmp(b));
    let noise = quiet.get(quiet.len() / 2).copied().unwrap_or(0.0);
    let pause_gain = if noise > 0.0 {
        (NOISE_CEILING / noise).clamp(1.0, db_to_linear(settings.max_boost_db))
    } else {
        1.0
    };

    // Gain per frame: each stretch's own, the pause gain elsewhere.
    let mut gains = vec![pause_gain; frames];
    for &(from, to) in &stretches {
        let voiced: Vec<f32> = (from..to)
            .filter(|&i| is_speech(i))
            .flat_map(|i| audio[frame_range(first_frame + i)].to_vec())
            .collect();
        let level = rms(&voiced);
        if level <= 0.0 {
            continue;
        }
        let gain = (TARGET_RMS / level).clamp(1.0, db_to_linear(settings.max_boost_db));
        for g in &mut gains[from..to] {
            *g = gain;
        }
    }

    // Apply with ramps at every change.
    let mut current = gains[0];
    let mut ramp_from = current;
    let mut ramp_left = 0usize;
    for (i, g) in gains.iter().enumerate() {
        if (*g - current).abs() > 1e-4 {
            ramp_from = current;
            current = *g;
            ramp_left = RAMP;
        }
        for s in &mut audio[frame_range(first_frame + i)] {
            let gain = if ramp_left > 0 {
                let t = 1.0 - ramp_left as f32 / RAMP as f32;
                ramp_left -= 1;
                ramp_from + (current - ramp_from) * t
            } else {
                current
            };
            *s = soft_limit(*s * gain);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(amplitude: f32, len: usize) -> Vec<f32> {
        (0..len)
            .map(|i| amplitude * (i as f32 * 2.0 * std::f32::consts::PI * 300.0 / 16_000.0).sin())
            .collect()
    }

    fn rms_db(s: &[f32]) -> f32 {
        20.0 * rms(s).max(1e-9).log10()
    }

    #[test]
    fn a_far_voice_is_raised_to_the_target_and_a_near_one_is_left_alone() {
        // 1 s near (-14 dBFS), 1 s pause with faint noise, 1 s far (-44 dBFS).
        let mut audio = tone(0.28, 16_000);
        audio.extend((0..16_000).map(|i| if i % 2 == 0 { 3e-4 } else { -3e-4 }));
        audio.extend(tone(0.009, 16_000));
        let frames = audio.len() / FRAME_SAMPLES;
        let speech: Vec<bool> = (0..frames)
            .map(|f| f * FRAME_SAMPLES < 16_000 || f * FRAME_SAMPLES >= 32_000)
            .collect();
        level(&mut audio, 0, &speech, LevelSettings::new(0.0, true));
        let near = rms_db(&audio[2_000..14_000]);
        let far = rms_db(&audio[34_000..46_000]);
        // Loud speech is never turned down, only quiet speech up.
        assert!((near - -14.0).abs() < 1.0, "near at {near} dB");
        assert!((far - -20.0).abs() < 2.0, "far at {far} dB");
        // The pause isn't lifted into hiss.
        let pause = rms_db(&audio[20_000..28_000]);
        assert!(pause < -44.0, "pause at {pause} dB");
    }

    #[test]
    fn boost_is_bounded_and_low_rumble_is_removed() {
        // Very quiet speech: at most +36 dB.
        let mut audio = tone(0.0005, 16_000);
        let speech = vec![true; 16_000 / FRAME_SAMPLES];
        level(&mut audio, 0, &speech, LevelSettings::new(0.0, true));
        let got = rms_db(&audio[2_000..14_000]);
        let input = rms_db(&tone(0.0005, 16_000));
        assert!(
            got - input <= MAX_BOOST_DB + 0.5,
            "boosted by {}",
            got - input
        );
        // 30 Hz rumble is cut by the high-pass.
        let mut rumble: Vec<f32> = (0..16_000)
            .map(|i| 0.3 * (i as f32 * 2.0 * std::f32::consts::PI * 30.0 / 16_000.0).sin())
            .collect();
        level(&mut rumble, 0, &[], LevelSettings::new(0.0, false));
        assert!(rms_db(&rumble[4_000..]) < rms_db(&[0.3 / std::f32::consts::SQRT_2]) - 12.0);
    }
}
