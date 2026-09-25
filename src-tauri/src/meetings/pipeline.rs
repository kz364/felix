//! Turning a recorded meeting into a transcript: find the speech in each
//! track with Silero VAD, cut it into chunks at pauses, transcribe the chunks
//! in time order and save after each one, so a quit or crash resumes where it
//! stopped. On a call, the mic's echo of the other side is then removed.
//!
//! The model is passed in as a closure, so the app can share the dictation
//! engine (see `TranscriptionManager::transcribe_for_meeting`) and the
//! `meeting_transcribe` example can run a model directly.

use super::capture::MeetingMode;
use super::level::LevelSettings;
use super::track::SAMPLE_RATE;
use super::transcript::{self, Chunk, Segment, Source, Transcript};
use crate::audio_toolkit::vad::{SileroVad, VoiceActivityDetector};
use std::path::Path;

/// Silero's speech threshold for meetings. A little stricter than dictation's
/// (0.3), since a meeting track has long stretches of room noise.
const VAD_THRESHOLD: f32 = 0.4;
const FRAME_SAMPLES: usize = (SAMPLE_RATE as u64 * transcript::FRAME_MS / 1000) as usize;

/// Per VAD frame of a track: is it speech, and how loud (mean square).
pub struct Analysis {
    pub speech: Vec<bool>,
    pub energy: Vec<f32>,
}

/// The gain that lifts a quiet track's room noise to about -45 dBFS, so the
/// VAD hears distant voices above it (in person). Only for finding speech;
/// the audio itself is levelled per chunk.
fn vad_gain(wav: &Path) -> Result<f32, String> {
    let mut reader = hound::WavReader::open(wav).map_err(|e| format!("{}: {e}", wav.display()))?;
    let mut levels = Vec::new();
    let mut sum = 0.0f32;
    let mut n = 0;
    for sample in reader.samples::<i16>() {
        let v = sample.map_err(|e| e.to_string())? as f32 / i16::MAX as f32;
        sum += v * v;
        n += 1;
        if n == FRAME_SAMPLES {
            levels.push((sum / n as f32).sqrt());
            (sum, n) = (0.0, 0);
        }
    }
    levels.retain(|l| *l > 0.0);
    if levels.is_empty() {
        return Ok(1.0);
    }
    levels.sort_by(|a, b| a.total_cmp(b));
    let noise = levels[levels.len() / 5];
    Ok((0.0056 / noise).clamp(
        1.0,
        crate::audio_toolkit::audio::db_to_linear(super::level::MAX_BOOST_DB),
    ))
}

/// Run the VAD over a 16 kHz mono WAV. With `lift_quiet`, a quiet track is
/// raised for the VAD (see [`vad_gain`]).
pub fn analyze(wav: &Path, vad_model: &Path, lift_quiet: bool) -> Result<Analysis, String> {
    let mut vad = SileroVad::new(vad_model, VAD_THRESHOLD).map_err(|e| e.to_string())?;
    let gain = if lift_quiet { vad_gain(wav)? } else { 1.0 };
    let mut reader = hound::WavReader::open(wav).map_err(|e| format!("{}: {e}", wav.display()))?;
    check_format(&reader, wav)?;
    let mut lifted = Vec::with_capacity(FRAME_SAMPLES);
    let frames = reader.duration() as usize / FRAME_SAMPLES + 1;
    let mut speech = Vec::with_capacity(frames);
    let mut energy = Vec::with_capacity(frames);
    let mut frame = Vec::with_capacity(FRAME_SAMPLES);
    for sample in reader.samples::<i16>() {
        frame.push(sample.map_err(|e| e.to_string())? as f32 / i16::MAX as f32);
        if frame.len() == FRAME_SAMPLES {
            let e = frame.iter().map(|s| s * s).sum::<f32>() / FRAME_SAMPLES as f32;
            // Digital silence (padding, nothing playing) needs no model.
            let frame_for_vad = if gain > 1.0 {
                lifted.clear();
                lifted.extend(
                    frame
                        .iter()
                        .map(|s| crate::audio_toolkit::audio::soft_limit(s * gain)),
                );
                &lifted
            } else {
                &frame
            };
            speech.push(
                e > 0.0
                    && vad
                        .push_frame(frame_for_vad)
                        .map_err(|e| e.to_string())?
                        .is_speech(),
            );
            energy.push(e);
            frame.clear();
        }
    }
    Ok(Analysis { speech, energy })
}

/// How far back to look for the system audio that could be echoing in the
/// mic: the speaker-to-mic delay plus the room's reverb tail.
const ECHO_LOOKBACK_FRAMES: usize = 8;
/// The mic counts as the user talking when it's this much louder than the
/// echo alone would be (power ratio; 6 dB).
const DOUBLE_TALK_RATIO: f32 = 4.0;

/// Which mic frames hold only the echo of the call (the Mac's audio coming
/// back through the speakers), not the user. The echo's level is learnt from
/// the whole meeting: the typical ratio of mic to system loudness while the
/// system plays. Mic frames not clearly louder than that are echo.
pub fn echo_mask(mic: &[f32], system: &[f32], system_speech: &[bool]) -> Vec<bool> {
    let loudest_recent = |i: usize| {
        let from = i.saturating_sub(ECHO_LOOKBACK_FRAMES);
        system
            .get(from..=i.min(system.len().saturating_sub(1)))
            .map(|w| w.iter().copied().fold(0.0f32, f32::max))
            .unwrap_or(0.0)
    };
    // Typical echo gain while the other side talks (the user is usually quiet).
    let mut ratios: Vec<f32> = (0..mic.len().min(system.len()))
        .filter(|&i| system_speech.get(i).copied().unwrap_or(false) && system[i] > 1e-7)
        .map(|i| mic[i] / loudest_recent(i))
        .collect();
    if ratios.len() < 30 {
        return vec![false; mic.len()];
    }
    let mid = ratios.len() / 2;
    let gain = *ratios.select_nth_unstable_by(mid, |a, b| a.total_cmp(b)).1;
    (0..mic.len())
        .map(|i| {
            let echo = gain * loudest_recent(i.min(system.len().saturating_sub(1)));
            echo > 1e-7 && mic[i] < DOUBLE_TALK_RATIO * echo
        })
        .collect()
}

fn check_format<R: std::io::Read>(reader: &hound::WavReader<R>, wav: &Path) -> Result<(), String> {
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE || spec.channels != 1 || spec.bits_per_sample != 16 {
        return Err(format!("{} isn't 16 kHz mono 16-bit", wav.display()));
    }
    Ok(())
}

/// The audio of one chunk, with the frames in `silence` (echo) zeroed.
pub fn read_chunk(wav: &Path, chunk: Chunk, silence: Option<&[bool]>) -> Result<Vec<f32>, String> {
    let mut reader = hound::WavReader::open(wav).map_err(|e| format!("{}: {e}", wav.display()))?;
    check_format(&reader, wav)?;
    let at = |ms: u64| (ms * SAMPLE_RATE as u64 / 1000) as u32;
    let start = at(chunk.start_ms).min(reader.duration());
    reader.seek(start).map_err(|e| e.to_string())?;
    let len = at(chunk.end_ms).min(reader.duration()) - start;
    let mut audio = reader
        .samples::<i16>()
        .take(len as usize)
        .map(|s| {
            s.map(|v| v as f32 / i16::MAX as f32)
                .map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<f32>, String>>()?;
    if let Some(silence) = silence {
        for (i, s) in audio.iter_mut().enumerate() {
            let frame = (start as usize + i) / FRAME_SAMPLES;
            if silence.get(frame).copied().unwrap_or(false) {
                *s = 0.0;
            }
        }
    }
    Ok(audio)
}

pub fn load(dir: &Path) -> Option<Transcript> {
    serde_json::from_slice(&std::fs::read(dir.join(transcript::FILE)).ok()?).ok()
}

pub fn save(dir: &Path, t: &Transcript) -> Result<(), String> {
    let json = serde_json::to_vec_pretty(t).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("{}.tmp", transcript::FILE));
    std::fs::write(&tmp, json)
        .and_then(|_| std::fs::rename(&tmp, dir.join(transcript::FILE)))
        .map_err(|e| format!("Couldn't save the transcript: {e}"))
}

/// Why a run stopped before the end.
#[derive(Debug)]
pub enum Stopped {
    Cancelled,
    Failed(String),
}

impl From<String> for Stopped {
    fn from(e: String) -> Self {
        Stopped::Failed(e)
    }
}

/// How far a run has got.
#[derive(Debug, Clone, Copy)]
pub enum Step {
    /// Telling the voices apart (in person, with a speaker model).
    Identifying,
    Transcribing {
        done: usize,
        total: usize,
    },
}

/// Transcribe the meeting in `dir`, continuing an unfinished transcript.
/// `transcribe` turns one chunk's audio into text; returning `Err(Stopped)`
/// ends the run (the work so far is saved). `progress` is called as the run
/// moves on. `level` is applied to the mic in person; with `speaker_model`,
/// an in-person mic track is also split by voice (see [`super::diarize`]).
#[allow(clippy::too_many_arguments)]
pub fn run(
    dir: &Path,
    mode: MeetingMode,
    vad_model: &Path,
    level: Option<LevelSettings>,
    speaker_model: Option<&Path>,
    engine: &str,
    mut transcribe: impl FnMut(Vec<f32>) -> Result<String, Stopped>,
    mut progress: impl FnMut(Step),
) -> Result<Transcript, Stopped> {
    let speaker_model = speaker_model.filter(|_| mode == MeetingMode::InPerson);
    // Resume only work done the same way; otherwise start over.
    let engine = match speaker_model {
        Some(_) => format!("{engine} +speakers"),
        None => engine.to_string(),
    };
    let engine = engine.as_str();
    let mut t = match load(dir) {
        Some(t) if t.version == transcript::VERSION && t.engine == engine => t,
        _ => Transcript {
            version: transcript::VERSION,
            engine: engine.to_string(),
            ..Default::default()
        },
    };
    t.complete = false;

    // Find the speech in each track. On a call, the mic's echo of the other
    // side is masked out first, so the user's own words get chunks of their
    // own instead of being merged with (and dropped as) echo.
    let mic_wav = dir.join(Source::Mic.file());
    let system_wav = dir.join(Source::System.file());
    let lift_mic = mode == MeetingMode::InPerson && level.is_some_and(|l| l.auto);
    let mic = if mic_wav.is_file() {
        Some(analyze(&mic_wav, vad_model, lift_mic)?)
    } else {
        None
    };
    let system = if mode == MeetingMode::Call && system_wav.is_file() {
        Some(analyze(&system_wav, vad_model, false)?)
    } else {
        None
    };
    let echo = match (&mic, &system) {
        (Some(m), Some(s)) => Some(echo_mask(&m.energy, &s.energy, &s.speech)),
        _ => None,
    };

    // Who speaks when, in person. If it fails, the transcript goes ahead
    // without speakers.
    let speakers = match (speaker_model, &mic) {
        (Some(model), Some(m)) => {
            progress(Step::Identifying);
            match super::diarize::speakers(&mic_wav, &m.speech, model) {
                Ok(labels) => Some(labels),
                Err(e) => {
                    log::warn!("Couldn't tell the speakers apart: {e}");
                    None
                }
            }
        }
        _ => None,
    };

    // Every chunk of every track, in time order, cut where the speaker changes.
    let mut plan: Vec<(Source, Chunk, Option<u32>)> = Vec::new();
    if let Some(m) = &mic {
        let speech: Vec<bool> = match &echo {
            Some(echo) => m.speech.iter().zip(echo).map(|(s, e)| *s && !*e).collect(),
            None => m.speech.clone(),
        };
        for c in transcript::plan_chunks(&speech) {
            match &speakers {
                Some(labels) => plan.extend(
                    super::diarize::split_by_speaker(c, labels)
                        .into_iter()
                        .map(|(c, s)| (Source::Mic, c, s)),
                ),
                None => plan.push((Source::Mic, c, None)),
            }
        }
    }
    if let Some(s) = &system {
        plan.extend(
            transcript::plan_chunks(&s.speech)
                .into_iter()
                .map(|c| (Source::System, c, None)),
        );
    }
    plan.sort_by_key(|(source, c, _)| (c.start_ms, *source == Source::System));

    let total = plan.len();
    let is_done = |t: &Transcript, source: Source, c: &Chunk| {
        t.segments
            .iter()
            .any(|s| s.source == source && s.start_ms == c.start_ms)
    };
    let mut done = plan.iter().filter(|(s, c, _)| is_done(&t, *s, c)).count();
    progress(Step::Transcribing { done, total });
    for (source, chunk, speaker) in plan {
        if is_done(&t, source, &chunk) {
            continue;
        }
        let silence = match source {
            Source::Mic => echo.as_deref(),
            Source::System => None,
        };
        let mut audio = read_chunk(&dir.join(source.file()), chunk, silence)?;
        if let (Source::Mic, MeetingMode::InPerson, Some(settings), Some(m)) =
            (source, mode, level, &mic)
        {
            let start = (chunk.start_ms * SAMPLE_RATE as u64 / 1000) as usize;
            super::level::level(&mut audio, start, &m.speech, settings);
        }
        let text = transcribe(audio)?;
        t.segments.push(Segment {
            source,
            start_ms: chunk.start_ms,
            end_ms: chunk.end_ms,
            text: text.trim().to_string(),
            echo: false,
            speaker,
        });
        save(dir, &t)?;
        done += 1;
        progress(Step::Transcribing { done, total });
    }

    t.segments
        .sort_by_key(|s| (s.start_ms, s.source == Source::System));
    if mode == MeetingMode::Call {
        transcript::mark_echo(&mut t.segments);
    }
    t.complete = true;
    save(dir, &t)?;
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chunk_is_read_from_where_it_starts() {
        let dir = std::env::temp_dir().join(format!("handy-pipeline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let wav = dir.join("mic.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&wav, spec).unwrap();
        // 1 s of silence, then 1 s at half scale.
        for i in 0..2 * SAMPLE_RATE {
            w.write_sample(if i < SAMPLE_RATE { 0i16 } else { i16::MAX / 2 })
                .unwrap();
        }
        w.finalize().unwrap();
        let audio = read_chunk(
            &wav,
            Chunk {
                start_ms: 900,
                end_ms: 5000,
            },
            None,
        )
        .unwrap();
        assert_eq!(audio.len(), 1100 * 16);
        assert_eq!(audio[1599], 0.0);
        assert!((audio[1600] - 0.5).abs() < 0.01);
        // Masked frames come back silent.
        let mut mask = vec![false; 100];
        mask[60] = true; // 1.80–1.83 s
        let audio = read_chunk(
            &wav,
            Chunk {
                start_ms: 1000,
                end_ms: 2000,
            },
            Some(&mask),
        )
        .unwrap();
        assert!(audio[12799] > 0.4);
        assert_eq!(audio[12800], 0.0);
        assert_eq!(audio[13279], 0.0);
        assert!(audio[13280] > 0.4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_calls_echo_is_masked_but_the_user_talking_over_it_is_not() {
        // 200 frames: the system talks in 20..120; the mic hears it back at
        // -18 dB, and the user talks in 90..110 (double-talk) and 150..170.
        let mut system = vec![0.0f32; 200];
        let mut system_speech = vec![false; 200];
        let mut mic = vec![1e-6f32; 200];
        for i in 20..120 {
            system[i] = 0.01 * (1.0 + (i % 7) as f32 / 7.0);
            system_speech[i] = true;
        }
        for i in 0..200usize {
            // Echo arrives 2 frames late with a reverb tail.
            let src = i.saturating_sub(2);
            mic[i] += system[src] * 0.016;
        }
        for i in (90..110).chain(150..170) {
            mic[i] += 0.005;
        }
        let mask = echo_mask(&mic, &system, &system_speech);
        assert!(
            mask[30..85].iter().all(|&m| m),
            "echo-only frames are masked"
        );
        assert!(mask[90..110].iter().all(|&m| !m), "double-talk is kept");
        assert!(mask[150..170].iter().all(|&m| !m), "the user alone is kept");
        // No system audio at all (headphones, in person): nothing is masked.
        assert!(echo_mask(&mic, &[0.0; 200], &[false; 200])
            .iter()
            .all(|&m| !m));
    }
}
