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
use std::collections::BTreeMap;
use std::path::Path;

/// Speakers told apart on the system track are numbered from here, so they
/// never share a number with the voices on the mic.
pub const SYSTEM_SPEAKERS: u32 = 100;
/// How sure each chunk's voice is (window margin), by `<track>-<start ms>`.
pub const VOICES_FILE: &str = "voices.json";

/// Silero's speech threshold for meetings. A little stricter than dictation's
/// (0.3), since a meeting track has long stretches of room noise.
const VAD_THRESHOLD: f32 = 0.4;
const FRAME_SAMPLES: usize = (SAMPLE_RATE as u64 * transcript::FRAME_MS / 1000) as usize;

/// Per VAD frame of a track: is it speech, and how loud (mean square).
pub struct Analysis {
    pub speech: Vec<bool>,
    pub energy: Vec<f32>,
}

/// Run the VAD over a 16 kHz mono WAV. With `live_gain`, the VAD hears the
/// track through the in-person gain ([`super::level::LiveGain`]), so quiet
/// and distant voices are found.
pub fn analyze(wav: &Path, vad_model: &Path, live_gain: bool) -> Result<Analysis, String> {
    let mut vad = SileroVad::new(vad_model, VAD_THRESHOLD).map_err(|e| e.to_string())?;
    let mut gain = live_gain.then(super::level::LiveGain::default);
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
            let frame_for_vad = match &mut gain {
                Some(gain) => {
                    lifted.clear();
                    lifted.extend_from_slice(&frame);
                    gain.process(&mut lifted);
                    &lifted
                }
                None => &frame,
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
///
/// `echo` is what cross-correlating the tracks found (see [`super::echo`]):
/// with it, only frames where echo was confirmed can be masked, and the
/// system audio is looked for at the measured delay.
pub fn echo_mask(
    mic: &[f32],
    system: &[f32],
    system_speech: &[bool],
    echo: Option<&super::echo::EchoReport>,
) -> Vec<bool> {
    if echo.is_some_and(|e| !e.has_echo()) {
        return vec![false; mic.len()];
    }
    // The delay in frames, and the reach either side of it.
    let lag = echo
        .and_then(|e| e.lag_ms)
        .map_or(0, |ms| (ms / transcript::FRAME_MS as f32).round() as usize);
    let allowed = |i: usize| echo.is_none_or(|e| e.frames.get(i).copied().unwrap_or(false));
    let loudest_recent = |i: usize| {
        let to = i.saturating_sub(lag.saturating_sub(2));
        let from = i.saturating_sub(lag + ECHO_LOOKBACK_FRAMES);
        system
            .get(from..=to.min(system.len().saturating_sub(1)))
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
            allowed(i) && echo > 1e-7 && mic[i] < DOUBLE_TALK_RATIO * echo
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
    voiceprint: Option<&[f32]>,
    engine: &str,
    mut transcribe: impl FnMut(Vec<f32>) -> Result<String, Stopped>,
    mut progress: impl FnMut(Step),
) -> Result<Transcript, Stopped> {
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
        (Some(m), Some(s)) => {
            // Headphones keep the call out of the mic: nothing to mask.
            let headphones = super::manager::read_info(dir)
                .and_then(|i| i.output_device)
                .is_some_and(|d| super::echo::is_headphones(&d));
            let report = if headphones {
                super::echo::EchoReport::none(m.energy.len())
            } else {
                super::echo::analyze(&mic_wav, &system_wav, m.energy.len()).unwrap_or_else(|e| {
                    log::warn!("Couldn't correlate the tracks: {e}");
                    super::echo::EchoReport::none(m.energy.len())
                })
            };
            Some(echo_mask(&m.energy, &s.energy, &s.speech, Some(&report)))
        }
        _ => None,
    };
    let mic_speech: Option<Vec<bool>> = mic.as_ref().map(|m| match &echo {
        Some(echo) => m.speech.iter().zip(echo).map(|(s, e)| *s && !*e).collect(),
        None => m.speech.clone(),
    });

    // Who speaks when. In person: every voice on the mic. On a call: the
    // voices on the system track, and anyone in the room with the user on
    // the mic. If it fails, the transcript goes ahead without speakers.
    let mut margins: Vec<(Source, Vec<f32>)> = Vec::new();
    let mut diarize = |source: Source, wav: &Path, speech: &[bool], print: Option<&[f32]>| {
        let hints = match source {
            Source::System => super::speakers::name_hints(dir, speech.len()),
            Source::Mic => Vec::new(),
        };
        let v = super::diarize::speakers_with(wav, speech, speaker_model?, print, &hints)
            .inspect_err(|e| log::warn!("Couldn't tell the speakers apart: {e}"))
            .ok()?;
        margins.push((source, v.margins));
        Some(v.labels)
    };
    let (mic_speakers, system_speakers) = if speaker_model.is_some() {
        progress(Step::Identifying);
        let mic_speakers = match (&mic_speech, mode) {
            (Some(speech), MeetingMode::InPerson) => {
                diarize(Source::Mic, &mic_wav, speech, voiceprint)
            }
            (Some(speech), MeetingMode::Call) => diarize(Source::Mic, &mic_wav, speech, None)
                .map(|labels| super::diarize::others_in_room(&labels)),
            _ => None,
        };
        let system_speakers = system.as_ref().and_then(|s| {
            diarize(Source::System, &system_wav, &s.speech, None)
                .map(|labels| super::diarize::offset(&labels, SYSTEM_SPEAKERS))
        });
        (mic_speakers, system_speakers)
    } else {
        (None, None)
    };

    // Every chunk of every track, in time order, cut where the speaker changes.
    let mut plan: Vec<(Source, Chunk, Option<u32>)> = Vec::new();
    let mut add = |source: Source, speech: &[bool], labels: &Option<Vec<Option<u32>>>| {
        for c in transcript::plan_chunks(speech) {
            match labels {
                Some(labels) => plan.extend(
                    super::diarize::split_by_speaker(c, labels)
                        .into_iter()
                        // On a call the user is the mic's unlabelled voice;
                        // in person, a voiceprint match stays labelled.
                        .map(|(c, s)| {
                            let me_unlabelled = mode == MeetingMode::Call;
                            (
                                source,
                                c,
                                s.filter(|&s| !(me_unlabelled && s == super::diarize::ME)),
                            )
                        }),
                ),
                None => plan.push((source, c, None)),
            }
        }
    };
    if let Some(speech) = &mic_speech {
        add(Source::Mic, speech, &mic_speakers);
    }
    if let Some(s) = &system {
        add(Source::System, &s.speech, &system_speakers);
    }
    plan.sort_by_key(|(source, c, _)| (c.start_ms, *source == Source::System));
    // How sure each chunk's voice is, for the transcript's "?" marks.
    if !margins.is_empty() {
        let sure: BTreeMap<String, f32> = plan
            .iter()
            .filter(|(_, _, s)| s.is_some())
            .filter_map(|(source, c, _)| {
                let m = &margins.iter().find(|(s, _)| s == source)?.1;
                let frames = m.get(
                    (c.start_ms / transcript::FRAME_MS) as usize
                        ..((c.end_ms / transcript::FRAME_MS) as usize).min(m.len()),
                )?;
                (!frames.is_empty()).then(|| {
                    (
                        format!("{}-{}", source.key(), c.start_ms),
                        frames.iter().sum::<f32>() / frames.len() as f32,
                    )
                })
            })
            .collect();
        let _ = super::summary::save_json(dir, VOICES_FILE, &sure);
    }

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
        let in_person_mic = source == Source::Mic && mode == MeetingMode::InPerson;
        let audio = match level {
            Some(settings) if in_person_mic && settings.auto => {
                // Run the gain over the lead-in too, so it starts settled.
                let from = chunk.start_ms.saturating_sub(super::level::WARMUP_MS);
                let lead_in = ((chunk.start_ms - from) * SAMPLE_RATE as u64 / 1000) as usize;
                let mut audio = read_chunk(
                    &dir.join(source.file()),
                    Chunk {
                        start_ms: from,
                        end_ms: chunk.end_ms,
                    },
                    silence,
                )?;
                let boost = crate::audio_toolkit::audio::db_to_linear(settings.boost_db);
                if (boost - 1.0).abs() > 1e-3 {
                    audio.iter_mut().for_each(|s| *s *= boost);
                }
                super::level::percentile_gain(&mut audio);
                audio.split_off(lead_in.min(audio.len()))
            }
            _ => {
                let mut audio = read_chunk(&dir.join(source.file()), chunk, silence)?;
                if let (true, Some(settings), Some(m)) = (in_person_mic, level, &mic) {
                    let start = (chunk.start_ms * SAMPLE_RATE as u64 / 1000) as usize;
                    super::level::level(&mut audio, start, &m.speech, settings);
                }
                audio
            }
        };
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
        let mask = echo_mask(&mic, &system, &system_speech, None);
        assert!(
            mask[30..85].iter().all(|&m| m),
            "echo-only frames are masked"
        );
        assert!(mask[90..110].iter().all(|&m| !m), "double-talk is kept");
        assert!(mask[150..170].iter().all(|&m| !m), "the user alone is kept");
        // No system audio at all (headphones, in person): nothing is masked.
        assert!(echo_mask(&mic, &[0.0; 200], &[false; 200], None)
            .iter()
            .all(|&m| !m));
        // Correlation found no echo (headphones): nothing is masked.
        let none = super::super::echo::EchoReport::none(200);
        assert!(echo_mask(&mic, &system, &system_speech, Some(&none))
            .iter()
            .all(|&m| !m));
        // Only frames where echo was confirmed can be masked.
        let mut some = super::super::echo::EchoReport::none(200);
        some.lag_ms = Some(60.0);
        for f in some.frames.iter_mut().take(60) {
            *f = true;
        }
        let mask = echo_mask(&mic, &system, &system_speech, Some(&some));
        assert!(mask[30..58].iter().all(|&m| m));
        assert!(mask[62..85].iter().all(|&m| !m));
    }
}
