//! Meetings: record a call or an in-person meeting to disk, as separate mic
//! and system-audio tracks, for transcription afterwards. Separate from
//! dictation: its own mic stream, no gain or VAD at capture time.

pub mod capture;
pub mod diarize;
mod jobs;
pub mod level;
pub mod llm;
pub mod manager;
mod mic;
pub mod pipeline;
pub mod remote;
pub mod summary;
#[cfg(target_os = "macos")]
mod system_audio;
pub mod track;
pub mod transcript;

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

/// What transcribes meetings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum MeetingTranscriber {
    /// The model selected for dictation, on this Mac.
    #[default]
    Local,
    /// OpenAI's transcription API, with the OpenAI key from cleanup providers.
    Openai,
    /// Groq's Whisper API, with the Groq key from cleanup providers.
    Groq,
}
pub use manager::MeetingManager;
