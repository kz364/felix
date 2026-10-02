//! Voices remembered across meetings. Every voice Felix tells apart is
//! kept as a fingerprint (`voices/remembered.json` in the app data, never
//! sent anywhere); in later meetings a voice that clearly matches one is the
//! same person, so a name given once (by the user, or by the call app)
//! carries over. Voices nobody named yet are "Unknown voice N", the same N
//! in every meeting they turn up in.
//!
//! The user's own voice is kept apart, per mic (see [`super::voiceprint`]):
//! on a call the mic's main voice is the user, which is how the print for
//! that mic is made and kept up to date.
//!
//! Each voice keeps what every meeting added to it, so when the user says
//! who spoke a paragraph the meeting's part is made again from the windows
//! that person actually spoke in (see [`corrected_prints`]).
//!
//! Settings → Meetings → Remembered voices lists them to rename, merge or
//! delete, and can switch remembering off.

use super::diarize::ME;
use super::transcript::Source;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tauri::AppHandle;

/// A meeting's voices' fingerprints, by speaker number (written by the
/// pipeline).
pub const PRINTS_FILE: &str = "prints.json";
/// Which remembered voice each of a meeting's voices was matched to.
pub const LINKS_FILE: &str = "remembered.json";
const STORE: &str = "voices/remembered.json";

/// A voice this close to a remembered one (cosine)...
const MATCH: f32 = 0.75;
/// ...and this much closer than to any other remembered one is the same
/// person. Tuned on AMI series (same people over four meetings): 38 voices
/// recognised, 1 wrong, 10 missed (`speaker_eval remember`).
const MARGIN: f32 = 0.05;
/// Voices with fewer windows than this (about 15 s) aren't remembered.
const MIN_WINDOWS: usize = 10;
/// A remembered print stops moving much after this many windows.
const MAX_WEIGHT: usize = 2_000;
/// Meetings kept apart per voice; older ones are folded into `base` (and
/// can't be corrected any more).
const KEEP_MEETINGS: usize = 50;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Print {
    pub print: Vec<f32>,
    pub windows: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct RememberedVoice {
    pub id: u32,
    /// None until someone names them.
    pub name: Option<String>,
    /// For "Unknown voice N".
    pub number: u32,
    pub meetings: u32,
    /// Unix ms.
    pub last_heard: i64,
    #[serde(default)]
    #[specta(skip)]
    pub print: Vec<f32>,
    #[serde(default)]
    pub windows: usize,
    /// What each meeting added, by meeting id.
    #[serde(default)]
    #[specta(skip)]
    pub from: BTreeMap<String, Print>,
    /// Added before meetings were kept apart, or by meetings too old to
    /// keep apart.
    #[serde(default)]
    #[specta(skip)]
    pub base: Option<Print>,
}

impl RememberedVoice {
    pub fn label(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(|| format!("Unknown voice {}", self.number))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Store {
    pub enabled: bool,
    pub voices: Vec<RememberedVoice>,
    /// Meetings started before this (unix ms) teach nothing: they were
    /// recorded while voices were told apart badly.
    #[serde(default)]
    pub learn_since: i64,
}

impl Default for Store {
    fn default() -> Self {
        Store {
            enabled: true,
            voices: Vec::new(),
            learn_since: 0,
        }
    }
}

fn store_path(app_data: &Path) -> PathBuf {
    app_data.join(STORE)
}

pub fn load_store(app_data: &Path) -> Store {
    let mut store: Store = std::fs::read(store_path(app_data))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    // Voices remembered before meetings were kept apart.
    for v in &mut store.voices {
        if v.from.is_empty() && v.base.is_none() && !v.print.is_empty() {
            v.base = Some(Print {
                print: v.print.clone(),
                windows: v.windows,
            });
        }
    }
    store
}

pub fn save_store(app_data: &Path, store: &Store) -> Result<(), String> {
    let path = store_path(app_data);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec(store).map_err(|e| e.to_string())?)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("Couldn't save the remembered voices: {e}"))
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
    v
}

/// The windows-weighted mean of prints (each weighing at most
/// [`MAX_WEIGHT`], so a print keeps moving), unit length, with every
/// window counted.
fn mean<'a>(parts: impl IntoIterator<Item = &'a Print>) -> Option<Print> {
    let mut sum: Vec<f32> = Vec::new();
    let mut windows = 0;
    for p in parts {
        if sum.is_empty() {
            sum = vec![0.0; p.print.len()];
        } else if sum.len() != p.print.len() {
            continue;
        }
        let w = p.windows.min(MAX_WEIGHT) as f32;
        for (a, x) in sum.iter_mut().zip(&p.print) {
            *a += x * w;
        }
        windows += p.windows;
    }
    (!sum.is_empty()).then(|| Print {
        print: unit(sum),
        windows,
    })
}

/// Meetings kept apart, plus what came before: fold the oldest meetings
/// (ids sort by start time) into `base` past [`KEEP_MEETINGS`], and give
/// the whole print.
pub(super) fn combine(base: &mut Option<Print>, from: &mut BTreeMap<String, Print>) -> Print {
    while from.len() > KEEP_MEETINGS {
        let Some((_, oldest)) = from.pop_first() else {
            break;
        };
        *base = mean(base.iter().chain([&oldest]));
    }
    mean(base.iter().chain(from.values())).unwrap_or(Print {
        print: Vec::new(),
        windows: 0,
    })
}

fn recompute(v: &mut RememberedVoice) {
    let p = combine(&mut v.base, &mut v.from);
    v.print = p.print;
    v.windows = p.windows;
}

/// The remembered voice a print belongs to, if one clearly matches.
pub fn best_match(store: &Store, print: &[f32], exclude: &[u32]) -> Option<u32> {
    best_match_with(store, print, exclude, MATCH, MARGIN)
}

fn best_match_with(
    store: &Store,
    print: &[f32],
    exclude: &[u32],
    matching: f32,
    margin: f32,
) -> Option<u32> {
    let mut scores: Vec<(u32, f32)> = store
        .voices
        .iter()
        .filter(|v| !exclude.contains(&v.id) && v.print.len() == print.len())
        .map(|v| (v.id, cosine(&v.print, print)))
        .collect();
    scores.sort_by(|a, b| b.1.total_cmp(&a.1));
    let (id, best) = *scores.first()?;
    let next = scores.get(1).map_or(-1.0, |s| s.1);
    (best >= matching && best - next >= margin).then_some(id)
}

/// Match a meeting's voices to remembered ones, remembering new ones, and
/// put the meeting's part in each. Again whenever the meeting's prints
/// change: its old part comes out first. `old` are the links from last
/// time, kept where they still fit; `user` the names the user gave its
/// voices, which pick the remembered voice of that name (or one nobody
/// named). Returns voice → remembered id.
pub fn link_meeting(
    store: &mut Store,
    prints: &BTreeMap<u32, Print>,
    meeting: &str,
    now: i64,
    old: &BTreeMap<u32, u32>,
    user: &BTreeMap<u32, String>,
) -> BTreeMap<u32, u32> {
    link_meeting_with(store, prints, meeting, now, MATCH, MARGIN, old, user)
}

/// [`link_meeting`] with other thresholds (for `examples/speaker_eval`).
#[allow(clippy::too_many_arguments)]
pub fn link_meeting_with(
    store: &mut Store,
    prints: &BTreeMap<u32, Print>,
    meeting: &str,
    now: i64,
    matching: f32,
    margin: f32,
    old: &BTreeMap<u32, u32>,
    user: &BTreeMap<u32, String>,
) -> BTreeMap<u32, u32> {
    for v in &mut store.voices {
        if v.from.remove(meeting).is_some() {
            v.meetings = v.meetings.saturating_sub(1);
            recompute(v);
        }
    }
    let mut links = BTreeMap::new();
    let mut parts: BTreeMap<u32, Vec<&Print>> = BTreeMap::new();
    // Biggest voices first, so they get first pick.
    let mut order: Vec<(&u32, &Print)> = prints.iter().filter(|(v, _)| **v != ME).collect();
    order.sort_by_key(|(_, p)| std::cmp::Reverse(p.windows));
    for (&voice, p) in order {
        if p.windows < MIN_WINDOWS {
            continue;
        }
        let name = user.get(&voice).map(|n| n.trim()).filter(|n| !n.is_empty());
        let named = name.and_then(|n| {
            store
                .voices
                .iter()
                .find(|v| v.name.as_deref().is_some_and(|m| m.eq_ignore_ascii_case(n)))
        });
        // Named by the user: only that name's voice or a nameless one.
        let fits = |v: &RememberedVoice| name.is_none() || v.name.is_none();
        let taken: Vec<u32> = links.values().copied().collect();
        let kept = old.get(&voice).copied().filter(|id| {
            !taken.contains(id) && store.voices.iter().any(|v| v.id == *id && fits(v))
        });
        let id = match named.map(|v| v.id).or(kept) {
            Some(id) => id,
            None => {
                let exclude: Vec<u32> = store
                    .voices
                    .iter()
                    .filter(|v| !fits(v))
                    .map(|v| v.id)
                    .chain(taken)
                    .collect();
                match best_match_with(store, &p.print, &exclude, matching, margin) {
                    Some(id) => id,
                    None => {
                        let id = store.voices.iter().map(|v| v.id + 1).max().unwrap_or(1);
                        let number = store.voices.iter().map(|v| v.number + 1).max().unwrap_or(1);
                        store.voices.push(RememberedVoice {
                            id,
                            name: None,
                            number,
                            meetings: 0,
                            last_heard: now,
                            print: Vec::new(),
                            windows: 0,
                            from: BTreeMap::new(),
                            base: None,
                        });
                        id
                    }
                }
            }
        };
        links.insert(voice, id);
        parts.entry(id).or_default().push(p);
    }
    for (id, ps) in parts {
        if let (Some(v), Some(p)) = (store.voices.iter_mut().find(|v| v.id == id), mean(ps)) {
            v.from.insert(meeting.to_string(), p);
            v.meetings += 1;
            v.last_heard = v.last_heard.max(now);
            recompute(v);
        }
    }
    // Voices only this meeting had, and no longer has.
    store
        .voices
        .retain(|v| v.name.is_some() || !v.from.is_empty() || v.base.is_some());
    links
}

/// What the remembered voices say about a meeting: names for its voices
/// (named remembered voices), and labels for the unnamed ones heard in
/// more than one meeting.
pub fn names_for(
    store: &Store,
    links: &BTreeMap<u32, u32>,
) -> (BTreeMap<u32, String>, BTreeMap<u32, String>) {
    let (mut named, mut unknown) = (BTreeMap::new(), BTreeMap::new());
    for (voice, id) in links {
        let Some(r) = store.voices.iter().find(|v| v.id == *id) else {
            continue;
        };
        match &r.name {
            Some(n) => {
                named.insert(*voice, n.clone());
            }
            None if r.meetings >= 2 => {
                unknown.insert(*voice, r.label());
            }
            None => {}
        }
    }
    (named, unknown)
}

/// Learn names: the user's always win; others only name a voice nobody
/// named yet.
pub fn learn(
    store: &mut Store,
    links: &BTreeMap<u32, u32>,
    user: &BTreeMap<u32, String>,
    seen: &BTreeMap<u32, String>,
) {
    for (voice, id) in links {
        let Some(r) = store.voices.iter_mut().find(|v| v.id == *id) else {
            continue;
        };
        if let Some(n) = user.get(voice).filter(|n| !n.trim().is_empty()) {
            r.name = Some(n.trim().to_string());
        } else if r.name.is_none() {
            if let Some(n) = seen.get(voice) {
                r.name = Some(n.clone());
            }
        }
    }
}

/// Keep the user's print for this mic up to date from a call (the mic's
/// main voice is the user's), with this meeting's part made again.
fn update_my_print(
    app_data: &Path,
    mic: &str,
    meeting: &str,
    prints: &BTreeMap<u32, Print>,
    now: i64,
) {
    if !super::voiceprint::enrolls_from(mic) {
        return;
    }
    let me = prints.get(&ME).filter(|p| p.windows >= MIN_WINDOWS);
    let mut vp = match super::voiceprint::load_full(app_data, mic) {
        Some(vp) => vp,
        None if me.is_some() => super::voiceprint::Voiceprint {
            mic: mic.to_string(),
            embedding: Vec::new(),
            windows: 0,
            updated_at: now,
            from: BTreeMap::new(),
            base: None,
        },
        None => return,
    };
    if vp.from.is_empty() && vp.base.is_none() && !vp.embedding.is_empty() {
        vp.base = Some(Print {
            print: vp.embedding.clone(),
            windows: vp.windows,
        });
    }
    let before = vp.from.get(meeting).cloned();
    match me {
        Some(p) => vp.from.insert(meeting.to_string(), p.clone()),
        None => vp.from.remove(meeting),
    };
    if vp.from.get(meeting) == before.as_ref() {
        return;
    }
    let p = combine(&mut vp.base, &mut vp.from);
    vp.embedding = p.print;
    vp.windows = p.windows;
    vp.updated_at = now.max(vp.updated_at);
    let _ = super::voiceprint::save(app_data, &vp);
}

/// Each voice's print in a meeting as the transcript now says (with the
/// user's speaker fixes, not the turns Felix guessed): every fingerprint window goes to the speaker of
/// the paragraph its middle falls in. On a call the mic's unlabelled
/// paragraphs are the user's. None when the meeting has no windows kept
/// (transcribed before they were).
pub fn corrected_prints(dir: &Path, call: bool) -> Option<BTreeMap<u32, Print>> {
    let tracks = super::windows::load(dir)?;
    let t = super::pipeline::load(dir)?;
    let paragraphs =
        super::transcript::fixed_paragraphs(&t.segments, &super::manager::user_speaker_fixes(dir));
    let mut sums: BTreeMap<u32, (Vec<f32>, usize)> = BTreeMap::new();
    for (source, (wins, embs)) in &tracks {
        let mine: Vec<_> = paragraphs.iter().filter(|p| p.source == *source).collect();
        for (&(from, to), e) in wins.iter().zip(embs) {
            let mid = (from + to) as u64 * super::transcript::FRAME_MS / 2;
            let Some(p) = mine.iter().find(|p| p.start_ms <= mid && mid < p.end_ms) else {
                continue;
            };
            let speaker = match (source, p.speaker) {
                (Source::Mic, None) if call => ME,
                (Source::System, Some(ME)) => continue,
                (_, Some(s)) => s,
                _ => continue,
            };
            let (sum, n) = sums
                .entry(speaker)
                .or_insert_with(|| (vec![0.0; e.len()], 0));
            if sum.len() != e.len() {
                continue;
            }
            for (a, x) in sum.iter_mut().zip(e) {
                *a += x;
            }
            *n += 1;
        }
    }
    Some(
        sums.into_iter()
            .map(|(s, (sum, windows))| {
                (
                    s,
                    Print {
                        print: unit(sum),
                        windows,
                    },
                )
            })
            .collect(),
    )
}

/// For a transcribed meeting: link its voices (again, as the transcript
/// now says), learn the names the user and the call app gave, and say what
/// the remembered voices name. `seen` are names the call app or extension
/// put on voices.
pub fn apply(
    dir: &Path,
    user: &BTreeMap<u32, String>,
    seen: &BTreeMap<u32, String>,
) -> (BTreeMap<u32, String>, BTreeMap<u32, String>) {
    let Some(app_data) = dir.parent().and_then(Path::parent) else {
        return Default::default();
    };
    let Some(meeting) = dir.file_name().and_then(|n| n.to_str()) else {
        return Default::default();
    };
    let mut store = load_store(app_data);
    if !store.enabled {
        return Default::default();
    }
    if super::manager::read_info(dir).is_some_and(|i| i.started_at < store.learn_since) {
        return Default::default();
    }
    let old: Option<BTreeMap<u32, u32>> = super::summary::load_json(dir, LINKS_FILE);
    // Linked before meetings were kept apart: its part is in the voices'
    // `base`, and linking again would count it twice.
    let legacy = old.as_ref().is_some_and(|l| {
        !store.voices.iter().any(|v| v.from.contains_key(meeting))
            && store
                .voices
                .iter()
                .any(|v| v.base.is_some() && l.values().any(|id| *id == v.id))
    });
    let links = match old {
        Some(l) if legacy => l,
        old => {
            let info = super::manager::read_info(dir);
            let call = info
                .as_ref()
                .is_some_and(|i| i.mode == super::MeetingMode::Call);
            let Some(prints) =
                corrected_prints(dir, call).or_else(|| super::summary::load_json(dir, PRINTS_FILE))
            else {
                return Default::default();
            };
            let now = info.as_ref().map_or(0, |i| i.started_at);
            let links = link_meeting(
                &mut store,
                &prints,
                meeting,
                now,
                &old.unwrap_or_default(),
                user,
            );
            if let Some(i) = info.filter(|_| call) {
                update_my_print(app_data, &i.mic, meeting, &prints, now);
            }
            let _ = super::summary::save_json(dir, LINKS_FILE, &links);
            links
        }
    };
    learn(&mut store, &links, user, seen);
    if let Err(e) = save_store(app_data, &store) {
        log::warn!("{e}");
    }
    names_for(&store, &links)
}

/// The user says this call is a clean recording of them: their print for
/// its mic takes the mic's main voice from it, even for a meeting from
/// before `learn_since`. Returns the mic's name.
pub fn learn_my_voice(dir: &Path) -> Result<String, String> {
    let app_data = dir.parent().and_then(Path::parent).ok_or("No app folder")?;
    let meeting = dir
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("No meeting")?;
    let info = super::manager::read_info(dir).ok_or("This meeting can't be read")?;
    if info.mode != super::MeetingMode::Call {
        return Err("Only a call tells which voice on the mic is yours".into());
    }
    if !super::voiceprint::enrolls_from(&info.mic) {
        return Err(format!(
            "{} is a dictation mic: it hears you unlike a meeting does",
            info.mic
        ));
    }
    let prints = corrected_prints(dir, true)
        .or_else(|| super::summary::load_json(dir, PRINTS_FILE))
        .ok_or("Transcribe this meeting first")?;
    if prints.get(&ME).is_none_or(|p| p.windows < MIN_WINDOWS) {
        return Err("You don't talk enough in this meeting to learn from".into());
    }
    update_my_print(app_data, &info.mic, meeting, &prints, info.started_at);
    Ok(info.mic)
}

#[tauri::command]
#[specta::specta]
pub fn learn_my_voice_from(app: AppHandle, id: String) -> Result<String, String> {
    learn_my_voice(&super::manager::meeting_dir(&app, &id)?)
}

// ---- Settings ----

fn app_data(app: &AppHandle) -> Result<PathBuf, String> {
    crate::portable::app_data_dir(app).map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Serialize, specta::Type)]
pub struct RememberedVoices {
    pub enabled: bool,
    pub voices: Vec<RememberedVoice>,
}

#[tauri::command]
#[specta::specta]
pub fn remembered_voices(app: AppHandle) -> Result<RememberedVoices, String> {
    let store = load_store(&app_data(&app)?);
    let mut voices = store.voices;
    voices.sort_by_key(|v| std::cmp::Reverse(v.last_heard));
    for v in &mut voices {
        v.print.clear();
        v.from.clear();
        v.base = None;
    }
    Ok(RememberedVoices {
        enabled: store.enabled,
        voices,
    })
}

fn change(app: &AppHandle, f: impl FnOnce(&mut Store) -> Result<(), String>) -> Result<(), String> {
    let dir = app_data(app)?;
    let mut store = load_store(&dir);
    f(&mut store)?;
    save_store(&dir, &store)
}

#[tauri::command]
#[specta::specta]
pub fn set_remember_voices(app: AppHandle, on: bool) -> Result<(), String> {
    change(&app, |s| {
        s.enabled = on;
        Ok(())
    })
}

#[tauri::command]
#[specta::specta]
pub fn rename_remembered_voice(app: AppHandle, id: u32, name: String) -> Result<(), String> {
    change(&app, |s| {
        let v = s
            .voices
            .iter_mut()
            .find(|v| v.id == id)
            .ok_or("Not found")?;
        let n = name.trim();
        v.name = (!n.is_empty()).then(|| n.to_string());
        Ok(())
    })
}

/// Two remembered voices are one person: `drop` goes into `keep`.
#[tauri::command]
#[specta::specta]
pub fn merge_remembered_voices(app: AppHandle, keep: u32, drop: u32) -> Result<(), String> {
    change(&app, |s| merge(s, keep, drop))
}

pub fn merge(s: &mut Store, keep: u32, drop: u32) -> Result<(), String> {
    if keep == drop {
        return Ok(());
    }
    let i = s
        .voices
        .iter()
        .position(|v| v.id == drop)
        .ok_or("Not found")?;
    let gone = s.voices.remove(i);
    let k = s
        .voices
        .iter_mut()
        .find(|v| v.id == keep)
        .ok_or("Not found")?;
    k.meetings += gone.meetings;
    for (meeting, p) in gone.from {
        let both = match k.from.remove(&meeting) {
            Some(mine) => {
                k.meetings = k.meetings.saturating_sub(1);
                mean([&mine, &p])
            }
            None => Some(p),
        };
        if let Some(both) = both {
            k.from.insert(meeting, both);
        }
    }
    k.base = mean(k.base.iter().chain(gone.base.iter()));
    recompute(k);
    k.last_heard = k.last_heard.max(gone.last_heard);
    if k.name.is_none() {
        k.name = gone.name;
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn delete_remembered_voice(app: AppHandle, id: u32) -> Result<(), String> {
    change(&app, |s| {
        s.voices.retain(|v| v.id != id);
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn print(v: &[f32], windows: usize) -> Print {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        Print {
            print: v.iter().map(|x| x / n).collect(),
            windows,
        }
    }

    fn none<K: Ord, V>() -> BTreeMap<K, V> {
        BTreeMap::new()
    }

    #[test]
    fn a_voice_heard_again_is_the_same_remembered_voice_and_keeps_its_name() {
        let mut store = Store::default();
        let first = BTreeMap::from([
            (100, print(&[1.0, 0.0, 0.0], 40)),
            (101, print(&[0.0, 1.0, 0.0], 30)),
            (102, print(&[0.0, 0.0, 1.0], 3)), // too little to remember
        ]);
        let links = link_meeting(&mut store, &first, "m1", 1, &none(), &none());
        assert_eq!(links.len(), 2);
        assert_eq!(store.voices.len(), 2);
        // The user names 100 Sam Rivera.
        let user = BTreeMap::from([(100, "Sam Rivera".to_string())]);
        learn(&mut store, &links, &user, &BTreeMap::new());

        // Next meeting: Sam is voice 101 now, a new person is 100.
        let second = BTreeMap::from([
            (101, print(&[0.95, 0.05, 0.0], 50)),
            (100, print(&[0.0, 0.1, 1.0], 20)),
        ]);
        let links2 = link_meeting(&mut store, &second, "m2", 2, &none(), &none());
        let (named, unknown) = names_for(&store, &links2);
        assert_eq!(named.get(&101).map(String::as_str), Some("Sam Rivera"));
        assert_eq!(store.voices.len(), 3);
        // Heard once, so not labelled yet.
        assert!(unknown.is_empty());
    }

    #[test]
    fn merging_keeps_the_name_and_both_meetings() {
        let mut store = Store::default();
        link_meeting(
            &mut store,
            &BTreeMap::from([(100, print(&[1.0, 0.0], 20))]),
            "m1",
            1,
            &none(),
            &none(),
        );
        link_meeting(
            &mut store,
            &BTreeMap::from([(100, print(&[0.0, 1.0], 20))]),
            "m2",
            2,
            &none(),
            &none(),
        );
        store.voices[1].name = Some("Priya".into());
        let (a, b) = (store.voices[0].id, store.voices[1].id);
        merge(&mut store, a, b).unwrap();
        assert_eq!(store.voices.len(), 1);
        assert_eq!(store.voices[0].name.as_deref(), Some("Priya"));
        assert_eq!(store.voices[0].meetings, 2);
    }

    #[test]
    fn linking_a_meeting_again_replaces_its_part() {
        let mut store = Store::default();
        let sam = print(&[1.0, 0.0, 0.0], 40);
        link_meeting(
            &mut store,
            &BTreeMap::from([(100, sam.clone())]),
            "m1",
            1,
            &none(),
            &none(),
        );
        // Meeting 2: Sam and someone else, first heard as one voice.
        let mixed = BTreeMap::from([(100, print(&[0.8, 0.6, 0.0], 60))]);
        let links = link_meeting(&mut store, &mixed, "m2", 2, &none(), &none());
        let sam_id = links[&100];
        // The user split them: 100 is Sam again, 101 someone new.
        let fixed = BTreeMap::from([
            (100, print(&[1.0, 0.0, 0.0], 30)),
            (101, print(&[0.0, 1.0, 0.0], 30)),
        ]);
        let links = link_meeting(&mut store, &fixed, "m2", 2, &links, &none());
        assert_eq!(links[&100], sam_id);
        let sam_voice = store.voices.iter().find(|v| v.id == sam_id).unwrap();
        assert_eq!(sam_voice.meetings, 2);
        assert_eq!(sam_voice.windows, 70);
        assert!(sam_voice.print[0] > 0.999, "the mixed part is gone");
        assert_eq!(store.voices.len(), 2);
    }

    #[test]
    fn a_name_the_user_gives_picks_that_remembered_voice() {
        let mut store = Store::default();
        let first = BTreeMap::from([(100, print(&[1.0, 0.0], 40)), (101, print(&[0.0, 1.0], 40))]);
        let links = link_meeting(&mut store, &first, "m1", 1, &none(), &none());
        let names = BTreeMap::from([(100, "Sam".to_string()), (101, "Aditya".to_string())]);
        learn(&mut store, &links, &names, &none());
        // Later the user says voice 100 (wrongly linked to Sam) is Aditya.
        let user = BTreeMap::from([(100, "Aditya".to_string())]);
        let again = link_meeting(&mut store, &first, "m1", 1, &links, &user);
        assert_eq!(again[&100], links[&101]);
        learn(&mut store, &again, &user, &none());
        assert!(store
            .voices
            .iter()
            .any(|v| v.name.as_deref() == Some("Sam")));
    }

    #[test]
    fn old_meetings_fold_into_the_base() {
        let mut base = None;
        let mut from: BTreeMap<String, Print> = (0..KEEP_MEETINGS + 2)
            .map(|i| (format!("m{i:03}"), print(&[1.0, 0.0], 10)))
            .collect();
        let all = combine(&mut base, &mut from);
        assert_eq!(from.len(), KEEP_MEETINGS);
        assert_eq!(base.map(|b| b.windows), Some(20));
        assert_eq!(all.windows, (KEEP_MEETINGS + 2) * 10);
    }
}
