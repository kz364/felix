//! Scoring who-spoke-when against a reference, for `examples/speaker_eval`.
//!
//! Diarization error rate (DER), frame by frame: speech the reference has
//! and the guess doesn't (missed), speech the guess has and the reference
//! doesn't (false alarm), and speech put under the wrong person
//! (confusion), over all reference speech. Guessed voices are matched to
//! reference people in the way that agrees most (exact, by search over
//! assignments). No collar; overlapping reference speech counts, so a
//! single-voice guess misses the second person in an overlap.

use std::collections::BTreeMap;

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Score {
    /// Reference speech, in frames (a frame with two people counts twice).
    pub reference: u64,
    pub missed: u64,
    pub false_alarm: u64,
    pub confusion: u64,
    pub reference_speakers: usize,
    pub guessed_speakers: usize,
}

impl Score {
    pub fn der(&self) -> f64 {
        (self.missed + self.false_alarm + self.confusion) as f64 / self.reference.max(1) as f64
    }

    /// Confusion alone: how well voices were told apart, leaving out what
    /// the speech detector found or missed.
    pub fn confusion_rate(&self) -> f64 {
        self.confusion as f64 / self.reference.max(1) as f64
    }

    pub fn add(&mut self, other: &Score) {
        self.reference += other.reference;
        self.missed += other.missed;
        self.false_alarm += other.false_alarm;
        self.confusion += other.confusion;
        self.reference_speakers += other.reference_speakers;
        self.guessed_speakers += other.guessed_speakers;
    }
}

/// Score a guess (one voice or none per frame) against the reference (the
/// people talking in each frame). Only frames where `scored` is true count.
pub fn score(reference: &[Vec<u32>], guess: &[Option<u32>], scored: &[bool]) -> Score {
    let frames = reference.len().min(guess.len()).min(scored.len());
    let mut refs: Vec<u32> = Vec::new();
    let mut guesses: Vec<u32> = Vec::new();
    // How many frames each (guess, reference) pair share.
    let mut together: BTreeMap<(u32, u32), u64> = BTreeMap::new();
    let mut s = Score::default();
    for i in (0..frames).filter(|&i| scored[i]) {
        let r = &reference[i];
        for p in r {
            if !refs.contains(p) {
                refs.push(*p);
            }
        }
        if let Some(g) = guess[i] {
            if !guesses.contains(&g) {
                guesses.push(g);
            }
            for p in r {
                *together.entry((g, *p)).or_default() += 1;
            }
        }
        let n_ref = r.len() as u64;
        let n_guess = guess[i].is_some() as u64;
        s.reference += n_ref;
        s.missed += n_ref.saturating_sub(n_guess);
        s.false_alarm += n_guess.saturating_sub(n_ref);
    }
    let mapping = best_mapping(&guesses, &refs, &together);
    let mut correct = 0u64;
    for i in (0..frames).filter(|&i| scored[i]) {
        if let Some(r) = guess[i].and_then(|g| mapping.get(&g)) {
            if reference[i].contains(r) {
                correct += 1;
            }
        }
    }
    let matched: u64 = (0..frames)
        .filter(|&i| scored[i] && guess[i].is_some() && !reference[i].is_empty())
        .count() as u64;
    s.confusion = matched - correct;
    s.reference_speakers = refs.len();
    s.guessed_speakers = guesses.len();
    s
}

/// Each guessed voice to at most one reference person (and the other way
/// round), maximising the frames they share.
fn best_mapping(
    guesses: &[u32],
    refs: &[u32],
    together: &BTreeMap<(u32, u32), u64>,
) -> BTreeMap<u32, u32> {
    // Search over which reference people are taken, voice by voice. Only
    // the 12 people heard most are considered, which is plenty for meetings.
    let mut refs: Vec<u32> = refs.to_vec();
    let heard = |r: u32| {
        together
            .iter()
            .filter(|((_, p), _)| *p == r)
            .map(|(_, n)| n)
            .sum::<u64>()
    };
    refs.sort_by_key(|&r| std::cmp::Reverse(heard(r)));
    refs.truncate(12);
    let states = 1usize << refs.len();
    // best[mask] = (frames, choices so far)
    type Best = Option<(u64, Vec<(u32, u32)>)>;
    let mut best: Vec<Best> = vec![None; states];
    best[0] = Some((0, Vec::new()));
    for &g in guesses {
        let mut next = best.clone();
        for (mask, state) in best.iter().enumerate() {
            let Some((frames, chosen)) = state else {
                continue;
            };
            for (k, &r) in refs.iter().enumerate() {
                if mask & (1 << k) != 0 {
                    continue;
                }
                let shared = together.get(&(g, r)).copied().unwrap_or(0);
                if shared == 0 {
                    continue;
                }
                let to = mask | (1 << k);
                let total = frames + shared;
                if next[to].as_ref().is_none_or(|(f, _)| total > *f) {
                    let mut c = chosen.clone();
                    c.push((g, r));
                    next[to] = Some((total, c));
                }
            }
        }
        best = next;
    }
    best.into_iter()
        .flatten()
        .max_by_key(|(f, _)| *f)
        .map(|(_, c)| c.into_iter().collect())
        .unwrap_or_default()
}

/// Reference speech from an RTTM file: per frame of `frame_ms`, the people
/// talking (numbered in order of first appearance), over `frames` frames.
pub fn from_rttm(rttm: &str, frame_ms: u64, frames: usize) -> Vec<Vec<u32>> {
    let mut names: Vec<String> = Vec::new();
    let mut out = vec![Vec::new(); frames];
    for line in rttm.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 8 || f[0] != "SPEAKER" {
            continue;
        }
        let (Ok(start), Ok(dur)) = (f[3].parse::<f64>(), f[4].parse::<f64>()) else {
            continue;
        };
        let who = match names.iter().position(|n| n == f[7]) {
            Some(i) => i as u32,
            None => {
                names.push(f[7].to_string());
                (names.len() - 1) as u32
            }
        };
        let from = (start * 1000.0 / frame_ms as f64).round() as usize;
        let to = ((start + dur) * 1000.0 / frame_ms as f64).round() as usize;
        for frame in out.iter_mut().take(to.min(frames)).skip(from) {
            if !frame.contains(&who) {
                frame.push(who);
            }
        }
    }
    out
}

/// Which frames are scored, from a UEM file (start and end in seconds in
/// the last two fields of each line).
pub fn from_uem(uem: &str, frame_ms: u64, frames: usize) -> Vec<bool> {
    let mut out = vec![false; frames];
    for line in uem.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 4 {
            continue;
        }
        let (Ok(start), Ok(end)) = (f[2].parse::<f64>(), f[3].parse::<f64>()) else {
            continue;
        };
        let from = (start * 1000.0 / frame_ms as f64).round() as usize;
        let to = (end * 1000.0 / frame_ms as f64).round() as usize;
        for s in out.iter_mut().take(to.min(frames)).skip(from) {
            *s = true;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_perfect_guess_scores_zero_whatever_the_numbers() {
        let reference = vec![vec![0], vec![0], vec![1], vec![1], vec![]];
        let guess = vec![Some(7), Some(7), Some(3), Some(3), None];
        let s = score(&reference, &guess, &[true; 5]);
        assert_eq!(s.der(), 0.0);
        assert_eq!((s.reference_speakers, s.guessed_speakers), (2, 2));
    }

    #[test]
    fn errors_are_split_into_missed_false_alarm_and_confusion() {
        // Two people; the guess merges them into one voice, misses one
        // frame, adds one, and can't cover an overlap.
        let reference = vec![
            vec![0],
            vec![0],
            vec![1],
            vec![1],
            vec![0, 1],
            vec![],
            vec![0],
        ];
        let guess = vec![Some(5), Some(5), Some(5), Some(5), Some(5), Some(5), None];
        let s = score(&reference, &guess, &[true; 7]);
        assert_eq!(s.reference, 7);
        assert_eq!(s.missed, 2); // the overlap's second person, the last frame
        assert_eq!(s.false_alarm, 1);
        assert_eq!(s.confusion, 2); // voice 5 is one person; the other's two frames are wrong
        assert!((s.der() - 5.0 / 7.0).abs() < 1e-9);
    }

    #[test]
    fn the_mapping_is_the_best_overall_not_greedy() {
        // Greedy would give voice 0 to person 0 (4 frames) and leave
        // voice 1 with nothing; the best gives 0→1 and 1→0 (3 + 3).
        let reference = vec![
            vec![0],
            vec![0],
            vec![0],
            vec![0],
            vec![1],
            vec![1],
            vec![1],
            vec![0],
            vec![0],
        ];
        let guess = vec![
            Some(0),
            Some(0),
            Some(0),
            Some(1),
            Some(0),
            Some(0),
            Some(0),
            Some(1),
            Some(1),
        ];
        let s = score(&reference, &guess, &[true; 9]);
        assert_eq!(s.confusion, 3);
    }

    #[test]
    fn rttm_and_uem_are_read_into_frames() {
        let rttm = "SPEAKER m 1 0.0 0.09 <NA> <NA> A <NA> <NA>\nSPEAKER m 1 0.06 0.06 <NA> <NA> B <NA> <NA>\n";
        let r = from_rttm(rttm, 30, 5);
        assert_eq!(r, vec![vec![0], vec![0], vec![0, 1], vec![1], vec![]]);
        let u = from_uem("m 1 0.030 0.090", 30, 5);
        assert_eq!(u, vec![false, true, true, false, false]);
    }
}
