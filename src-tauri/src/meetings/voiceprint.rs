//! The user's voice as a fingerprint, one per mic: the same voice sounds
//! different through a clip mic and a laptop mic, so a print only matches
//! on the mic it was made with. In person, the speaker matching it becomes
//! "Me" (see [`super::diarize::mark_me`]).
//!
//! Prints are made from calls: there the mic's main voice is the user (see
//! [`super::remembered`]). With no file for a mic, meetings number every
//! voice as before.

use super::remembered::Print;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const DIR: &str = "voiceprints";

#[derive(Serialize, Deserialize)]
pub struct Voiceprint {
    /// The mic's name, as the meeting records it.
    pub mic: String,
    /// Mean of the user's unit-length speaker embeddings, unit length.
    pub embedding: Vec<f32>,
    /// How many 1.5 s windows went into it.
    pub windows: usize,
    /// Unix milliseconds.
    pub updated_at: i64,
    /// What each call added, by meeting id, to make again when the user
    /// says who spoke what.
    #[serde(default)]
    pub from: BTreeMap<String, Print>,
    /// Added before calls were kept apart, or by calls too old to keep.
    #[serde(default)]
    pub base: Option<Print>,
}

/// Mics made for dictation (clip-on and wireless lav receivers): never
/// used for a meeting, and never used to make a print (they hear the user
/// 2 cm away in a voice unlike the one the room hears).
pub fn is_dictation_mic(mic: &str) -> bool {
    let mic = mic.to_lowercase();
    [
        "wireless mic",
        "dji",
        "rode wireless",
        "lavalier",
        "lav mic",
    ]
    .iter()
    .any(|m| mic.contains(m))
}

pub fn enrolls_from(mic: &str) -> bool {
    !is_dictation_mic(mic)
}

fn file_for(app_data: &Path, mic: &str) -> PathBuf {
    let slug: String = mic
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    app_data
        .join(DIR)
        .join(format!("{}.json", slug.trim_matches('-')))
}

/// The user's print for this mic, if there is one.
pub fn load(app_data: &Path, mic: &str) -> Option<Vec<f32>> {
    let print: Voiceprint =
        serde_json::from_slice(&std::fs::read(file_for(app_data, mic)).ok()?).ok()?;
    (print.mic == mic && !print.embedding.is_empty()).then_some(print.embedding)
}

/// The user's print for this mic or, without one, the one made from the
/// most speech on another mic: the same voice through a different mic is
/// still nearer than anyone else's.
pub fn load_or_nearest(app_data: &Path, mic: &str) -> Option<Vec<f32>> {
    load(app_data, mic).or_else(|| {
        std::fs::read_dir(app_data.join(DIR))
            .ok()?
            .flatten()
            .filter_map(|e| {
                serde_json::from_slice::<Voiceprint>(&std::fs::read(e.path()).ok()?).ok()
            })
            .filter(|p| !p.embedding.is_empty())
            .max_by_key(|p| p.windows)
            .map(|p| p.embedding)
    })
}

/// The whole print for this mic, to add to.
pub fn load_full(app_data: &Path, mic: &str) -> Option<Voiceprint> {
    let print: Voiceprint =
        serde_json::from_slice(&std::fs::read(file_for(app_data, mic)).ok()?).ok()?;
    (print.mic == mic).then_some(print)
}

pub fn save(app_data: &Path, print: &Voiceprint) -> Result<(), String> {
    let path = file_for(app_data, &print.mic);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_vec(print).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prints_are_kept_per_mic_and_not_made_on_dictation_mics() {
        assert!(!enrolls_from("Wireless Mic Rx"));
        assert!(!enrolls_from("DJI Mic 2"));
        assert!(is_dictation_mic("DJI MIC MINI"));
        assert!(!is_dictation_mic("MacBook Pro Microphone"));
        assert!(enrolls_from("MacBook Pro Microphone"));

        let dir = std::env::temp_dir().join(format!("felix-voiceprint-{}", std::process::id()));
        let print = Voiceprint {
            mic: "MacBook Pro Microphone".into(),
            embedding: vec![0.6, 0.8],
            windows: 40,
            updated_at: 0,
            from: BTreeMap::new(),
            base: None,
        };
        save(&dir, &print).unwrap();
        assert_eq!(load(&dir, "MacBook Pro Microphone"), Some(vec![0.6, 0.8]));
        assert_eq!(load(&dir, "Studio Display Microphone"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
