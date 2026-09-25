#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use crate::apple_intelligence;
use crate::audio_feedback::{play_feedback_sound, play_feedback_sound_blocking, SoundType};
use crate::audio_toolkit::{is_microphone_access_denied, is_no_input_device_error, VadPolicy};
use crate::managers::audio::AudioRecordingManager;
use crate::managers::history::HistoryManager;
use crate::managers::model::ModelManager;
use crate::managers::transcription::StreamWorkKind;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::{
    get_settings, write_settings, AppSettings, CleanupLevel, OverlayStyle,
    APPLE_INTELLIGENCE_PROVIDER_ID,
};
use crate::shortcut;
use crate::tray::{set_tray_state, TrayIconState};
use crate::utils::{
    self, show_processing_overlay, show_recording_overlay, show_transcribing_overlay,
};
use crate::TranscriptionCoordinator;
use ferrous_opencc::{config::BuiltinConfig, OpenCC};
use log::{debug, error, info, warn};
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::Manager;
use tauri::{AppHandle, Emitter};

const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(25);

#[derive(Clone, serde::Serialize)]
struct RecordingErrorEvent {
    error_type: String,
    detail: Option<String>,
}

/// Drop guard that finishes the transcription pipeline, including immediate
/// model unloading on early exits.
struct FinishGuard(AppHandle, Arc<TranscriptionManager>);
impl Drop for FinishGuard {
    fn drop(&mut self) {
        self.1.maybe_unload_immediately("transcription session");
        if let Some(c) = self.0.try_state::<TranscriptionCoordinator>() {
            c.notify_processing_finished();
        }
        // The pipeline just freed its large transient buffers (captured PCM,
        // WAV copy, engine scratch); hand the cached pages back to the OS so
        // they don't sit in malloc arenas until they get swapped out (#1792).
        crate::memory::trim_freed_memory();
    }
}

// Shortcut Action Trait
pub trait ShortcutAction: Send + Sync {
    fn start(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str);
    fn stop(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str);
}

// Transcribe Action
struct TranscribeAction {
    post_process: bool,
}

/// Field name for structured output JSON schema
const TRANSCRIPTION_FIELD: &str = "transcription";

/// Strip invisible Unicode characters that some LLMs may insert
fn strip_invisible_chars(s: &str) -> String {
    s.replace(['\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}'], "")
}

/// Strip a leading `<think>...</think>` block. Some endpoints can't disable
/// reasoning, and some local servers put the reasoning text into `content`
/// instead of a separate field — without this the user would get the model's
/// chain of thought pasted along with the cleaned transcription.
fn strip_think_block(s: &str) -> &str {
    if let Some(rest) = s.trim_start().strip_prefix("<think>") {
        if let Some(end) = rest.find("</think>") {
            return rest[end + "</think>".len()..].trim_start();
        }
    }
    s
}

/// Build a system prompt from the user's prompt template.
/// Removes `${output}` placeholder since the transcription is sent as the user message.
fn build_system_prompt(prompt_template: &str) -> String {
    prompt_template.replace("${output}", "").trim().to_string()
}

/// Returns `true` when a transcription has no meaningful content to
/// post-process (empty or whitespace-only). Used to skip the post-processing
/// LLM call when nothing was actually transcribed, which would otherwise make
/// the model reply with an error message such as "you need to provide the
/// transcription".
fn is_blank_transcription(transcription: &str) -> bool {
    transcription.trim().is_empty()
}

async fn complete_unless_cancelled<F, C>(operation: F, is_cancelled: C) -> Option<F::Output>
where
    F: Future,
    C: Fn() -> bool,
{
    tokio::pin!(operation);

    loop {
        if is_cancelled() {
            return None;
        }

        if let Ok(result) =
            tokio::time::timeout(CANCELLATION_POLL_INTERVAL, operation.as_mut()).await
        {
            return Some(result);
        }
    }
}

/// Whether custom instructions apply to the current destination.
fn has_custom_instructions(settings: &AppSettings) -> bool {
    if !settings.custom_instructions.trim().is_empty() {
        return true;
    }
    let context = crate::app_context::current();
    let (category, _) = crate::app_context::resolve(&context, &settings.app_rules);
    !settings
        .category_instructions
        .get(category)
        .trim()
        .is_empty()
}

/// The level prompt plus the user's instructions for the current destination.
fn level_prompt_with_instructions(settings: &AppSettings, level: CleanupLevel) -> Option<String> {
    let prompt = crate::cleanup::level_prompt_for(settings, level)?;
    let context = crate::app_context::current();
    let (category, _) = crate::app_context::resolve(&context, &settings.app_rules);
    Some(
        match crate::cleanup::instructions_block(
            &settings.custom_instructions,
            settings.category_instructions.get(category),
        ) {
            Some(block) => format!("{prompt}\n\n{block}"),
            None => prompt.to_string(),
        },
    )
}

/// While the user speaks, load Apple Intelligence with the exact
/// instructions the cleanup will use, so the model and prompt are ready when
/// the transcript arrives (~120 ms of its ~500 ms time to first token). A
/// mismatch (e.g. settings changed mid-dictation) just falls back to a cold
/// session.
/// Whether this cleanup provider gets the text on screen: the model on this
/// Mac always (when the setting is on), online providers only if allowed.
/// Not Apple Intelligence, whose context is too small.
fn screen_context_for(settings: &AppSettings, provider_id: &str) -> bool {
    settings.screen_context
        && settings.post_process_enabled
        && provider_id != APPLE_INTELLIGENCE_PROVIDER_ID
        && (provider_id == crate::local_llm::LOCAL_PROVIDER_ID || settings.screen_context_online)
}

/// Start the local cleanup model when Handy starts, with its instructions
/// already processed, so the first dictation has no cold start.
pub(crate) fn warm_up_at_launch(app: &AppHandle) {
    let settings = crate::rules::with_rules(get_settings(app));
    if !(uses_level_cleanup(&settings)
        && settings.post_process_provider_id == crate::local_llm::LOCAL_PROVIDER_ID)
    {
        return;
    }
    let Some(prompt) = level_prompt_with_instructions(&settings, settings.cleanup_level) else {
        return;
    };
    let model = settings
        .post_process_models
        .get(crate::local_llm::LOCAL_PROVIDER_ID)
        .cloned()
        .unwrap_or_default();
    if model.trim().is_empty() {
        return;
    }
    info!("Starting the local cleanup model ({model}) at launch");
    crate::local_llm::prewarm(
        model,
        crate::cleanup::add_context(&build_system_prompt(&prompt), &settings, None),
        crate::cleanup::CLEANUP_USER_PREFIX.to_string(),
        settings.local_model_keep_loaded,
    );
}

fn prewarm_cleanup(settings: &AppSettings, screen_generation: u64) {
    let with_screen = screen_context_for(settings, &settings.post_process_provider_id);
    if uses_level_cleanup(settings)
        && settings.post_process_provider_id == crate::local_llm::LOCAL_PROVIDER_ID
    {
        let settings = settings.clone();
        std::thread::spawn(move || {
            // Read the screen first: the local model then processes it
            // along with the instructions while the user speaks.
            let screen = if with_screen {
                match crate::screen_context::read_for(screen_generation) {
                    Some(block) => block,
                    // Cleanup already started without it.
                    None => return,
                }
            } else {
                None
            };
            if crate::app_context::current()
                .bundle_id
                .is_some_and(|b| crate::app_context::is_browser(&b))
            {
                std::thread::sleep(Duration::from_millis(800));
            }
            let Some(prompt) = level_prompt_with_instructions(&settings, settings.cleanup_level)
            else {
                return;
            };
            let model = settings
                .post_process_models
                .get(crate::local_llm::LOCAL_PROVIDER_ID)
                .cloned()
                .unwrap_or_default();
            crate::local_llm::prewarm(
                model,
                crate::cleanup::with_screen_context(
                    crate::cleanup::add_context(&build_system_prompt(&prompt), &settings, None),
                    screen.as_deref(),
                ),
                crate::cleanup::CLEANUP_USER_PREFIX.to_string(),
                settings.local_model_keep_loaded,
            );
        });
        return;
    }
    if with_screen {
        std::thread::spawn(move || {
            crate::screen_context::read_for(screen_generation);
        });
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        if !uses_level_cleanup(settings)
            || settings.post_process_provider_id != APPLE_INTELLIGENCE_PROVIDER_ID
        {
            return;
        }
        let settings = settings.clone();
        std::thread::spawn(move || {
            // Browsers resolve the tab's website (and so its category's
            // instructions) in the background; give that a moment.
            let in_browser = crate::app_context::current()
                .bundle_id
                .is_some_and(|b| crate::app_context::is_browser(&b));
            if in_browser {
                std::thread::sleep(Duration::from_millis(800));
            }
            let Some(prompt) = level_prompt_with_instructions(&settings, settings.cleanup_level)
            else {
                return;
            };
            if !apple_intelligence::check_apple_intelligence_availability() {
                return;
            }
            let system_prompt =
                crate::cleanup::add_context(&build_system_prompt(&prompt), &settings, None);
            apple_intelligence::prewarm_session(
                &system_prompt,
                crate::cleanup::CLEANUP_USER_PREFIX,
            );
            debug!("Prewarmed Apple Intelligence for cleanup");
        });
    }
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    let _ = settings;
}

/// Whether every dictation gets the level-based AI cleanup.
fn uses_level_cleanup(settings: &AppSettings) -> bool {
    settings.post_process_enabled && settings.cleanup_level != CleanupLevel::None
}

fn should_use_streaming_overlay(style: OverlayStyle, is_streaming: bool) -> bool {
    style == OverlayStyle::Live && is_streaming
}

/// Which prompt drives an LLM pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CleanupRequest {
    /// The prompt selected on the Post Processing page (post-process hotkey).
    SelectedPrompt,
    /// The built-in cleanup prompt for a level (every dictation).
    Level(CleanupLevel),
}

/// Run the LLM cleanup and apply the output guard. Returns `None` (so the
/// rule-cleaned text is used) when the model fails or its output is rejected.
async fn post_process_transcription(
    settings: &AppSettings,
    transcription: &str,
    request: CleanupRequest,
) -> Option<String> {
    let output = run_post_process_llm(settings, transcription, request).await?;
    let output = crate::cleanup::unwrap_output(&output);
    let level = match request {
        CleanupRequest::Level(level) => Some(level),
        CleanupRequest::SelectedPrompt => None,
    };
    let has_instructions =
        matches!(request, CleanupRequest::Level(_)) && has_custom_instructions(settings);
    match crate::cleanup::accept_cleanup(transcription, &output, level, has_instructions) {
        Ok(()) => Some(output),
        Err(reason) => {
            warn!(
                "Discarding LLM cleanup ({reason}); using rule-cleaned text. Output was: '{}'",
                utils::redact_text(&output)
            );
            None
        }
    }
}

/// Dictation goes to a cloud provider instead of the model on this Mac.
pub(crate) fn uses_cloud_transcription(settings: &AppSettings) -> bool {
    !matches!(settings.transcription_provider.as_str(), "" | "local")
}

/// Transcribe a dictation where the settings say: with a cloud provider,
/// falling back to the model on this Mac if that fails; or locally. Also
/// returns which model did it ("Groq/whisper-large-v3-turbo", or the local
/// model's id).
pub(crate) async fn transcribe_dictation(
    app: &AppHandle,
    tm: &TranscriptionManager,
    samples: Vec<f32>,
) -> (anyhow::Result<String>, Option<String>) {
    let settings = crate::rules::with_rules(get_settings(app));
    match crate::meetings::remote::Remote::for_dictation(&settings) {
        Ok(None) => {}
        Ok(Some(remote)) => {
            let started = Instant::now();
            match remote.transcribe(&samples).await {
                Ok(text) => {
                    info!(
                        "{} transcribed {:.1}s of audio in {:?}",
                        remote.name,
                        samples.len() as f64 / 16_000.0,
                        started.elapsed()
                    );
                    let label = format!("{}/{}", remote.name, remote.model);
                    return (Ok(tm.finish_cloud_text(text)), Some(label));
                }
                Err(e) => warn!("{e}; transcribing on this Mac instead"),
            }
        }
        Err(e) => warn!("Cloud transcription unavailable ({e}); transcribing on this Mac"),
    }
    tm.initiate_model_load();
    let result = tm.transcribe(samples);
    (result, tm.get_current_model())
}

/// ChatGPT's fastest model; cleanup needs no reasoning.
const CHATGPT_CLEANUP_MODEL: &str = "gpt-6-luna";

/// Cleanup through the ChatGPT sign-in (the user's plan).
async fn chatgpt_cleanup(model: &str, system_prompt: &str, user_content: &str) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        if crate::chatgpt::signed_in_as().is_none() {
            error!("ChatGPT cleanup selected but not signed in");
            return None;
        }
        let started = Instant::now();
        match crate::chatgpt::complete(crate::chatgpt::Request {
            model,
            effort: "none",
            instructions: system_prompt,
            input: user_content,
            schema: None,
        })
        .await
        {
            Ok(result) if !result.trim().is_empty() => {
                debug!("ChatGPT cleanup took {:?}", started.elapsed());
                Some(strip_invisible_chars(strip_think_block(&result)))
            }
            Ok(_) => None,
            Err(e) => {
                error!("ChatGPT cleanup failed: {e}");
                None
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (model, system_prompt, user_content);
        None
    }
}

async fn run_post_process_llm(
    settings: &AppSettings,
    transcription: &str,
    request: CleanupRequest,
) -> Option<String> {
    if is_blank_transcription(transcription) {
        debug!("Post-processing skipped because the transcription is empty");
        return None;
    }

    // Taken even when unused, so a late read can't leak into the next one.
    let screen = crate::screen_context::take_for_cleanup();
    let provider = match settings.active_post_process_provider().cloned() {
        Some(provider) => provider,
        None => {
            debug!("Post-processing enabled but no provider is selected");
            return None;
        }
    };
    let screen = screen.filter(|_| screen_context_for(settings, &provider.id));

    let mut model = settings
        .post_process_models
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();
    if provider.id == crate::settings::CHATGPT_PROVIDER_ID && model.trim().is_empty() {
        model = CHATGPT_CLEANUP_MODEL.to_string();
    }

    if model.trim().is_empty() {
        debug!(
            "Post-processing skipped because provider '{}' has no model configured",
            provider.id
        );
        return None;
    }

    let (prompt, user_prefix) = match request {
        CleanupRequest::Level(level) => match level_prompt_with_instructions(settings, level) {
            Some(prompt) => (prompt, crate::cleanup::CLEANUP_USER_PREFIX),
            None => return None,
        },
        CleanupRequest::SelectedPrompt => {
            let selected_prompt_id = match &settings.post_process_selected_prompt_id {
                Some(id) => id.clone(),
                None => {
                    debug!("Post-processing skipped because no prompt is selected");
                    return None;
                }
            };
            match settings
                .post_process_prompts
                .iter()
                .find(|prompt| prompt.id == selected_prompt_id)
            {
                Some(prompt) => (prompt.prompt.clone(), ""),
                None => {
                    debug!(
                        "Post-processing skipped because prompt '{}' was not found",
                        selected_prompt_id
                    );
                    return None;
                }
            }
        }
    };
    // The app name only helps free-form custom prompts; the level prompts
    // are tuned without it and styling is applied deterministically later.
    let app_name = match request {
        CleanupRequest::SelectedPrompt => crate::app_context::current().app_name,
        CleanupRequest::Level(_) => None,
    };

    if prompt.trim().is_empty() {
        debug!("Post-processing skipped because the selected prompt is empty");
        return None;
    }

    debug!(
        "Starting LLM post-processing with provider '{}' (model: {})",
        provider.id, model
    );

    let api_key = settings
        .post_process_api_keys
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();

    // Ask these providers to skip reasoning/thinking — post-processing rarely
    // benefits from it and it adds seconds of latency. llm_client picks the
    // field the endpoint understands and retries without it if rejected.
    let disable_reasoning = matches!(provider.id.as_str(), "custom" | "openrouter");

    if provider.supports_structured_output {
        debug!("Using structured outputs for provider '{}'", provider.id);

        let system_prompt = crate::cleanup::with_screen_context(
            crate::cleanup::add_context(
                &build_system_prompt(&prompt),
                settings,
                app_name.as_deref(),
            ),
            screen.as_deref(),
        );
        let user_content = if prompt.contains("${output}") {
            transcription.to_string()
        } else {
            format!(
                "{user_prefix}{}",
                crate::cleanup::wrap_transcript(transcription)
            )
        };

        if provider.id == crate::local_llm::LOCAL_PROVIDER_ID {
            return match crate::local_llm::complete(
                &model,
                &system_prompt,
                &user_content,
                settings.local_model_keep_loaded,
            )
            .await
            {
                Ok(result) if !result.trim().is_empty() => {
                    Some(strip_invisible_chars(strip_think_block(&result)))
                }
                Ok(_) => {
                    debug!("Local model returned an empty response");
                    None
                }
                Err(err) => {
                    error!("Local model post-processing failed: {}", err);
                    None
                }
            };
        }

        if provider.id == crate::settings::CHATGPT_PROVIDER_ID {
            return chatgpt_cleanup(&model, &system_prompt, &user_content).await;
        }

        // Handle Apple Intelligence separately since it uses native Swift APIs
        if provider.id == APPLE_INTELLIGENCE_PROVIDER_ID {
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            {
                if !apple_intelligence::check_apple_intelligence_availability() {
                    debug!(
                        "Apple Intelligence selected but not currently available on this device"
                    );
                    return None;
                }

                let token_limit = model.trim().parse::<i32>().unwrap_or(0);
                return match apple_intelligence::process_text_with_system_prompt(
                    &system_prompt,
                    &user_content,
                    token_limit,
                ) {
                    Ok(result) => {
                        if result.trim().is_empty() {
                            debug!("Apple Intelligence returned an empty response");
                            None
                        } else {
                            let result = strip_invisible_chars(&result);
                            debug!(
                                "Apple Intelligence post-processing succeeded. Output length: {} chars",
                                result.len()
                            );
                            Some(result)
                        }
                    }
                    Err(err) => {
                        error!("Apple Intelligence post-processing failed: {}", err);
                        None
                    }
                };
            }

            #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
            {
                debug!("Apple Intelligence provider selected on unsupported platform");
                return None;
            }
        }

        // Define JSON schema for transcription output
        let json_schema = serde_json::json!({
            "type": "object",
            "properties": {
                (TRANSCRIPTION_FIELD): {
                    "type": "string",
                    "description": "The cleaned and processed transcription text"
                }
            },
            "required": [TRANSCRIPTION_FIELD],
            "additionalProperties": false
        });

        match crate::llm_client::send_chat_completion_with_schema(
            &provider,
            api_key.clone(),
            &model,
            user_content,
            Some(system_prompt),
            Some(json_schema),
            disable_reasoning,
        )
        .await
        {
            Ok(Some(content)) => {
                // Parse the JSON response to extract the transcription field
                let content = strip_think_block(&content);
                match serde_json::from_str::<serde_json::Value>(content) {
                    Ok(json) => {
                        if let Some(transcription_value) =
                            json.get(TRANSCRIPTION_FIELD).and_then(|t| t.as_str())
                        {
                            let result = strip_invisible_chars(transcription_value);
                            debug!(
                                "Structured output post-processing succeeded for provider '{}'. Output length: {} chars",
                                provider.id,
                                result.len()
                            );
                            return Some(result);
                        } else {
                            error!("Structured output response missing 'transcription' field");
                            return Some(strip_invisible_chars(content));
                        }
                    }
                    Err(e) => {
                        error!(
                            "Failed to parse structured output JSON: {}. Returning raw content.",
                            e
                        );
                        return Some(strip_invisible_chars(content));
                    }
                }
            }
            Ok(None) => {
                error!("LLM API response has no content");
                return None;
            }
            Err(e) => {
                warn!(
                    "Structured output failed for provider '{}': {}. Falling back to legacy mode.",
                    provider.id, e
                );
                // Fall through to legacy mode below
            }
        }
    }

    // Legacy mode: Replace ${output} variable in the prompt with the actual text.
    // Prompts written as pure system prompts (no placeholder) get the wrapped
    // transcript appended instead.
    let prompt = crate::cleanup::with_screen_context(
        crate::cleanup::add_context(&prompt, settings, app_name.as_deref()),
        screen.as_deref(),
    );
    let processed_prompt = if prompt.contains("${output}") {
        prompt.replace("${output}", transcription)
    } else {
        format!(
            "{}\n\n{}{}",
            prompt,
            user_prefix,
            crate::cleanup::wrap_transcript(transcription)
        )
    };
    debug!("Processed prompt length: {} chars", processed_prompt.len());

    match crate::llm_client::send_chat_completion(
        &provider,
        api_key,
        &model,
        processed_prompt,
        disable_reasoning,
    )
    .await
    {
        Ok(Some(content)) => {
            let content = strip_invisible_chars(strip_think_block(&content));
            debug!(
                "LLM post-processing succeeded for provider '{}'. Output length: {} chars",
                provider.id,
                content.len()
            );
            Some(content)
        }
        Ok(None) => {
            error!("LLM API response has no content");
            None
        }
        Err(e) => {
            error!(
                "LLM post-processing failed for provider '{}': {}. Falling back to original transcription.",
                provider.id,
                e
            );
            None
        }
    }
}

async fn maybe_convert_chinese_variant(
    effective_language: &str,
    transcription: &str,
) -> Option<String> {
    // Gate on the language the model actually transcribed in (the effective
    // language), not the persisted intent. A leftover zh-Hans/zh-Hant intent
    // from a previously selected model must not run OpenCC S2T/T2S over output a
    // non-Chinese model produced — that would silently rewrite any shared CJK
    // characters (e.g. Japanese kanji) in the result.
    let is_simplified = effective_language == "zh-Hans";
    let is_traditional = effective_language == "zh-Hant";

    if !is_simplified && !is_traditional {
        debug!("effective language is not Simplified or Traditional Chinese; skipping conversion");
        return None;
    }

    debug!(
        "Starting Chinese variant conversion using OpenCC for language: {}",
        effective_language
    );

    // Use OpenCC to convert based on selected language
    let config = if is_simplified {
        // Convert Traditional Chinese to Simplified Chinese
        BuiltinConfig::Tw2sp
    } else {
        // Convert Simplified Chinese to Traditional Chinese
        BuiltinConfig::S2tw
    };

    match OpenCC::from_config(config) {
        Ok(converter) => {
            let converted = converter.convert(transcription);
            debug!(
                "OpenCC translation completed. Input length: {}, Output length: {}",
                transcription.len(),
                converted.len()
            );
            Some(converted)
        }
        Err(e) => {
            error!("Failed to initialize OpenCC converter: {}. Falling back to original transcription.", e);
            None
        }
    }
}

pub(crate) struct ProcessedTranscription {
    pub final_text: String,
    pub post_processed_text: Option<String>,
    pub post_process_prompt: Option<String>,
    /// Key a voice trigger asked for after the paste ("… press enter").
    pub submit_key: Option<crate::settings::AutoSubmitKey>,
    /// App to bring to the front instead of pasting ("go to Claude").
    pub switch_to_app: Option<String>,
    /// Where the text goes, when the assistant wrote it.
    pub placement: Option<crate::assistant::Placement>,
    /// Why the assistant couldn't help; the dictation is shown, not pasted.
    pub assistant_error: Option<String>,
}

/// Resolve the persisted language *intent* into the language the currently-loaded
/// model will actually use — the same capability-aware coercion the transcription
/// paths apply (see [`crate::managers::model::effective_language`]). Post-processing
/// resolves it independently so it agrees with the language the transcription ran
/// in, without threading a value through the pipeline.
fn resolve_effective_language(app: &AppHandle, settings: &AppSettings) -> String {
    let tm = app.state::<Arc<TranscriptionManager>>();
    let model_manager = app.state::<Arc<ModelManager>>();
    let active_model = tm
        .get_current_model()
        .unwrap_or_else(|| settings.selected_model.clone());
    match model_manager.get_model_info(&active_model) {
        Some(info) => crate::managers::model::effective_language(
            &settings.selected_language,
            &info.supported_languages,
            info.supports_language_detection,
        ),
        None => settings.selected_language.clone(),
    }
}

/// Felix's "start a Claude Code session": runs in the background (it waits on
/// the Claude app) and says how it went on the result card.
fn start_claude_session(app: &AppHandle, project: String, prompt: String, send: bool) {
    let app = app.clone();
    std::thread::spawn(move || {
        let started = Instant::now();
        let result = crate::agent_skills::start_claude_session(&project, &prompt, send);
        info!("Claude session skill took {:?}", started.elapsed());
        let (title, text) = match result {
            Ok(outcome) => {
                let folder = outcome.folder.display().to_string();
                let title = match (outcome.sent, send) {
                    (true, _) => "Started a Claude Code session",
                    (false, true) => "Opened Claude Code; press Send to start",
                    (false, false) => "Opened Claude Code with your prompt",
                };
                let mut text = format!(
                    "{} {folder}\n\n{prompt}",
                    if outcome.created {
                        "New project:"
                    } else {
                        "Project:"
                    }
                );
                if !outcome.verified {
                    warn!("Claude session: couldn't confirm the folder {folder}");
                    text.push_str("\n\nCheck the session opened in this folder.");
                }
                (title.to_string(), text)
            }
            Err(e) => {
                error!("Claude session failed: {e}");
                ("Couldn't start the Claude session".to_string(), e)
            }
        };
        crate::overlay::show_result_overlay_titled(&app, text, Some(title));
    });
}

/// Felix's computer task: says it's on it, runs Codex with Cua in the
/// background, and shows Codex's answer when it's done.
fn run_computer_task(app: &AppHandle, task: String) {
    let app = app.clone();
    let name = get_settings(&app).assistant_name;
    crate::overlay::show_result_overlay_titled(
        &app,
        task.clone(),
        Some(format!("{name} is on it")),
    );
    std::thread::spawn(move || {
        let started = Instant::now();
        let result = crate::agent::run_task(&task);
        info!("Computer task took {:?}", started.elapsed());
        let (title, text) = match result {
            Ok(reply) => (format!("{name} is done"), reply),
            Err(e) => {
                error!("Computer task failed: {e}");
                (format!("{name} couldn't finish"), e)
            }
        };
        crate::overlay::show_result_overlay_titled(&app, text, Some(title));
    });
}

/// A mistake reported to the assistant by voice: have the rules fixed, and
/// apply the fix straight away if it's safe (tests pass, nothing else
/// needed); otherwise say why and leave it for the Vocabulary page.
fn fix_reported_mistake(app: &AppHandle, report: String) {
    let app = app.clone();
    let name = get_settings(&app).assistant_name;
    crate::overlay::show_result_overlay_titled(
        &app,
        report.clone(),
        Some(format!("{name} is fixing the rules")),
    );
    tauri::async_runtime::spawn(async move {
        let history = app.state::<Arc<HistoryManager>>().inner().clone();
        let recent = crate::commands::rules::recent_dictations(&history).await;
        // The app's own settings, without the rules file: the proposal is
        // tested against the new file.
        let settings = get_settings(&app);
        let (title, text) = match crate::rules::propose(&settings, &report, &recent, "voice").await
        {
            Ok(p) if crate::rules::safe_to_apply(&p) => match crate::rules::save(&p.rules) {
                Ok(()) => {
                    crate::rules::log_applied(Some(&p.id), "applied");
                    (format!("{name} fixed it"), p.explanation)
                }
                Err(e) => (format!("{name} couldn't save the fix"), e),
            },
            Ok(p) => {
                let why = if p.needs_code_change {
                    "Rules alone can't fix this; Felix itself needs a change."
                } else if p.error.is_some() || p.tests.iter().any(|t| !t.passed) {
                    "The suggested rules didn't pass their tests, so nothing was changed."
                } else {
                    "Nothing to change."
                };
                (
                    format!("{name} didn't change anything"),
                    format!("{}\n\n{why}", p.explanation),
                )
            }
            Err(e) => {
                error!("Mistake report failed: {e}");
                (format!("{name} couldn't fix it"), e)
            }
        };
        let _ = app.emit("rules-changed", ());
        crate::overlay::show_result_overlay_titled(&app, text, Some(title));
    });
}

pub(crate) async fn process_transcription_output(
    app: &AppHandle,
    transcription: &str,
    post_process: bool,
) -> ProcessedTranscription {
    let settings = crate::rules::with_rules(get_settings(app));
    let mut final_text = transcription.to_string();
    let mut post_processed_text: Option<String> = None;
    let mut post_process_prompt: Option<String> = None;

    // Resolve the language the transcription actually ran in (the persisted
    // intent coerced against the loaded model's capabilities) so OpenCC keys off
    // the effective language rather than a possibly-stale intent.
    let effective_language = resolve_effective_language(app, &settings);
    if let Some(converted_text) =
        maybe_convert_chinese_variant(&effective_language, transcription).await
    {
        final_text = converted_text;
    }

    // Names heard as ordinary words ("cloud" for "Claude"), decided per
    // occurrence by the local model, before rules and the assistant see it.
    final_text = crate::soundalikes::resolve(&final_text, &settings).await;

    // Scratchpad rules run before the LLM so voice commands are stripped and
    // replacements applied deterministically; the model never sees them.
    let scratchpad = crate::scratchpad::run_rules(&final_text, &settings);
    final_text = scratchpad.text;

    // Saying the assistant's name hands the dictation to it instead of the
    // cleanup model.
    if settings.assistant_enabled
        && scratchpad.switch_to_app.is_none()
        && crate::assistant::is_addressed(&final_text, &settings.assistant_name)
    {
        crate::overlay::show_assistant_overlay(app);
        let field = crate::text_field::focused_field();
        let context = crate::app_context::current();
        let destination = crate::assistant::Destination {
            app_name: context.app_name.as_deref(),
            url_host: context.url_host.as_deref(),
            field: field.as_ref(),
        };
        let started = std::time::Instant::now();
        let result = crate::assistant::run(&settings, &final_text, &destination).await;
        let label = format!(
            "{} ({}, {})",
            settings.assistant_name, settings.assistant_model, settings.assistant_effort
        );
        return match result {
            Ok(crate::assistant::Edit {
                text,
                placement: crate::assistant::Placement::ClaudeSession { project },
            }) => {
                info!(
                    "{label} starts a Claude session after {:?}",
                    started.elapsed()
                );
                start_claude_session(app, project, text.clone(), settings.agent_auto_send);
                ProcessedTranscription {
                    final_text: String::new(),
                    post_processed_text: Some(text),
                    post_process_prompt: Some(label),
                    submit_key: None,
                    switch_to_app: None,
                    placement: None,
                    assistant_error: None,
                }
            }
            Ok(crate::assistant::Edit {
                text,
                placement: crate::assistant::Placement::ReportMistake,
            }) => {
                info!(
                    "{label} takes a mistake report after {:?}",
                    started.elapsed()
                );
                fix_reported_mistake(app, text.clone());
                ProcessedTranscription {
                    final_text: String::new(),
                    post_processed_text: Some(text),
                    post_process_prompt: Some(label),
                    submit_key: None,
                    switch_to_app: None,
                    placement: None,
                    assistant_error: None,
                }
            }
            Ok(crate::assistant::Edit {
                text,
                placement: crate::assistant::Placement::ComputerTask,
            }) => {
                info!(
                    "{label} starts a computer task after {:?}",
                    started.elapsed()
                );
                run_computer_task(app, text.clone());
                ProcessedTranscription {
                    final_text: String::new(),
                    post_processed_text: Some(text),
                    post_process_prompt: Some(label),
                    submit_key: None,
                    switch_to_app: None,
                    placement: None,
                    assistant_error: None,
                }
            }
            Ok(edit) => {
                info!(
                    "{label} answered in {:?}: {:?}",
                    started.elapsed(),
                    edit.placement_kind()
                );
                ProcessedTranscription {
                    final_text: edit.text.clone(),
                    post_processed_text: Some(edit.text),
                    post_process_prompt: Some(label),
                    submit_key: scratchpad.submit_key,
                    switch_to_app: None,
                    placement: Some(edit.placement),
                    assistant_error: None,
                }
            }
            Err(e) => {
                error!("{label} failed after {:?}: {e}", started.elapsed());
                ProcessedTranscription {
                    final_text,
                    post_processed_text: None,
                    post_process_prompt: Some(label),
                    submit_key: None,
                    switch_to_app: None,
                    placement: None,
                    assistant_error: Some(e),
                }
            }
        };
    }

    // AI cleanup: the post-process hotkey uses the selected prompt; otherwise
    // the cleanup level (when Post Processing is on) applies to every dictation.
    let request = if post_process {
        Some(CleanupRequest::SelectedPrompt)
    } else if uses_level_cleanup(&settings) {
        let has_instructions = has_custom_instructions(&settings);
        if crate::cleanup::needs_ai_cleanup(&final_text, settings.cleanup_level, has_instructions) {
            Some(CleanupRequest::Level(settings.cleanup_level))
        } else {
            debug!("Dictation already clean; skipping the AI pass");
            None
        }
    } else {
        None
    };
    if let (Some(request), None) = (request, &scratchpad.switch_to_app) {
        if let Some(processed_text) =
            post_process_transcription(&settings, &final_text, request).await
        {
            post_processed_text = Some(processed_text.clone());
            final_text = processed_text;

            post_process_prompt = match request {
                CleanupRequest::Level(level) => {
                    crate::cleanup::level_prompt_for(&settings, level).map(str::to_string)
                }
                CleanupRequest::SelectedPrompt => settings
                    .post_process_selected_prompt_id
                    .as_ref()
                    .and_then(|id| settings.post_process_prompts.iter().find(|p| &p.id == id))
                    .map(|p| p.prompt.clone()),
            };
        }
    } else if final_text != transcription {
        post_processed_text = Some(final_text.clone());
    }
    // Deterministic styling for the destination: list layout, email layout
    // and the category's formality. Last, so the LLM can't undo it.
    if scratchpad.switch_to_app.is_none() {
        let context = crate::app_context::current();
        let (category, _) = crate::app_context::resolve(&context, &settings.app_rules);
        final_text = crate::style::apply(
            &final_text,
            category,
            settings.category_styles.get(category),
            &settings.custom_words,
        );
        let mut updated = get_settings(app);
        if crate::app_context::remember_recent(&mut updated, &context) {
            write_settings(app, updated);
        }
    }
    // History keeps what was actually pasted (after cleanup and styling)
    // next to the raw transcript.
    if final_text != transcription {
        post_processed_text = Some(final_text.clone());
    }

    ProcessedTranscription {
        final_text,
        post_processed_text,
        post_process_prompt,
        submit_key: scratchpad.submit_key,
        switch_to_app: scratchpad.switch_to_app,
        placement: None,
        assistant_error: None,
    }
}

impl ShortcutAction for TranscribeAction {
    fn start(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        let start_time = Instant::now();
        debug!("TranscribeAction::start called for binding: {}", binding_id);

        // Load model in the background
        let tm = app.state::<Arc<TranscriptionManager>>();
        let rm = app.state::<Arc<AudioRecordingManager>>();

        // Load ASR model and VAD model in parallel (no local speech model
        // when dictation is transcribed in the cloud).
        let kickoff_started = Instant::now();
        if !uses_cloud_transcription(&get_settings(app)) {
            tm.initiate_model_load();
        }
        let rm_clone = Arc::clone(&rm);
        std::thread::spawn(move || {
            if let Err(e) = rm_clone.preload_vad() {
                debug!("VAD pre-load failed: {}", e);
            }
        });
        let kickoff_elapsed = kickoff_started.elapsed();

        crate::app_context::capture();
        crate::dictation_log::capture_at_start();
        let screen_generation = crate::screen_context::begin();
        prewarm_cleanup(
            &crate::rules::with_rules(get_settings(app)),
            screen_generation,
        );

        let binding_id = binding_id.to_string();
        let tray_started = Instant::now();
        set_tray_state(app, TrayIconState::Recording);
        let tray_elapsed = tray_started.elapsed();

        // Get the microphone mode to determine audio feedback timing
        let plan_started = Instant::now();
        let settings = get_settings(app);
        let is_always_on = settings.always_on_microphone;

        let selected_model_info = app
            .state::<Arc<ModelManager>>()
            .get_model_info(&settings.selected_model);

        // Use the app-facing model capability as the single pre-recording source
        // for live streaming decisions. Unknown support is represented as false
        // until the model registry is updated by discovery or runtime load.
        let model_supports_streaming = !uses_cloud_transcription(&settings)
            && selected_model_info
                .as_ref()
                .map(|m| m.supports_streaming)
                .unwrap_or(false);
        let vad_policy = if !settings.vad_enabled {
            VadPolicy::Disabled
        } else if model_supports_streaming {
            VadPolicy::Streaming
        } else {
            VadPolicy::Offline
        };
        if model_supports_streaming {
            tm.start_stream();
        }
        let plan_elapsed = plan_started.elapsed();

        // Sizing the overlay follows the same advertised capability. A model that
        // doesn't stream (or whose capability is not known yet) gets the compact
        // pill instead of an oversized transparent live window.
        let overlay_started = Instant::now();
        match settings.overlay_style {
            OverlayStyle::Live if model_supports_streaming => utils::show_streaming_overlay(app),
            OverlayStyle::Live | OverlayStyle::Minimal => show_recording_overlay(app),
            OverlayStyle::None => {} // show_overlay_state no-ops on None anyway
        }
        // Everything above runs before capture can begin, so each span here is
        // added keypress->capture latency.
        debug!(
            "start-path pre-recording steps: model_kickoff={:?} tray={:?} settings+stream_plan={:?} overlay={:?}",
            kickoff_elapsed,
            tray_elapsed,
            plan_elapsed,
            overlay_started.elapsed()
        );
        debug!("Microphone mode - always_on: {}", is_always_on);

        let mut recording_error: Option<String> = None;
        let recording_start_time = Instant::now();
        match rm.try_start_recording(&binding_id, vad_policy) {
            Ok(readiness) => {
                debug!(
                    "Recording request accepted in {:?}; waiting for first microphone samples",
                    recording_start_time.elapsed()
                );
                let generation = readiness.generation();
                let app_clone = app.clone();
                let rm_clone = Arc::clone(&rm);
                std::thread::spawn(move || {
                    if !readiness.wait() {
                        debug!("Microphone readiness wait ended without receiving samples");
                        return;
                    }

                    // Development-only preview hook for evaluating the brief
                    // arming animation on hardware that normally starts too fast
                    // to make it visible.
                    #[cfg(debug_assertions)]
                    if let Ok(delay_ms) = std::env::var("HANDY_DEBUG_MIC_READY_DELAY_MS")
                        .unwrap_or_default()
                        .parse::<u64>()
                    {
                        let delay_ms = delay_ms.min(10_000);
                        if delay_ms > 0 {
                            debug!("Delaying microphone-ready cue by {delay_ms}ms for UI preview");
                            std::thread::sleep(Duration::from_millis(delay_ms));
                        }
                    }

                    if !rm_clone.is_recording_readiness_current(generation) {
                        debug!("Microphone became ready for an inactive recording");
                        return;
                    }

                    debug!("Microphone is receiving samples; recording is ready");
                    utils::emit_recording_ready(&app_clone);

                    // The start chime is a readiness cue, so it must follow the
                    // first real input callback rather than Stream::play() or a
                    // fixed delay. The helper returns immediately when feedback
                    // is disabled; mute still follows the same readiness point.
                    if rm_clone.is_recording_readiness_current(generation) {
                        play_feedback_sound_blocking(&app_clone, SoundType::Start);
                    }
                    if rm_clone.is_recording_readiness_current(generation) {
                        rm_clone.apply_mute();
                    }
                });
            }
            Err(e) => {
                debug!("Failed to start recording: {}", e);
                recording_error = Some(e);
            }
        }

        if recording_error.is_none() {
            // Dynamically register the cancel shortcut in a separate task to avoid deadlock
            shortcut::register_cancel_shortcut(app);
        } else {
            // Starting failed (for example due to blocked microphone permissions).
            // Revert UI state so we don't stay stuck in the recording overlay.
            tm.cancel_stream();
            utils::hide_recording_overlay(app);
            set_tray_state(app, TrayIconState::Idle);
            if let Some(err) = recording_error {
                let error_type = if is_microphone_access_denied(&err) {
                    "microphone_permission_denied"
                } else if is_no_input_device_error(&err) {
                    "no_input_device"
                } else {
                    "unknown"
                };
                let _ = app.emit(
                    "recording-error",
                    RecordingErrorEvent {
                        error_type: error_type.to_string(),
                        detail: Some(err),
                    },
                );
            }
        }

        debug!(
            "TranscribeAction::start completed in {:?}",
            start_time.elapsed()
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, _shortcut_str: &str) {
        // Prevent a slow microphone from emitting a ready event or start chime
        // after the user has already requested stop.
        app.state::<Arc<AudioRecordingManager>>()
            .invalidate_recording_readiness();

        // Unregister the cancel shortcut when transcription stops
        shortcut::unregister_cancel_shortcut(app);

        let stop_time = Instant::now();
        debug!("TranscribeAction::stop called for binding: {}", binding_id);

        let ah = app.clone();
        let rm = Arc::clone(&app.state::<Arc<AudioRecordingManager>>());
        let tm = Arc::clone(&app.state::<Arc<TranscriptionManager>>());
        let hm = Arc::clone(&app.state::<Arc<HistoryManager>>());

        set_tray_state(app, TrayIconState::Transcribing);
        // Stop should give immediate visual feedback. Live streaming can keep
        // the larger panel, but it still switches from listening to a working
        // spinner while the stream finalizes. Non-streaming paths use the
        // compact transcribing pill (None no-ops in show_*).
        let style = get_settings(app).overlay_style;
        // Capture this before finalizing the stream so every later working state
        // targets the same overlay that was shown for this transcription.
        let use_streaming_overlay = should_use_streaming_overlay(style, tm.is_streaming());
        if use_streaming_overlay {
            tm.emit_stream_working(StreamWorkKind::Transcribing);
        } else {
            show_transcribing_overlay(app);
        }

        // Unmute before playing audio feedback so the stop sound is audible
        rm.remove_mute();

        // Play audio feedback for recording stop
        play_feedback_sound(app, SoundType::Stop);

        let binding_id = binding_id.to_string(); // Clone binding_id for the async task
        let post_process = self.post_process;
        let cancel_generation = rm.cancel_generation();

        tauri::async_runtime::spawn(async move {
            let _guard = FinishGuard(ah.clone(), Arc::clone(&tm));
            debug!(
                "Starting async transcription task for binding: {}",
                binding_id
            );

            let stop_recording_time = Instant::now();
            if let Some(samples) = rm.stop_recording(&binding_id, cancel_generation) {
                debug!(
                    "Recording stopped and samples retrieved in {:?}, sample count: {}",
                    stop_recording_time.elapsed(),
                    samples.len()
                );

                if rm.was_cancelled_since(cancel_generation) {
                    debug!("Transcription operation cancelled after recording stop");
                    tm.cancel_stream();
                    utils::hide_recording_overlay(&ah);
                    set_tray_state(&ah, TrayIconState::Idle);
                    return;
                }

                // Benchmark recording (dev setting): keep the audio before
                // gain and VAD, even when VAD kept nothing.
                let bench_id = crate::benchmark::begin(&ah, &rm, samples.len());

                if samples.is_empty() {
                    debug!("Recording produced no audio samples; skipping persistence");
                    // Tear down any streaming worker so its channel doesn't leak
                    // and block the next start_stream.
                    tm.cancel_stream();
                    utils::hide_recording_overlay(&ah);
                    set_tray_state(&ah, TrayIconState::Idle);
                } else {
                    // Save WAV concurrently with transcription — only when audio
                    // is being kept. History entries are saved either way.
                    let keep_audio = get_settings(&ah).recording_retention_days > 0;
                    let sample_count = samples.len();
                    let file_name = format!("handy-{}.wav", chrono::Utc::now().timestamp());
                    // Held in memory for a while, so a mistake report can
                    // keep the audio even when history doesn't.
                    crate::rules::keep_recent_audio(&file_name, &samples);
                    let mut transcription_model = tm.get_current_model();
                    let wav_path = hm.recordings_dir().join(&file_name);
                    let wav_path_for_verify = wav_path.clone();
                    let samples_for_wav = if keep_audio {
                        samples.clone()
                    } else {
                        Vec::new()
                    };
                    let wav_handle = tauri::async_runtime::spawn_blocking(move || {
                        if keep_audio {
                            crate::audio_toolkit::save_wav_file(&wav_path, &samples_for_wav)
                        } else {
                            Ok(())
                        }
                    });

                    // Transcribe concurrently with WAV save. If a live stream was
                    // running, finalize it and use its text (all audio was already
                    // fed to the stream); otherwise batch-transcribe the samples.
                    let transcription_time = Instant::now();
                    let transcription_result = match tm.finalize_stream() {
                        // A finalized stream with usable text wins. An empty result
                        // (no active stream, produced nothing, or a finalize error
                        // after the engine was returned) falls back to a full batch
                        // transcription of the same audio. A finalize timeout is
                        // surfaced instead — the worker may still hold the engine,
                        // so a batch fallback would contend with it.
                        Ok(Some(text)) if !text.trim().is_empty() => Ok(text),
                        Ok(_) => {
                            let (result, model) = transcribe_dictation(&ah, &tm, samples).await;
                            transcription_model = model;
                            result
                        }
                        Err(err) => Err(err),
                    };

                    // Await WAV save and verify
                    let wav_saved = match wav_handle.await {
                        Ok(Ok(())) if !keep_audio => false,
                        Ok(Ok(())) => {
                            match crate::audio_toolkit::verify_wav_file(
                                &wav_path_for_verify,
                                sample_count,
                            ) {
                                Ok(()) => true,
                                Err(e) => {
                                    error!("WAV verification failed: {}", e);
                                    false
                                }
                            }
                        }
                        Ok(Err(e)) => {
                            error!("Failed to save WAV file: {}", e);
                            false
                        }
                        Err(e) => {
                            error!("WAV save task panicked: {}", e);
                            false
                        }
                    };

                    if rm.was_cancelled_since(cancel_generation) {
                        debug!("Transcription operation cancelled before output handling");
                        utils::hide_recording_overlay(&ah);
                        set_tray_state(&ah, TrayIconState::Idle);
                        return;
                    }

                    if let Some(id) = &bench_id {
                        let model = transcription_model.clone();
                        let (text, error) = match &transcription_result {
                            Ok(text) => (Some(text.clone()), None),
                            Err(e) => (None, Some(e.to_string())),
                        };
                        crate::benchmark::update(&ah, id, move |r| {
                            r.model = model;
                            r.transcript = text;
                            r.error = error;
                        });
                    }

                    match transcription_result {
                        Ok(transcription) => {
                            debug!(
                                "Transcription completed in {:?}: '{}'",
                                transcription_time.elapsed(),
                                utils::redact_text(&transcription)
                            );

                            if post_process || uses_level_cleanup(&get_settings(&ah)) {
                                if use_streaming_overlay {
                                    tm.emit_stream_working(StreamWorkKind::Polishing);
                                } else {
                                    show_processing_overlay(&ah);
                                }
                            }
                            let Some(processed) = complete_unless_cancelled(
                                process_transcription_output(&ah, &transcription, post_process),
                                || rm.was_cancelled_since(cancel_generation),
                            )
                            .await
                            else {
                                debug!("Transcription operation cancelled during output handling");
                                utils::hide_recording_overlay(&ah);
                                set_tray_state(&ah, TrayIconState::Idle);
                                return;
                            };

                            if rm.was_cancelled_since(cancel_generation) {
                                debug!("Transcription operation cancelled before paste");
                                utils::hide_recording_overlay(&ah);
                                set_tray_state(&ah, TrayIconState::Idle);
                                return;
                            }

                            // Save to history; the audio reference only when kept.
                            let entry_file = if wav_saved { file_name } else { String::new() };
                            if let Err(err) = hm.save_entry(
                                entry_file,
                                transcription,
                                post_process,
                                processed.post_processed_text.clone(),
                                processed.post_process_prompt.clone(),
                                transcription_model.clone(),
                            ) {
                                error!("Failed to save history entry: {}", err);
                            }

                            if let Some(app_path) = processed.switch_to_app {
                                debug!("Voice control: switching to {app_path}");
                                if let Err(e) = crate::app_switcher::activate(&app_path) {
                                    error!("App switch failed: {e}");
                                }
                                utils::hide_recording_overlay(&ah);
                                set_tray_state(&ah, TrayIconState::Idle);
                            } else if processed.final_text.is_empty()
                                && processed.submit_key.is_none()
                                && processed.assistant_error.is_none()
                            {
                                utils::hide_recording_overlay(&ah);
                                set_tray_state(&ah, TrayIconState::Idle);
                            } else {
                                let ah_clone = ah.clone();
                                let paste_time = Instant::now();
                                let final_text = processed.final_text;
                                let submit_key = processed.submit_key;
                                let placement = processed.placement;
                                let assistant_error = processed.assistant_error;
                                let rm_for_paste = Arc::clone(&rm);
                                let bench_id = bench_id.clone();
                                ah.run_on_main_thread(move || {
                                    if rm_for_paste.was_cancelled_since(cancel_generation) {
                                        debug!("Transcription operation cancelled before paste");
                                        utils::hide_recording_overlay(&ah_clone);
                                        set_tray_state(&ah_clone, TrayIconState::Idle);
                                        return;
                                    }

                                    // Fit the text to what's around the cursor and
                                    // remember the field for the dictation log.
                                    let settings =
                                        crate::rules::with_rules(get_settings(&ah_clone));
                                    // The assistant couldn't help: show the
                                    // dictation and why, rather than pasting a
                                    // request as if it were text.
                                    if let Some(error) = assistant_error {
                                        crate::overlay::show_result_overlay_titled(
                                            &ah_clone,
                                            final_text,
                                            Some(error),
                                        );
                                        set_tray_state(&ah_clone, TrayIconState::Idle);
                                        return;
                                    }
                                    if let Some(crate::assistant::Placement::ReplaceField(
                                        expected,
                                    )) = &placement
                                    {
                                        if !crate::text_field::select_whole_field(expected) {
                                            info!("Couldn't select the field to replace; showing the assistant's text instead");
                                            crate::overlay::show_result_overlay_titled(
                                                &ah_clone,
                                                final_text,
                                                Some(format!(
                                                    "Couldn't replace the text, so here is {}'s version",
                                                    settings.assistant_name
                                                )),
                                            );
                                            set_tray_state(&ah_clone, TrayIconState::Idle);
                                            return;
                                        }
                                    }
                                    let inserting = matches!(
                                        placement,
                                        None | Some(crate::assistant::Placement::Insert)
                                    );
                                    // In Claude, Codex or WhatsApp with no text box
                                    // focused, put the cursor in the thread's
                                    // message box (not a terminal) first.
                                    if !final_text.is_empty()
                                        && inserting
                                        && settings.focus_message_box
                                    {
                                        crate::screen_context::focus_message_box();
                                    }
                                    // Nothing focused that takes text: show it
                                    // instead of pasting into the void.
                                    if !final_text.is_empty()
                                        && settings.result_popup_enabled
                                        && crate::text_field::paste_target()
                                            == crate::text_field::PasteTarget::NoText
                                    {
                                        info!("No text field focused; showing the dictation instead of pasting");
                                        crate::overlay::show_result_overlay(&ah_clone, final_text);
                                        set_tray_state(&ah_clone, TrayIconState::Idle);
                                        return;
                                    }
                                    let before = (!final_text.is_empty())
                                        .then(crate::text_field::focused_field)
                                        .flatten();
                                    let final_text = match (&before, settings.context_aware_paste && inserting) {
                                        (Some(field), true) => crate::text_field::adapt_to_context(
                                            &final_text,
                                            &field.text_before_cursor(40),
                                            &field.text_after_cursor(10),
                                            &settings.custom_words,
                                        ),
                                        _ => final_text,
                                    };
                                    let pasted_text = final_text.clone();
                                    match utils::paste(final_text, ah_clone.clone(), submit_key) {
                                        Ok(()) => {
                                            debug!(
                                                "Text pasted successfully in {:?}",
                                                paste_time.elapsed()
                                            );
                                            if let Some(id) = bench_id {
                                                let text = pasted_text.clone();
                                                crate::benchmark::update(
                                                    &ah_clone,
                                                    &id,
                                                    move |r| r.pasted = Some(text),
                                                );
                                                if !pasted_text.is_empty() {
                                                    crate::benchmark::watch_edits(
                                                        &ah_clone,
                                                        id,
                                                        pasted_text.clone(),
                                                        before.clone(),
                                                    );
                                                }
                                            }
                                            if !pasted_text.is_empty() {
                                                crate::dictation_log::record_after_paste(
                                                    pasted_text,
                                                    before,
                                                );
                                            }
                                        }
                                        Err(e) => {
                                            error!("Failed to paste transcription: {}", e);
                                            let _ = ah_clone.emit("paste-error", ());
                                        }
                                    }
                                    utils::hide_recording_overlay(&ah_clone);
                                    set_tray_state(&ah_clone, TrayIconState::Idle);
                                })
                                .unwrap_or_else(|e| {
                                    error!("Failed to run paste on main thread: {:?}", e);
                                    utils::hide_recording_overlay(&ah);
                                    set_tray_state(&ah, TrayIconState::Idle);
                                });
                            }
                        }
                        Err(err) => {
                            if rm.was_cancelled_since(cancel_generation) {
                                debug!(
                                    "Transcription operation cancelled after transcription error"
                                );
                                utils::hide_recording_overlay(&ah);
                                set_tray_state(&ah, TrayIconState::Idle);
                                return;
                            }

                            error!("Transcription failed: {}", err);
                            // Surface the failure to the UI (toast). The full
                            // message is also in handy.log via the line above.
                            let _ = ah.emit("transcription-error", err.to_string());
                            // Save entry with empty text so user can retry (if the
                            // audio was kept).
                            if wav_saved {
                                if let Err(save_err) = hm.save_entry(
                                    file_name,
                                    String::new(),
                                    post_process,
                                    None,
                                    None,
                                    transcription_model.clone(),
                                ) {
                                    error!("Failed to save failed history entry: {}", save_err);
                                }
                            }
                            utils::hide_recording_overlay(&ah);
                            set_tray_state(&ah, TrayIconState::Idle);
                        }
                    }
                }
            } else {
                debug!("No samples retrieved from recording stop");
                // Tear down any streaming worker so its channel doesn't leak.
                tm.cancel_stream();
                utils::hide_recording_overlay(&ah);
                set_tray_state(&ah, TrayIconState::Idle);
            }
        });

        debug!(
            "TranscribeAction::stop completed in {:?}",
            stop_time.elapsed()
        );
    }
}

// Cancel Action
struct CancelAction;

impl ShortcutAction for CancelAction {
    fn start(&self, app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        utils::cancel_current_operation(app);
    }

    fn stop(&self, _app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        // Nothing to do on stop for cancel
    }
}

// Test Action
struct TestAction;

impl ShortcutAction for TestAction {
    fn start(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str) {
        log::info!(
            "Shortcut ID '{}': Started - {} (App: {})", // Changed "Pressed" to "Started" for consistency
            binding_id,
            shortcut_str,
            app.package_info().name
        );
    }

    fn stop(&self, app: &AppHandle, binding_id: &str, shortcut_str: &str) {
        log::info!(
            "Shortcut ID '{}': Stopped - {} (App: {})", // Changed "Released" to "Stopped" for consistency
            binding_id,
            shortcut_str,
            app.package_info().name
        );
    }
}

/// Starts or stops a meeting recording (on press; release does nothing).
struct MeetingAction;

impl ShortcutAction for MeetingAction {
    fn start(&self, app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        crate::meetings::manager::toggle_in_background(app);
    }

    fn stop(&self, _app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {}
}

// Static Action Map
pub static ACTION_MAP: Lazy<HashMap<String, Arc<dyn ShortcutAction>>> = Lazy::new(|| {
    let mut map = HashMap::new();
    map.insert(
        "transcribe".to_string(),
        Arc::new(TranscribeAction {
            post_process: false,
        }) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "transcribe_with_post_process".to_string(),
        Arc::new(TranscribeAction { post_process: true }) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "cancel".to_string(),
        Arc::new(CancelAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "meeting".to_string(),
        Arc::new(MeetingAction) as Arc<dyn ShortcutAction>,
    );
    map.insert(
        "test".to_string(),
        Arc::new(TestAction) as Arc<dyn ShortcutAction>,
    );
    map
});

#[cfg(test)]
mod tests {
    use super::{
        complete_unless_cancelled, is_blank_transcription, should_use_streaming_overlay,
        strip_think_block,
    };
    use crate::settings::OverlayStyle;
    use std::future;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn blank_transcription_is_detected() {
        assert!(is_blank_transcription(""));
        assert!(is_blank_transcription("   "));
        assert!(is_blank_transcription("\t\n  \r\n"));
    }

    #[test]
    fn non_blank_transcription_is_kept() {
        assert!(!is_blank_transcription("hello"));
        assert!(!is_blank_transcription("  hello  "));
    }

    #[test]
    fn completed_operation_returns_its_output() {
        let result = tauri::async_runtime::block_on(complete_unless_cancelled(
            future::ready("done"),
            || false,
        ));

        assert_eq!(result, Some("done"));
    }

    #[test]
    fn pending_operation_stops_after_cancellation() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancelled_for_thread = Arc::clone(&cancelled);
        let cancel_thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(10));
            cancelled_for_thread.store(true, Ordering::Release);
        });

        let result = tauri::async_runtime::block_on(complete_unless_cancelled(
            future::pending::<()>(),
            || cancelled.load(Ordering::Acquire),
        ));

        cancel_thread.join().unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn leading_think_block_is_stripped() {
        assert_eq!(
            strip_think_block("<think>pondering...</think>Cleaned text."),
            "Cleaned text."
        );
        assert_eq!(
            strip_think_block("  \n<think>multi\nline</think>\n  Cleaned text."),
            "Cleaned text."
        );
    }

    #[test]
    fn content_without_think_block_is_unchanged() {
        assert_eq!(strip_think_block("Cleaned text."), "Cleaned text.");
        assert_eq!(
            strip_think_block("Mentions <think> mid-sentence."),
            "Mentions <think> mid-sentence."
        );
        // Unclosed block: leave untouched rather than guess
        assert_eq!(
            strip_think_block("<think>never closed"),
            "<think>never closed"
        );
    }

    #[test]
    fn live_overlay_uses_streaming_states_only_for_streaming_models() {
        assert!(should_use_streaming_overlay(OverlayStyle::Live, true));
        assert!(!should_use_streaming_overlay(OverlayStyle::Live, false));
        assert!(!should_use_streaming_overlay(OverlayStyle::Minimal, true));
        assert!(!should_use_streaming_overlay(OverlayStyle::None, true));
    }
}
