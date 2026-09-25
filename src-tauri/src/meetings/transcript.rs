//! A meeting's transcript: timestamped segments from each track, the plan for
//! cutting a track into chunks at pauses, echo removal and the paragraphs the
//! Meetings page shows. Pure logic, no audio or model here.
//!
//! Timestamps are per chunk: each track is cut at pauses into chunks of up to
//! [`MAX_CHUNK_MS`], and each chunk becomes one segment. That works with any
//! model, including ones that return text without timestamps.

use serde::{Deserialize, Serialize};
use specta::Type;
use std::ops::Range;

pub const FILE: &str = "transcript.json";
/// Bumped if the chunking changes, so an unfinished transcript isn't resumed
/// with different chunks.
pub const VERSION: u32 = 2;

/// VAD frame length (Silero: 480 samples at 16 kHz).
pub const FRAME_MS: u64 = 30;
/// A pause at least this long ends a chunk.
const PAUSE_MS: u64 = 800;
/// Chunks never run longer than this: long enough to give the model context,
/// short enough for useful timestamps and for dictation never to wait long.
pub const MAX_CHUNK_MS: u64 = 15_000;
/// Chunks with less speech than this are noise (a cough, a click).
const MIN_SPEECH_MS: u64 = 300;
/// Audio kept either side of the speech, so first and last words aren't clipped.
const PAD_MS: u64 = 200;

/// Where a segment was heard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The user's mic: "Me" on a call.
    Mic,
    /// What the Mac played: "Them" on a call.
    System,
}

impl Source {
    pub fn file(self) -> &'static str {
        match self {
            Source::Mic => "mic.wav",
            Source::System => "system.wav",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
pub struct Segment {
    pub source: Source,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// The mic picking up the other side of a call (speakers, no headphones):
    /// kept on disk but left out of the transcript.
    #[serde(default)]
    pub echo: bool,
    /// Which voice it is, when speakers were told apart (in person).
    #[serde(default)]
    pub speaker: Option<u32>,
}

/// `transcript.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Type)]
pub struct Transcript {
    pub version: u32,
    /// All chunks are transcribed and echo is removed.
    pub complete: bool,
    /// What transcribed it ("local", "OpenAI gpt-transcribe", …).
    #[serde(default)]
    pub engine: String,
    pub segments: Vec<Segment>,
}

/// A run of one speaker's segments, as shown on the page.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct Paragraph {
    pub source: Source,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Cleaned up when cleanup ran, otherwise as transcribed.
    pub text: String,
    /// As transcribed, when cleanup changed it.
    pub raw: Option<String>,
    /// Which voice, when speakers were told apart.
    pub speaker: Option<u32>,
}

/// A stretch of a track to transcribe, in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Cut a track into chunks from its per-frame speech flags (one per
/// [`FRAME_MS`]): a chunk ends at a pause of [`PAUSE_MS`], or at the last
/// pause before it would pass [`MAX_CHUNK_MS`]; unbroken speech longer than
/// that is cut hard.
pub fn plan_chunks(speech: &[bool]) -> Vec<Chunk> {
    let frames = |ms: u64| (ms / FRAME_MS) as usize;
    let (pause, max, min_speech) = (
        frames(PAUSE_MS),
        frames(MAX_CHUNK_MS),
        frames(MIN_SPEECH_MS),
    );

    // Runs of speech frames.
    let mut runs: Vec<Range<usize>> = Vec::new();
    let mut start = None;
    for (i, &s) in speech.iter().chain(std::iter::once(&false)).enumerate() {
        match (s, start) {
            (true, None) => start = Some(i),
            (false, Some(st)) => {
                // Unbroken speech longer than a chunk: cut it into pieces.
                let mut st = st;
                while i - st > max {
                    runs.push(st..st + max);
                    st += max;
                }
                runs.push(st..i);
                start = None;
            }
            _ => {}
        }
    }

    // Group runs into chunks.
    let mut chunks: Vec<(Range<usize>, usize)> = Vec::new();
    for run in runs {
        let len = run.len();
        match chunks.last_mut() {
            Some((cur, voiced)) if run.start - cur.end < pause && run.end - cur.start <= max => {
                cur.end = run.end;
                *voiced += len;
            }
            _ => chunks.push((run, len)),
        }
    }

    let total_ms = speech.len() as u64 * FRAME_MS;
    chunks
        .into_iter()
        .filter(|(_, voiced)| *voiced >= min_speech)
        .map(|(r, _)| Chunk {
            start_ms: (r.start as u64 * FRAME_MS).saturating_sub(PAD_MS),
            end_ms: (r.end as u64 * FRAME_MS + PAD_MS).min(total_ms),
        })
        .collect()
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// How much of `a` also appears in `b`: the share of `a`'s words found in `b`
/// in order (longest common subsequence), 0 to 1.
fn contained_in(a: &[String], b: &[String]) -> f32 {
    if a.is_empty() {
        return 0.0;
    }
    let mut prev = vec![0usize; b.len() + 1];
    for wa in a {
        let mut cur = vec![0usize; b.len() + 1];
        for (j, wb) in b.iter().enumerate() {
            cur[j + 1] = if wa == wb {
                prev[j] + 1
            } else {
                cur[j].max(prev[j + 1])
            };
        }
        prev = cur;
    }
    prev[b.len()] as f32 / a.len() as f32
}

/// Timing slack when matching echo: the two tracks' chunks don't start and
/// end at the same moments.
const ECHO_SLACK_MS: u64 = 1_500;
/// Share of a mic segment's words that must also be in what the Mac played
/// around then for it to count as echo.
const ECHO_MATCH: f32 = 0.6;

/// Flag mic segments that repeat what the Mac played at the same time: the
/// call coming out of the speakers and back into the mic.
pub fn mark_echo(segments: &mut [Segment]) {
    let system: Vec<(u64, u64, Vec<String>)> = segments
        .iter()
        .filter(|s| s.source == Source::System)
        .map(|s| (s.start_ms, s.end_ms, words(&s.text)))
        .collect();
    for seg in segments.iter_mut().filter(|s| s.source == Source::Mic) {
        let mine = words(&seg.text);
        if mine.is_empty() {
            continue;
        }
        let heard: Vec<String> = system
            .iter()
            .filter(|(start, end, _)| {
                *start < seg.end_ms + ECHO_SLACK_MS && seg.start_ms < end + ECHO_SLACK_MS
            })
            .flat_map(|(_, _, w)| w.iter().cloned())
            .collect();
        seg.echo = contained_in(&mine, &heard) >= ECHO_MATCH;
    }
}

/// A pause this long starts a new paragraph.
const PARAGRAPH_PAUSE_MS: u64 = 2_000;
/// Paragraphs are split after about this long, so timestamps stay useful.
const PARAGRAPH_MAX_MS: u64 = 60_000;

/// Merge the segments into paragraphs in time order: consecutive segments
/// from the same source join unless there's a long pause or the paragraph is
/// already long. Echo and empty segments are left out.
pub fn paragraphs(segments: &[Segment]) -> Vec<Paragraph> {
    let mut segs: Vec<&Segment> = segments
        .iter()
        .filter(|s| !s.echo && !s.text.trim().is_empty())
        .collect();
    segs.sort_by_key(|s| (s.start_ms, s.source == Source::System));
    let mut out: Vec<Paragraph> = Vec::new();
    for s in segs {
        let text = s.text.trim();
        match out.last_mut() {
            Some(p)
                if p.source == s.source
                    && p.speaker == s.speaker
                    && s.start_ms < p.end_ms + PARAGRAPH_PAUSE_MS
                    && s.end_ms - p.start_ms <= PARAGRAPH_MAX_MS =>
            {
                p.text.push(' ');
                p.text.push_str(text);
                p.end_ms = p.end_ms.max(s.end_ms);
            }
            _ => out.push(Paragraph {
                source: s.source,
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                text: text.to_string(),
                raw: None,
                speaker: s.speaker,
            }),
        }
    }
    out
}

/// `mm:ss`, or `h:mm:ss` from an hour.
pub fn timestamp(ms: u64) -> String {
    super::manager::format_elapsed(std::time::Duration::from_millis(ms))
}

/// The transcript as plain text, one paragraph per line: `[m:ss] Me: …`.
/// `label` names each paragraph's speaker (e.g. "Me"/"Them"; `None`
/// leaves it out).
pub fn to_text(paragraphs: &[Paragraph], label: impl Fn(&Paragraph) -> Option<String>) -> String {
    paragraphs
        .iter()
        .map(|p| match label(p) {
            Some(who) => format!("[{}] {who}: {}", timestamp(p.start_ms), p.text),
            None => format!("[{}] {}", timestamp(p.start_ms), p.text),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Speech flags from (speech?, milliseconds) spans.
    fn flags(spans: &[(bool, u64)]) -> Vec<bool> {
        spans
            .iter()
            .flat_map(|&(s, ms)| std::iter::repeat_n(s, (ms / FRAME_MS) as usize))
            .collect()
    }

    fn seg(source: Source, start_s: u64, end_s: u64, text: &str) -> Segment {
        Segment {
            source,
            start_ms: start_s * 1000,
            end_ms: end_s * 1000,
            text: text.into(),
            echo: false,
            speaker: None,
        }
    }

    #[test]
    fn chunks_end_at_pauses_and_short_gaps_stay_together() {
        let speech = flags(&[
            (false, 990),
            (true, 2010),
            (false, 300), // a breath: same chunk
            (true, 990),
            (false, 1500), // a pause: new chunk
            (true, 990),
            (false, 990),
        ]);
        let chunks = plan_chunks(&speech);
        assert_eq!(
            chunks,
            vec![
                Chunk {
                    start_ms: 790,
                    end_ms: 4490
                },
                Chunk {
                    start_ms: 5590,
                    end_ms: 6980
                },
            ]
        );
    }

    #[test]
    fn long_speech_is_cut_at_a_pause_before_the_limit_or_hard() {
        // 10 s, a breath, 10 s: too long together, so cut at the breath.
        let speech = flags(&[(true, 9990), (false, 300), (true, 9990)]);
        let chunks = plan_chunks(&speech);
        assert_eq!(chunks.len(), 2);
        assert!(chunks
            .iter()
            .all(|c| c.end_ms - c.start_ms <= MAX_CHUNK_MS + 2 * PAD_MS));
        // 40 s without a gap: hard cuts.
        let chunks = plan_chunks(&flags(&[(true, 39990)]));
        assert_eq!(chunks.len(), 3);
        assert!(chunks
            .iter()
            .all(|c| c.end_ms - c.start_ms <= MAX_CHUNK_MS + 2 * PAD_MS));
    }

    #[test]
    fn clicks_and_silence_make_no_chunks() {
        assert!(plan_chunks(&flags(&[(false, 5010)])).is_empty());
        assert!(plan_chunks(&flags(&[(false, 990), (true, 90), (false, 990)])).is_empty());
    }

    #[test]
    fn echo_of_the_call_is_dropped_but_my_own_words_stay() {
        let mut segments = vec![
            seg(
                Source::System,
                10,
                14,
                "Can you send the deck to Sam by Friday?",
            ),
            // The mic heard the same, a little late and garbled.
            seg(Source::Mic, 11, 15, "can you send the deck to sam friday"),
            // My answer, while they were still talking.
            seg(Source::Mic, 14, 16, "Yes, I'll send it tomorrow."),
            // Same words much later aren't echo.
            seg(
                Source::Mic,
                60,
                63,
                "can you send the deck to sam by friday",
            ),
        ];
        mark_echo(&mut segments);
        let echo: Vec<bool> = segments.iter().map(|s| s.echo).collect();
        assert_eq!(echo, vec![false, true, false, false]);
    }

    #[test]
    fn paragraphs_join_one_speaker_and_break_on_turns_and_pauses() {
        let segments = vec![
            seg(Source::Mic, 0, 5, "Hi everyone."),
            seg(Source::Mic, 6, 9, "Let's start."),
            seg(Source::System, 9, 12, "Sounds good."),
            seg(Source::Mic, 12, 14, ""),
            seg(Source::Mic, 20, 22, "Next item."),
        ];
        let p = paragraphs(&segments);
        let got: Vec<(Source, u64, &str)> = p
            .iter()
            .map(|p| (p.source, p.start_ms, p.text.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (Source::Mic, 0, "Hi everyone. Let's start."),
                (Source::System, 9000, "Sounds good."),
                (Source::Mic, 20000, "Next item."),
            ]
        );
        let text = to_text(&p, |p| {
            Some(
                if p.source == Source::Mic {
                    "Me"
                } else {
                    "Them"
                }
                .into(),
            )
        });
        assert_eq!(
            text,
            "[0:00] Me: Hi everyone. Let's start.\n[0:09] Them: Sounds good.\n[0:20] Me: Next item."
        );
    }
}
