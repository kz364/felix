//! Telling voices apart in an in-person meeting (one mic, several people).
//!
//! The speech is cut into short windows; a speaker-embedding model (WeSpeaker
//! ResNet34, ONNX, downloaded on first use) turns each window into a voice
//! fingerprint; fingerprints are grouped by similarity into speakers. The
//! result is a speaker number per VAD frame, which the pipeline uses to cut
//! chunks where the speaker changes, so each transcript segment has one voice.

use super::transcript::{Chunk, FRAME_MS};
use ndarray::Array3;
use ort::session::Session;
use ort::value::Value;
use rustfft::num_complex::Complex;
use std::path::Path;

pub const MODEL_FILE: &str = "wespeaker_en_voxceleb_resnet34_LM.onnx";
pub const MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/wespeaker_en_voxceleb_resnet34_LM.onnx";
pub const MODEL_BYTES: u64 = 26_530_550;

const SAMPLE_RATE: usize = 16_000;
const FRAME_SAMPLES: usize = SAMPLE_RATE * FRAME_MS as usize / 1000;
/// A fingerprint window: 1.5 s of speech.
const WINDOW_FRAMES: usize = 50;
/// Shortest stretch worth a fingerprint of its own (0.6 s).
const MIN_WINDOW_FRAMES: usize = 20;
/// Speech closer than this (300 ms) belongs to one stretch.
const JOIN_GAP_FRAMES: usize = 10;
/// Fingerprints at least this similar (cosine) are the same voice.
pub const SAME_SPEAKER: f32 = 0.5;
/// A "speaker" heard in fewer windows than this is folded into the nearest one.
const MIN_WINDOWS_PER_SPEAKER: usize = 3;
/// Speaker turns shorter than this (1 s) are treated as noise in the labels.
const MIN_TURN_FRAMES: usize = 33;

// Kaldi-style filterbank features, as WeSpeaker models expect.
const MEL_BINS: usize = 80;
const FFT_SIZE: usize = 512;
const WIN_LEN: usize = 400; // 25 ms
const HOP: usize = 160; // 10 ms

fn mel(f: f32) -> f32 {
    1127.0 * (1.0 + f / 700.0).ln()
}

/// Triangular mel filters over the FFT bins, 20 Hz to 7.6 kHz.
fn mel_filters() -> Vec<Vec<(usize, f32)>> {
    let (low, high) = (mel(20.0), mel(SAMPLE_RATE as f32 / 2.0 - 400.0));
    let step = (high - low) / (MEL_BINS + 1) as f32;
    (0..MEL_BINS)
        .map(|m| {
            let (left, center, right) = (
                low + m as f32 * step,
                low + (m + 1) as f32 * step,
                low + (m + 2) as f32 * step,
            );
            (0..FFT_SIZE / 2)
                .filter_map(|k| {
                    let f = mel(k as f32 * SAMPLE_RATE as f32 / FFT_SIZE as f32);
                    let w = if f > left && f <= center {
                        (f - left) / (center - left)
                    } else if f > center && f < right {
                        (right - f) / (right - center)
                    } else {
                        0.0
                    };
                    (w > 0.0).then_some((k, w))
                })
                .collect()
        })
        .collect()
}

/// Log mel filterbank features, one row of [`MEL_BINS`] per 10 ms.
pub fn fbank(samples: &[f32]) -> Vec<[f32; MEL_BINS]> {
    if samples.len() < WIN_LEN {
        return Vec::new();
    }
    let filters = mel_filters();
    let window: Vec<f32> = (0..WIN_LEN)
        .map(|i| {
            // Povey window.
            (0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (WIN_LEN - 1) as f32).cos())
                .powf(0.85)
        })
        .collect();
    let fft = rustfft::FftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
    let mut buf = vec![Complex::new(0.0f32, 0.0); FFT_SIZE];
    let frames = 1 + (samples.len() - WIN_LEN) / HOP;
    (0..frames)
        .map(|t| {
            let frame = &samples[t * HOP..t * HOP + WIN_LEN];
            let mean = frame.iter().sum::<f32>() / WIN_LEN as f32;
            let mut prev = frame[0] - mean;
            for (i, c) in buf.iter_mut().enumerate() {
                *c = if i < WIN_LEN {
                    let x = frame[i] - mean;
                    let emphasised = x - 0.97 * prev;
                    prev = x;
                    Complex::new(emphasised * window[i], 0.0)
                } else {
                    Complex::new(0.0, 0.0)
                };
            }
            fft.process(&mut buf);
            let mut row = [0.0f32; MEL_BINS];
            for (m, filter) in filters.iter().enumerate() {
                let energy: f32 = filter.iter().map(|&(k, w)| w * buf[k].norm_sqr()).sum();
                row[m] = energy.max(f32::EPSILON).ln();
            }
            row
        })
        .collect()
}

pub struct Embedder {
    session: Session,
    /// The model wants samples in [-1, 1] (else int16 scale).
    normalize_samples: bool,
    /// Subtract the mean of each feature over the window.
    global_mean: bool,
}

impl Embedder {
    pub fn new(model: &Path) -> Result<Self, String> {
        let fail = |e: String| format!("Couldn't load the speaker model: {e}");
        let session = Session::builder()
            .map_err(|e| fail(e.to_string()))?
            .with_intra_threads(2)
            .map_err(|e| fail(e.to_string()))?
            .commit_from_file(model)
            .map_err(|e| fail(e.to_string()))?;
        let custom = |key: &str| session.metadata().ok().and_then(|m| m.custom(key));
        let normalize_samples = custom("normalize_samples").is_some_and(|v| v.trim() == "1");
        let global_mean =
            custom("feature_normalize_type").is_none_or(|v| v.trim() == "global-mean");
        Ok(Self {
            session,
            normalize_samples,
            global_mean,
        })
    }

    /// A unit-length voice fingerprint for 16 kHz audio.
    pub fn embed(&mut self, audio: &[f32]) -> Result<Vec<f32>, String> {
        let scale = if self.normalize_samples { 1.0 } else { 32768.0 };
        let scaled: Vec<f32> = audio.iter().map(|s| s * scale).collect();
        let mut feats = fbank(&scaled);
        if feats.is_empty() {
            return Err("Too little audio for a fingerprint".into());
        }
        if self.global_mean {
            let mut mean = [0.0f32; MEL_BINS];
            for row in &feats {
                for (m, v) in row.iter().enumerate() {
                    mean[m] += v;
                }
            }
            for m in &mut mean {
                *m /= feats.len() as f32;
            }
            for row in &mut feats {
                for (m, v) in row.iter_mut().enumerate() {
                    *v -= mean[m];
                }
            }
        }
        let frames = feats.len();
        let input =
            Array3::from_shape_vec((1, frames, MEL_BINS), feats.into_iter().flatten().collect())
                .map_err(|e| e.to_string())?;
        let input = Value::from_array(input).map_err(|e| e.to_string())?;
        let name = self.session.inputs()[0].name().to_string();
        let outputs = self
            .session
            .run(ort::inputs![name => input])
            .map_err(|e| format!("Speaker model failed: {e}"))?;
        let (_, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        Ok(unit(data.to_vec()))
    }
}

fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Frame ranges to fingerprint: speech stretches cut into 1.5 s windows.
pub fn windows(speech: &[bool]) -> Vec<(usize, usize)> {
    let mut stretches: Vec<(usize, usize)> = Vec::new();
    for (i, _) in speech.iter().enumerate().filter(|(_, s)| **s) {
        match stretches.last_mut() {
            Some((_, end)) if i - *end <= JOIN_GAP_FRAMES => *end = i + 1,
            _ => stretches.push((i, i + 1)),
        }
    }
    let mut out = Vec::new();
    for (start, end) in stretches {
        let len = end - start;
        if len < MIN_WINDOW_FRAMES {
            continue;
        }
        if len < WINDOW_FRAMES * 3 / 2 {
            out.push((start, end));
            continue;
        }
        let mut at = start;
        while end - at >= WINDOW_FRAMES * 3 / 2 {
            out.push((at, at + WINDOW_FRAMES));
            at += WINDOW_FRAMES;
        }
        out.push((at, end));
    }
    out
}

/// Group fingerprints into speakers. Returns a speaker number per
/// fingerprint, numbered in order of first appearance.
pub fn cluster(embeddings: &[Vec<f32>]) -> Vec<u32> {
    if embeddings.is_empty() {
        return Vec::new();
    }
    // Online pass: join the most similar speaker, or start a new one.
    let mut centroids: Vec<Vec<f32>> = Vec::new();
    let mut labels: Vec<usize> = Vec::with_capacity(embeddings.len());
    for e in embeddings {
        let best = centroids
            .iter()
            .enumerate()
            .map(|(k, c)| (k, cosine(e, c)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((k, sim)) if sim >= SAME_SPEAKER => labels.push(k),
            _ => {
                centroids.push(e.clone());
                labels.push(centroids.len() - 1);
            }
        }
        let k = *labels.last().unwrap();
        centroids[k] = centroid(embeddings, &labels, k);
    }
    // Refine: reassign to the nearest centroid a few times, merging
    // speakers whose centroids turn out alike.
    for _ in 0..5 {
        let mut changed = false;
        for (i, e) in embeddings.iter().enumerate() {
            let best = (0..centroids.len())
                .max_by(|&a, &b| cosine(e, &centroids[a]).total_cmp(&cosine(e, &centroids[b])))
                .unwrap_or(0);
            if labels[i] != best {
                labels[i] = best;
                changed = true;
            }
        }
        for (k, c) in centroids.iter_mut().enumerate() {
            *c = centroid(embeddings, &labels, k);
        }
        // Merge the closest pair of speakers if they're the same voice.
        let mut merge = None;
        for a in 0..centroids.len() {
            for b in a + 1..centroids.len() {
                let sim = cosine(&centroids[a], &centroids[b]);
                if sim >= SAME_SPEAKER && merge.is_none_or(|(_, _, s)| sim > s) {
                    merge = Some((a, b, sim));
                }
            }
        }
        if let Some((a, b, _)) = merge {
            for l in &mut labels {
                if *l == b {
                    *l = a;
                }
            }
            centroids[a] = centroid(embeddings, &labels, a);
            changed = true;
        }
        if !changed {
            break;
        }
    }
    // Fold rarely heard "speakers" (a cough, a door) into the nearest real one.
    let count = |labels: &[usize], k: usize| labels.iter().filter(|&&l| l == k).count();
    let big: Vec<usize> = (0..centroids.len())
        .filter(|&k| count(&labels, k) >= MIN_WINDOWS_PER_SPEAKER)
        .collect();
    if !big.is_empty() {
        for i in 0..labels.len() {
            if !big.contains(&labels[i]) {
                labels[i] = *big
                    .iter()
                    .max_by(|&&a, &&b| {
                        cosine(&embeddings[i], &centroids[a])
                            .total_cmp(&cosine(&embeddings[i], &centroids[b]))
                    })
                    .unwrap();
            }
        }
    }
    // Number speakers by first appearance.
    let mut order: Vec<usize> = Vec::new();
    labels
        .iter()
        .map(|l| {
            let pos = order.iter().position(|o| o == l).unwrap_or_else(|| {
                order.push(*l);
                order.len() - 1
            });
            pos as u32
        })
        .collect()
}

fn centroid(embeddings: &[Vec<f32>], labels: &[usize], k: usize) -> Vec<f32> {
    let dim = embeddings[0].len();
    let mut sum = vec![0.0f32; dim];
    for (e, _) in embeddings.iter().zip(labels).filter(|(_, &l)| l == k) {
        for (s, x) in sum.iter_mut().zip(e) {
            *s += x;
        }
    }
    unit(sum)
}

/// A speaker per VAD frame from the windows' speakers: speech frames take
/// the label of the window covering them; turns shorter than 1 s take their
/// neighbour's; non-speech frames are `None`.
pub fn frame_labels(
    speech: &[bool],
    windows: &[(usize, usize)],
    speakers: &[u32],
) -> Vec<Option<u32>> {
    let mut labels: Vec<Option<u32>> = vec![None; speech.len()];
    for (&(from, to), &s) in windows.iter().zip(speakers) {
        for l in &mut labels[from..to.min(speech.len())] {
            *l = Some(s);
        }
    }
    // Speech not in any window (short bits) takes the nearest window's speaker.
    let nearest = |i: usize| {
        windows
            .iter()
            .zip(speakers)
            .min_by_key(|((from, to), _)| {
                if i < *from {
                    from - i
                } else {
                    i.saturating_sub(*to)
                }
            })
            .map(|(_, &s)| s)
    };
    for i in 0..speech.len() {
        if speech[i] && labels[i].is_none() {
            labels[i] = nearest(i);
        }
        if !speech[i] {
            labels[i] = None;
        }
    }
    // Smooth out turns too short to be real.
    let mut runs: Vec<(usize, usize, u32)> = Vec::new();
    for (i, l) in labels.iter().enumerate() {
        if let Some(s) = l {
            match runs.last_mut() {
                Some((_, end, last)) if *last == *s => *end = i + 1,
                _ => runs.push((i, i + 1, *s)),
            }
        }
    }
    for r in 0..runs.len() {
        let (from, to, s) = runs[r];
        if to - from >= MIN_TURN_FRAMES {
            continue;
        }
        let neighbour = match (r.checked_sub(1).map(|p| runs[p]), runs.get(r + 1)) {
            (Some(p), Some(n)) if n.1 - n.0 > p.1 - p.0 => Some(n.2),
            (Some(p), _) => Some(p.2),
            (None, Some(n)) => Some(n.2),
            (None, None) => None,
        };
        if let Some(n) = neighbour.filter(|&n| n != s) {
            for l in labels[from..to].iter_mut().filter(|l| l.is_some()) {
                *l = Some(n);
            }
            runs[r].2 = n;
        }
    }
    labels
}

/// Cut a chunk where the speaker changes, so each piece has one voice.
/// Returns the pieces with their speaker (the one heard most in each).
pub fn split_by_speaker(chunk: Chunk, labels: &[Option<u32>]) -> Vec<(Chunk, Option<u32>)> {
    let frame = |ms: u64| (ms / FRAME_MS) as usize;
    let (first, last) = (frame(chunk.start_ms), frame(chunk.end_ms).min(labels.len()));
    // Where the speaker changes: the middle of the gap between two turns.
    let mut cuts = Vec::new();
    let mut prev: Option<(usize, u32)> = None;
    for (i, label) in labels.iter().enumerate().take(last).skip(first) {
        if let Some(s) = *label {
            if let Some((at, p)) = prev {
                if p != s {
                    cuts.push((at + 1 + i) / 2);
                }
            }
            prev = Some((i, s));
        }
    }
    let mut bounds = vec![chunk.start_ms];
    bounds.extend(cuts.iter().map(|&f| f as u64 * FRAME_MS));
    bounds.push(chunk.end_ms);
    bounds
        .windows(2)
        .map(|w| {
            let piece = Chunk {
                start_ms: w[0],
                end_ms: w[1],
            };
            let mut counts = std::collections::BTreeMap::<u32, usize>::new();
            for s in labels[frame(w[0]).min(labels.len())..frame(w[1]).min(labels.len())]
                .iter()
                .flatten()
            {
                *counts.entry(*s).or_default() += 1;
            }
            let speaker = counts.into_iter().max_by_key(|&(_, n)| n).map(|(s, _)| s);
            (piece, speaker)
        })
        .collect()
}

/// The user's own voice on the mic during a call; turned back into "Me"
/// once chunks are cut.
pub const ME: u32 = u32::MAX;
/// On a call, other voices on the mic must make up this share of it to count
/// as people in the room with the user (not the user's voice split in two).
const ROOM_SHARE: f32 = 0.15;
/// And each of them at least this share.
const ROOM_SPEAKER_SHARE: f32 = 0.05;

/// On a call, the mic is the user plus, in a meeting room, others with them.
/// The voice heard most is the user ([`ME`]); other clear voices are kept as
/// people in the room, numbered from 0; stray bits are the user's.
pub fn others_in_room(labels: &[Option<u32>]) -> Vec<Option<u32>> {
    let mut counts = std::collections::BTreeMap::<u32, usize>::new();
    for s in labels.iter().flatten() {
        *counts.entry(*s).or_default() += 1;
    }
    let total: usize = counts.values().sum();
    let Some((&me, &mine)) = counts.iter().max_by_key(|&(_, n)| *n) else {
        return labels.to_vec();
    };
    let share = |n: usize| n as f32 / total.max(1) as f32;
    let room: Vec<u32> = counts
        .iter()
        .filter(|&(&s, &n)| s != me && share(n) >= ROOM_SPEAKER_SHARE)
        .map(|(&s, _)| s)
        .collect();
    let others: usize = room.iter().map(|s| counts[s]).sum();
    let in_room = share(others) >= ROOM_SHARE && share(mine) < 1.0;
    // Room voices renumbered by first appearance.
    let mut order: Vec<u32> = Vec::new();
    for s in labels.iter().flatten() {
        if in_room && room.contains(s) && !order.contains(s) {
            order.push(*s);
        }
    }
    labels
        .iter()
        .map(|l| {
            l.map(|s| match order.iter().position(|&o| o == s) {
                Some(i) => i as u32,
                None => ME,
            })
        })
        .collect()
}

/// Speaker numbers moved up by `base`.
pub fn offset(labels: &[Option<u32>], base: u32) -> Vec<Option<u32>> {
    labels.iter().map(|l| l.map(|s| s + base)).collect()
}

/// A speaker at least this close to the user's voiceprint (cosine of the
/// centroids) can be the user...
pub const VOICEPRINT_MATCH: f32 = 0.8;
/// ...and must be this much closer than any other speaker.
pub const VOICEPRINT_MARGIN: f32 = 0.1;

/// Relabel as [`ME`] the one speaker whose voice matches the user's print,
/// if one clearly does. `speakers` holds a speaker per embedding.
pub fn mark_me(embeddings: &[Vec<f32>], speakers: &mut [u32], voiceprint: &[f32]) {
    let Some(&last) = speakers.iter().max() else {
        return;
    };
    let labels: Vec<usize> = speakers.iter().map(|&s| s as usize).collect();
    let mut scores: Vec<(u32, f32)> = (0..=last)
        .filter(|s| speakers.contains(s))
        .map(|s| {
            (
                s,
                cosine(&centroid(embeddings, &labels, s as usize), voiceprint),
            )
        })
        .collect();
    scores.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (me, best) = scores[0];
    let runner_up = scores.get(1).map_or(-1.0, |s| s.1);
    log::info!("Voiceprint: best speaker {best:.2}, next {runner_up:.2}");
    if best >= VOICEPRINT_MATCH && best - runner_up >= VOICEPRINT_MARGIN {
        for s in speakers.iter_mut().filter(|s| **s == me) {
            *s = ME;
        }
    }
}

/// Windows fingerprinted on one thread before it's replaced (see
/// [`embed_windows`]).
const WINDOWS_PER_THREAD: usize = 64;

/// A fingerprint per window. ONNX Runtime's KleidiAI convolutions (Apple
/// silicon) keep a per-thread cache entry for every new input, about 8 MB
/// a window, freed only when the thread ends: one thread for a whole
/// meeting grew to over 10 GB and ran the Mac out of memory. So the windows
/// are done in batches, each on a fresh thread with its own session, which
/// keeps memory under about a gigabyte however long the meeting.
fn embed_windows(
    samples: &[f32],
    wins: &[(usize, usize)],
    model: &Path,
) -> Result<Vec<Vec<f32>>, String> {
    let mut embeddings = Vec::with_capacity(wins.len());
    for batch in wins.chunks(WINDOWS_PER_THREAD) {
        let done = std::thread::scope(|scope| {
            scope
                .spawn(|| -> Result<Vec<Vec<f32>>, String> {
                    let mut embedder = Embedder::new(model)?;
                    batch
                        .iter()
                        .map(|&(from, to)| {
                            let a = (from * FRAME_SAMPLES).min(samples.len());
                            let b = (to * FRAME_SAMPLES).min(samples.len());
                            embedder.embed(&samples[a..b])
                        })
                        .collect()
                })
                .join()
                .map_err(|_| "The speaker model crashed".to_string())?
        })?;
        embeddings.extend(done);
    }
    Ok(embeddings)
}

/// Label every speech frame of a mic track with a speaker. With the user's
/// `voiceprint` for this mic, their voice is labelled [`ME`].
pub fn speakers(
    wav: &Path,
    speech: &[bool],
    model: &Path,
    voiceprint: Option<&[f32]>,
) -> Result<Vec<Option<u32>>, String> {
    let wins = windows(speech);
    if wins.is_empty() {
        return Ok(vec![None; speech.len()]);
    }
    let mut reader = hound::WavReader::open(wav).map_err(|e| e.to_string())?;
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let embeddings = embed_windows(&samples, &wins, model)?;
    let mut speakers = cluster(&embeddings);
    if let Some(print) = voiceprint {
        mark_me(&embeddings, &mut speakers, print);
    }
    log::info!(
        "Meeting diarization: {} windows, {} speakers",
        wins.len(),
        speakers.iter().max().map_or(0, |m| m + 1)
    );
    Ok(frame_labels(speech, &wins, &speakers))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn around(base: &[f32], noise: f32, seed: u32) -> Vec<f32> {
        let mut x = seed;
        unit(
            base.iter()
                .map(|b| {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                    b + noise * ((x >> 16) as f32 / 32768.0 - 1.0)
                })
                .collect(),
        )
    }

    #[test]
    fn the_voice_matching_the_print_becomes_me_only_when_clear() {
        let a: Vec<f32> = (0..32).map(|i| if i < 16 { 1.0 } else { 0.0 }).collect();
        let b: Vec<f32> = (0..32).map(|i| if i < 16 { 0.0 } else { 1.0 }).collect();
        let embs: Vec<Vec<f32>> = (0..10)
            .map(|i| around(if i % 2 == 0 { &a } else { &b }, 0.2, i))
            .collect();
        let labels: Vec<u32> = (0..10).map(|i| i % 2).collect();
        let mut marked = labels.clone();
        mark_me(&embs, &mut marked, &unit(b.clone()));
        assert!(marked
            .iter()
            .enumerate()
            .all(|(i, &s)| s == if i % 2 == 1 { ME } else { 0 }));
        // A print halfway between the two voices matches neither.
        let mut unsure = labels.clone();
        let half: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
        mark_me(&embs, &mut unsure, &unit(half));
        assert_eq!(unsure, labels);
    }

    #[test]
    fn two_voices_are_told_apart_and_numbered_in_order() {
        let a: Vec<f32> = (0..32).map(|i| if i < 16 { 1.0 } else { 0.0 }).collect();
        let b: Vec<f32> = (0..32).map(|i| if i < 16 { 0.0 } else { 1.0 }).collect();
        let mut embs = Vec::new();
        for i in 0..10 {
            embs.push(around(&b, 0.3, i));
        }
        for i in 10..20 {
            embs.push(around(&a, 0.3, i));
        }
        for i in 20..25 {
            embs.push(around(&b, 0.3, i));
        }
        let labels = cluster(&embs);
        assert!(labels[..10].iter().all(|&l| l == 0));
        assert!(labels[10..20].iter().all(|&l| l == 1));
        assert!(labels[20..].iter().all(|&l| l == 0));
    }

    #[test]
    fn one_voice_stays_one_speaker_and_strays_are_folded_in() {
        let a: Vec<f32> = (0..32).map(|i| (i as f32).sin()).collect();
        let mut embs: Vec<Vec<f32>> = (0..12).map(|i| around(&a, 0.2, i)).collect();
        // One odd window (a cough) isn't a new person.
        embs.push(unit((0..32).map(|i| (i as f32 * 3.0).cos()).collect()));
        let labels = cluster(&embs);
        assert!(labels.iter().all(|&l| l == 0), "{labels:?}");
    }

    #[test]
    fn windows_cover_speech_in_short_pieces() {
        let mut speech = vec![false; 400];
        for s in &mut speech[10..200] {
            *s = true; // 5.7 s
        }
        for s in &mut speech[250..265] {
            *s = true; // too short alone
        }
        let w = windows(&speech);
        assert!(w
            .iter()
            .all(|(a, b)| b - a >= MIN_WINDOW_FRAMES && b - a < WINDOW_FRAMES * 3 / 2));
        assert_eq!(w.first().unwrap().0, 10);
        assert_eq!(w.last().unwrap().1, 200);
    }

    #[test]
    fn short_flickers_in_the_labels_are_smoothed() {
        let speech = vec![true; 200];
        let wins = vec![(0, 90), (90, 100), (100, 200)];
        let labels = frame_labels(&speech, &wins, &[0, 1, 0]);
        assert!(labels.iter().all(|l| *l == Some(0)));
        let labels = frame_labels(&speech, &[(0, 100), (100, 200)], &[0, 1]);
        assert_eq!(labels[99], Some(0));
        assert_eq!(labels[100], Some(1));
    }

    #[test]
    fn a_chunk_is_cut_where_the_speaker_changes() {
        // 0-3 s speaker 0, pause, 3.6-6 s speaker 1.
        let mut labels = vec![None; 200];
        for l in &mut labels[..100] {
            *l = Some(0);
        }
        for l in &mut labels[120..200] {
            *l = Some(1);
        }
        let pieces = split_by_speaker(
            Chunk {
                start_ms: 0,
                end_ms: 6000,
            },
            &labels,
        );
        assert_eq!(pieces.len(), 2);
        assert_eq!(
            pieces[0],
            (
                Chunk {
                    start_ms: 0,
                    end_ms: 3300
                },
                Some(0)
            )
        );
        assert_eq!(
            pieces[1],
            (
                Chunk {
                    start_ms: 3300,
                    end_ms: 6000
                },
                Some(1)
            )
        );
        // One voice: the chunk stays whole.
        let one = split_by_speaker(
            Chunk {
                start_ms: 0,
                end_ms: 2900,
            },
            &labels,
        );
        assert_eq!(
            one,
            vec![(
                Chunk {
                    start_ms: 0,
                    end_ms: 2900
                },
                Some(0)
            )]
        );
    }

    #[test]
    fn filterbank_sees_where_the_energy_is() {
        // A 300 Hz tone puts its energy in a low mel band, not a high one.
        let tone: Vec<f32> = (0..16_000)
            .map(|i| 10_000.0 * (i as f32 * 2.0 * std::f32::consts::PI * 300.0 / 16_000.0).sin())
            .collect();
        let feats = fbank(&tone);
        assert_eq!(feats.len(), 1 + (16_000 - WIN_LEN) / HOP);
        let row = feats[50];
        let peak = (0..MEL_BINS)
            .max_by(|&a, &b| row[a].total_cmp(&row[b]))
            .unwrap();
        assert!(peak < 20, "peak in band {peak}");
        assert!(row[peak] > row[70] + 10.0);
    }

    #[test]
    fn on_a_call_the_main_voice_is_me_and_clear_others_are_the_room() {
        let l = |v: &[i64]| -> Vec<Option<u32>> {
            v.iter().map(|&x| (x >= 0).then_some(x as u32)).collect()
        };
        // Mostly voice 1 (me), voice 0 a fair share, voice 2 a stray bit.
        let mut labels = l(&[0; 30]);
        labels.extend(l(&[1; 100]));
        labels.extend(l(&[2; 3]));
        labels.extend(l(&[-1; 5]));
        let got = others_in_room(&labels);
        assert_eq!(got[0], Some(0));
        assert_eq!(got[40], Some(ME));
        assert_eq!(got[131], Some(ME));
        assert_eq!(got[134], None);
        // Just me, split in two by accident: all mine.
        let mut labels = l(&[0; 95]);
        labels.extend(l(&[1; 8]));
        assert!(others_in_room(&labels).iter().all(|s| *s == Some(ME)));
        assert_eq!(offset(&l(&[0, -1, 2]), 100), l(&[100, -1, 102]));
    }
}
