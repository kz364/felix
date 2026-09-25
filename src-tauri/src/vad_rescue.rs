//! A safety net for quiet speech that silence detection threw away.
//!
//! After each dictation the raw microphone audio (before gain and VAD) is run
//! through the same gain and a much more sensitive Silero threshold. That
//! costs a few milliseconds per second of audio. Only when the sensitive pass
//! keeps clearly more audio is it transcribed as well, and its transcript
//! replaces the first one only if it contains the first one's words plus real
//! new words, not the phrases speech models invent from breathing or noise.

use crate::audio_toolkit::audio::{GainConfig, GainState, InputGain};
use crate::audio_toolkit::vad::{
    frames_for_duration_ms, SmoothedVad, VadFrame, VAD_OFFLINE_HANGOVER_MS, VAD_ONSET_MS,
    VAD_PREFILL_MS,
};
use crate::audio_toolkit::{SileroVad, VoiceActivityDetector};
use std::path::Path;

const RATE: usize = crate::audio_toolkit::constants::WHISPER_SAMPLE_RATE as usize;

/// The raw audio of one recording and the gain it was recorded with.
pub struct Raw {
    pub samples: Vec<f32>,
    pub gain_at_start: GainState,
    pub gain_db: f32,
    pub auto_gain: bool,
}

/// The sensitive threshold for a microphone whose normal one is `base`.
pub fn rescue_threshold(base: f32) -> f32 {
    if base > 0.1 {
        0.1
    } else {
        0.05
    }
}

/// The audio a sensitive pass keeps, through the same gain as the live path.
pub fn sensitive_pass(model: &Path, raw: &Raw, threshold: f32) -> anyhow::Result<Vec<f32>> {
    let silero = SileroVad::new(model, threshold)?;
    let n = silero.frame_samples();
    let mut vad = SmoothedVad::new(
        Box::new(silero),
        frames_for_duration_ms(VAD_PREFILL_MS, n),
        frames_for_duration_ms(VAD_OFFLINE_HANGOVER_MS, n),
        frames_for_duration_ms(VAD_ONSET_MS, n),
    );
    let mut gain = InputGain::new(GainConfig::seeded(
        raw.gain_db,
        raw.auto_gain,
        raw.gain_at_start,
    ));
    let mut kept = Vec::new();
    for frame in raw.samples.chunks_exact(n) {
        let frame = gain.process(frame).to_vec();
        if let Ok(VadFrame::Speech(s)) = vad.push_frame(&frame) {
            kept.extend_from_slice(s);
        }
    }
    Ok(kept)
}

/// Whether the sensitive pass kept enough more audio to be worth
/// transcribing: at least half a second when nothing was kept, otherwise a
/// second and a fifth more.
pub fn worth_transcribing(kept: usize, rescued: usize) -> bool {
    if kept == 0 {
        return rescued >= RATE / 2;
    }
    rescued >= kept + RATE && rescued * 5 >= kept * 6
}

/// What speech models write for silence, breath or noise.
const INVENTED: &[&str] = &[
    "thank you",
    "thanks for watching",
    "thank you for watching",
    "please subscribe",
    "subscribe",
    "bye",
    "you",
    "so",
    "okay",
    "oh",
    "uh",
    "um",
    "hmm",
    "mm",
];

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// Length of the longest common subsequence of two word lists.
fn common(a: &[String], b: &[String]) -> (usize, Vec<bool>) {
    let mut t = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            t[i][j] = if a[i] == b[j] {
                t[i + 1][j + 1] + 1
            } else {
                t[i + 1][j].max(t[i][j + 1])
            };
        }
    }
    // Mark the words of `b` that are part of the common subsequence.
    let mut in_common = vec![false; b.len()];
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            in_common[j] = true;
            i += 1;
            j += 1;
        } else if t[i + 1][j] >= t[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    (t[0][0], in_common)
}

/// Whether the rescued transcript should replace the first one: it keeps
/// (almost) all of the first one's words in order, and adds at least two
/// words that aren't the usual inventions, with no word stuck on repeat.
pub fn accept(first: &str, rescued: &str) -> bool {
    let a = words(first);
    let b = words(rescued);
    if b.len() < a.len() + 2 {
        return false;
    }
    if b.windows(4).any(|w| w.iter().all(|x| *x == w[0])) {
        return false;
    }
    let (shared, in_common) = common(&a, &b);
    if shared * 10 < a.len() * 9 {
        return false;
    }
    let added: Vec<&str> = b
        .iter()
        .zip(&in_common)
        .filter(|(_, c)| !**c)
        .map(|(w, _)| w.as_str())
        .collect();
    let added_text = added.join(" ");
    let mut rest = format!(" {added_text} ");
    // Longest phrases first, so "thank you for watching" goes as a whole.
    let mut invented: Vec<&str> = INVENTED.to_vec();
    invented.sort_by_key(|p| std::cmp::Reverse(p.len()));
    // Repeat until nothing changes: neighbouring phrases share a space.
    loop {
        let before = rest.clone();
        for phrase in &invented {
            rest = rest.replace(&format!(" {phrase} "), " ");
        }
        if rest == before {
            break;
        }
    }
    rest.split_whitespace().count() >= 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lost_words_are_taken_back() {
        assert!(accept(
            "I think we should the cache",
            "I think we should probably clear the cache before the deploy"
        ));
        // Nothing heard at first, real words from the sensitive pass.
        assert!(accept("", "can you check the logs"));
    }

    #[test]
    fn inventions_and_rewrites_are_not() {
        // Only the usual phrases added.
        assert!(!accept("Clear the cache", "Clear the cache. Thank you."));
        assert!(!accept("", "Thanks for watching!"));
        assert!(!accept("", "you you you you"));
        // One word more isn't worth the risk.
        assert!(!accept("Clear the cache", "Clear the whole cache"));
        // A different transcript, not the first one plus more.
        assert!(!accept(
            "Deploy it on Friday afternoon please",
            "The weather looks nice today and tomorrow too"
        ));
    }

    #[test]
    fn transcribe_only_when_clearly_more_was_kept() {
        assert!(worth_transcribing(0, RATE));
        assert!(!worth_transcribing(0, RATE / 4));
        assert!(worth_transcribing(5 * RATE, 7 * RATE));
        assert!(!worth_transcribing(5 * RATE, 5 * RATE + RATE / 2));
        assert!(!worth_transcribing(20 * RATE, 21 * RATE));
    }
}
