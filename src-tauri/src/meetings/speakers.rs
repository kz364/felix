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
    /// The conversation hands this line to the person ([`super::floor`]).
    Floor,
    /// On the call (the extension's participant list), not necessarily talking.
    Participant,
    /// Invited to the calendar event.
    Invited,
    /// Invited, known only by a one-word e-mail handle ("Peterlai").
    InvitedHandle,
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
/// A line the conversation hands to someone ("Joseph, you're up"): the
/// model read it closely and quoted the hand-over, but it can misjudge
/// where a turn ends.
const FLOOR: f32 = 0.8;

/// Everything known about a meeting that hints at who spoke.
#[derive(Default)]
pub struct Sources {
    /// The call app's active-speaker marks (accessibility tree).
    pub seen: Vec<Seen>,
    /// What the meetings extension heard.
    pub heard: Vec<super::extension::Logged>,
    pub clues: Vec<super::clues::Clue>,
    /// Lines named from the conversation, as said (see [`floor_turns`]).
    pub floor: Vec<super::floor::Turn>,
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
            floor: super::summary::load_json(dir, super::floor::FILE).unwrap_or_default(),
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
    out.extend(
        floor_turns(&src.floor, src.invite.as_ref())
            .into_iter()
            .map(|t| Evidence {
                track: t.source,
                start_ms: t.start_ms,
                end_ms: t.end_ms,
                who: Who::Name(t.name),
                strength: FLOOR,
                from: Kind::Floor,
            }),
    );
    if let Some(invite) = &src.invite {
        out.extend(invite.attendees.iter().map(|n| {
            let kind = if invite.handles.contains(n) {
                Kind::InvitedHandle
            } else {
                Kind::Invited
            };
            name_evidence(n, kind)
        }));
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
                // Meet's tooltips can come through as names ("Others might
                // still see your full video.").
                let names = names.iter().filter(|n| active_speaker::looks_like_name(n));
                out.extend(names.map(|name| Evidence {
                    track: Source::System,
                    start_ms: h.at_ms,
                    end_ms: h.at_ms + active_speaker::TTL_MS,
                    who: Who::Name(name.clone()),
                    strength: EXTENSION_SPEAKING,
                    from: Kind::Extension,
                }));
            }
            Message::Participants { names, .. } => {
                for n in names.iter().filter(|n| active_speaker::looks_like_name(n)) {
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
    let mut names = call_app_names(segments, evidence, &BTreeMap::new());
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

/// The names the call app (or the extension) put on voices. `prints` are
/// the voices' prints, which tell one person split into several voices
/// from a room of people on one account.
pub fn call_app_names(
    segments: &mut [Segment],
    evidence: &[Evidence],
    prints: &BTreeMap<u32, Vec<f32>>,
) -> BTreeMap<u32, String> {
    // The extension sees the page itself; when it was there, the
    // accessibility tree's guesses are left out.
    let has_extension = evidence
        .iter()
        .any(|e| matches!(e.from, Kind::Extension | Kind::Caption));
    let marks = |e: &&Evidence| match e.from {
        Kind::ActiveSpeaker => !has_extension,
        Kind::Extension | Kind::Caption | Kind::Floor => true,
        _ => false,
    };
    let mine = my_tile(segments, evidence);
    let mut seen: Vec<Seen> = Vec::new();
    for e in evidence.iter().filter(marks) {
        let Who::Name(name) = &e.who else { continue };
        if mine.as_deref() == Some(name.as_str()) && e.from != Kind::Floor {
            continue;
        }
        let votes = if matches!(e.from, Kind::Caption | Kind::Floor) {
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
    active_speaker::names_for(segments, &seen, prints)
}

/// The user's own tile lights up when they talk, and that's the mic, not a
/// voice on the call: their name in the call app, if one is. It's the name
/// marked talking mostly while the mic has speech, or their name on the
/// invite marked at least as often during the mic as during the call.
fn my_tile(segments: &[Segment], evidence: &[Evidence]) -> Option<String> {
    let me = evidence.iter().find_map(|e| match (&e.from, &e.who) {
        (Kind::Myself, Who::Name(n)) => Some(n.to_lowercase()),
        _ => None,
    });
    let talking = |track: Source, at: u64| {
        segments.iter().any(|s| {
            s.source == track
                && !s.echo
                && !s.text.trim().is_empty()
                && s.start_ms <= at
                && at <= s.end_ms
        })
    };
    let mut counts: BTreeMap<&str, (usize, usize, usize)> = BTreeMap::new();
    for e in evidence
        .iter()
        .filter(|e| matches!(e.from, Kind::Extension | Kind::ActiveSpeaker))
    {
        let Who::Name(name) = &e.who else { continue };
        let c = counts.entry(name.as_str()).or_default();
        c.0 += 1;
        c.1 += usize::from(talking(Source::Mic, e.start_ms));
        c.2 += usize::from(talking(Source::System, e.start_ms));
    }
    let is_me = |name: &str| {
        me.as_deref().is_some_and(|me| {
            let first = |n: &str| n.split_whitespace().next().unwrap_or("").to_lowercase();
            name.to_lowercase() == me || first(name) == first(me)
        })
    };
    counts
        .into_iter()
        .filter(|(name, (all, mic, system))| {
            (*all >= MY_TILE_MIN && *mic as f32 >= MY_TILE_SHARE * *all as f32 && mic > system)
                || (is_me(name) && mic >= system && *mic > 0)
        })
        .max_by_key(|(_, (_, mic, _))| *mic)
        .map(|(name, _)| name.to_string())
}

/// Marks it takes to tell the user's tile by when it lights up…
const MY_TILE_MIN: usize = 10;
/// …and the share of them while the mic has speech.
const MY_TILE_SHARE: f32 = 0.5;

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

/// The user's own voice, when it's labelled (in person), always has their
/// name: as set, or else from the invite. Never a clue's spelling of it
/// ("Casper").
fn name_me(
    segments: &[Segment],
    evidence: &[Evidence],
    me: Option<&str>,
    names: &mut BTreeMap<u32, String>,
) {
    let me = me.map(str::to_string).or_else(|| {
        evidence
            .iter()
            .filter(|e| e.from == Kind::Myself)
            .find_map(|e| match &e.who {
                Who::Name(n) => Some(n.clone()),
                Who::Voice(_) => None,
            })
    });
    let heard = segments
        .iter()
        .any(|s| s.speaker == Some(super::diarize::ME));
    match me {
        Some(me) if heard => {
            names.insert(super::diarize::ME, me);
        }
        _ => {
            names.remove(&super::diarize::ME);
        }
    }
}

fn same_name(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// A clue's name as the meeting knows it: "Sam" is "Sam Rivera" when he's
/// the only Sam on the call or the invite, and "Peter" is "Peter Lai" when
/// the invite only has his e-mail handle "Peterlai" (one of `handles`).
fn full_name(name: &str, known: &[String], handles: &[String]) -> String {
    let first = |n: &str| n.split_whitespace().next().unwrap_or("").to_lowercase();
    let split = |k: &String| handles.contains(k).then(|| split_handle(k, name)).flatten();
    let matches: Vec<&String> = known
        .iter()
        .filter(|k| {
            same_name(k, name)
                || (!name.contains(' ') && first(k) == name.trim().to_lowercase())
                || split(k).is_some()
        })
        .collect();
    match matches.as_slice() {
        [one] => split(one).unwrap_or_else(|| (*one).clone()),
        _ => name.trim().to_string(),
    }
}

/// An invitee known only by an e-mail handle ("Peterlai") written as the
/// name said on the call starts it: "Peter Lai".
fn split_handle(handle: &str, said: &str) -> Option<String> {
    let (handle, said) = (handle.trim(), said.trim());
    if handle.contains(' ') || said.contains(' ') || said.chars().count() < 3 {
        return None;
    }
    let rest = handle.get(said.len()..)?;
    if !handle.to_lowercase().starts_with(&said.to_lowercase())
        || rest.chars().count() < 2
        || !rest.chars().all(char::is_alphabetic)
    {
        return None;
    }
    let first = &handle[..said.len()];
    let mut rest = rest.chars();
    let rest: String = rest
        .next()?
        .to_uppercase()
        .chain(rest.flat_map(char::to_lowercase))
        .collect();
    Some(format!("{first} {rest}"))
}

/// The name to show for an invitee: the handle split as it was said on the
/// call, when it was.
fn as_said(invitee: &str, said: &[String]) -> String {
    said.iter()
        .find_map(|s| split_handle(invitee, s))
        .unwrap_or_else(|| invitee.to_string())
}

/// On a 1-on-1 (one person invited besides the user), that person, written
/// as the call said them when the invite only has a handle.
pub fn one_on_one_name(dir: &Path) -> Option<String> {
    let invite = super::calendar::load(dir)?;
    let others: Vec<&String> = invite
        .attendees
        .iter()
        .filter(|n| !invite.me.as_deref().is_some_and(|m| same_name(m, n)))
        .collect();
    let [other] = others.as_slice() else {
        return None;
    };
    if !invite.handles.contains(other) {
        return Some((*other).clone());
    }
    let clues: Vec<super::clues::Clue> =
        super::summary::load_json(dir, super::clues::FILE).unwrap_or_default();
    let said: Vec<String> = clues.into_iter().map(|c| c.name).collect();
    Some(as_said(other, &said))
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
        Kind::InvitedHandle,
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

    // A 1-on-1 on the calendar: the one voice heard from the call is the
    // other person invited, whatever names came up in passing.
    let said: Vec<String> = evidence
        .iter()
        .filter(|e| matches!(e.from, Kind::Clue | Kind::NotClue))
        .filter_map(name_of)
        .collect();
    let handles = of_kind(&[Kind::InvitedHandle]);
    let others: Vec<String> = of_kind(&[Kind::Invited, Kind::InvitedHandle])
        .into_iter()
        .filter(|n| !me.iter().any(|m| same_name(m, n)))
        .collect();
    let mut from_call: BTreeMap<u32, u64> = BTreeMap::new();
    for s in segments
        .iter()
        .filter(|s| s.source == Source::System && !s.echo)
    {
        if let Some(v) = s.speaker.filter(|&v| v != super::diarize::ME) {
            *from_call.entry(v).or_default() += s.end_ms.saturating_sub(s.start_ms);
        }
    }
    let heard: Vec<u32> = from_call
        .iter()
        .filter(|(_, ms)| **ms >= ELIMINATE_MS)
        .map(|(v, _)| *v)
        .collect();
    if let ([other], [v]) = (others.as_slice(), heard.as_slice()) {
        let name = if handles.contains(other) {
            as_said(other, &said)
        } else {
            other.clone()
        };
        if !named(names, v) && !is_taken(&taken, other) && !is_taken(&taken, &name) {
            taken.push(name.clone());
            names.insert(*v, name);
        }
    }

    // Clues, per voice.
    let mut score: BTreeMap<u32, BTreeMap<String, f32>> = BTreeMap::new();
    let mut not: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    // Lines the conversation handed to someone count too in the room (the
    // mic); on the call they named the voices already (`call_app_names`).
    for e in evidence.iter().filter(|e| {
        matches!(e.from, Kind::Clue | Kind::NotClue)
            || (e.from == Kind::Floor && e.track == Source::Mic)
    }) {
        let (Some(n), Some(v)) = (
            name_of(e),
            voice_at(segments, e.track, e.start_ms, e.end_ms),
        ) else {
            continue;
        };
        // The user's own voice is named from the settings, not clues.
        if v == super::diarize::ME {
            continue;
        }
        let n = full_name(&n, &known, &handles);
        let entry = score.entry(v).or_default().entry(n.clone()).or_default();
        if matches!(e.from, Kind::Clue | Kind::Floor) {
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
    // People known to be there, and names a clue says someone is (not
    // ones only ever ruled out: "Felix", "Claude" said in passing).
    let mut candidates: Vec<String> = known.clone();
    for s in score.values() {
        for (n, w) in s {
            if *w > 0.0 && !candidates.iter().any(|c| same_name(c, n)) {
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

/// Whether `said` (as speech recognition wrote it) is the person `name`:
/// their name or first name, a longer mishearing of it ("Kasparov" for
/// Kaspar), or one letter off.
fn sounds_like(said: &str, name: &str) -> bool {
    let said = said.trim().to_lowercase();
    let name = name.trim().to_lowercase();
    let first = name.split_whitespace().next().unwrap_or("");
    if said == name || said == first {
        return true;
    }
    if said.contains(' ') || first.chars().count() < 4 {
        return false;
    }
    said.starts_with(first) || one_letter_off(&said, first)
}

fn one_letter_off(a: &str, b: &str) -> bool {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let (short, long) = if a.len() <= b.len() {
        (&a, &b)
    } else {
        (&b, &a)
    };
    match long.len() - short.len() {
        0 => a.iter().zip(&b).filter(|(x, y)| x != y).count() == 1,
        1 => (0..long.len()).any(|skip| {
            long.iter()
                .enumerate()
                .filter(|(i, _)| *i != skip)
                .map(|(_, c)| c)
                .eq(short.iter())
        }),
        _ => false,
    }
}

/// The floor's lines (see [`super::floor`]) under the names the meeting
/// knows: matched to the invite ("joseph" is "Joseph Chong" when the invite
/// has "Josephchong"). Lines handed to the user are dropped (on a call
/// they're never on the call's track, and their mic is theirs anyway), and
/// with an invite so are names on the call nobody invited sounds like:
/// speech recognition's mishearings. In the room (the mic) a guest needn't
/// be on the invite.
pub fn floor_turns(
    turns: &[super::floor::Turn],
    invite: Option<&super::calendar::Invite>,
) -> Vec<super::floor::Turn> {
    let empty = super::calendar::Invite::default();
    let invite = invite.unwrap_or(&empty);
    turns
        .iter()
        .filter(|t| {
            !invite
                .me
                .as_deref()
                .is_some_and(|me| sounds_like(&t.name, me))
        })
        .filter_map(|t| {
            let said = t.name.trim();
            let as_said = if t.said.trim().is_empty() {
                said
            } else {
                t.said.trim()
            };
            let matches: Vec<String> = invite
                .attendees
                .iter()
                .filter_map(|a| {
                    if invite.handles.contains(a) {
                        if let Some(split) = split_handle(a, said).or_else(|| {
                            same_name(a, said)
                                .then(|| split_handle(a, as_said))
                                .flatten()
                        }) {
                            return Some(split);
                        }
                    }
                    sounds_like(said, a).then(|| a.clone())
                })
                .collect();
            // Someone in the room with the user (the mic) needn't be invited.
            let name = match matches.as_slice() {
                [one] => one.clone(),
                [] if invite.attendees.is_empty() || t.source == Source::Mic => said.to_string(),
                _ => return None,
            };
            Some(super::floor::Turn { name, ..t.clone() })
        })
        .collect()
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
    // Where the call app said nothing, the lines the conversation named.
    let floor: Vec<super::floor::Turn> =
        super::summary::load_json(dir, super::floor::FILE).unwrap_or_default();
    let invite = super::calendar::load(dir);
    for t in floor_turns(&floor, invite.as_ref()) {
        if t.source != Source::System {
            continue;
        }
        let from = ((t.start_ms / frame) as usize).min(frames);
        let to = ((t.end_ms / frame) as usize).min(frames);
        for o in &mut out[from..to] {
            if o.is_none() {
                *o = Some(t.name.clone());
            }
        }
    }
    out
}

/// Tell the call's voices apart again from the kept windows with the names
/// as hints ([`name_hints`], now with the floor's): voices that merged
/// several people (a standup's) come apart where the conversation named
/// them. Gives each line of the call's track the voice of most of its
/// frames; false when there's nothing to go on or nothing changed.
fn regroup_call(dir: &Path, segments: &mut [Segment]) -> bool {
    let Some((wins, embs)) = super::windows::load(dir).and_then(|tracks| {
        tracks
            .into_iter()
            .find(|(s, _)| *s == Source::System)
            .map(|(_, p)| p)
    }) else {
        return false;
    };
    let frame = super::transcript::FRAME_MS;
    let frames = wins.iter().map(|w| w.1).max().unwrap_or(0).max(
        segments
            .iter()
            .map(|s| (s.end_ms / frame) as usize)
            .max()
            .unwrap_or(0),
    );
    let mut speech = vec![false; frames];
    for &(from, to) in &wins {
        for x in &mut speech[from.min(frames)..to.min(frames)] {
            *x = true;
        }
    }
    let hints = name_hints(dir, frames);
    if hints.iter().all(Option::is_none) {
        return false;
    }
    let voices = super::diarize::label_with(&speech, &wins, &embs, None, &hints);
    let labels = super::diarize::offset(&voices.labels, super::pipeline::SYSTEM_SPEAKERS);
    let mut changed = false;
    for s in segments
        .iter_mut()
        .filter(|s| s.source == Source::System && !s.echo)
    {
        let from = ((s.start_ms / frame) as usize).min(frames);
        let to = ((s.end_ms / frame) as usize).min(frames);
        let mut count: BTreeMap<u32, usize> = BTreeMap::new();
        for l in labels[from..to].iter().flatten() {
            *count.entry(*l).or_default() += 1;
        }
        let voice = count.into_iter().max_by_key(|(_, c)| *c).map(|(v, _)| v);
        if voice.is_some() && voice != s.speaker {
            s.speaker = voice;
            changed = true;
        }
    }
    // A name the conversation gave is one person (unlike a call tile, which
    // can be a room): voices whose named lines are mostly one name are one
    // voice.
    let floor: Vec<super::floor::Turn> =
        super::summary::load_json(dir, super::floor::FILE).unwrap_or_default();
    let mut named: BTreeMap<u32, BTreeMap<String, usize>> = BTreeMap::new();
    for t in floor_turns(&floor, super::calendar::load(dir).as_ref()) {
        let line = segments
            .iter()
            .find(|s| s.source == t.source && s.start_ms == t.start_ms);
        if let Some(v) = line
            .filter(|s| s.source == Source::System)
            .and_then(|s| s.speaker)
        {
            *named.entry(v).or_default().entry(t.name).or_default() += 1;
        }
    }
    let mut first_with: BTreeMap<String, u32> = BTreeMap::new();
    let mut into: BTreeMap<u32, u32> = BTreeMap::new();
    for (v, names) in &named {
        let total: usize = names.values().sum();
        if let Some((name, n)) = names.iter().max_by_key(|(_, n)| **n) {
            if n * 2 > total {
                let to = *first_with.entry(name.clone()).or_insert(*v);
                if to != *v {
                    into.insert(*v, to);
                }
            }
        }
    }
    for s in segments.iter_mut().filter(|s| s.source == Source::System) {
        if let Some(to) = s.speaker.and_then(|v| into.get(&v)) {
            s.speaker = Some(*to);
            changed = true;
        }
    }
    changed
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
/// names split the system track. `me` is the user's name (see
/// [`crate::rules::user_name`]): their own voice always gets it.
pub fn apply(dir: &Path, me: Option<&str>) -> BTreeMap<u32, String> {
    let Some(mut t) = super::pipeline::load(dir) else {
        return BTreeMap::new();
    };
    let info = super::manager::read_info(dir);
    if let Some(i) = &info {
        let end = i.ended_at.unwrap_or(i.started_at + 60 * 60 * 1000);
        super::calendar::for_meeting(dir, i.started_at, end);
    }
    let user = info.map(|i| i.speakers).unwrap_or_default();
    let before = t.segments.clone();
    // The conversation named people: the call's voices again with those
    // names, unless the user has already named or moved voices (their
    // names are by voice number).
    let floor = dir.join(super::floor::FILE).exists();
    let untouched = user.keys().all(|v| *v < super::pipeline::SYSTEM_SPEAKERS)
        && super::manager::user_speaker_fixes(dir).is_empty();
    let regrouped = floor && untouched && regroup_call(dir, &mut t.segments);
    if regrouped {
        // The clue step's turns name voices by their old numbers.
        let _ = super::summary::save_json(
            dir,
            super::clues::TURNS_FILE,
            &BTreeMap::<String, u32>::new(),
        );
    }
    let mut evidence = record(dir, &t);
    if let Some(me) = me {
        evidence.push(name_evidence(me, Kind::Myself));
    }
    // Only voices still numbered as when their prints were taken.
    let prints: BTreeMap<u32, Vec<f32>> = if regrouped {
        BTreeMap::new()
    } else {
        super::summary::load_json::<BTreeMap<u32, super::remembered::Print>>(
            dir,
            super::remembered::PRINTS_FILE,
        )
        .unwrap_or_default()
        .into_iter()
        .map(|(v, p)| (v, p.print))
        .collect()
    };
    let mut names = call_app_names(&mut t.segments, &evidence, &prints);
    let (named, unknown) = super::remembered::apply(dir, &user, &names);
    let guessed = finish(
        &t.segments,
        &evidence,
        &user,
        &mut names,
        &Remembered { named, unknown },
    );
    name_me(&t.segments, &evidence, me, &mut names);
    let doubts = doubts(dir, &t.segments, &user, &guessed);
    let _ = super::summary::save_json(dir, DOUBTS_FILE, &doubts);
    // Saved when the voices were told apart again or the names took over
    // the track.
    if (regrouped || !names.is_empty()) && t.segments != before {
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
    let picked: Vec<(u32, &str, u64)> = by_name
        .into_iter()
        .filter_map(|(v, names)| {
            let (name, ms) = names.into_iter().max_by_key(|(_, ms)| *ms)?;
            (ms * 2 > total.get(&v).copied().unwrap_or(0)).then_some((v, name, ms))
        })
        .collect();
    // A name is one person: when the voices split since (one named voice
    // was really several), only the one that said the most keeps it.
    picked
        .iter()
        .filter(|(v, name, ms)| {
            !picked.iter().any(|(w, n, m)| {
                n == name && w != v && (m, std::cmp::Reverse(w)) > (ms, std::cmp::Reverse(v))
            })
        })
        .map(|(v, name, _)| (*v, name.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_carries_to_one_voice_only() {
        let named = |start_s: u64, end_s: u64| NamedStretch {
            source: Source::System,
            start_ms: start_s * 1000,
            end_ms: end_s * 1000,
            name: "Ron".into(),
        };
        // One "Ron" voice split into two since: the one who said more keeps it.
        let segments = vec![seg(0, 10, Some(100)), seg(10, 30, Some(101))];
        let names = names_by_overlap(&segments, &[named(0, 30)]);
        assert_eq!(names, BTreeMap::from([(101, "Ron".to_string())]));
    }

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

    #[test]
    fn the_users_voice_has_their_name_never_a_clues_spelling() {
        use super::super::diarize::ME;
        let mut segments = vec![seg(0, 30, Some(ME)), seg(30, 60, Some(0))];
        for s in &mut segments {
            s.source = Source::Mic;
        }
        let clue = |name: &str, from_s: u64| Evidence {
            track: Source::Mic,
            start_ms: from_s * 1000,
            end_ms: (from_s + 10) * 1000,
            who: Who::Name(name.into()),
            strength: 1.0,
            from: Kind::Clue,
        };
        let mut evidence = vec![
            clue("Casper", 5),
            clue("Casper", 15),
            clue("Shubham", 35),
            name_evidence("Kaspar", Kind::Myself),
        ];
        let mut names = resolve(&mut segments, &evidence, &BTreeMap::new());
        name_me(&segments, &evidence, Some("Kaspar Hidayat"), &mut names);
        assert_eq!(names.get(&ME).map(String::as_str), Some("Kaspar Hidayat"));
        assert_eq!(names.get(&0).map(String::as_str), Some("Shubham"));
        // Without a setting, the invite's name for the user.
        evidence.retain(|e| e.from != Kind::Clue);
        let mut names = BTreeMap::new();
        name_me(&segments, &evidence, None, &mut names);
        assert_eq!(names.get(&ME).map(String::as_str), Some("Kaspar"));
        // On a call the user's voice isn't labelled: nothing to name.
        let mut names = BTreeMap::new();
        name_me(&segments[1..], &evidence, Some("Kaspar"), &mut names);
        assert!(names.is_empty());
    }

    #[test]
    fn in_the_room_a_guest_named_in_passing_and_the_one_invited_get_named() {
        use super::super::diarize::ME;
        let mut segments = vec![
            seg(0, 40, Some(0)),
            seg(40, 70, Some(2)),
            seg(70, 200, Some(ME)),
        ];
        for s in &mut segments {
            s.source = Source::Mic;
        }
        let invite = super::super::calendar::Invite {
            attendees: vec!["Shubham Rampalliwar".into()],
            me: Some("Kaspar".into()),
            ..Default::default()
        };
        // The conversation hands lines to "Bering", who wasn't invited.
        let floor = vec![super::super::floor::Turn {
            source: Source::Mic,
            start_ms: 45_000,
            end_ms: 60_000,
            name: "Bering".into(),
            said: "Bering".into(),
        }];
        let turns = floor_turns(&floor, Some(&invite));
        assert_eq!(turns.len(), 1);
        let ruled_out = |name: &str, at_s: u64| Evidence {
            track: Source::Mic,
            start_ms: at_s * 1000,
            end_ms: (at_s + 5) * 1000,
            who: Who::Name(name.into()),
            strength: 0.5,
            from: Kind::NotClue,
        };
        let mut evidence: Vec<Evidence> = turns
            .into_iter()
            .map(|t| Evidence {
                track: t.source,
                start_ms: t.start_ms,
                end_ms: t.end_ms,
                who: Who::Name(t.name),
                strength: FLOOR,
                from: Kind::Floor,
            })
            .collect();
        evidence.extend([
            ruled_out("Felix", 50),
            ruled_out("Claude", 5),
            name_evidence("Shubham Rampalliwar", Kind::Invited),
            name_evidence("Kaspar", Kind::Myself),
        ]);
        let names = resolve(&mut segments, &evidence, &BTreeMap::new());
        assert_eq!(names.get(&2).map(String::as_str), Some("Bering"));
        assert_eq!(
            names.get(&0).map(String::as_str),
            Some("Shubham Rampalliwar")
        );
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
            active_speaker::names_for(&mut direct, &sightings, &BTreeMap::new())
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
            active_speaker::names_for(&mut b, &sightings, &BTreeMap::new())
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
                ..Default::default()
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
    fn a_one_on_one_names_the_call_from_the_invite() {
        use crate::meetings::calendar::Invite;
        use crate::meetings::clues::Clue;
        // One voice on the call; Charlie and Peter come up in passing, and
        // the invite only knows Peter's e-mail handle.
        let mut t = Transcript {
            segments: vec![seg(0, 300, Some(100)), seg(300, 302, Some(101))],
            ..Default::default()
        };
        let clue = |start_s: u64, name: &str, is: bool| Clue {
            source: Source::System,
            start_ms: start_s * 1000,
            end_ms: (start_s + 5) * 1000,
            name: name.into(),
            is,
            strength: 0.5,
            quote: String::new(),
        };
        let src = Sources {
            clues: vec![clue(10, "Charlie", false), clue(20, "Peter", false)],
            invite: Some(Invite {
                title: "Catch-up".into(),
                attendees: vec!["Peterlai".into()],
                me: Some("Kaspar".into()),
                handles: vec!["Peterlai".into()],
                version: crate::meetings::calendar::VERSION,
                ..Default::default()
            }),
            ..Default::default()
        };
        let evidence = gather(&t, &src);
        let names = resolve(&mut t.segments, &evidence, &BTreeMap::new());
        assert_eq!(names.get(&100).map(String::as_str), Some("Peter Lai"));
        assert_eq!(names.get(&101), None);

        // Two people talking from the call isn't a 1-on-1.
        t.segments.push(seg(400, 460, Some(101)));
        let names = resolve(&mut t.segments, &evidence, &BTreeMap::new());
        assert_eq!(names.get(&100), None);
    }

    #[test]
    fn floor_names_are_matched_to_the_invite() {
        use crate::meetings::calendar::Invite;
        use crate::meetings::floor::Turn;
        let turn = |at: u64, name: &str| Turn {
            source: Source::System,
            start_ms: at,
            end_ms: at + 1000,
            name: name.into(),
            said: String::new(),
        };
        let invite = Invite {
            attendees: vec![
                "Josephchong".into(),
                "Paul".into(),
                "Jiaming".into(),
                "Sam Rivera".into(),
            ],
            handles: vec!["Josephchong".into()],
            me: Some("Kaspar".into()),
            ..Default::default()
        };
        let turns = vec![
            turn(0, "joseph"),
            turn(1, "Kasparov"), // the user, misheard
            turn(2, "poop"),     // nobody invited
            turn(3, "Paul"),
            turn(4, "Jiamin"), // one letter off
            turn(5, "Sam"),
            // Matched by the model to the invite, said as "Joseph".
            Turn {
                said: "Joseph".into(),
                ..turn(6, "Josephchong")
            },
        ];
        let names: Vec<String> = floor_turns(&turns, Some(&invite))
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(
            names,
            [
                "Joseph Chong",
                "Paul",
                "Jiaming",
                "Sam Rivera",
                "Joseph Chong"
            ]
        );
        // Without an invite the names stand as said.
        let names: Vec<String> = floor_turns(&turns, None)
            .into_iter()
            .map(|t| t.name)
            .collect();
        assert_eq!(names.len(), 7);
    }

    #[test]
    fn handles_split_only_where_the_name_said_starts_them() {
        assert_eq!(
            split_handle("Peterlai", "Peter").as_deref(),
            Some("Peter Lai")
        );
        assert_eq!(split_handle("Peterlai", "Pe"), None);
        assert_eq!(split_handle("Peter", "Peter"), None);
        assert_eq!(split_handle("Peter Lai", "Peter"), None);
        assert_eq!(
            split_handle("Samantha", "Sam").as_deref(),
            Some("Sam Antha")
        );
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

    #[test]
    fn the_users_own_tile_never_names_a_voice_on_the_call() {
        // The user talks 0–20 s on the mic, Sam 20–40 s on the call; the
        // user's tile ("Kaspar Hidayat") lingers into Sam's first seconds.
        let mut segments = vec![
            Segment {
                source: Source::Mic,
                ..seg(0, 20, None)
            },
            seg(20, 40, Some(100)),
            seg(40, 42, Some(101)),
        ];
        let mut evidence: Vec<Evidence> = (0..22)
            .map(|i| Evidence {
                track: Source::System,
                start_ms: i * 1000,
                end_ms: i * 1000 + 1500,
                who: Who::Name("Kaspar Hidayat".into()),
                strength: EXTENSION_SPEAKING,
                from: Kind::Extension,
            })
            .collect();
        let mark = evidence[0].clone();
        evidence.extend((24..40).map(|i| Evidence {
            who: Who::Name("Sam Rivera".into()),
            start_ms: i * 1000,
            end_ms: i * 1000 + 1500,
            ..mark.clone()
        }));
        // A blip at the hand-over has more of the user's marks than Sam's.
        evidence.extend((40..43).map(|i| Evidence {
            start_ms: i * 1000,
            end_ms: i * 1000 + 1500,
            ..mark.clone()
        }));
        let names = call_app_names(&mut segments, &evidence, &BTreeMap::new());
        assert_eq!(names.get(&100).map(String::as_str), Some("Sam Rivera"));
        assert!(names.values().all(|n| n != "Kaspar Hidayat"));
    }
}
