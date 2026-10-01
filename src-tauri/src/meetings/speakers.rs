//! Who spoke each part of a meeting, decided in one place from everything
//! that hints at it. Each source (voice clustering, the call app's
//! active-speaker marks, the user's fixes; later the Chrome extension,
//! transcript clues and voiceprints) writes evidence: a stretch of a track
//! with a voice or a name and a strength. [`resolve`] turns the evidence
//! into names for the voices. The evidence is kept in `evidence.jsonl`, so
//! names can be worked out again without the audio.
//!
//! The resolver votes names onto voices as the active-speaker step did (see
//! [`super::active_speaker::names_for`]); the meetings extension's marks
//! replace the accessibility tree's when it was there, and a caption counts
//! as several marks on the segment it matches.

use super::active_speaker::{self, Seen};
use super::transcript::{Segment, Source, Transcript};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const FILE: &str = "evidence.jsonl";

/// Who a piece of evidence points at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Who {
    /// A person's name, as the call app or the user gave it.
    Name(String),
    /// An anonymous voice, by speaker number.
    Voice(u32),
}

/// Where a piece of evidence came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// Voice clustering: this stretch is this voice.
    Cluster,
    /// The call app marked this person as talking.
    ActiveSpeaker,
    /// The user said who spoke a paragraph.
    User,
    /// The meetings extension saw this person's tile light up.
    Extension,
    /// A caption line (extension, captions on) matched this segment's words.
    Caption,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub track: Source,
    pub start_ms: u64,
    pub end_ms: u64,
    pub who: Who,
    /// 0 to 1.
    pub strength: f32,
    pub from: Kind,
}

/// Strength of one active-speaker mark (one name seen as talking).
const ACTIVE_SPEAKER: f32 = 0.6;
/// The extension's speaking indicator: the page itself, not a guess from
/// the accessibility tree.
const EXTENSION_SPEAKING: f32 = 0.7;
/// A caption whose words match what was transcribed.
const CAPTION: f32 = 0.95;
/// A caption names a segment that shares this share of its words…
const CAPTION_MATCH: f32 = 0.5;
/// …and was said up to this long before the caption arrived.
const CAPTION_LAG_MS: u64 = 10_000;
/// Votes one caption is worth against single sightings.
const CAPTION_VOTES: usize = 3;

/// The evidence for a transcribed meeting: its voices, the call app's marks,
/// what the extension heard and the user's fixes (`fixes`, by paragraph key).
pub fn gather(
    t: &Transcript,
    seen: &[Seen],
    heard: &[super::extension::Logged],
    fixes: &BTreeMap<String, u32>,
) -> Vec<Evidence> {
    let mut out: Vec<Evidence> = t
        .segments
        .iter()
        .filter(|s| !s.echo)
        .filter_map(|s| {
            Some(Evidence {
                track: s.source,
                start_ms: s.start_ms,
                end_ms: s.end_ms,
                who: Who::Voice(s.speaker?),
                strength: 1.0,
                from: Kind::Cluster,
            })
        })
        .collect();
    out.extend(seen.iter().map(|s| Evidence {
        track: Source::System,
        start_ms: s.at_ms,
        end_ms: s.at_ms + active_speaker::TTL_MS,
        who: Who::Name(s.name.clone()),
        strength: ACTIVE_SPEAKER,
        from: Kind::ActiveSpeaker,
    }));
    out.extend(from_extension(&t.segments, heard));
    for p in super::transcript::paragraphs(&t.segments) {
        if let Some(&s) = fixes.get(&super::summary::paragraph_key(&p)) {
            out.push(Evidence {
                track: p.source,
                start_ms: p.start_ms,
                end_ms: p.end_ms,
                who: Who::Voice(s),
                strength: 1.0,
                from: Kind::User,
            });
        }
    }
    out
}

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

/// Share of the caption's words found in the segment.
fn shared_words(caption: &str, segment: &str) -> f32 {
    let c = words(caption);
    if c.is_empty() {
        return 0.0;
    }
    let s: std::collections::HashSet<String> = words(segment).into_iter().collect();
    c.iter().filter(|w| s.contains(*w)).count() as f32 / c.len() as f32
}

fn from_extension(segments: &[Segment], heard: &[super::extension::Logged]) -> Vec<Evidence> {
    use super::extension::Message;
    let mut out = Vec::new();
    for h in heard {
        match &h.message {
            Message::Speaking { names, .. } if names.len() <= 2 => {
                out.extend(names.iter().map(|name| Evidence {
                    track: Source::System,
                    start_ms: h.at_ms,
                    end_ms: h.at_ms + active_speaker::TTL_MS,
                    who: Who::Name(name.clone()),
                    strength: EXTENSION_SPEAKING,
                    from: Kind::Extension,
                }));
            }
            Message::Caption { name, text, .. } => {
                let best = segments
                    .iter()
                    .filter(|s| s.source == Source::System && !s.echo)
                    .filter(|s| s.start_ms <= h.at_ms && s.end_ms + CAPTION_LAG_MS >= h.at_ms)
                    .map(|s| (shared_words(text, &s.text), s))
                    .filter(|(share, _)| *share >= CAPTION_MATCH)
                    .max_by(|a, b| a.0.total_cmp(&b.0));
                if let Some((_, s)) = best {
                    out.push(Evidence {
                        track: Source::System,
                        start_ms: s.start_ms,
                        end_ms: s.end_ms,
                        who: Who::Name(name.clone()),
                        strength: CAPTION,
                        from: Kind::Caption,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

pub fn save(dir: &Path, evidence: &[Evidence]) -> Result<(), String> {
    let mut text = String::new();
    for e in evidence {
        text.push_str(&serde_json::to_string(e).map_err(|e| e.to_string())?);
        text.push('\n');
    }
    let tmp = dir.join(format!("{FILE}.tmp"));
    std::fs::write(&tmp, text)
        .and_then(|_| std::fs::rename(&tmp, dir.join(FILE)))
        .map_err(|e| format!("Couldn't save the speaker evidence: {e}"))
}

pub fn load(dir: &Path) -> Vec<Evidence> {
    std::fs::read_to_string(dir.join(FILE))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Names for the voices, by speaker number. May give the system track's
/// segments their speakers (when the call app's names have to split it),
/// in which case the transcript needs saving.
pub fn resolve(segments: &mut [Segment], evidence: &[Evidence]) -> BTreeMap<u32, String> {
    // The extension sees the page itself; when it was there, the
    // accessibility tree's guesses are left out.
    let has_extension = evidence
        .iter()
        .any(|e| matches!(e.from, Kind::Extension | Kind::Caption));
    let marks = |e: &&Evidence| match e.from {
        Kind::ActiveSpeaker => !has_extension,
        Kind::Extension | Kind::Caption => true,
        Kind::Cluster | Kind::User => false,
    };
    let mut seen: Vec<Seen> = Vec::new();
    for e in evidence.iter().filter(marks) {
        let Who::Name(name) = &e.who else { continue };
        let votes = if e.from == Kind::Caption {
            CAPTION_VOTES
        } else {
            1
        };
        for _ in 0..votes {
            seen.push(Seen {
                at_ms: e.start_ms,
                name: name.clone(),
            });
        }
    }
    seen.sort_by_key(|s| s.at_ms);
    active_speaker::names_for(segments, &seen)
}

/// Write `evidence.jsonl` again from what the meeting has now (after
/// transcription, or when the user fixes a speaker).
pub fn record(dir: &Path, t: &Transcript) -> Vec<Evidence> {
    let evidence = gather(
        t,
        &active_speaker::load(dir),
        &super::extension::load(dir),
        &super::manager::speaker_fixes(dir),
    );
    if let Err(e) = save(dir, &evidence) {
        log::warn!("{e}");
    }
    evidence
}

/// After transcription: record the evidence and work out the names. Writes
/// the transcript back if the names split the system track.
pub fn apply(dir: &Path) -> BTreeMap<u32, String> {
    let Some(mut t) = super::pipeline::load(dir) else {
        return BTreeMap::new();
    };
    let evidence = record(dir, &t);
    let before = t.segments.clone();
    let names = resolve(&mut t.segments, &evidence);
    // Saved only when the names took over the track, as before.
    if !names.is_empty() && t.segments != before {
        if let Err(e) = super::pipeline::save(dir, &t) {
            log::warn!("Couldn't save the transcript with names: {e}");
            return BTreeMap::new();
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start_s: u64, end_s: u64, speaker: Option<u32>) -> Segment {
        Segment {
            source: Source::System,
            start_ms: start_s * 1000,
            end_ms: end_s * 1000,
            text: "words".into(),
            echo: false,
            speaker,
        }
    }

    fn seen(at_s: u64, name: &str) -> Seen {
        Seen {
            at_ms: at_s * 1000,
            name: name.into(),
        }
    }

    /// The resolver, reading evidence, names voices as the active-speaker
    /// step did reading its sightings.
    #[test]
    fn evidence_gives_the_same_names_as_before() {
        let segments = vec![
            seg(0, 10, Some(100)),
            seg(10, 20, Some(101)),
            seg(20, 30, Some(100)),
        ];
        let sightings: Vec<Seen> = (0..10)
            .map(|i| seen(i, "Sam Rivera"))
            .chain((10..20).map(|i| seen(i, "Priya")))
            .chain((20..30).map(|i| seen(i, "Sam Rivera")))
            .collect();
        let t = Transcript {
            segments: segments.clone(),
            ..Default::default()
        };
        let evidence = gather(&t, &sightings, &[], &BTreeMap::new());
        let mut via_evidence = segments.clone();
        let mut direct = segments.clone();
        assert_eq!(
            resolve(&mut via_evidence, &evidence),
            active_speaker::names_for(&mut direct, &sightings)
        );
        assert_eq!(via_evidence, direct);
        assert_eq!(
            resolve(&mut via_evidence, &evidence)
                .get(&100)
                .map(String::as_str),
            Some("Sam Rivera")
        );

        // One voice but two names: the names split the track, the same way.
        let one: Vec<Segment> = segments
            .iter()
            .map(|s| seg(s.start_ms / 1000, s.end_ms / 1000, Some(100)))
            .collect();
        let t = Transcript {
            segments: one.clone(),
            ..Default::default()
        };
        let evidence = gather(&t, &sightings, &[], &BTreeMap::new());
        let (mut a, mut b) = (one.clone(), one);
        assert_eq!(
            resolve(&mut a, &evidence),
            active_speaker::names_for(&mut b, &sightings)
        );
        assert_eq!(a, b);
    }

    #[test]
    fn evidence_round_trips_and_records_the_users_fixes() {
        let t = Transcript {
            segments: vec![seg(0, 5, Some(100)), seg(10, 15, Some(100))],
            ..Default::default()
        };
        let fixes = BTreeMap::from([("system-10000".to_string(), 101)]);
        let evidence = gather(&t, &[seen(1, "Sam Rivera")], &[], &fixes);
        assert_eq!(
            evidence.iter().filter(|e| e.from == Kind::Cluster).count(),
            2
        );
        assert!(evidence
            .iter()
            .any(|e| e.from == Kind::User && e.who == Who::Voice(101) && e.start_ms == 10_000));
        let dir = std::env::temp_dir().join(format!("felix-evidence-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        save(&dir, &evidence).unwrap();
        assert_eq!(load(&dir), evidence);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_extension_outranks_the_accessibility_tree_and_captions_name_segments() {
        use crate::meetings::extension::{Logged, Message};
        let mut t = Transcript {
            segments: vec![seg(0, 10, Some(100)), seg(10, 20, Some(101))],
            ..Default::default()
        };
        t.segments[1].text = "shall we look at the pricing table".into();
        // The tree wrongly calls everything Priya; the extension's tiles and a
        // caption say otherwise.
        let wrong: Vec<Seen> = (0..20).map(|i| seen(i, "Priya")).collect();
        let speaking = |at_s: u64, name: &str| Logged {
            at_ms: at_s * 1000,
            message: Message::Speaking {
                app: "meet".into(),
                names: vec![name.into()],
            },
        };
        let mut heard: Vec<Logged> = (0..10).map(|i| speaking(i, "Sam Rivera")).collect();
        heard.push(Logged {
            at_ms: 21_000,
            message: Message::Caption {
                app: "meet".into(),
                name: "Priya".into(),
                text: "Shall we look at the pricing table?".into(),
            },
        });
        let evidence = gather(&t, &wrong, &heard, &BTreeMap::new());
        assert!(evidence
            .iter()
            .any(|e| e.from == Kind::Caption && e.start_ms == 10_000));
        let names = resolve(&mut t.segments, &evidence);
        assert_eq!(names.get(&100).map(String::as_str), Some("Sam Rivera"));
        assert_eq!(names.get(&101).map(String::as_str), Some("Priya"));
    }
}
