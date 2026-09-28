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
const TTL_MS: u64 = 1_500;
/// A voice is named only with this many sightings…
const MIN_VOTES: usize = 3;
/// …and when this share of its sightings agree.
const MIN_SHARE: f32 = 0.6;
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
];

/// Teams marks the talking tile by id rather than words.
fn is_speaker_id(text: &str) -> bool {
    let t = text.to_lowercase();
    t.contains("speakername") || t.contains("active-speaker") || t.contains("activespeaker")
}

fn looks_like_name(text: &str) -> bool {
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
/// [`SYSTEM_SPEAKERS`]), where the sightings clearly agree.
pub fn name_voices(segments: &[Segment], seen: &[Seen]) -> BTreeMap<u32, String> {
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
        .into_iter()
        .filter_map(|(id, v)| winner(&v, MIN_VOTES).map(|name| (id, name)))
        .collect()
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

/// After transcription: the names the call app gave, by speaker id. Writes
/// the transcript back when it had to split the system track by name.
pub fn apply(dir: &Path) -> BTreeMap<u32, String> {
    let seen = load(dir);
    if seen.is_empty() {
        return BTreeMap::new();
    }
    let Some(mut transcript) = super::pipeline::load(dir) else {
        return BTreeMap::new();
    };
    let told_apart = transcript
        .segments
        .iter()
        .any(|s| s.source == Source::System && s.speaker.is_some());
    if told_apart {
        return name_voices(&transcript.segments, &seen);
    }
    let names = split_by_name(&mut transcript.segments, &seen);
    if !names.is_empty() {
        if let Err(e) = super::pipeline::save(dir, &transcript) {
            log::warn!("Couldn't save the transcript with names: {e}");
            return BTreeMap::new();
        }
    }
    names
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
        let names = name_voices(&segments, &sightings);
        assert_eq!(names.get(&a).unwrap(), "Sam");
        assert_eq!(names.get(&b).unwrap(), "Ana");
        // Too few sightings: no name.
        assert!(!name_voices(&segments, &sightings[..2]).contains_key(&a));
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
}
