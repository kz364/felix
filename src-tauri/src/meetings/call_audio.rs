//! The call's audio as the Felix Meetings extension hears it in the call
//! tab: only the other people, not the YouTube tab next to it. While it
//! comes in, the system audio tap leaves the browser out and this is mixed
//! into the system track in its place (see `SystemInput` in capture.rs).
//!
//! It arrives in bursts, so it waits in a small jitter buffer that the
//! system track's writer drains in step with the tap.

use super::track::SAMPLE_RATE;
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Audio held back before mixing starts, to ride out bursty delivery.
const PRIME: usize = SAMPLE_RATE as usize / 5;
/// More than this waiting means the writer fell behind: drop the oldest.
const MAX_BUFFERED: usize = SAMPLE_RATE as usize;
/// The extension counts as sending the call while it was heard this recently.
const LIVE_FOR: Duration = Duration::from_secs(3);

struct Buffer {
    samples: VecDeque<f32>,
    primed: bool,
    heard: Option<Instant>,
    /// The browser the extension runs in (the native host's parent).
    browser: Option<i32>,
}

impl Buffer {
    const fn new() -> Self {
        Self {
            samples: VecDeque::new(),
            primed: false,
            heard: None,
            browser: None,
        }
    }

    fn push(&mut self, browser: Option<i32>, samples: &[f32], now: Instant) {
        self.heard = Some(now);
        self.browser = browser.or(self.browser);
        self.samples.extend(samples);
        let over = self.samples.len().saturating_sub(MAX_BUFFERED);
        if over > 0 {
            // Keep it primed-deep, not a full second behind.
            self.samples.drain(..over + MAX_BUFFERED - PRIME);
        }
    }

    fn mix_into(&mut self, frame: &mut [f32]) {
        if !self.primed {
            if self.samples.len() < PRIME {
                return;
            }
            self.primed = true;
        }
        for s in frame.iter_mut() {
            match self.samples.pop_front() {
                Some(v) => *s += v,
                None => {
                    self.primed = false;
                    break;
                }
            }
        }
    }

    fn live_browser(&self, now: Instant) -> Option<i32> {
        self.heard
            .filter(|&t| now.duration_since(t) < LIVE_FOR)
            .and(self.browser)
    }
}

static BUFFER: Mutex<Buffer> = Mutex::new(Buffer::new());

fn buffer() -> std::sync::MutexGuard<'static, Buffer> {
    BUFFER.lock().unwrap_or_else(|e| e.into_inner())
}

/// Call audio from the extension, at [`SAMPLE_RATE`].
pub fn push(browser: Option<i32>, samples: &[f32]) {
    buffer().push(browser, samples, Instant::now());
}

/// Add the next stretch of call audio to a frame of the system track.
pub fn mix_into(frame: &mut [f32]) {
    buffer().mix_into(frame);
}

/// The browser whose call the extension is sending right now.
pub fn live_browser() -> Option<i32> {
    buffer().live_browser(Instant::now())
}

/// Little-endian 16-bit PCM, as the extension sends it.
pub fn decode(pcm: &[u8]) -> Vec<f32> {
    pcm.as_chunks::<2>()
        .0
        .iter()
        .map(|&b| i16::from_le_bytes(b) as f32 / i16::MAX as f32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_audio_is_held_back_then_mixed_in_order() {
        let mut b = Buffer::new();
        let now = Instant::now();
        let mut frame = [0.5f32; 4];
        b.push(Some(42), &[0.25; 100], now);
        b.mix_into(&mut frame);
        assert_eq!(frame, [0.5; 4], "not primed yet");
        let ramp: Vec<f32> = (0..PRIME).map(|i| i as f32).collect();
        b.push(None, &ramp, now);
        b.mix_into(&mut frame);
        assert_eq!(frame, [0.75; 4]);
        assert_eq!(b.live_browser(now), Some(42));
        assert_eq!(b.live_browser(now + LIVE_FOR), None);
    }

    #[test]
    fn running_dry_waits_to_prime_again_and_a_backlog_is_trimmed() {
        let mut b = Buffer::new();
        let now = Instant::now();
        b.push(None, &vec![0.1; PRIME], now);
        let mut frame = vec![0.0f32; PRIME + 10];
        b.mix_into(&mut frame);
        assert!(!b.primed);
        assert_eq!(frame[PRIME + 5], 0.0);
        b.push(None, &vec![0.1; MAX_BUFFERED + 500], now);
        assert_eq!(b.samples.len(), PRIME);
    }

    #[test]
    fn pcm_is_little_endian_16_bit() {
        assert_eq!(decode(&[0xff, 0x7f, 0x00, 0x00, 0x01]), vec![1.0, 0.0]);
    }
}
