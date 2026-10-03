//! The language model meetings use for cleanup and summaries: Felix's
//! ChatGPT model (with the ChatGPT sign-in), or the provider picked for
//! dictation cleanup (an API-key provider or the local model).

use super::MeetingLlm;
use crate::settings::{AppSettings, PostProcessProvider};
use serde_json::Value;

pub enum Llm {
    Chatgpt {
        model: String,
    },
    Provider {
        provider: PostProcessProvider,
        model: String,
        api_key: String,
    },
    Local {
        model: String,
        keep_loaded: bool,
    },
}

/// How much text one request may carry, in characters.
pub struct Budget {
    /// Transcript text per cleanup request.
    pub cleanup: usize,
    /// Transcript text per summary request; longer meetings are summarised
    /// in sections first.
    pub summary: usize,
}

/// The ChatGPT model for reading a whole transcript closely (cleanup, who
/// holds the floor), whatever the assistant's model is: it keeps names,
/// numbers and who-said-what straight better than a faster one.
pub const CAREFUL_CHATGPT_MODEL: &str = "gpt-6.1-sol";

impl Llm {
    /// This model, or [`CAREFUL_CHATGPT_MODEL`] when it's ChatGPT.
    pub fn careful(&self) -> Option<Llm> {
        match self {
            Llm::Chatgpt { .. } => Some(Llm::Chatgpt {
                model: CAREFUL_CHATGPT_MODEL.into(),
            }),
            _ => None,
        }
    }

    /// The model the settings pick, or why there's none.
    pub fn from_settings(settings: &AppSettings) -> Result<Self, String> {
        match settings.meeting_llm {
            MeetingLlm::Chatgpt => {
                if !chatgpt_signed_in() {
                    return Err("Sign in with ChatGPT (Felix settings) to summarise meetings, or pick the cleanup model in Meeting Settings".into());
                }
                Ok(Llm::Chatgpt {
                    model: settings.assistant_model.clone(),
                })
            }
            MeetingLlm::Cleanup => {
                let provider = settings
                    .active_post_process_provider()
                    .cloned()
                    .ok_or("No cleanup provider is selected")?;
                let model = settings
                    .post_process_models
                    .get(&provider.id)
                    .cloned()
                    .unwrap_or_default();
                if model.trim().is_empty() {
                    return Err(format!("{} has no model selected", provider.label));
                }
                if provider.id == crate::settings::APPLE_INTELLIGENCE_PROVIDER_ID {
                    return Err("Apple Intelligence can't summarise meetings; pick another provider or ChatGPT".into());
                }
                if provider.id == crate::local_llm::LOCAL_PROVIDER_ID {
                    return Ok(Llm::Local {
                        model,
                        keep_loaded: settings.local_model_keep_loaded,
                    });
                }
                let api_key = settings
                    .post_process_api_keys
                    .get(&provider.id)
                    .cloned()
                    .unwrap_or_default();
                Ok(Llm::Provider {
                    provider,
                    model,
                    api_key,
                })
            }
        }
    }

    pub fn budget(&self) -> Budget {
        match self {
            // Small context (4k tokens) and short replies.
            Llm::Local { .. } => Budget {
                cleanup: 1_200,
                summary: 5_000,
            },
            _ => Budget {
                cleanup: 6_000,
                summary: 120_000,
            },
        }
    }

    pub fn label(&self) -> String {
        match self {
            Llm::Chatgpt { model } => format!("ChatGPT {model}"),
            Llm::Provider {
                provider, model, ..
            } => format!("{} {model}", provider.label),
            Llm::Local { model, .. } => format!("local {model}"),
        }
    }

    /// Ask for a JSON reply following `schema`. `effort` is the reasoning
    /// effort where the model has one ("low", "medium").
    pub async fn ask_json(
        &self,
        instructions: &str,
        input: &str,
        schema: &Value,
        effort: &str,
    ) -> Result<Value, String> {
        let reply = match self {
            Llm::Chatgpt { model } => {
                chatgpt_complete(model, effort, instructions, input, schema).await?
            }
            Llm::Provider {
                provider,
                model,
                api_key,
            } => {
                let instructions = if provider.supports_structured_output {
                    instructions.to_string()
                } else {
                    format!(
                        "{instructions}\n\nReply with only JSON matching this schema:\n{schema}"
                    )
                };
                crate::llm_client::send_chat_completion_with_schema(
                    provider,
                    api_key.clone(),
                    model,
                    input.to_string(),
                    Some(instructions),
                    provider.supports_structured_output.then(|| schema.clone()),
                    matches!(provider.id.as_str(), "custom" | "openrouter"),
                )
                .await?
                .ok_or("The model sent an empty reply")?
            }
            Llm::Local { model, keep_loaded } => {
                let instructions = format!(
                    "{instructions}\n\nReply with only JSON matching this schema:\n{schema}"
                );
                crate::local_llm::complete(model, &instructions, input, *keep_loaded).await?
            }
        };
        parse_json(&reply)
    }
}

#[cfg(target_os = "macos")]
fn chatgpt_signed_in() -> bool {
    crate::chatgpt::signed_in_as().is_some()
}

#[cfg(not(target_os = "macos"))]
fn chatgpt_signed_in() -> bool {
    false
}

#[cfg(target_os = "macos")]
async fn chatgpt_complete(
    model: &str,
    effort: &str,
    instructions: &str,
    input: &str,
    schema: &Value,
) -> Result<String, String> {
    crate::chatgpt::complete(crate::chatgpt::Request {
        model,
        effort,
        instructions,
        input,
        schema: Some(schema),
    })
    .await
}

#[cfg(not(target_os = "macos"))]
async fn chatgpt_complete(
    _model: &str,
    _effort: &str,
    _instructions: &str,
    _input: &str,
    _schema: &Value,
) -> Result<String, String> {
    Err("ChatGPT is only available on macOS".into())
}

/// The JSON object in a reply, skipping a `<think>` block or code fences.
pub fn parse_json(reply: &str) -> Result<Value, String> {
    let text = match reply.find("</think>") {
        Some(end) => &reply[end + "</think>".len()..],
        None => reply,
    };
    let start = text.find('{').ok_or("The model didn't reply with JSON")?;
    let end = text.rfind('}').ok_or("The model didn't reply with JSON")?;
    if end < start {
        return Err("The model didn't reply with JSON".into());
    }
    serde_json::from_str(&text[start..=end])
        .or_else(|e| {
            // Small local models sometimes stop before the last brackets.
            close_brackets(text[start..].trim_end())
                .and_then(|closed| serde_json::from_str(&closed).ok())
                .ok_or(e)
        })
        .map_err(|e| format!("The model's reply wasn't valid JSON: {e}"))
}

/// `json` with the brackets it leaves open closed, if it ends outside a
/// string.
fn close_brackets(json: &str) -> Option<String> {
    let mut open = Vec::new();
    let (mut in_string, mut escaped) = (false, false);
    for c in json.chars() {
        match c {
            _ if escaped => escaped = false,
            '\\' if in_string => escaped = true,
            '"' => in_string = !in_string,
            '{' | '[' if !in_string => open.push(if c == '{' { '}' } else { ']' }),
            '}' | ']' if !in_string && open.pop() != Some(c) => return None,
            _ => {}
        }
    }
    if in_string || open.is_empty() {
        return None;
    }
    Some(json.chars().chain(open.into_iter().rev()).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn replies_cut_before_the_last_brackets_are_closed() {
        let cut = r#"{"a":[{"b":"x}]"}],"c":["d"]"#;
        assert_eq!(
            super::parse_json(cut).unwrap(),
            serde_json::json!({"a":[{"b":"x}]"}],"c":["d"]})
        );
        assert!(super::parse_json(r#"{"a":"unfinished"#).is_err());
    }

    use super::*;

    #[test]
    fn json_is_found_in_chatty_replies() {
        let v = parse_json("<think>hmm {no}</think>\n```json\n{\"a\": 1}\n```").unwrap();
        assert_eq!(v["a"], 1);
        assert!(parse_json("no json here").is_err());
    }
}
