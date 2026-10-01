//! Where the speaker changes and where two people talk at once, from
//! pyannote's segmentation model (segmentation-3.0, ONNX via sherpa-onnx).
//! It looks at 10 s of audio and says, every ~17 ms, which of up to three
//! local speakers talk (alone or two at once). Fingerprint windows are then
//! cut at turns so each holds one voice, and overlapped speech is left out
//! of fingerprints (it sounds like neither person) and flagged as unsure.

use super::transcript::FRAME_MS;
use ndarray::Array3;
use ort::session::Session;
use ort::value::Value;
use std::path::Path;

pub const MODEL_DIR: &str = "sherpa-onnx-pyannote-segmentation-3-0";
pub const MODEL_FILE: &str = "model.onnx";
pub const MODEL_URL: &str =
    "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx";
pub const MODEL_BYTES: u64 = 5_992_913;

/// The segmentation model next to the speaker model, if it's there.
pub fn model_beside(speaker_model: &std::path::Path) -> Option<std::path::PathBuf> {
    let p = speaker_model.parent()?.join(MODEL_DIR).join(MODEL_FILE);
    std::fs::metadata(&p)
        .is_ok_and(|m| m.len() == MODEL_BYTES)
        .then_some(p)
}

const SAMPLE_RATE: usize = 16_000;
/// The model's window: 10 s.
const CHUNK: usize = 10 * SAMPLE_RATE;
/// Chunks overlap by half; each one's middle half is trusted.
const STEP: usize = CHUNK / 2;
/// Chunks per thread (see `diarize::WINDOWS_PER_THREAD` on ONNX Runtime's
/// per-thread cache).
const CHUNKS_PER_THREAD: usize = 64;
const FRAME_SAMPLES: usize = SAMPLE_RATE * FRAME_MS as usize / 1000;

/// Per 30 ms frame of a track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Turns {
    /// A new person starts talking here.
    pub change: Vec<bool>,
    /// Two people talk at once.
    pub overlap: Vec<bool>,
}

/// Which local speakers each powerset class means (none; A, B, C; A+B,
/// A+C, B+C).
const CLASSES: [&[u8]; 7] = [&[], &[0], &[1], &[2], &[0, 1], &[0, 2], &[1, 2]];

/// Turns from one chunk's classes (one per model frame), into `out` for the
/// track frames `keep` covers. `offset` is the chunk's first sample.
fn read_chunk(classes: &[usize], offset: usize, keep: (usize, usize), out: &mut Turns) {
    if classes.is_empty() {
        return;
    }
    let per_frame = CHUNK as f32 / classes.len() as f32;
    let frames = out.change.len();
    let first = offset / FRAME_SAMPLES;
    let last = ((offset + CHUNK) / FRAME_SAMPLES).min(frames);
    let mut speaker: Option<u8> = None;
    for f in first..last {
        let centre = (f * FRAME_SAMPLES + FRAME_SAMPLES / 2).saturating_sub(offset);
        let k = ((centre as f32 / per_frame) as usize).min(classes.len() - 1);
        let who = CLASSES[classes[k].min(6)];
        let kept = f >= keep.0 && f < keep.1;
        if kept && who.len() == 2 {
            out.overlap[f] = true;
        }
        if let [one] = who {
            if speaker.is_some_and(|s| s != *one) && kept {
                out.change[f] = true;
            }
            speaker = Some(*one);
        }
    }
}

struct Segmenter {
    session: Session,
}

impl Segmenter {
    fn new(model: &Path) -> Result<Self, String> {
        let fail = |e: String| format!("Couldn't load the segmentation model: {e}");
        let session = Session::builder()
            .map_err(|e| fail(e.to_string()))?
            .with_intra_threads(2)
            .map_err(|e| fail(e.to_string()))?
            .commit_from_file(model)
            .map_err(|e| fail(e.to_string()))?;
        Ok(Self { session })
    }

    /// The most likely class per model frame for 10 s of audio.
    fn classes(&mut self, audio: &[f32]) -> Result<Vec<usize>, String> {
        let mut x = audio.to_vec();
        x.resize(CHUNK, 0.0);
        let input = Array3::from_shape_vec((1, 1, CHUNK), x).map_err(|e| e.to_string())?;
        let input = Value::from_array(input).map_err(|e| e.to_string())?;
        let name = self.session.inputs()[0].name().to_string();
        let outputs = self
            .session
            .run(ort::inputs![name => input])
            .map_err(|e| format!("Segmentation model failed: {e}"))?;
        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| e.to_string())?;
        let n_classes = *shape.last().unwrap_or(&7) as usize;
        Ok(data
            .chunks(n_classes.max(1))
            .map(|row| {
                row.iter()
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .map_or(0, |(i, _)| i)
            })
            .collect())
    }
}

/// Turns and overlaps over a whole track (`frames` frames of 30 ms).
pub fn turns(samples: &[f32], frames: usize, model: &Path) -> Result<Turns, String> {
    turns_cached(samples, frames, model, &mut Vec::new())
}

/// [`turns`], keeping the model's answer for each chunk it heard whole in
/// `cache` (by chunk), so a track still being recorded only runs the model
/// on what's new. The same samples give the same turns.
pub fn turns_cached(
    samples: &[f32],
    frames: usize,
    model: &Path,
    cache: &mut Vec<Option<Vec<usize>>>,
) -> Result<Turns, String> {
    let mut out = Turns {
        change: vec![false; frames],
        overlap: vec![false; frames],
    };
    if samples.is_empty() {
        return Ok(out);
    }
    let offsets: Vec<usize> = (0..)
        .map(|i| i * STEP)
        .take_while(|&o| o == 0 || o + STEP < samples.len())
        .collect();
    let n = offsets.len();
    let whole = |o: usize| o + CHUNK <= samples.len();
    let mut classes: Vec<Option<Vec<usize>>> = (0..n)
        .map(|i| {
            cache
                .get(i)
                .cloned()
                .flatten()
                .filter(|_| whole(offsets[i]))
        })
        .collect();
    let todo: Vec<usize> = (0..n).filter(|&i| classes[i].is_none()).collect();
    for batch in todo.chunks(CHUNKS_PER_THREAD) {
        let found = std::thread::scope(|scope| {
            scope
                .spawn(|| -> Result<Vec<Vec<usize>>, String> {
                    let mut seg = Segmenter::new(model)?;
                    batch
                        .iter()
                        .map(|&i| {
                            let o = offsets[i];
                            seg.classes(&samples[o..(o + CHUNK).min(samples.len())])
                        })
                        .collect()
                })
                .join()
                .map_err(|_| "The segmentation model crashed".to_string())?
        })?;
        for (&i, c) in batch.iter().zip(found) {
            classes[i] = Some(c);
        }
    }
    cache.resize(n.max(cache.len()), None);
    for (i, c) in classes.iter().enumerate() {
        let o = offsets[i];
        if whole(o) {
            cache[i] = c.clone();
        }
        let Some(c) = c else { continue };
        // The middle half, or out to the edge for the first and last.
        let from = if i == 0 {
            0
        } else {
            (o + CHUNK / 4) / FRAME_SAMPLES
        };
        let to = if i + 1 == n {
            frames
        } else {
            (o + CHUNK * 3 / 4) / FRAME_SAMPLES
        };
        read_chunk(c, o, (from, to), &mut out);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classes_become_turns_and_overlaps() {
        // 10 s at 30 ms is 333 frames; give the model 100 frames: A for 40,
        // A+B for 10, B for 50.
        let classes: Vec<usize> = (0..100)
            .map(|i| match i {
                0..=39 => 1,
                40..=49 => 4,
                _ => 2,
            })
            .collect();
        let mut t = Turns {
            change: vec![false; 333],
            overlap: vec![false; 333],
        };
        read_chunk(&classes, 0, (0, 333), &mut t);
        let changes: Vec<usize> = (0..333).filter(|&f| t.change[f]).collect();
        assert_eq!(changes.len(), 1);
        // Half way: frame 166 or so (B starts at 50% of the chunk).
        assert!((165..=168).contains(&changes[0]), "{changes:?}");
        let overlapped = t.overlap.iter().filter(|&&o| o).count();
        assert!((30..=36).contains(&overlapped), "{overlapped}");
    }
}
