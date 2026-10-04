//! Meetings: record a call or an in-person meeting to disk, as separate mic
//! and system-audio tracks, for transcription afterwards. Separate from
//! dictation: its own mic stream, no gain or VAD at capture time.

pub mod active_speaker;
pub mod aec;
pub mod ask;
mod awake;
pub mod calendar;
pub mod call_apps;
pub mod capture;
pub mod clues;
pub mod detect;
pub mod diarize;
pub mod echo;
pub mod extension;
pub mod floor;
mod jobs;
pub use jobs::split_runaways;
pub mod language;
pub mod level;
pub mod live;
pub mod llm;
pub mod manager;
mod mic;
pub mod pipeline;
pub mod remembered;
pub mod remote;
pub mod segment;
pub mod speaker_score;
pub mod speakers;
pub mod summary;
#[cfg(target_os = "macos")]
mod system_audio;
pub mod track;
pub mod transcript;
pub mod voiceprint;
pub mod watch;
pub mod windows;

pub use capture::{MeetingMode, Recording};

use serde::{Deserialize, Serialize};

/// The model that tidies and summarises meeting transcripts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingLlm {
    /// Felix's model, with the ChatGPT sign-in.
    #[default]
    Chatgpt,
    /// The provider and model chosen for dictation cleanup.
    Cleanup,
}

/// What transcribes meetings. Meetings are cloud-first: local models
/// aren't good enough for long, many-voiced audio, and nobody is waiting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingTranscriber {
    /// A cloud provider whose key is set (OpenAI, then Groq), otherwise the
    /// model on this Mac.
    #[default]
    Auto,
    /// The model on this Mac chosen for meetings (see [`local_model`]).
    Local,
    /// OpenAI's transcription API, with the OpenAI key from cleanup providers.
    Openai,
    /// Groq's Whisper API, with the Groq key from cleanup providers.
    Groq,
    /// The model picked for OpenRouter, with the OpenRouter key.
    Openrouter,
    /// ElevenLabs Scribe, with the ElevenLabs key.
    Elevenlabs,
}
pub use manager::MeetingManager;

/// The model on this Mac that transcribes meetings: the one chosen for them,
/// if it's downloaded and runs through transcribe.cpp, otherwise dictation's.
pub fn local_model(
    settings: &crate::settings::AppSettings,
    models: &[crate::managers::model::ModelInfo],
) -> String {
    let chosen = &settings.meeting_model;
    if chosen.is_empty() || *chosen == settings.selected_model {
        return settings.selected_model.clone();
    }
    let usable = models.iter().any(|m| {
        m.id == *chosen
            && m.is_downloaded
            && matches!(
                m.engine_type,
                crate::managers::model::EngineType::TranscribeCpp
            )
    });
    if usable {
        chosen.clone()
    } else {
        log::warn!("The meeting model {chosen} isn't downloaded; meetings use the dictation model");
        settings.selected_model.clone()
    }
}

/// How a meeting transcript made on this Mac by `model` is labelled, so the
/// final pass reuses only what the live pass heard with the same model.
pub fn local_engine(model: &str) -> String {
    format!("local {model}")
}
