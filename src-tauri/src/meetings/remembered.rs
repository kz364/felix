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
//! Settings → Meetings → Remembered voices lists them to rename, merge or
//! delete, and can switch remembering off.

use super::diarize::ME;
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
}

impl Default for Store {
    fn default() -> Self {
        Store {
            enabled: true,
            voices: Vec::new(),
        }
    }
}

fn store_path(app_data: &Path) -> PathBuf {
    app_data.join(STORE)
}

pub fn load_store(app_data: &Path) -> Store {
    std::fs::read(store_path(app_data))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
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

fn blend(into: &mut Vec<f32>, weight: usize, add: &[f32], add_weight: usize) {
    if into.len() != add.len() {
        *into = add.to_vec();
        return;
    }
    let (w, a) = (weight.min(MAX_WEIGHT) as f32, add_weight as f32);
    for (x, y) in into.iter_mut().zip(add) {
        *x = (*x * w + y * a) / (w + a).max(1.0);
    }
    let n = into.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 0.0 {
        into.iter_mut().for_each(|x| *x /= n);
    }
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

/// Match a meeting's voices to remembered ones, remembering new ones, once
/// per meeting (the links are kept with it). Returns voice → remembered id.
pub fn link_meeting(
    store: &mut Store,
    prints: &BTreeMap<u32, Print>,
    now: i64,
) -> BTreeMap<u32, u32> {
    link_meeting_with(store, prints, now, MATCH, MARGIN)
}

/// [`link_meeting`] with other thresholds (for `examples/speaker_eval`).
pub fn link_meeting_with(
    store: &mut Store,
    prints: &BTreeMap<u32, Print>,
    now: i64,
    matching: f32,
    margin: f32,
) -> BTreeMap<u32, u32> {
    let mut links = BTreeMap::new();
    // Biggest voices first, so they get first pick.
    let mut order: Vec<(&u32, &Print)> = prints.iter().filter(|(v, _)| **v != ME).collect();
    order.sort_by_key(|(_, p)| std::cmp::Reverse(p.windows));
    for (&voice, p) in order {
        if p.windows < MIN_WINDOWS {
            continue;
        }
        let taken: Vec<u32> = links.values().copied().collect();
        let id = match best_match_with(store, &p.print, &taken, matching, margin) {
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
                });
                id
            }
        };
        if let Some(v) = store.voices.iter_mut().find(|v| v.id == id) {
            blend(&mut v.print, v.windows, &p.print, p.windows);
            v.windows += p.windows;
            v.meetings += 1;
            v.last_heard = v.last_heard.max(now);
        }
        links.insert(voice, id);
    }
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
/// main voice is the user's).
fn update_my_print(app_data: &Path, mic: &str, prints: &BTreeMap<u32, Print>, now: i64) {
    let Some(me) = prints.get(&ME).filter(|p| p.windows >= MIN_WINDOWS) else {
        return;
    };
    if !super::voiceprint::enrolls_from(mic) {
        return;
    }
    let (mut embedding, windows) = match super::voiceprint::load_full(app_data, mic) {
        Some(p) => (p.embedding, p.windows),
        None => (Vec::new(), 0),
    };
    blend(&mut embedding, windows, &me.print, me.windows);
    let _ = super::voiceprint::save(
        app_data,
        &super::voiceprint::Voiceprint {
            mic: mic.to_string(),
            embedding,
            windows: windows + me.windows,
            updated_at: now,
        },
    );
}

/// For a transcribed meeting: link its voices (once), learn the names the
/// user and the call app gave, and say what the remembered voices name.
/// `seen` are names the call app or extension put on voices.
pub fn apply(
    dir: &Path,
    user: &BTreeMap<u32, String>,
    seen: &BTreeMap<u32, String>,
) -> (BTreeMap<u32, String>, BTreeMap<u32, String>) {
    let Some(app_data) = dir.parent().and_then(Path::parent) else {
        return Default::default();
    };
    let mut store = load_store(app_data);
    if !store.enabled {
        return Default::default();
    }
    let links: BTreeMap<u32, u32> = match super::summary::load_json(dir, LINKS_FILE) {
        Some(l) => l,
        None => {
            let Some(prints) = super::summary::load_json::<BTreeMap<u32, Print>>(dir, PRINTS_FILE)
            else {
                return Default::default();
            };
            let info = super::manager::read_info(dir);
            let now = info.as_ref().map_or(0, |i| i.started_at);
            let links = link_meeting(&mut store, &prints, now);
            if let Some(i) = info.filter(|i| i.mode == super::MeetingMode::Call) {
                update_my_print(app_data, &i.mic, &prints, now);
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
    blend(&mut k.print, k.windows, &gone.print, gone.windows);
    k.windows += gone.windows;
    k.meetings += gone.meetings;
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

    #[test]
    fn a_voice_heard_again_is_the_same_remembered_voice_and_keeps_its_name() {
        let mut store = Store::default();
        let first = BTreeMap::from([
            (100, print(&[1.0, 0.0, 0.0], 40)),
            (101, print(&[0.0, 1.0, 0.0], 30)),
            (102, print(&[0.0, 0.0, 1.0], 3)), // too little to remember
        ]);
        let links = link_meeting(&mut store, &first, 1);
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
        let links2 = link_meeting(&mut store, &second, 2);
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
            1,
        );
        link_meeting(
            &mut store,
            &BTreeMap::from([(100, print(&[0.0, 1.0], 20))]),
            2,
        );
        store.voices[1].name = Some("Priya".into());
        let (a, b) = (store.voices[0].id, store.voices[1].id);
        merge(&mut store, a, b).unwrap();
        assert_eq!(store.voices.len(), 1);
        assert_eq!(store.voices[0].name.as_deref(), Some("Priya"));
        assert_eq!(store.voices[0].meetings, 2);
    }
}
