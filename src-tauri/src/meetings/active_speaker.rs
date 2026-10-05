//! Names for the other side of a call, from the call app itself. Zoom, Teams,
//! Meet and friends mark who's talking in their accessibility tree ("Sam Lee,
//! speaking", "Active speaker: Sam Lee"). While a call records, Felix reads
//! that about once a second and logs who it saw when (`speakers_seen.jsonl`).
//! After transcription the sightings vote names onto the voices the
//! diarizer told apart on the system track, or, without speaker detection,
//! split the system track by name directly.

use super::pipeline::SYSTEM_SPEAKERS;
use super::transcript::{Segment, Source};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};

pub const FILE: &str = "speakers_seen.jsonl";
/// How often the call app is read.
const POLL: Duration = Duration::from_secs(1);
/// A sighting names whoever talks up to this long after it.
pub(super) const TTL_MS: u64 = 1_500;
/// A voice is named only with this many sightings…
const MIN_VOTES: usize = 3;
/// …and when this share of its sightings agree.
const MIN_SHARE: f32 = 0.6;
/// One voice on the call's track is several people only when a second name
/// has at least this share of the sightings.
const SPLIT_SHARE: f32 = 0.1;
/// One name on several voices that sound like different people, each
/// talking at least this long, is a room on the call (several people on one
/// account): the voices are numbered instead of all getting the same name.
const ROOM_VOICE_MS: u64 = 20_000;
/// A voice nobody clearly won takes a name no voice has yet when at least
/// this share of that name's sightings fell on it: someone who spoke little,
/// with a busier tile lighting up over them.
const LEFTOVER_SHARE: f32 = 0.5;
/// Limits on one read of the call app, so a huge browser tree can't stall.
const MAX_NODES: usize = 4_000;
const READ_BUDGET: Duration = Duration::from_millis(400);

/// One sighting: this name was marked as talking at this point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Seen {
    pub at_ms: u64,
    pub name: String,
}

/// Words in a control that say someone's talking.
const MARKERS: &[&str] = &[
    "active speaker",
    "is speaking",
    "is talking",
    "currently speaking",
    "speaking",
    "talking",
];
/// The user's own tile.
const SELF_MARKERS: &[&str] = &["you are speaking", "you're speaking", "(you)", "(me)"];
/// Status words call apps put next to names.
const STATUS_WORDS: &[&str] = &[
    "muted",
    "unmuted",
    "video on",
    "video off",
    "camera on",
    "camera off",
    "host",
    "co-host",
    "(host)",
    "pinned",
    "spotlighted",
    "hand raised",
    "presenting",
    "guest",
    "external",
    "organizer",
    "mic on",
    "mic off",
];
/// Not a person.
const NOT_NAMES: &[&str] = &[
    "you",
    "me",
    "no one",
    "nobody",
    "everyone",
    "someone",
    "speaker",
    "participants",
    "unknown",
    // Chrome's Edit → Speech → Start / Stop Speaking.
    "start",
    "stop",
];

/// Teams marks the talking tile by id rather than words.
fn is_speaker_id(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("speakername") || t.contains("active-speaker") || t.contains("activespeaker")
}

pub(super) fn looks_like_name(text: &str) -> bool {
    let t = text.trim();
    let words = t.split_whitespace().count();
    let lower = t.to_lowercase();
    (1..=5).contains(&words)
        && (2..=40).contains(&t.chars().count())
        && t.chars().next().is_some_and(char::is_alphabetic)
        && t.chars().filter(|c| c.is_ascii_digit()).count() <= 2
        && !NOT_NAMES.contains(&lower.as_str())
        && !MARKERS.iter().any(|m| lower.contains(m))
        && !STATUS_WORDS.contains(&lower.as_str())
}

/// Strip markers and status words from one text, leaving the name if one's left.
fn name_in(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let marker = MARKERS.iter().find(|m| lower.contains(*m))?;
    let at = lower.find(marker)?;
    // Byte offsets match: markers are ASCII, and lowercasing ASCII keeps lengths.
    if !text.is_char_boundary(at) || !text.is_char_boundary(at + marker.len()) {
        return None;
    }
    let rest = format!("{} , {}", &text[..at], &text[at + marker.len()..]);
    rest.split([',', ':', '(', ')', '|', '·', '–', '—', '\n'])
        .flat_map(|part| part.split(" - "))
        .map(str::trim)
        .find(|part| looks_like_name(part))
        .map(str::to_string)
}

/// The talking person one control names, if it names one. `texts` are the
/// control's title, description, value, help and ids.
pub fn speaker_in(texts: &[String]) -> Option<String> {
    let lower: Vec<String> = texts.iter().map(|t| t.to_lowercase()).collect();
    if lower
        .iter()
        .any(|t| SELF_MARKERS.iter().any(|m| t.contains(m)))
    {
        return None;
    }
    if let Some(name) = texts.iter().find_map(|t| name_in(t)) {
        return Some(name);
    }
    // "Speaking" on its own (a badge) or a speaker id: the name is another text.
    let marked = lower
        .iter()
        .any(|t| MARKERS.contains(&t.trim()) || is_speaker_id(t));
    marked
        .then(|| {
            texts
                .iter()
                .find(|t| !is_speaker_id(t) && looks_like_name(t))
                .map(|t| t.trim().to_string())
        })
        .flatten()
}

/// Everyone the call app marks as talking right now.
pub fn speakers_in(nodes: &[Vec<String>]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for node in nodes {
        if let Some(name) = speaker_in(node) {
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

pub fn load(dir: &Path) -> Vec<Seen> {
    std::fs::read_to_string(dir.join(FILE))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

fn overlaps(seen: &Seen, s: &Segment) -> bool {
    seen.at_ms <= s.end_ms && seen.at_ms + TTL_MS >= s.start_ms
}

fn winner(votes: &BTreeMap<String, usize>, min_votes: usize) -> Option<String> {
    let total: usize = votes.values().sum();
    let (name, &count) = votes.iter().max_by_key(|(_, &c)| c)?;
    (count >= min_votes && count as f32 >= MIN_SHARE * total as f32).then(|| name.clone())
}

/// Names for the voices told apart on the system track (ids from
/// [`SYSTEM_SPEAKERS`]), where the sightings clearly agree. Several voices
/// under one name that sound like the same person (`prints`, by voice) are
/// one person the voice step split, and share the name. Voices that sound
/// different are a room on one account: "Name (1)", "Name (2)" in the order
/// they first spoke.
pub fn name_voices(
    segments: &[Segment],
    seen: &[Seen],
    prints: &BTreeMap<u32, Vec<f32>>,
) -> BTreeMap<u32, String> {
    let mut names = vote_names(segments, seen);
    // How long each voice talks, and when it first does.
    let mut talk: BTreeMap<u32, (u64, u64)> = BTreeMap::new();
    for s in segments.iter().filter(|s| s.source == Source::System) {
        if let Some(id) = s.speaker {
            let e = talk.entry(id).or_insert((0, s.start_ms));
            e.0 += s.end_ms.saturating_sub(s.start_ms);
            e.1 = e.1.min(s.start_ms);
        }
    }
    let mut by_name: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (id, name) in &names {
        by_name.entry(name.clone()).or_default().push(*id);
    }
    for (name, mut ids) in by_name {
        if ids.len() < 2 {
            continue;
        }
        ids.sort_by_key(|id| talk.get(id).map_or(u64::MAX, |t| t.1));
        let people = same_people(&ids, prints);
        let talks = |group: &Vec<u32>| {
            group
                .iter()
                .map(|id| talk.get(id).map_or(0, |t| t.0))
                .sum::<u64>()
                >= ROOM_VOICE_MS
        };
        if people.len() < 2 || !people.iter().all(talks) {
            continue;
        }
        for (n, group) in people.iter().enumerate() {
            for id in group {
                names.insert(*id, format!("{name} ({})", n + 1));
            }
        }
    }
    names
}

/// `ids` (in the order they first spoke) grouped into people: a voice joins
/// the first group with a voice it sounds like (any, as a brief voice's
/// print is a poor stand-in for the group). Without prints each voice is
/// its own.
fn same_people(ids: &[u32], prints: &BTreeMap<u32, Vec<f32>>) -> Vec<Vec<u32>> {
    let mut groups: Vec<Vec<u32>> = Vec::new();
    for &id in ids {
        let sounds_like = |other: &u32| match (prints.get(other), prints.get(&id)) {
            (Some(a), Some(b)) => cosine(a, b) >= super::remembered::SAME_PERSON,
            _ => false,
        };
        let alike = |g: &&mut Vec<u32>| g.iter().any(sounds_like);
        match groups.iter_mut().find(alike) {
            Some(g) => g.push(id),
            None => groups.push(vec![id]),
        }
    }
    groups
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    let n = norm(a) * norm(b);
    if n > 0.0 {
        dot / n
    } else {
        0.0
    }
}

/// Each voice's sightings, by name.
fn votes_by_voice(segments: &[Segment], seen: &[Seen]) -> BTreeMap<u32, BTreeMap<String, usize>> {
    let mut votes: BTreeMap<u32, BTreeMap<String, usize>> = BTreeMap::new();
    for s in segments.iter().filter(|s| s.source == Source::System) {
        let Some(id) = s.speaker.filter(|&n| n >= SYSTEM_SPEAKERS) else {
            continue;
        };
        for sighting in seen.iter().filter(|x| overlaps(x, s)) {
            *votes
                .entry(id)
                .or_default()
                .entry(sighting.name.clone())
                .or_default() += 1;
        }
    }
    votes
}

/// The clear winners, then, for the voices left, names no voice has yet
/// whose sightings mostly fell on them (see [`LEFTOVER_SHARE`]).
fn vote_names(segments: &[Segment], seen: &[Seen]) -> BTreeMap<u32, String> {
    let votes = votes_by_voice(segments, seen);
    let mut names: BTreeMap<u32, String> = votes
        .iter()
        .filter_map(|(id, v)| winner(v, MIN_VOTES).map(|name| (*id, name)))
        .collect();
    let mut on_voices: BTreeMap<&str, usize> = BTreeMap::new();
    for v in votes.values() {
        for (name, n) in v {
            *on_voices.entry(name.as_str()).or_default() += n;
        }
    }
    for (id, v) in &votes {
        if names.contains_key(id) {
            continue;
        }
        let free: BTreeMap<String, usize> = v
            .iter()
            .filter(|(name, _)| !names.values().any(|n| n == *name))
            .map(|(name, n)| (name.clone(), *n))
            .collect();
        let Some(name) = winner(&free, MIN_VOTES) else {
            continue;
        };
        let share = free[&name] as f32 / on_voices.get(name.as_str()).copied().unwrap_or(1) as f32;
        if share >= LEFTOVER_SHARE {
            names.insert(*id, name);
        }
    }
    names
}

/// Without speaker detection: give each system segment the name seen while
/// it was said (numbered from [`SYSTEM_SPEAKERS`]), and return the names.
pub fn split_by_name(segments: &mut [Segment], seen: &[Seen]) -> BTreeMap<u32, String> {
    let mut ids: BTreeMap<String, u32> = BTreeMap::new();
    for s in segments.iter_mut().filter(|s| s.source == Source::System) {
        let mut votes: BTreeMap<String, usize> = BTreeMap::new();
        for sighting in seen.iter().filter(|x| overlaps(x, s)) {
            *votes.entry(sighting.name.clone()).or_default() += 1;
        }
        if let Some(name) = winner(&votes, 2) {
            let next = SYSTEM_SPEAKERS + ids.len() as u32;
            s.speaker = Some(*ids.entry(name).or_insert(next));
        }
    }
    ids.into_iter().map(|(name, id)| (id, name)).collect()
}

/// The names the call app gave, by speaker id. Without voices on the system
/// track, or with one voice but several names (a muddy line), the names
/// split the track instead, setting the segments' speakers.
pub fn names_for(
    segments: &mut [Segment],
    seen: &[Seen],
    prints: &BTreeMap<u32, Vec<f32>>,
) -> BTreeMap<u32, String> {
    if seen.is_empty() {
        return BTreeMap::new();
    }
    let voices: std::collections::BTreeSet<u32> = segments
        .iter()
        .filter(|s| s.source == Source::System)
        .filter_map(|s| s.speaker)
        .collect();
    // Names seen enough to be someone talking on the line, not a stray mark
    // (a muted tile lighting up from room noise).
    let mut sightings: BTreeMap<&str, usize> = BTreeMap::new();
    for s in seen {
        *sightings.entry(s.name.as_str()).or_default() += 1;
    }
    let names_seen = sightings
        .values()
        .filter(|n| **n as f32 >= SPLIT_SHARE * seen.len() as f32)
        .count();
    if !voices.is_empty() && !(voices.len() == 1 && names_seen >= 2) {
        return name_voices(segments, seen, prints);
    }
    if voices.len() == 1 {
        for s in segments.iter_mut().filter(|s| s.source == Source::System) {
            s.speaker = None;
        }
    }
    split_by_name(segments, seen)
}

/// While a call records: read who's talking in the call app about once a
/// second until `stop` is set.
pub fn spawn_watcher(
    app: &AppHandle,
    dir: &Path,
    bundle_id: Option<String>,
    stop: Arc<AtomicBool>,
) {
    let app = app.clone();
    let path = dir.join(FILE);
    std::thread::spawn(move || {
        if !crate::ax_tree::is_trusted() {
            return;
        }
        let mut target: Option<(String, i32)> = None;
        let mut last_lookup: Option<Instant> = None;
        let mut file = None;
        while !stop.load(Ordering::Acquire) {
            let tick = Instant::now();
            if target.is_none() && last_lookup.is_none_or(|t| t.elapsed() > Duration::from_secs(10))
            {
                last_lookup = Some(Instant::now());
                let bundle = bundle_id
                    .clone()
                    .or_else(|| super::call_apps::current().map(|a| a.bundle_id.to_string()));
                target = bundle.and_then(|b| {
                    let pid = crate::ax_tree::pid_of(&b)?;
                    crate::ax_tree::expose_electron_tree(pid);
                    Some((b, pid))
                });
                if let Some((b, _)) = &target {
                    log::info!("Reading speaker names from {b}");
                }
            }
            if let Some((_, pid)) = &target {
                let nodes = crate::ax_tree::texts(
                    *pid,
                    crate::ax_tree::DEFAULT_DEPTH,
                    MAX_NODES,
                    READ_BUDGET,
                );
                if nodes.is_empty() {
                    // The app quit or stopped answering; look again later.
                    target = None;
                }
                let names = speakers_in(&nodes);
                // More than two "talking" at once is a gallery of badges, not a signal.
                if !names.is_empty() && names.len() <= 2 {
                    let at_ms = app
                        .try_state::<Arc<super::manager::MeetingManager>>()
                        .and_then(|m| m.elapsed_ms());
                    if let Some(at_ms) = at_ms {
                        if file.is_none() {
                            file = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&path)
                                .ok();
                        }
                        if let Some(f) = file.as_mut() {
                            for name in names {
                                if let Ok(line) = serde_json::to_string(&Seen { at_ms, name }) {
                                    let _ = writeln!(f, "{line}");
                                }
                            }
                        }
                    }
                }
            }
            std::thread::sleep(POLL.saturating_sub(tick.elapsed()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(t: &[&str]) -> Vec<String> {
        t.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn names_are_read_from_speaking_markers() {
        assert_eq!(
            speaker_in(&texts(&["Sam Lee, speaking"])).unwrap(),
            "Sam Lee"
        );
        assert_eq!(
            speaker_in(&texts(&["Active speaker: Priya Patel"])).unwrap(),
            "Priya Patel"
        );
        assert_eq!(
            speaker_in(&texts(&["Jo Park, Unmuted, Speaking"])).unwrap(),
            "Jo Park"
        );
        assert_eq!(speaker_in(&texts(&["Ana is talking"])).unwrap(), "Ana");
        assert_eq!(
            speaker_in(&texts(&["Speaking", "Marta Rossi"])).unwrap(),
            "Marta Rossi"
        );
        assert_eq!(
            speaker_in(&texts(&["Chris Wu", "calling-roster-item-speakername"])).unwrap(),
            "Chris Wu"
        );
    }

    #[test]
    fn the_user_and_non_names_are_ignored() {
        assert_eq!(speaker_in(&texts(&["You are speaking"])), None);
        assert_eq!(speaker_in(&texts(&["Kaspar (You), speaking"])), None);
        assert_eq!(speaker_in(&texts(&["No one is speaking"])), None);
        assert_eq!(speaker_in(&texts(&["Speaking"])), None);
        assert_eq!(speaker_in(&texts(&["Mute", "Sam Lee"])), None);
        assert_eq!(speaker_in(&texts(&["Speaker view"])), None);
    }

    fn seg(source: Source, start_ms: u64, end_ms: u64, speaker: Option<u32>) -> Segment {
        Segment {
            source,
            start_ms,
            end_ms,
            text: "hi".into(),
            echo: false,
            speaker,
        }
    }

    fn seen(at_ms: u64, name: &str) -> Seen {
        Seen {
            at_ms,
            name: name.into(),
        }
    }

    #[test]
    fn voices_get_the_name_seen_while_they_talk() {
        let a = SYSTEM_SPEAKERS;
        let b = SYSTEM_SPEAKERS + 1;
        let segments = vec![
            seg(Source::System, 0, 5_000, Some(a)),
            seg(Source::System, 5_000, 10_000, Some(b)),
            seg(Source::Mic, 0, 10_000, None),
        ];
        let mut sightings: Vec<Seen> = (0..5).map(|i| seen(i * 1_000, "Sam")).collect();
        sightings.extend((6..10).map(|i| seen(i * 1_000, "Ana")));
        let names = name_voices(&segments, &sightings, &BTreeMap::new());
        assert_eq!(names.get(&a).unwrap(), "Sam");
        assert_eq!(names.get(&b).unwrap(), "Ana");
        // Too few sightings: no name.
        assert!(!name_voices(&segments, &sightings[..2], &BTreeMap::new()).contains_key(&a));
    }

    #[test]
    fn menu_items_about_speaking_are_not_names() {
        assert_eq!(speaker_in(&texts(&["Start Speaking"])), None);
        assert_eq!(speaker_in(&texts(&["Stop Speaking"])), None);
    }

    #[test]
    fn a_room_on_one_account_gets_numbered_voices() {
        let a = SYSTEM_SPEAKERS;
        let b = SYSTEM_SPEAKERS + 1;
        // Two voices, both while "Yohannes" is marked as talking.
        let segments = vec![
            seg(Source::System, 30_000, 60_000, Some(b)),
            seg(Source::System, 0, 30_000, Some(a)),
        ];
        let sightings: Vec<Seen> = (0..60).map(|i| seen(i * 1_000, "Yohannes")).collect();
        let names = name_voices(&segments, &sightings, &BTreeMap::new());
        assert_eq!(names[&a], "Yohannes (1)");
        assert_eq!(names[&b], "Yohannes (2)");
        // A brief second voice is more likely the same person: one name.
        let segments = vec![
            seg(Source::System, 0, 50_000, Some(a)),
            seg(Source::System, 50_000, 55_000, Some(b)),
        ];
        let names = name_voices(&segments, &sightings, &BTreeMap::new());
        assert_eq!(names[&a], "Yohannes");
        assert_eq!(names[&b], "Yohannes");
    }

    #[test]
    fn without_voices_the_system_track_is_split_by_name() {
        let mut segments = vec![
            seg(Source::System, 0, 5_000, None),
            seg(Source::System, 5_000, 10_000, None),
            seg(Source::System, 10_000, 15_000, None),
        ];
        let sightings = vec![
            seen(1_000, "Sam"),
            seen(3_000, "Sam"),
            seen(6_000, "Ana"),
            seen(8_000, "Ana"),
        ];
        let names = split_by_name(&mut segments, &sightings);
        assert_eq!(names.len(), 2);
        assert_eq!(names[&segments[0].speaker.unwrap()], "Sam");
        assert_eq!(names[&segments[1].speaker.unwrap()], "Ana");
        assert_eq!(segments[2].speaker, None);
    }

    #[test]
    fn a_stray_mark_for_someone_else_keeps_the_one_voice() {
        let mut segments: Vec<Segment> = (0..30)
            .map(|i| seg(Source::System, i * 5_000, (i + 1) * 5_000, Some(100)))
            .collect();
        let mut sightings: Vec<Seen> = (0..30).map(|i| seen(i * 5_000 + 1_000, "Era")).collect();
        sightings.push(seen(12_000, "Eugene"));
        let names = names_for(&mut segments, &sightings, &BTreeMap::new());
        assert_eq!(names[&100], "Era");
        assert!(segments.iter().all(|s| s.speaker == Some(100)));
    }

    #[test]
    fn one_person_split_into_voices_keeps_one_name() {
        let (a, b) = (SYSTEM_SPEAKERS, SYSTEM_SPEAKERS + 1);
        let segments = vec![
            seg(Source::System, 0, 30_000, Some(a)),
            seg(Source::System, 30_000, 60_000, Some(b)),
        ];
        let sightings: Vec<Seen> = (0..60).map(|i| seen(i * 1_000, "Hai Song")).collect();
        // The voice step split one person: the voices sound alike.
        let alike = BTreeMap::from([(a, vec![1.0, 0.2, 0.0]), (b, vec![0.9, 0.3, 0.1])]);
        let names = name_voices(&segments, &sightings, &alike);
        assert_eq!(names[&a], "Hai Song");
        assert_eq!(names[&b], "Hai Song");
        // Two people in one room: they don't.
        let apart = BTreeMap::from([(a, vec![1.0, 0.0, 0.0]), (b, vec![0.0, 1.0, 0.0])]);
        let names = name_voices(&segments, &sightings, &apart);
        assert_eq!(names[&a], "Hai Song (1)");
        assert_eq!(names[&b], "Hai Song (2)");
    }

    #[test]
    fn a_quiet_speaker_under_a_busy_tile_gets_their_name() {
        let (a, b) = (SYSTEM_SPEAKERS, SYSTEM_SPEAKERS + 1);
        let segments = vec![
            seg(Source::System, 0, 60_000, Some(a)),
            seg(Source::System, 60_000, 66_000, Some(b)),
        ];
        // Hai Song's tile lights over everyone; Xu speaks once, briefly.
        let mut sightings: Vec<Seen> = (0..67).map(|i| seen(i * 1_000, "Hai Song")).collect();
        sightings.extend((60..67).map(|i| seen(i * 1_000, "Xu")));
        let names = name_voices(&segments, &sightings, &BTreeMap::new());
        assert_eq!(names[&a], "Hai Song");
        assert_eq!(names[&b], "Xu");
        // A name that lit up all over isn't pinned on a voice by leftovers.
        sightings.extend((0..40).map(|i| seen(i * 1_000 + 500, "Xu")));
        let names = name_voices(&segments, &sightings, &BTreeMap::new());
        assert!(!names.contains_key(&b));
    }
}
