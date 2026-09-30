//! The user's voice as a fingerprint, one per mic: the same voice sounds
//! different through a clip mic and a laptop mic, so a print only matches
//! on the mic it was made with. In person, the speaker matching it becomes
//! "Me" (see [`super::diarize::mark_me`]).
//!
//! Nothing records a print yet: with no file for a mic, meetings number
//! every voice as before.

use serde::{Deserialize, Serialize};
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
}

/// Mics made for dictation (clip-on and wireless lav receivers) aren't
/// used to make a print: they hear the user 2 cm away in a voice unlike the
/// one the room hears.
pub fn enrolls_from(mic: &str) -> bool {
    let mic = mic.to_lowercase();
    ![
        "wireless mic",
        "dji",
        "rode wireless",
        "lavalier",
        "lav mic",
    ]
    .iter()
    .any(|m| mic.contains(m))
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
        assert!(enrolls_from("MacBook Pro Microphone"));

        let dir = std::env::temp_dir().join(format!("felix-voiceprint-{}", std::process::id()));
        let print = Voiceprint {
            mic: "MacBook Pro Microphone".into(),
            embedding: vec![0.6, 0.8],
            windows: 40,
            updated_at: 0,
        };
        save(&dir, &print).unwrap();
        assert_eq!(load(&dir, "MacBook Pro Microphone"), Some(vec![0.6, 0.8]));
        assert_eq!(load(&dir, "Studio Display Microphone"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
