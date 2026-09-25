//! Transcribing meeting chunks with a provider's API instead of the local
//! model: OpenAI or Groq, through their documented `/audio/transcriptions`
//! endpoint and the API key already saved for that provider in cleanup
//! settings. (Never the ChatGPT sign-in: see the spec's decisions.)
//!
//! Chunks are at most 15 s, so each is one small WAV upload; timestamps,
//! echo masking and resuming work the same as with the local model.

use super::MeetingTranscriber;
use crate::settings::AppSettings;
use std::time::Duration;

pub struct Remote {
    pub name: &'static str,
    url: String,
    pub model: &'static str,
    api_key: String,
    /// Vocabulary, as a spelling hint.
    prompt: Option<String>,
    /// The spoken language, when the user picked one.
    language: Option<String>,
    attempts: usize,
    client: reqwest::Client,
}

/// Providers with a documented, API-key transcription endpoint: id, name,
/// model.
pub const PROVIDERS: &[(&str, &str, &str)] = &[
    ("openai", "OpenAI", "gpt-transcribe"),
    ("groq", "Groq", "whisper-large-v3-turbo"),
];

/// Whisper-style prompts are cut at 224 tokens; stay well under.
const PROMPT_CHARS: usize = 600;
const ATTEMPTS: usize = 5;

impl Remote {
    /// The provider the meeting settings pick, or `None` for the local
    /// model.
    pub fn from_settings(settings: &AppSettings) -> Result<Option<Self>, String> {
        let provider_id = match settings.meeting_transcriber {
            MeetingTranscriber::Local => return Ok(None),
            MeetingTranscriber::Openai => "openai",
            MeetingTranscriber::Groq => "groq",
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
        let &(_, name, model) = PROVIDERS
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
        let mut prompt = String::new();
        for word in &settings.custom_words {
            if prompt.len() + word.len() + 2 > PROMPT_CHARS {
                break;
            }
            if !prompt.is_empty() {
                prompt.push_str(", ");
            }
            prompt.push_str(word);
        }
        let language = Some(settings.selected_language.clone())
            .filter(|l| !l.is_empty() && l != "auto")
            .map(|l| l.split(['-', '_']).next().unwrap_or(&l).to_lowercase());
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(if quick { 5 } else { 10 }))
            .timeout(Duration::from_secs(if quick { 30 } else { 90 }))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            name,
            url: format!(
                "{}/audio/transcriptions",
                provider.base_url.trim_end_matches('/')
            ),
            model,
            api_key,
            prompt: (!prompt.is_empty()).then_some(prompt),
            language,
            attempts: if quick { 2 } else { ATTEMPTS },
            client,
        })
    }

    /// Transcribe one chunk (16 kHz mono). Retries rate limits and server
    /// errors with backoff; other errors fail straight away.
    pub async fn transcribe(&self, audio: &[f32]) -> Result<String, String> {
        let wav = wav_bytes(audio);
        let mut delay = Duration::from_secs(if self.attempts < ATTEMPTS { 1 } else { 2 });
        let attempts = self.attempts;
        for attempt in 1..=attempts {
            let mut form = reqwest::multipart::Form::new()
                .text("model", self.model)
                .text("response_format", "json")
                .part(
                    "file",
                    reqwest::multipart::Part::bytes(wav.clone())
                        .file_name("chunk.wav")
                        .mime_str("audio/wav")
                        .map_err(|e| e.to_string())?,
                );
            if let Some(prompt) = &self.prompt {
                form = form.text("prompt", prompt.clone());
            }
            if let Some(language) = &self.language {
                form = form.text("language", language.clone());
            }
            let response = self
                .client
                .post(&self.url)
                .bearer_auth(&self.api_key)
                .multipart(form)
                .send()
                .await;
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
                return Ok(body["text"].as_str().unwrap_or("").trim().to_string());
            }
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<f64>().ok())
                .map(Duration::from_secs_f64);
            let body = response.text().await.unwrap_or_default();
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

/// The provider's error message, never the request (which holds the key).
fn error_message(name: &str, status: reqwest::StatusCode, body: &str) -> String {
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let message = parsed["error"]["message"]
        .as_str()
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
            model: "gpt-transcribe",
            api_key: "sk-test".into(),
            prompt: Some("Handy, Rivera".into()),
            language: None,
            attempts: ATTEMPTS,
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
        assert!(req.contains("name=\"model\"\r\n\r\ngpt-transcribe"));
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
}
