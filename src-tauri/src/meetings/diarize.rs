//! Telling voices apart in an in-person meeting (one mic, several people).
//!
//! The speech is cut into short windows; a speaker-embedding model (3D-Speaker
//! ERes2Net, ONNX, downloaded on first use) turns each window into a voice
//! fingerprint; fingerprints are grouped by similarity into speakers. The
//! result is a speaker number per VAD frame, which the pipeline uses to cut
//! chunks where the speaker changes, so each transcript segment has one voice.

use super::segment::Turns;
use super::transcript::{Chunk, FRAME_MS};
use ndarray::Array3;
use ort::session::Session;
use ort::value::Value;
use rustfft::num_complex::Complex;
use std::path::Path;

/// 3D-Speaker ERes2Net (base): on AMI it tells people apart better than
/// WeSpeaker ResNet34 (DER 37% vs 42%) and recognises them across meetings
/// far more reliably (notes/benchmarking.md).
pub const MODEL_FILE: &str = "3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx";
pub const MODEL_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx";
pub const MODEL_BYTES: u64 = 39_593_761;

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
    windows_with(speech, None)
}

/// [`windows`], also cut where the speaker changes and leaving out speech
/// where two people talk at once.
pub fn windows_with(speech: &[bool], turns: Option<&super::segment::Turns>) -> Vec<(usize, usize)> {
    let change = |i: usize| turns.is_some_and(|t| t.change.get(i) == Some(&true));
    let overlap = |i: usize| turns.is_some_and(|t| t.overlap.get(i) == Some(&true));
    let mut stretches: Vec<(usize, usize)> = Vec::new();
    for (i, _) in speech
        .iter()
        .enumerate()
        .filter(|(i, s)| **s && !overlap(*i))
    {
        match stretches.last_mut() {
            Some((_, end)) if i - *end <= JOIN_GAP_FRAMES && !change(i) => *end = i + 1,
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
    Ok(speakers_with(wav, speech, model, voiceprint, &[])?.labels)
}

/// Who speaks each frame of a track, and how sure.
#[derive(Debug, Clone, Default)]
pub struct Voices {
    pub labels: Vec<Option<u32>>,
    /// Per frame: how much closer the frame's window is to its own voice
    /// than to the next nearest (0 where there's no speech; small or
    /// negative is unsure).
    pub margins: Vec<f32>,
    /// The fingerprint windows and their fingerprints, for remembering voices.
    pub windows: Vec<(usize, usize)>,
    pub embeddings: Vec<Vec<f32>>,
}

/// Each voice's fingerprint (the mean of its windows', unit length) and how
/// many windows went into it, from final per-frame `labels` (a window
/// belongs to the voice of its middle frame). The user's voice, [`ME`] or
/// unlabelled, is keyed [`ME`].
pub fn voice_prints(
    labels: &[Option<u32>],
    windows: &[(usize, usize)],
    embeddings: &[Vec<f32>],
) -> std::collections::BTreeMap<u32, (Vec<f32>, usize)> {
    let mut out: std::collections::BTreeMap<u32, (Vec<f32>, usize)> = Default::default();
    for (&(from, to), e) in windows.iter().zip(embeddings) {
        let mid = (from + to) / 2;
        let Some(v) = labels.get(mid).copied().flatten() else {
            continue;
        };
        let entry = out.entry(v).or_insert_with(|| (vec![0.0; e.len()], 0));
        for (a, x) in entry.0.iter_mut().zip(e) {
            *a += x;
        }
        entry.1 += 1;
    }
    for (print, _) in out.values_mut() {
        *print = unit(std::mem::take(print));
    }
    out
}

/// [`speakers`], with names the call app gave per frame (`hints`, may be
/// empty): windows under different names are never one voice, and windows
/// under the same name join more readily.
pub fn speakers_with(
    wav: &Path,
    speech: &[bool],
    model: &Path,
    voiceprint: Option<&[f32]>,
    hints: &[Option<String>],
) -> Result<Voices, String> {
    let seg = super::segment::model_beside(model);
    let (wins, embeddings, turns) = fingerprint_with(wav, speech, model, seg.as_deref())?;
    let mut voices = label_with(speech, &wins, &embeddings, voiceprint, hints);
    // Two people at once: whoever the frame went to, it's unsure.
    if let Some(t) = turns {
        for (m, o) in voices.margins.iter_mut().zip(&t.overlap) {
            if *o {
                *m = m.min(OVERLAP_MARGIN);
            }
        }
    }
    Ok(voices)
}

/// The margin given to frames where two people talk at once.
const OVERLAP_MARGIN: f32 = -0.1;

/// Fingerprint windows (frame ranges) and a fingerprint for each.
pub type Prints = (Vec<(usize, usize)>, Vec<Vec<f32>>);
/// [`Prints`] and the turns the windows were cut at.
pub type PrintsAndTurns = (Vec<(usize, usize)>, Vec<Vec<f32>>, Option<Turns>);

/// The windows of a track's speech and a fingerprint for each.
pub fn fingerprint(wav: &Path, speech: &[bool], model: &Path) -> Result<Prints, String> {
    fingerprint_with(wav, speech, model, None).map(|(w, e, _)| (w, e))
}

/// [`fingerprint`], with windows cut at turns found by the segmentation
/// model when given (and the turns, for flagging overlaps).
pub fn fingerprint_with(
    wav: &Path,
    speech: &[bool],
    model: &Path,
    segmentation: Option<&Path>,
) -> Result<PrintsAndTurns, String> {
    let mut reader = hound::WavReader::open(wav).map_err(|e| e.to_string())?;
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    let turns = match segmentation {
        Some(m) => super::segment::turns(&samples, speech.len(), m)
            .inspect_err(|e| log::warn!("Couldn't find speaker turns: {e}"))
            .ok(),
        None => None,
    };
    let wins = windows_with(speech, turns.as_ref());
    if wins.is_empty() {
        return Ok((wins, Vec::new(), turns));
    }
    let embeddings = embed_windows(&samples, &wins, model)?;
    Ok((wins, embeddings, turns))
}

/// A speaker per speech frame from the windows' fingerprints. With the
/// user's `voiceprint` for this mic, their voice is labelled [`ME`].
pub fn label(
    speech: &[bool],
    wins: &[(usize, usize)],
    embeddings: &[Vec<f32>],
    voiceprint: Option<&[f32]>,
) -> Vec<Option<u32>> {
    label_with(speech, wins, embeddings, voiceprint, &[]).labels
}

pub fn label_with(
    speech: &[bool],
    wins: &[(usize, usize)],
    embeddings: &[Vec<f32>],
    voiceprint: Option<&[f32]>,
    hints: &[Option<String>],
) -> Voices {
    if wins.is_empty() {
        return Voices {
            labels: vec![None; speech.len()],
            margins: vec![0.0; speech.len()],
            ..Default::default()
        };
    }
    let names = window_names(wins, hints);
    let (mut speakers, window_margins) =
        group_voices_with(embeddings, &Grouping::default(), &names);
    if let Some(print) = voiceprint {
        mark_me(embeddings, &mut speakers, print);
    }
    log::info!(
        "Meeting diarization: {} windows, {} speakers, {} named windows",
        wins.len(),
        speakers.iter().max().map_or(0, |m| m + 1),
        names.iter().filter(|n| n.is_some()).count()
    );
    let mut margins = vec![0.0f32; speech.len()];
    for (&(from, to), &m) in wins.iter().zip(&window_margins) {
        for x in &mut margins[from..to.min(speech.len())] {
            *x = m;
        }
    }
    Voices {
        labels: frame_labels(speech, wins, &speakers),
        margins,
        windows: wins.to_vec(),
        embeddings: embeddings.to_vec(),
    }
}

/// A window's name: the one hint covering most of it (at least
/// [`HINT_SHARE`]), numbered by first appearance.
pub fn window_names(wins: &[(usize, usize)], hints: &[Option<String>]) -> Vec<Option<u32>> {
    let mut known: Vec<&str> = Vec::new();
    wins.iter()
        .map(|&(from, to)| {
            let mut count: Vec<(&str, usize)> = Vec::new();
            for h in hints
                .get(from..to.min(hints.len()))
                .unwrap_or(&[])
                .iter()
                .flatten()
            {
                match count.iter_mut().find(|(n, _)| *n == h.as_str()) {
                    Some((_, c)) => *c += 1,
                    None => count.push((h.as_str(), 1)),
                }
            }
            let (name, n) = count.into_iter().max_by_key(|(_, c)| *c)?;
            if (n as f32) < HINT_SHARE * (to - from) as f32 {
                return None;
            }
            let i = known.iter().position(|k| *k == name).unwrap_or_else(|| {
                known.push(name);
                known.len() - 1
            });
            Some(i as u32)
        })
        .collect()
}

/// Share of a window one name must cover to count.
const HINT_SHARE: f32 = 0.6;
/// Added to the similarity of two groups under the same name.
const SAME_NAME_BONUS: f32 = 0.15;

/// Settings for [`group_voices`].
#[derive(Debug, Clone, Copy)]
pub struct Grouping {
    /// Subtract the meeting's mean fingerprint first, so what every window
    /// shares (the room, the mic, the line) stops making voices look alike.
    pub normalise: bool,
    /// First pass: windows at least this similar form a small group.
    pub first_pass: f32,
    /// Groups keep merging while the closest two are at least this similar.
    pub stop: f32,
    /// A voice heard in fewer windows than this is folded into the nearest.
    pub min_windows: usize,
}

impl Default for Grouping {
    fn default() -> Self {
        Self {
            normalise: true,
            first_pass: 0.7,
            // Tuned on AMI with ERes2Net (notes/benchmarking.md): 0.0 is as
            // good as 0.03 there and doesn't split a call's voices in two.
            stop: 0.0,
            min_windows: MIN_WINDOWS_PER_SPEAKER,
        }
    }
}

/// Group fingerprints into voices over the whole meeting: an online pass
/// with a strict threshold makes small, surely-one-voice groups, then the
/// most similar groups merge (average similarity of their windows, kept as
/// sums so each merge is cheap) until none are similar enough; voices heard
/// in too few windows join the nearest. Numbered in order of first word.
pub fn group_voices(embeddings: &[Vec<f32>], g: &Grouping) -> Vec<u32> {
    group_voices_with(embeddings, g, &[]).0
}

/// [`group_voices`] with a name per window where the call app gave one
/// (`names`, may be empty), and each window's margin: its similarity to its
/// own voice minus to the nearest other.
pub fn group_voices_with(
    embeddings: &[Vec<f32>],
    g: &Grouping,
    names: &[Option<u32>],
) -> (Vec<u32>, Vec<f32>) {
    if embeddings.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let name_of = |i: usize| names.get(i).copied().flatten();
    let dim = embeddings[0].len();
    let embs: Vec<Vec<f32>> = if g.normalise && embeddings.len() > 1 {
        let mut mean = vec![0.0f32; dim];
        for e in embeddings {
            for (m, x) in mean.iter_mut().zip(e) {
                *m += x / embeddings.len() as f32;
            }
        }
        embeddings
            .iter()
            .map(|e| unit(e.iter().zip(&mean).map(|(x, m)| x - m).collect()))
            .collect()
    } else {
        embeddings.to_vec()
    };

    // First pass: small groups of near-identical windows.
    let mut sums: Vec<Vec<f32>> = Vec::new();
    let mut counts: Vec<usize> = Vec::new();
    let mut gname: Vec<Option<u32>> = Vec::new();
    let mut labels: Vec<usize> = Vec::with_capacity(embs.len());
    for (i, e) in embs.iter().enumerate() {
        let n = name_of(i);
        let best = sums
            .iter()
            .enumerate()
            .filter(|(k, _)| n.is_none() || gname[*k].is_none() || gname[*k] == n)
            .map(|(k, s)| (k, cosine(e, s) / norm(s).max(1e-9)))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        match best {
            Some((k, sim)) if sim >= g.first_pass => {
                for (a, x) in sums[k].iter_mut().zip(e) {
                    *a += x;
                }
                counts[k] += 1;
                gname[k] = gname[k].or(n);
                labels.push(k);
            }
            _ => {
                sums.push(e.clone());
                counts.push(1);
                gname.push(n);
                labels.push(sums.len() - 1);
            }
        }
    }

    // Merge: average similarity between two groups is the dot product of
    // their sums over the product of their sizes (windows are unit length).
    let k = sums.len();
    let mut alive = vec![true; k];
    let mut parent: Vec<usize> = (0..k).collect();
    // Average similarity, kept apart under different names and pulled
    // together under the same one.
    let avg = |sums: &[Vec<f32>], counts: &[usize], gname: &[Option<u32>], a: usize, b: usize| {
        let s = cosine(&sums[a], &sums[b]) / (counts[a] * counts[b]) as f32;
        match (gname[a], gname[b]) {
            (Some(x), Some(y)) if x != y => f32::NEG_INFINITY,
            (Some(_), Some(_)) => s + SAME_NAME_BONUS,
            _ => s,
        }
    };
    let mut sim = vec![vec![f32::NEG_INFINITY; k]; k];
    #[allow(clippy::needless_range_loop)]
    for a in 0..k {
        for b in a + 1..k {
            let s = avg(&sums, &counts, &gname, a, b);
            sim[a][b] = s;
            sim[b][a] = s;
        }
    }
    // Nearest-neighbour chain: follow each group to its most similar one
    // until two point at each other, and merge those. For average
    // similarity this gives the same groups as always merging the closest
    // pair first, without searching every pair each time. A group whose
    // best match is below `stop` can never reach it later (merging others
    // only averages similarities), so it's set aside.
    let mut open = alive.clone();
    let mut chain: Vec<usize> = Vec::new();
    loop {
        if chain.is_empty() {
            match (0..k).find(|&c| open[c]) {
                Some(c) => chain.push(c),
                None => break,
            }
        }
        let a = *chain.last().unwrap();
        let prev = chain.len().checked_sub(2).map(|i| chain[i]);
        let mut best: Option<(usize, f32)> = None;
        for c in (0..k).filter(|&c| open[c] && c != a) {
            let better = match best {
                None => true,
                Some((_, s)) => sim[a][c] > s || (sim[a][c] == s && Some(c) == prev),
            };
            if better {
                best = Some((c, sim[a][c]));
            }
        }
        match best {
            Some((b, s)) if s >= g.stop => {
                if Some(b) == prev {
                    chain.pop();
                    chain.pop();
                    let moved = std::mem::take(&mut sums[b]);
                    for (x, y) in sums[a].iter_mut().zip(&moved) {
                        *x += y;
                    }
                    counts[a] += counts[b];
                    gname[a] = gname[a].or(gname[b]);
                    alive[b] = false;
                    open[b] = false;
                    parent[b] = a;
                    for c in (0..k).filter(|&c| alive[c] && c != a) {
                        let s = avg(&sums, &counts, &gname, a, c);
                        sim[a][c] = s;
                        sim[c][a] = s;
                    }
                } else {
                    chain.push(b);
                }
            }
            _ => {
                open[a] = false;
                chain.clear();
            }
        }
    }
    let root = |mut i: usize| {
        while parent[i] != i {
            i = parent[i];
        }
        i
    };
    let mut labels: Vec<usize> = labels.into_iter().map(root).collect();

    // Fold voices heard in too few windows into the nearest real one.
    let size = |labels: &[usize], c: usize| labels.iter().filter(|&&l| l == c).count();
    let big: Vec<usize> = (0..k)
        .filter(|&c| alive[c] && size(&labels, c) >= g.min_windows)
        .collect();
    if !big.is_empty() {
        for i in 0..labels.len() {
            if !big.contains(&labels[i]) {
                labels[i] = *big
                    .iter()
                    .max_by(|&&a, &&b| {
                        (cosine(&embs[i], &sums[a]) / counts[a] as f32)
                            .total_cmp(&(cosine(&embs[i], &sums[b]) / counts[b] as f32))
                    })
                    .unwrap();
            }
        }
    }
    // Each window's margin over the nearest other voice.
    let finals: Vec<usize> = {
        let mut f: Vec<usize> = labels.clone();
        f.sort_unstable();
        f.dedup();
        f
    };
    let margins: Vec<f32> = labels
        .iter()
        .zip(&embs)
        .map(|(&l, e)| {
            let to = |c: usize| cosine(e, &sums[c]) / norm(&sums[c]).max(1e-9);
            let own = to(l);
            let other = finals
                .iter()
                .filter(|&&c| c != l)
                .map(|&c| to(c))
                .fold(f32::NEG_INFINITY, f32::max);
            if other.is_finite() {
                own - other
            } else {
                1.0
            }
        })
        .collect();
    let mut order: Vec<usize> = Vec::new();
    let numbered = labels
        .iter()
        .map(|l| {
            let pos = order.iter().position(|o| o == l).unwrap_or_else(|| {
                order.push(*l);
                order.len() - 1
            });
            pos as u32
        })
        .collect();
    (numbered, margins)
}

fn norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
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

    #[test]
    fn names_keep_alike_voices_apart_and_pull_one_voice_together() {
        // Two voices so alike that grouping alone makes them one.
        let a: Vec<f32> = unit((0..32).map(|i| (i as f32).sin()).collect());
        let b: Vec<f32> = unit(
            a.iter()
                .enumerate()
                .map(|(i, x)| x + 0.05 * (i as f32).cos())
                .collect(),
        );
        let embs: Vec<Vec<f32>> = (0..40u32)
            .map(|i| around(if i % 2 == 0 { &a } else { &b }, 0.02, i))
            .collect();
        let g = Grouping {
            normalise: false,
            ..Grouping::default()
        };
        let alone = group_voices(&embs, &g);
        assert!(alone.iter().all(|&l| l == alone[0]));
        // The call app names them apart.
        let names: Vec<Option<u32>> = (0..40).map(|i| Some(i % 2)).collect();
        let (named, margins) = group_voices_with(&embs, &g, &names);
        assert_ne!(named[0], named[1]);
        assert!(named.iter().step_by(2).all(|&l| l == named[0]));
        assert_eq!(margins.len(), 40);
    }

    #[test]
    fn a_window_takes_the_name_covering_most_of_it() {
        let hints: Vec<Option<String>> = (0..100)
            .map(|i| match i {
                0..=39 => Some("Sam".to_string()),
                40..=49 => Some("Priya".to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(
            window_names(&[(0, 50), (50, 100), (30, 80)], &hints),
            vec![Some(0), None, None]
        );
    }
}
