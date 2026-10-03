//! Transcribing with a provider's API instead of the local model, through
//! each provider's documented API-key endpoint and the key already saved for
//! it under Models → Accounts. (Never the ChatGPT sign-in: see the spec's
//! decisions.)
//!
//! - OpenAI and Groq: multipart upload to `/audio/transcriptions`.
//! - OpenRouter: the same path, with the audio base64 in JSON; one key
//!   reaches many models, picked in settings.
//! - ElevenLabs Scribe: multipart upload to `/speech-to-text`.
//!
//! Meeting chunks are at most 15 s and dictations a few minutes, so each is
//! one small WAV upload; timestamps, echo masking and resuming work the same
//! as with the local model.

use super::MeetingTranscriber;
use crate::settings::AppSettings;
use base64::Engine;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How a provider takes the audio and the vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// Whisper-style multipart upload; vocabulary as a spelling `prompt`.
    Whisper,
    /// OpenAI's gpt-transcribe: the same upload, vocabulary as literal
    /// `keywords[]` and the language as `languages[]` (it replaces the
    /// singular field; sending both is rejected).
    Keywords,
    /// OpenRouter: base64 audio in JSON, vocabulary as `keyterms`.
    OpenRouter,
    /// ElevenLabs Scribe: multipart with `model_id`, vocabulary as `keyterms`.
    Scribe,
}

pub struct Remote {
    pub name: &'static str,
    url: String,
    pub model: String,
    api_key: String,
    shape: Shape,
    /// The user's vocabulary.
    words: Vec<String>,
    /// A note for the model (the languages a meeting mixes).
    note: Option<String>,
    /// The spoken language, when the user picked one.
    language: Option<String>,
    attempts: usize,
    /// OpenRouter said this model can't take `keyterms`: send none.
    no_keyterms: AtomicBool,
    client: reqwest::Client,
}

/// What a transcription cost, when the provider says.
pub struct Transcribed {
    pub text: String,
    /// US dollars (OpenRouter's `usage.cost`).
    pub cost: Option<f64>,
}

/// Providers with a documented, API-key transcription endpoint: id, name,
/// model (OpenRouter's is the default; the setting picks it).
pub const PROVIDERS: &[(&str, &str, &str)] = &[
    ("openai", "OpenAI", "gpt-transcribe"),
    ("groq", "Groq", "whisper-large-v3-turbo"),
    ("openrouter", "OpenRouter", DEFAULT_OPENROUTER_MODEL),
    ("elevenlabs", "ElevenLabs", "scribe_v2"),
];

/// The ones meetings' "Auto" and the benchmark's reference pick from: the
/// newer providers are chosen by hand, so adding them changes nothing (and
/// costs nothing) for anyone who already has their keys.
pub const AUTO_PROVIDERS: &[&str] = &["openai", "groq"];

pub const DEFAULT_OPENROUTER_MODEL: &str = "microsoft/mai-transcribe-2";

/// Whisper-style prompts are cut at 224 tokens; stay well under.
const PROMPT_CHARS: usize = 600;
/// Vocabulary terms sent as keywords/keyterms. ElevenLabs bills every
/// request as at least 20 s past 100 terms, and few vocabularies are longer.
const MAX_TERMS: usize = 100;
/// The strictest documented term limits (ElevenLabs: under 50 characters, at
/// most 5 words; xAI 50 characters), so one list works everywhere.
const TERM_CHARS: usize = 49;
const TERM_WORDS: usize = 5;
const ATTEMPTS: usize = 5;

fn shape(provider_id: &str) -> Shape {
    match provider_id {
        "openai" => Shape::Keywords,
        "openrouter" => Shape::OpenRouter,
        "elevenlabs" => Shape::Scribe,
        _ => Shape::Whisper,
    }
}

/// Vocabulary as literal terms: one line each, none of the characters the
/// providers reject (`<>` for OpenAI, also `{}[]\` for ElevenLabs), short
/// enough for all of them.
fn terms(words: &[String]) -> Vec<String> {
    words
        .iter()
        .map(|w| {
            w.chars()
                .filter(|c| !matches!(c, '<' | '>' | '{' | '}' | '[' | ']' | '\\'))
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|w| {
            !w.is_empty() && w.chars().count() <= TERM_CHARS && w.split(' ').count() <= TERM_WORDS
        })
        .take(MAX_TERMS)
        .collect()
}

impl Remote {
    /// The provider the meeting settings pick, or `None` for the local
    /// model.
    pub fn from_settings(settings: &AppSettings) -> Result<Option<Self>, String> {
        let provider_id = match settings.meeting_transcriber {
            MeetingTranscriber::Auto => {
                return Ok(AUTO_PROVIDERS
                    .iter()
                    .find_map(|id| Self::for_provider(settings, id, false).ok()))
            }
            MeetingTranscriber::Local => return Ok(None),
            MeetingTranscriber::Openai => "openai",
            MeetingTranscriber::Groq => "groq",
            MeetingTranscriber::Openrouter => "openrouter",
            MeetingTranscriber::Elevenlabs => "elevenlabs",
        };
        Self::for_provider(settings, provider_id, false).map(Some)
    }

    /// The provider dictation uses (`transcription_provider`), or `None`
    /// for the model on this Mac.
    pub fn for_dictation(settings: &AppSettings) -> Result<Option<Self>, String> {
        match settings.transcription_provider.as_str() {
            "" | "local" => Ok(None),
            id => Self::for_provider(settings, id, true).map(Some),
        }
    }

    /// `quick`: for dictation, where someone is waiting (fewer retries,
    /// shorter time limit).
    pub fn for_provider(
        settings: &AppSettings,
        provider_id: &str,
        quick: bool,
    ) -> Result<Self, String> {
        let &(id, name, default_model) = PROVIDERS
            .iter()
            .find(|(id, _, _)| *id == provider_id)
            .ok_or_else(|| format!("{provider_id} can't transcribe"))?;
        let provider = settings
            .post_process_provider(provider_id)
            .ok_or_else(|| format!("{name} isn't set up"))?;
        let api_key = settings
            .post_process_api_keys
            .get(provider_id)
            .cloned()
            .unwrap_or_default();
        if api_key.trim().is_empty() {
            return Err(format!(
                "Add an {name} API key (Models → Accounts) to transcribe with {name}"
            ));
        }
        let model = match id {
            "openrouter" => Some(settings.openrouter_transcription_model.trim())
                .filter(|m| !m.is_empty())
                .unwrap_or(DEFAULT_OPENROUTER_MODEL)
                .to_string(),
            _ => default_model.to_string(),
        };
        let shape = shape(id);
        let language = Some(settings.selected_language.clone())
            .filter(|l| !l.is_empty() && l != "auto")
            .map(|l| l.split(['-', '_']).next().unwrap_or(&l).to_lowercase());
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(if quick { 5 } else { 10 }))
            .timeout(Duration::from_secs(if quick { 30 } else { 90 }))
            .build()
            .map_err(|e| e.to_string())?;
        let base = provider.base_url.trim_end_matches('/');
        Ok(Self {
            name,
            url: match shape {
                Shape::Scribe => format!("{base}/speech-to-text"),
                _ => format!("{base}/audio/transcriptions"),
            },
            model,
            api_key,
            shape,
            words: settings.custom_words.clone(),
            note: None,
            language,
            attempts: if quick { 2 } else { ATTEMPTS },
            no_keyterms: AtomicBool::new(false),
            client,
        })
    }

    /// For a meeting in these languages: one is passed as the language;
    /// several are left to the model, with a note that they mix.
    pub fn for_languages(mut self, languages: &[String]) -> Self {
        match languages {
            [] => {}
            [one] => self.language = Some(one.clone()),
            many => {
                self.language = None;
                self.note = super::language::prompt_for(many);
            }
        }
        self
    }

    /// The Whisper-style prompt: the note, then as much vocabulary as fits.
    fn prompt(&self) -> Option<String> {
        let mut prompt = String::new();
        for word in self
            .words
            .iter()
            .map(|w| w.trim())
            .filter(|w| !w.is_empty())
        {
            if prompt.len() + word.len() + 2 > PROMPT_CHARS {
                break;
            }
            if !prompt.is_empty() {
                prompt.push_str(", ");
            }
            prompt.push_str(word);
        }
        match (&self.note, prompt.is_empty()) {
            (Some(note), true) => Some(note.clone()),
            (Some(note), false) => Some(format!("{note} {prompt}")),
            (None, true) => None,
            (None, false) => Some(prompt),
        }
    }

    fn file_part(wav: &[u8]) -> Result<reqwest::multipart::Part, String> {
        reqwest::multipart::Part::bytes(wav.to_vec())
            .file_name("chunk.wav")
            .mime_str("audio/wav")
            .map_err(|e| e.to_string())
    }

    /// One attempt's request.
    fn request(&self, wav: &[u8], keyterms: bool) -> Result<reqwest::RequestBuilder, String> {
        let post = self.client.post(&self.url);
        Ok(match self.shape {
            Shape::Whisper | Shape::Keywords => {
                let mut form = reqwest::multipart::Form::new()
                    .text("model", self.model.clone())
                    .text("response_format", "json")
                    .part("file", Self::file_part(wav)?);
                if self.shape == Shape::Keywords {
                    if let Some(note) = &self.note {
                        form = form.text("prompt", note.clone());
                    }
                    for term in terms(&self.words) {
                        form = form.text("keywords[]", term);
                    }
                    if let Some(language) = &self.language {
                        form = form.text("languages[]", language.clone());
                    }
                } else {
                    if let Some(prompt) = self.prompt() {
                        form = form.text("prompt", prompt);
                    }
                    if let Some(language) = &self.language {
                        form = form.text("language", language.clone());
                    }
                }
                post.bearer_auth(&self.api_key).multipart(form)
            }
            Shape::OpenRouter => {
                let mut body = serde_json::json!({
                    "model": self.model,
                    "input_audio": {
                        "data": base64::engine::general_purpose::STANDARD.encode(wav),
                        "format": "wav",
                    },
                });
                if let Some(language) = &self.language {
                    body["language"] = language.clone().into();
                }
                let terms = terms(&self.words);
                if keyterms && !terms.is_empty() {
                    body["keyterms"] = terms.into();
                }
                // Whisper models take vocabulary only as a prompt, passed
                // under the provider serving them (only that one's options
                // are forwarded).
                if let Some(prompt) = self.prompt().filter(|_| self.model.contains("whisper")) {
                    body["provider"] = serde_json::json!({
                        "options": { "groq": { "prompt": prompt }, "openai": { "prompt": prompt } }
                    });
                }
                post.bearer_auth(&self.api_key).json(&body)
            }
            Shape::Scribe => {
                let mut form = reqwest::multipart::Form::new()
                    .text("model_id", self.model.clone())
                    .text("tag_audio_events", "false")
                    .part("file", Self::file_part(wav)?);
                if let Some(language) = &self.language {
                    form = form.text("language_code", language.clone());
                }
                for term in terms(&self.words) {
                    form = form.text("keyterms", term);
                }
                post.header("xi-api-key", &self.api_key).multipart(form)
            }
        })
    }

    /// Transcribe one chunk (16 kHz mono). Retries rate limits and server
    /// errors with backoff; other errors fail straight away.
    pub async fn transcribe(&self, audio: &[f32]) -> Result<String, String> {
        self.transcribe_wav(&wav_bytes(audio)).await.map(|t| t.text)
    }

    /// [`Self::transcribe`] for WAV bytes, with the cost when the provider
    /// reports it.
    pub async fn transcribe_wav(&self, wav: &[u8]) -> Result<Transcribed, String> {
        let mut delay = Duration::from_secs(if self.attempts < ATTEMPTS { 1 } else { 2 });
        let attempts = self.attempts;
        let mut attempt = 0;
        while attempt < attempts {
            attempt += 1;
            let keyterms = !self.no_keyterms.load(Ordering::Relaxed);
            let response = self.request(wav, keyterms)?.send().await;
            let response = match response {
                Ok(r) => r,
                Err(e) if attempt < attempts => {
                    log::warn!("{} transcription request failed, retrying: {e}", self.name);
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                    continue;
                }
                Err(e) => return Err(format!("Couldn't reach {}: {e}", self.name)),
            };
            let status = response.status();
            if status.is_success() {
                let body: serde_json::Value = response
                    .json()
                    .await
                    .map_err(|e| format!("{} sent an unreadable reply: {e}", self.name))?;
                return Ok(Transcribed {
                    text: body["text"].as_str().unwrap_or("").trim().to_string(),
                    cost: body["usage"]["cost"].as_f64(),
                });
            }
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<f64>().ok())
                .map(Duration::from_secs_f64);
            let body = response.text().await.unwrap_or_default();
            // OpenRouter rejects keyterms for a model that can't use them:
            // the vocabulary is a hint, so go on without it.
            if self.shape == Shape::OpenRouter
                && keyterms
                && status.as_u16() == 400
                && body.to_lowercase().contains("keyterm")
            {
                log::warn!(
                    "{} {} takes no vocabulary; sending none",
                    self.name,
                    self.model
                );
                self.no_keyterms.store(true, Ordering::Relaxed);
                attempt -= 1;
                continue;
            }
            let retryable = status.as_u16() == 429 || status.is_server_error();
            if retryable && attempt < attempts {
                let wait = retry_after.unwrap_or(delay).min(Duration::from_secs(60));
                log::warn!(
                    "{} transcription got {status}, retrying in {:.0}s",
                    self.name,
                    wait.as_secs_f64()
                );
                tokio::time::sleep(wait).await;
                delay *= 2;
                continue;
            }
            return Err(error_message(self.name, status, &body));
        }
        Err(format!("{} kept failing", self.name))
    }
}

/// A speech-to-text model OpenRouter offers.
#[derive(Debug, Clone, serde::Serialize, specta::Type)]
pub struct OpenRouterModel {
    pub id: String,
    pub name: String,
}

/// OpenRouter's speech-to-text models, from its public model list (no key
/// sent: the list is open).
#[tauri::command]
#[specta::specta]
pub async fn openrouter_transcription_models() -> Result<Vec<OpenRouterModel>, String> {
    let body: serde_json::Value = reqwest::Client::new()
        .get("https://openrouter.ai/api/v1/models?output_modalities=transcription")
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| format!("Couldn't reach OpenRouter: {e}"))?
        .error_for_status()
        .map_err(|e| format!("OpenRouter's model list failed: {e}"))?
        .json()
        .await
        .map_err(|e| format!("OpenRouter sent an unreadable model list: {e}"))?;
    let mut models: Vec<OpenRouterModel> = body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str()?.to_string();
            let name = m["name"].as_str().unwrap_or(&id).to_string();
            Some(OpenRouterModel { id, name })
        })
        .collect();
    models.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(models)
}

/// The provider's error message, never the request (which holds the key).
fn error_message(name: &str, status: reqwest::StatusCode, body: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let message = parsed["error"]["message"]
        .as_str()
        .or(parsed["detail"]["message"].as_str())
        .or(parsed["detail"].as_str())
        .unwrap_or_else(|| body.get(..body.len().min(200)).unwrap_or(""))
        .trim();
    if message.is_empty() {
        format!("{name} transcription failed ({status})")
    } else {
        format!("{name} transcription failed ({status}): {message}")
    }
}

/// 16 kHz mono 16-bit WAV in memory.
pub fn wav_bytes(audio: &[f32]) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::with_capacity(44 + audio.len() * 2));
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16_000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    {
        let mut w = hound::WavWriter::new(&mut out, spec).expect("in-memory WAV");
        for s in audio {
            let _ = w.write_sample((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16);
        }
        let _ = w.finalize();
    }
    out.into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A one-request server: answers with `status` and `body`, returns the request.
    async fn serve_once(
        status: &'static str,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 65536];
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req);
                if let Some(head_end) = text.find("\r\n\r\n") {
                    let len = text[..head_end]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if req.len() >= head_end + 4 + len {
                        break;
                    }
                }
                if n == 0 {
                    break;
                }
            }
            let reply = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(reply.as_bytes()).await.unwrap();
            req
        });
        (url, handle)
    }

    fn remote(url: &str) -> Remote {
        Remote {
            name: "Test",
            url: format!("{url}/audio/transcriptions"),
            model: "whisper-large-v3-turbo".into(),
            api_key: "sk-test".into(),
            shape: Shape::Whisper,
            words: vec!["Handy".into(), "Rivera".into()],
            note: None,
            language: None,
            attempts: ATTEMPTS,
            no_keyterms: AtomicBool::new(false),
            client: reqwest::Client::new(),
        }
    }

    #[tokio::test]
    async fn a_chunk_is_sent_as_a_multipart_upload_and_the_text_comes_back() {
        let (url, server) = serve_once("200 OK", r#"{"text":" Hello Sam. "}"#).await;
        let text = remote(&url).transcribe(&[0.1; 1600]).await.unwrap();
        assert_eq!(text, "Hello Sam.");
        let req = String::from_utf8_lossy(&server.await.unwrap()).to_string();
        assert!(req.starts_with("POST /audio/transcriptions"));
        assert!(
            req.contains("authorization: Bearer sk-test")
                || req.contains("Authorization: Bearer sk-test")
        );
        assert!(req.contains("name=\"model\"\r\n\r\nwhisper-large-v3-turbo"));
        assert!(req.contains("name=\"prompt\"\r\n\r\nHandy, Rivera"));
        assert!(req.contains("filename=\"chunk.wav\""));
        assert!(req.contains("RIFF"));
    }

    #[tokio::test]
    async fn a_bad_key_fails_without_retrying() {
        let (url, _server) = serve_once(
            "401 Unauthorized",
            r#"{"error":{"message":"Incorrect API key provided"}}"#,
        )
        .await;
        let e = remote(&url).transcribe(&[0.0; 160]).await.unwrap_err();
        assert!(e.contains("Incorrect API key"), "{e}");
    }

    #[test]
    fn chunks_upload_as_valid_wav() {
        let bytes = wav_bytes(&[0.0, 0.5, -0.5]);
        let reader = hound::WavReader::new(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(reader.spec().sample_rate, 16_000);
        assert_eq!(reader.duration(), 3);
    }

    #[test]
    fn errors_show_the_providers_message() {
        let e = error_message(
            "OpenAI",
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"Incorrect API key provided"}}"#,
        );
        assert_eq!(
            e,
            "OpenAI transcription failed (401 Unauthorized): Incorrect API key provided"
        );
    }

    #[tokio::test]
    async fn dictation_uses_the_chosen_provider_quickly_with_the_language() {
        let (url, server) = serve_once("200 OK", r#"{"text":"Hi Sam"}"#).await;
        let mut settings = crate::settings::get_default_settings();
        assert!(
            Remote::for_dictation(&settings).unwrap().is_none(),
            "local by default"
        );

        settings.transcription_provider = "groq".into();
        let err = Remote::for_dictation(&settings).err().unwrap();
        assert!(err.contains("API key"), "{err}");

        settings
            .post_process_api_keys
            .insert("groq".into(), "sk-test".into());
        for p in &mut settings.post_process_providers {
            if p.id == "groq" {
                p.base_url = url.clone();
            }
        }
        settings.selected_language = "en".into();
        settings.custom_words = vec!["Rivera".into()];
        let remote = Remote::for_dictation(&settings).unwrap().unwrap();
        assert_eq!(remote.attempts, 2);
        assert_eq!(remote.model, "whisper-large-v3-turbo");
        assert_eq!(remote.transcribe(&[0.1; 1600]).await.unwrap(), "Hi Sam");
        let req = String::from_utf8_lossy(&server.await.unwrap()).to_string();
        assert!(req.contains("name=\"language\"\r\n\r\nen"));
        assert!(req.contains("name=\"prompt\"\r\n\r\nRivera"));
    }

    fn with_shape(url: &str, shape: Shape, model: &str) -> Remote {
        Remote {
            shape,
            model: model.into(),
            words: vec!["Rivera".into(), "kube<ctl>".into(), "".into()],
            language: Some("en".into()),
            ..remote(url)
        }
    }

    #[tokio::test]
    async fn openai_gets_the_vocabulary_as_keywords_and_the_language_as_languages() {
        let (url, server) = serve_once("200 OK", r#"{"text":"Hi Sam"}"#).await;
        let remote = with_shape(&url, Shape::Keywords, "gpt-transcribe");
        assert_eq!(remote.transcribe(&[0.1; 1600]).await.unwrap(), "Hi Sam");
        let req = String::from_utf8_lossy(&server.await.unwrap()).to_string();
        assert!(req.contains("name=\"keywords[]\"\r\n\r\nRivera\r\n"));
        assert!(req.contains("name=\"keywords[]\"\r\n\r\nkubectl\r\n"));
        assert_eq!(req.matches("name=\"keywords[]\"").count(), 2);
        assert!(req.contains("name=\"languages[]\"\r\n\r\nen"));
        assert!(!req.contains("name=\"language\""), "never both");
        assert!(!req.contains("name=\"prompt\""), "no vocabulary prompt");
    }

    #[tokio::test]
    async fn openrouter_gets_base64_json_with_keyterms_and_reports_the_cost() {
        let (url, server) = serve_once(
            "200 OK",
            r#"{"text":" Hi Sam ","usage":{"seconds":0.1,"cost":0.000003}}"#,
        )
        .await;
        let remote = with_shape(&url, Shape::OpenRouter, "microsoft/mai-transcribe-2");
        let out = remote
            .transcribe_wav(&wav_bytes(&[0.1; 1600]))
            .await
            .unwrap();
        assert_eq!(out.text, "Hi Sam");
        assert_eq!(out.cost, Some(0.000003));
        let req = String::from_utf8_lossy(&server.await.unwrap()).to_string();
        let body: serde_json::Value =
            serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(body["model"], "microsoft/mai-transcribe-2");
        assert_eq!(body["input_audio"]["format"], "wav");
        let audio = base64::engine::general_purpose::STANDARD
            .decode(body["input_audio"]["data"].as_str().unwrap())
            .unwrap();
        assert!(audio.starts_with(b"RIFF"));
        assert_eq!(body["keyterms"], serde_json::json!(["Rivera", "kubectl"]));
        assert_eq!(body["language"], "en");
        assert!(body.get("provider").is_none());
    }

    #[tokio::test]
    async fn a_model_that_cant_take_keyterms_is_asked_again_without() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut bodies = Vec::new();
            for reply in [
                (
                    "400 Bad Request",
                    r#"{"error":{"message":"This model does not support keyterms"}}"#,
                ),
                ("200 OK", r#"{"text":"Hi"}"#),
            ] {
                let (mut sock, _) = listener.accept().await.unwrap();
                let mut req = Vec::new();
                let mut buf = [0u8; 65536];
                loop {
                    let n = sock.read(&mut buf).await.unwrap();
                    req.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&req);
                    let done = text.find("\r\n\r\n").is_some_and(|end| {
                        let len = text[..end]
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        req.len() >= end + 4 + len
                    });
                    if done || n == 0 {
                        break;
                    }
                }
                let out = format!(
                    "HTTP/1.1 {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    reply.0,
                    reply.1.len(),
                    reply.1
                );
                sock.write_all(out.as_bytes()).await.unwrap();
                bodies.push(String::from_utf8_lossy(&req).to_string());
            }
            bodies
        });
        let remote = with_shape(&url, Shape::OpenRouter, "meta/muse-voice-transcribe-1.0");
        assert_eq!(remote.transcribe(&[0.1; 1600]).await.unwrap(), "Hi");
        let bodies = server.await.unwrap();
        assert!(bodies[0].contains("\"keyterms\""));
        assert!(!bodies[1].contains("\"keyterms\""));
        assert!(remote.no_keyterms.load(Ordering::Relaxed), "remembered");
    }

    #[tokio::test]
    async fn whisper_on_openrouter_gets_the_vocabulary_as_a_prompt() {
        let (url, server) = serve_once("200 OK", r#"{"text":"Hi"}"#).await;
        let remote = with_shape(&url, Shape::OpenRouter, "openai/whisper-large-v3-turbo");
        remote.transcribe(&[0.1; 1600]).await.unwrap();
        let req = String::from_utf8_lossy(&server.await.unwrap()).to_string();
        let body: serde_json::Value =
            serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(
            body["provider"]["options"]["groq"]["prompt"],
            "Rivera, kube<ctl>"
        );
    }

    #[tokio::test]
    async fn scribe_gets_its_own_upload_with_keyterms_and_no_sound_tags() {
        let (url, server) = serve_once("200 OK", r#"{"text":"Hi Sam","language_code":"en"}"#).await;
        let remote = Remote {
            url: format!("{url}/speech-to-text"),
            ..with_shape(&url, Shape::Scribe, "scribe_v2")
        };
        assert_eq!(remote.transcribe(&[0.1; 1600]).await.unwrap(), "Hi Sam");
        let req = String::from_utf8_lossy(&server.await.unwrap()).to_string();
        assert!(req.starts_with("POST /speech-to-text"));
        assert!(req.to_lowercase().contains("xi-api-key: sk-test"));
        assert!(!req.to_lowercase().contains("authorization"));
        assert!(req.contains("name=\"model_id\"\r\n\r\nscribe_v2"));
        assert!(req.contains("name=\"keyterms\"\r\n\r\nRivera\r\n"));
        assert!(req.contains("name=\"tag_audio_events\"\r\n\r\nfalse"));
        assert!(req.contains("name=\"language_code\"\r\n\r\nen"));
    }

    #[test]
    fn terms_follow_the_strictest_provider_rules() {
        let words: Vec<String> = [
            "Claude Code",
            "a{b}[c]\\d",
            "one two three four five six",
            &"x".repeat(60),
            "line\nbreak",
        ]
        .iter()
        .map(|s| s.to_string())
        .chain((0..200).map(|i| format!("w{i}")))
        .collect();
        let t = terms(&words);
        assert_eq!(&t[..3], ["Claude Code", "abcd", "line break"]);
        assert_eq!(t.len(), MAX_TERMS);
    }

    #[test]
    fn elevenlabs_errors_show_their_message() {
        let e = error_message(
            "ElevenLabs",
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"detail":{"status":"invalid_api_key","message":"Invalid API key"}}"#,
        );
        assert!(e.ends_with(": Invalid API key"), "{e}");
    }

    #[tokio::test]
    async fn openrouter_dictation_uses_the_model_from_settings() {
        let (url, server) = serve_once("200 OK", r#"{"text":"Hi"}"#).await;
        let mut settings = crate::settings::get_default_settings();
        settings.transcription_provider = "openrouter".into();
        settings
            .post_process_api_keys
            .insert("openrouter".into(), "sk-test".into());
        for p in &mut settings.post_process_providers {
            if p.id == "openrouter" {
                p.base_url = url.clone();
            }
        }
        assert_eq!(
            Remote::for_dictation(&settings).unwrap().unwrap().model,
            DEFAULT_OPENROUTER_MODEL
        );
        settings.openrouter_transcription_model = "google/gemini-3.5-transcribe".into();
        let remote = Remote::for_dictation(&settings).unwrap().unwrap();
        remote.transcribe(&[0.1; 1600]).await.unwrap();
        let req = String::from_utf8_lossy(&server.await.unwrap()).to_string();
        assert!(req.starts_with("POST /audio/transcriptions"));
        assert!(req.contains("\"model\":\"google/gemini-3.5-transcribe\""));
    }

    #[test]
    fn meetings_on_auto_never_pick_the_newer_providers() {
        let mut settings = crate::settings::get_default_settings();
        settings
            .post_process_api_keys
            .insert("openrouter".into(), "sk-test".into());
        settings
            .post_process_api_keys
            .insert("elevenlabs".into(), "sk-test".into());
        assert!(Remote::from_settings(&settings).unwrap().is_none());
        settings.meeting_transcriber = MeetingTranscriber::Elevenlabs;
        assert_eq!(
            Remote::from_settings(&settings).unwrap().unwrap().name,
            "ElevenLabs"
        );
    }
}
