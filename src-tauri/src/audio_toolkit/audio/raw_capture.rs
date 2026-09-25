//! Optional copy of the microphone audio *before* input gain and VAD, for
//! benchmarking (Settings → For developers). Keeping the untouched signal
//! lets the same dictation be replayed offline with gain on, off or at other
//! levels, through the same VAD and speech model.
//!
//! Costs one relaxed atomic load per 30 ms frame while switched off.

use super::gain::GainState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Take {
    samples: Vec<f32>,
    gain_at_start: GainState,
}

pub struct RawCapture {
    enabled: AtomicBool,
    take: Mutex<Take>,
}

impl RawCapture {
    pub fn new(enabled: bool) -> Arc<Self> {
        Arc::new(Self {
            enabled: AtomicBool::new(enabled),
            take: Mutex::new(Take::default()),
        })
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
        if !enabled {
            *self.take.lock().unwrap() = Take::default();
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// A recording starts: forget the last one and note the AGC's state, so
    /// a replay can start from the same learned levels.
    pub(crate) fn begin(&self, gain_at_start: GainState) {
        if self.enabled() {
            *self.take.lock().unwrap() = Take {
                samples: Vec::new(),
                gain_at_start,
            };
        }
    }

    /// One 16 kHz frame, before any gain.
    pub(crate) fn push(&self, frame: &[f32]) {
        if self.enabled() {
            self.take.lock().unwrap().samples.extend_from_slice(frame);
        }
    }

    /// The last recording's raw audio and the AGC state it started from.
    pub fn take(&self) -> (Vec<f32>, GainState) {
        let take = std::mem::take(&mut *self.take.lock().unwrap());
        (take.samples, take.gain_at_start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_nothing_while_off() {
        let raw = RawCapture::new(false);
        raw.begin(GainState::default());
        raw.push(&[0.1; 480]);
        assert!(raw.take().0.is_empty());
    }

    #[test]
    fn each_recording_starts_empty() {
        let raw = RawCapture::new(true);
        raw.begin(GainState::default());
        raw.push(&[0.1; 480]);
        raw.begin(GainState {
            speech_level: Some(0.01),
            noise_floor: Some(0.001),
        });
        raw.push(&[0.2; 480]);
        let (samples, gain) = raw.take();
        assert_eq!(samples.len(), 480);
        assert_eq!(gain.speech_level, Some(0.01));
        assert!(raw.take().0.is_empty());
    }
}
