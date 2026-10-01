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
/// Paragraphs whose speaker is unsure, by paragraph key, with why.
pub const DOUBTS_FILE: &str = "speakers.json";
/// A chunk's voice margin below this is "close" (see `diarize::Voices`).
const CLOSE_MARGIN: f32 = 0.05;
/// …and at or below this, two people talked at once.
const OVERLAP_MARGIN: f32 = -0.1;

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
    /// Something said points at (or, `NotClue`, away from) this person.
    Clue,
    NotClue,
    /// On the call (the extension's participant list), not necessarily talking.
    Participant,
    /// Invited to the calendar event.
    Invited,
    /// The user's own name (on the invite); never one of the voices.
    Myself,
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

/// Everything known about a meeting that hints at who spoke.
#[derive(Default)]
pub struct Sources {
    /// The call app's active-speaker marks (accessibility tree).
    pub seen: Vec<Seen>,
    /// What the meetings extension heard.
    pub heard: Vec<super::extension::Logged>,
    pub clues: Vec<super::clues::Clue>,
    pub invite: Option<super::calendar::Invite>,
    /// The user's fixes, by paragraph key.
    pub fixes: BTreeMap<String, u32>,
}

impl Sources {
    pub fn load(dir: &Path) -> Self {
        Sources {
            seen: active_speaker::load(dir),
            heard: super::extension::load(dir),
            clues: super::summary::load_json(dir, super::clues::FILE).unwrap_or_default(),
            invite: super::calendar::load(dir),
            fixes: super::manager::user_speaker_fixes(dir),
        }
    }
}

fn name_evidence(name: &str, from: Kind) -> Evidence {
    Evidence {
        track: Source::System,
        start_ms: 0,
        end_ms: 0,
        who: Who::Name(name.to_string()),
        strength: 0.0,
        from,
    }
}

/// The evidence for a transcribed meeting.
pub fn gather(t: &Transcript, src: &Sources) -> Vec<Evidence> {
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
    out.extend(src.seen.iter().map(|s| Evidence {
        track: Source::System,
        start_ms: s.at_ms,
        end_ms: s.at_ms + active_speaker::TTL_MS,
        who: Who::Name(s.name.clone()),
        strength: ACTIVE_SPEAKER,
        from: Kind::ActiveSpeaker,
    }));
    out.extend(from_extension(&t.segments, &src.heard));
    out.extend(src.clues.iter().map(|c| Evidence {
        track: c.source,
        start_ms: c.start_ms,
        end_ms: c.end_ms,
        who: Who::Name(c.name.clone()),
        strength: c.strength,
        from: if c.is { Kind::Clue } else { Kind::NotClue },
    }));
    if let Some(invite) = &src.invite {
        out.extend(
            invite
                .attendees
                .iter()
                .map(|n| name_evidence(n, Kind::Invited)),
        );
        out.extend(invite.me.iter().map(|n| name_evidence(n, Kind::Myself)));
    }
    for p in super::transcript::fixed_paragraphs(&t.segments, &src.fixes) {
        if let Some(&s) = src.fixes.get(&super::summary::paragraph_key(&p)) {
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
            Message::Participants { names, .. } => {
                for n in names {
                    if !out.iter().any(|e: &Evidence| {
                        e.from == Kind::Participant && e.who == Who::Name(n.clone())
                    }) {
                        out.push(name_evidence(n, Kind::Participant));
                    }
                }
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

/// A voice is named from clues alone once they add up to this.
const CLUES_NAME: f32 = 0.6;
/// Only voices that talk this long are named by elimination.
const ELIMINATE_MS: u64 = 20_000;

/// Names for the voices, by speaker number. May give the system track's
/// segments their speakers (when the call app's names have to split it),
/// in which case the transcript needs saving. `user` are the names the user
/// gave voices; they're kept and their names aren't given to anyone else.
pub fn resolve(
    segments: &mut [Segment],
    evidence: &[Evidence],
    user: &BTreeMap<u32, String>,
) -> BTreeMap<u32, String> {
    let mut names = call_app_names(segments, evidence);
    finish(segments, evidence, user, &mut names, &Remembered::default());
    names
}

/// What remembered voices say about a meeting's voices.
#[derive(Default)]
pub struct Remembered {
    /// Voices matching a remembered voice someone named.
    pub named: BTreeMap<u32, String>,
    /// "Unknown voice N" for voices heard before but never named.
    pub unknown: BTreeMap<u32, String>,
}

/// The names the call app (or the extension) put on voices.
pub fn call_app_names(segments: &mut [Segment], evidence: &[Evidence]) -> BTreeMap<u32, String> {
    // The extension sees the page itself; when it was there, the
    // accessibility tree's guesses are left out.
    let has_extension = evidence
        .iter()
        .any(|e| matches!(e.from, Kind::Extension | Kind::Caption));
    let marks = |e: &&Evidence| match e.from {
        Kind::ActiveSpeaker => !has_extension,
        Kind::Extension | Kind::Caption => true,
        _ => false,
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

/// After the call app: remembered voices, then clues and elimination, then
/// "Unknown voice N" for the rest that were heard before.
fn finish(
    segments: &[Segment],
    evidence: &[Evidence],
    user: &BTreeMap<u32, String>,
    names: &mut BTreeMap<u32, String>,
    remembered: &Remembered,
) -> Vec<u32> {
    for (v, n) in &remembered.named {
        let taken = names.values().chain(user.values()).any(|t| same_name(t, n));
        if !names.contains_key(v) && !user.contains_key(v) && !taken {
            names.insert(*v, n.clone());
        }
    }
    let before: Vec<u32> = names.keys().copied().collect();
    by_clues_and_elimination(segments, evidence, user, names);
    let guessed: Vec<u32> = names
        .keys()
        .filter(|v| !before.contains(v))
        .copied()
        .collect();
    for (v, n) in &remembered.unknown {
        if !names.contains_key(v) && !user.contains_key(v) {
            names.insert(*v, n.clone());
        }
    }
    guessed
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// A clue's name as the meeting knows it: "Sam" is "Sam Rivera" when he's
/// the only Sam on the call or the invite.
fn full_name(name: &str, known: &[String]) -> String {
    let first = |n: &str| n.split_whitespace().next().unwrap_or("").to_lowercase();
    let matches: Vec<&String> = known
        .iter()
        .filter(|k| {
            same_name(k, name) || (!name.contains(' ') && first(k) == name.trim().to_lowercase())
        })
        .collect();
    match matches.as_slice() {
        [one] => (*one).clone(),
        _ => name.trim().to_string(),
    }
}

/// The voice that talks most in a stretch of a track.
fn voice_at(segments: &[Segment], track: Source, start_ms: u64, end_ms: u64) -> Option<u32> {
    let mut overlap: BTreeMap<u32, u64> = BTreeMap::new();
    for s in segments.iter().filter(|s| s.source == track && !s.echo) {
        let Some(v) = s.speaker else { continue };
        let o = s
            .end_ms
            .min(end_ms)
            .saturating_sub(s.start_ms.max(start_ms));
        if o > 0 {
            *overlap.entry(v).or_default() += o;
        }
    }
    overlap.into_iter().max_by_key(|(_, o)| *o).map(|(v, _)| v)
}

/// Voices still without a name: first from what was said ("I'm Priya"),
/// then by elimination when one voice and one person are left over.
fn by_clues_and_elimination(
    segments: &[Segment],
    evidence: &[Evidence],
    user: &BTreeMap<u32, String>,
    names: &mut BTreeMap<u32, String>,
) {
    let name_of = |e: &Evidence| match &e.who {
        Who::Name(n) => Some(n.clone()),
        Who::Voice(_) => None,
    };
    let of_kind = |kinds: &[Kind]| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for n in evidence
            .iter()
            .filter(|e| kinds.contains(&e.from))
            .filter_map(name_of)
        {
            if !out.iter().any(|o| same_name(o, &n)) {
                out.push(n);
            }
        }
        out
    };
    let known = of_kind(&[
        Kind::Participant,
        Kind::Invited,
        Kind::Extension,
        Kind::Caption,
        Kind::ActiveSpeaker,
    ]);
    let me = of_kind(&[Kind::Myself]);

    let mut talk: BTreeMap<u32, u64> = BTreeMap::new();
    for s in segments.iter().filter(|s| !s.echo) {
        if let Some(v) = s.speaker.filter(|&v| v != super::diarize::ME) {
            *talk.entry(v).or_default() += s.end_ms.saturating_sub(s.start_ms);
        }
    }
    let mut taken: Vec<String> = names
        .values()
        .chain(user.values())
        .cloned()
        .chain(me.iter().cloned())
        .collect();
    let is_taken = |taken: &[String], n: &str| taken.iter().any(|t| same_name(t, n));
    let named =
        |names: &BTreeMap<u32, String>, v: &u32| names.contains_key(v) || user.contains_key(v);

    // Clues, per voice.
    let mut score: BTreeMap<u32, BTreeMap<String, f32>> = BTreeMap::new();
    let mut not: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for e in evidence
        .iter()
        .filter(|e| matches!(e.from, Kind::Clue | Kind::NotClue))
    {
        let (Some(n), Some(v)) = (
            name_of(e),
            voice_at(segments, e.track, e.start_ms, e.end_ms),
        ) else {
            continue;
        };
        let n = full_name(&n, &known);
        let entry = score.entry(v).or_default().entry(n.clone()).or_default();
        if e.from == Kind::Clue {
            *entry += e.strength;
        } else {
            *entry -= e.strength;
            not.entry(v).or_default().push(n);
        }
    }
    let mut best: Vec<(u32, String, f32)> = score
        .iter()
        .filter_map(|(v, s)| {
            let (n, w) = s.iter().max_by(|a, b| a.1.total_cmp(b.1))?;
            Some((*v, n.clone(), *w))
        })
        .collect();
    best.sort_by(|a, b| b.2.total_cmp(&a.2));
    for (v, n, w) in best {
        if w >= CLUES_NAME && !named(names, &v) && !is_taken(&taken, &n) {
            taken.push(n.clone());
            names.insert(v, n);
        }
    }

    // Elimination: one voice that talks and one person left.
    let mut candidates: Vec<String> = known.clone();
    for s in score.values() {
        for n in s.keys() {
            if !candidates.iter().any(|c| same_name(c, n)) {
                candidates.push(n.clone());
            }
        }
    }
    let open: Vec<u32> = talk
        .iter()
        .filter(|(v, ms)| **ms >= ELIMINATE_MS && !named(names, v))
        .map(|(v, _)| *v)
        .collect();
    if let [v] = open.as_slice() {
        let left: Vec<&String> = candidates
            .iter()
            .filter(|c| !is_taken(&taken, c))
            .filter(|c| {
                !not.get(v)
                    .is_some_and(|n| n.iter().any(|x| same_name(x, c)))
            })
            .collect();
        if let [only] = left.as_slice() {
            names.insert(*v, (*only).clone());
        }
    }
}

/// The name marked as talking in each frame of the system track, for
/// telling voices apart: the extension's marks when it was there, otherwise
/// the call app's. Frames where two names overlap have none.
pub fn name_hints(dir: &Path, frames: usize) -> Vec<Option<String>> {
    use super::extension::Message;
    let frame = super::transcript::FRAME_MS;
    let mut marks: Vec<(u64, String)> = super::extension::load(dir)
        .into_iter()
        .filter_map(|h| match h.message {
            Message::Speaking { names, .. } if names.len() == 1 => {
                Some((h.at_ms, names.into_iter().next()?))
            }
            _ => None,
        })
        .collect();
    if marks.is_empty() {
        marks = active_speaker::load(dir)
            .into_iter()
            .map(|s| (s.at_ms, s.name))
            .collect();
    }
    let mut out: Vec<Option<String>> = vec![None; frames];
    let mut clash = vec![false; frames];
    for (at, name) in marks {
        let from = (at / frame) as usize;
        let to = (((at + active_speaker::TTL_MS) / frame) as usize).min(frames);
        for i in from.min(frames)..to {
            match &out[i] {
                Some(n) if *n != name => clash[i] = true,
                _ => out[i] = Some(name.clone()),
            }
        }
    }
    for (o, c) in out.iter_mut().zip(clash) {
        if c {
            *o = None;
        }
    }
    out
}

/// Which paragraphs' speakers are unsure, and why. Paragraphs the user
/// fixed and voices the user named aren't.
pub fn doubts(
    dir: &Path,
    segments: &[Segment],
    user: &BTreeMap<u32, String>,
    guessed: &[u32],
) -> BTreeMap<String, String> {
    let margins: BTreeMap<String, f32> =
        super::summary::load_json(dir, super::pipeline::VOICES_FILE).unwrap_or_default();
    let turns: BTreeMap<String, u32> =
        super::summary::load_json(dir, super::clues::TURNS_FILE).unwrap_or_default();
    let mine = super::manager::user_speaker_fixes(dir);
    let mut out = BTreeMap::new();
    for p in super::transcript::fixed_paragraphs(segments, &super::manager::speaker_fixes(dir)) {
        let key = super::summary::paragraph_key(&p);
        let Some(v) = p.speaker else { continue };
        if mine.contains_key(&key) {
            continue;
        }
        if turns.contains_key(&key) {
            out.insert(key, "turn".to_string());
            continue;
        }
        if user.contains_key(&v) {
            continue;
        }
        let worst = segments
            .iter()
            .filter(|s| s.source == p.source && s.start_ms >= p.start_ms && s.start_ms < p.end_ms)
            .filter_map(|s| margins.get(&super::transcript::segment_key(s)))
            .fold(f32::INFINITY, |a, &b| a.min(b));
        let why = if worst <= OVERLAP_MARGIN {
            "overlap"
        } else if worst < CLOSE_MARGIN {
            "close"
        } else if guessed.contains(&v) {
            "guessed"
        } else {
            continue;
        };
        out.insert(key, why.to_string());
    }
    out
}

/// Write `evidence.jsonl` again from what the meeting has now (after
/// transcription, or when the user fixes a speaker).
pub fn record(dir: &Path, t: &Transcript) -> Vec<Evidence> {
    let evidence = gather(t, &Sources::load(dir));
    if let Err(e) = save(dir, &evidence) {
        log::warn!("{e}");
    }
    evidence
}

/// After transcription (and again once clues are found): record the
/// evidence and work out the names. Writes the transcript back if the
/// names split the system track.
pub fn apply(dir: &Path) -> BTreeMap<u32, String> {
    let Some(mut t) = super::pipeline::load(dir) else {
        return BTreeMap::new();
    };
    let info = super::manager::read_info(dir);
    if let Some(i) = &info {
        let end = i.ended_at.unwrap_or(i.started_at + 60 * 60 * 1000);
        super::calendar::for_meeting(dir, i.started_at, end);
    }
    let user = info.map(|i| i.speakers).unwrap_or_default();
    let evidence = record(dir, &t);
    let before = t.segments.clone();
    let mut names = call_app_names(&mut t.segments, &evidence);
    let (named, unknown) = super::remembered::apply(dir, &user, &names);
    let guessed = finish(
        &t.segments,
        &evidence,
        &user,
        &mut names,
        &Remembered { named, unknown },
    );
    let doubts = doubts(dir, &t.segments, &user, &guessed);
    let _ = super::summary::save_json(dir, DOUBTS_FILE, &doubts);
    // Saved only when the names took over the track, as before.
    if !names.is_empty() && t.segments != before {
        if let Err(e) = super::pipeline::save(dir, &t) {
            log::warn!("Couldn't save the transcript with names: {e}");
            return BTreeMap::new();
        }
    }
    names
}

/// The names the user gave, by stretch of a track, kept while a meeting is
/// transcribed again: the new transcript numbers its voices afresh, so
/// names kept by number would land on the wrong people.
const NAMES_BEFORE_FILE: &str = "names_before.json";

#[derive(Serialize, Deserialize)]
struct NamedStretch {
    source: Source,
    start_ms: u64,
    end_ms: u64,
    name: String,
}

/// Before transcribing a meeting again: keep who the user said spoke when,
/// and drop what's tied to the old voice numbers (the user's fixes and
/// the links to remembered voices).
pub fn keep_names(dir: &Path, names: &BTreeMap<u32, String>) {
    if let Some(t) = super::pipeline::load(dir) {
        let stretches: Vec<NamedStretch> =
            super::transcript::fixed_paragraphs(&t.segments, &super::manager::speaker_fixes(dir))
                .into_iter()
                .filter_map(|p| {
                    let name = names.get(&p.speaker?)?.clone();
                    Some(NamedStretch {
                        source: p.source,
                        start_ms: p.start_ms,
                        end_ms: p.end_ms,
                        name,
                    })
                })
                .collect();
        if let Err(e) = super::summary::save_json(dir, NAMES_BEFORE_FILE, &stretches) {
            log::warn!("{e}");
            return;
        }
    }
    for file in [
        super::manager::SPEAKER_FIXES_FILE,
        super::remembered::LINKS_FILE,
    ] {
        let _ = std::fs::remove_file(dir.join(file));
    }
}

/// After transcribing again: the names for the new voices, each the name
/// over more than half of that voice's talk, or None if nothing was kept.
pub fn carry_names(dir: &Path, t: &Transcript) -> Option<BTreeMap<u32, String>> {
    let stretches: Vec<NamedStretch> = super::summary::load_json(dir, NAMES_BEFORE_FILE)?;
    let _ = std::fs::remove_file(dir.join(NAMES_BEFORE_FILE));
    Some(names_by_overlap(&t.segments, &stretches))
}

fn names_by_overlap(segments: &[Segment], stretches: &[NamedStretch]) -> BTreeMap<u32, String> {
    let mut total: BTreeMap<u32, u64> = BTreeMap::new();
    let mut by_name: BTreeMap<u32, BTreeMap<&str, u64>> = BTreeMap::new();
    for s in segments.iter().filter(|s| !s.echo) {
        let Some(v) = s.speaker.filter(|v| *v != super::diarize::ME) else {
            continue;
        };
        *total.entry(v).or_default() += s.end_ms.saturating_sub(s.start_ms);
        for n in stretches.iter().filter(|n| n.source == s.source) {
            let both = s
                .end_ms
                .min(n.end_ms)
                .saturating_sub(s.start_ms.max(n.start_ms));
            if both > 0 {
                *by_name.entry(v).or_default().entry(&n.name).or_default() += both;
            }
        }
    }
    by_name
        .into_iter()
        .filter_map(|(v, names)| {
            let (name, ms) = names.into_iter().max_by_key(|(_, ms)| *ms)?;
            (ms * 2 > total.get(&v).copied().unwrap_or(0)).then(|| (v, name.to_string()))
        })
        .collect()
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
        let evidence = gather(
            &t,
            &Sources {
                seen: sightings.clone(),
                ..Default::default()
            },
        );
        let mut via_evidence = segments.clone();
        let mut direct = segments.clone();
        assert_eq!(
            resolve(&mut via_evidence, &evidence, &BTreeMap::new()),
            active_speaker::names_for(&mut direct, &sightings)
        );
        assert_eq!(via_evidence, direct);
        assert_eq!(
            resolve(&mut via_evidence, &evidence, &BTreeMap::new())
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
        let evidence = gather(
            &t,
            &Sources {
                seen: sightings.clone(),
                ..Default::default()
            },
        );
        let (mut a, mut b) = (one.clone(), one);
        assert_eq!(
            resolve(&mut a, &evidence, &BTreeMap::new()),
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
        let evidence = gather(
            &t,
            &Sources {
                seen: vec![seen(1, "Sam Rivera")],
                fixes,
                ..Default::default()
            },
        );
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
        let evidence = gather(
            &t,
            &Sources {
                seen: wrong,
                heard,
                ..Default::default()
            },
        );
        assert!(evidence
            .iter()
            .any(|e| e.from == Kind::Caption && e.start_ms == 10_000));
        let names = resolve(&mut t.segments, &evidence, &BTreeMap::new());
        assert_eq!(names.get(&100).map(String::as_str), Some("Sam Rivera"));
        assert_eq!(names.get(&101).map(String::as_str), Some("Priya"));
    }

    #[test]
    fn clues_name_voices_and_the_last_one_goes_by_elimination() {
        use crate::meetings::calendar::Invite;
        use crate::meetings::clues::Clue;
        // Three voices on the call, each talking 30 s; nobody marked by the
        // call app. Priya introduces herself; Sam and Jo are invited.
        let mut t = Transcript {
            segments: vec![
                seg(0, 30, Some(100)),
                seg(30, 60, Some(101)),
                seg(60, 90, Some(102)),
            ],
            ..Default::default()
        };
        let clue = |start_s: u64, name: &str, is: bool, strength: f32| Clue {
            source: Source::System,
            start_ms: start_s * 1000,
            end_ms: (start_s + 5) * 1000,
            name: name.into(),
            is,
            strength,
            quote: String::new(),
        };
        let src = Sources {
            clues: vec![
                clue(0, "Priya", true, 0.6),
                // "Sam, what do you think?" from 101 → 101 isn't Sam, 102 is.
                clue(30, "Sam", false, 0.5),
                clue(60, "Sam", true, 0.4),
            ],
            invite: Some(Invite {
                title: "Pricing review".into(),
                attendees: vec!["Sam Rivera".into(), "Jo Park".into(), "Priya".into()],
                me: Some("Kaspar".into()),
            }),
            ..Default::default()
        };
        let evidence = gather(&t, &src);
        let names = resolve(&mut t.segments, &evidence, &BTreeMap::new());
        assert_eq!(names.get(&100).map(String::as_str), Some("Priya"));
        // One clue at 0.4 isn't enough alone…
        assert_eq!(names.get(&102), None);
        // …so two voices are open and nothing is eliminated.
        assert_eq!(names.get(&101), None);

        // With 102 named by the user, 101 is the one voice left and Jo Park
        // the one invitee left (101 isn't Sam).
        let user = BTreeMap::from([(102, "Sam Rivera".to_string())]);
        let names = resolve(&mut t.segments, &evidence, &user);
        assert_eq!(names.get(&101).map(String::as_str), Some("Jo Park"));
    }

    #[test]
    fn names_follow_the_talk_to_the_new_voice_numbers() {
        let stretches = vec![
            NamedStretch {
                source: Source::System,
                start_ms: 0,
                end_ms: 10_000,
                name: "Aditya".into(),
            },
            NamedStretch {
                source: Source::System,
                start_ms: 10_000,
                end_ms: 14_000,
                name: "Sam Rivera".into(),
            },
        ];
        // Retranscribed: Aditya is 105 now; 106 is mostly unnamed talk.
        let segments = vec![
            seg(0, 9, Some(105)),
            seg(10, 12, Some(106)),
            seg(12, 20, Some(106)),
        ];
        let names = names_by_overlap(&segments, &stretches);
        assert_eq!(names.get(&105).map(String::as_str), Some("Aditya"));
        assert_eq!(names.get(&106), None);
    }
}
