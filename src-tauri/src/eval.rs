//! What the speech benchmark (`examples/hosted_eval.rs`) needs from the
//! app, run outside it: your saved settings and vocabulary, the steps a
//! dictation goes through after the speech model, and the reference guess.
//! Nothing here writes to the app's settings.

use crate::meetings::remote::Remote;
use crate::settings::AppSettings;
use std::path::Path;
use transcribe_cpp::Session;

pub use crate::benchmark::{Guess, Heard, Record};

pub struct Setup {
    settings: AppSettings,
}

impl Setup {
    /// Your settings as Felix saved them in `app_data`, rules applied.
    pub fn load(app_data: &Path) -> Result<Self, String> {
        let path = app_data.join(crate::settings::SETTINGS_STORE_PATH);
        let store: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&path).map_err(|e| format!("No settings: {e}"))?,
        )
        .map_err(|e| format!("Settings unreadable: {e}"))?;
        let stored = store.get("settings").ok_or("No settings saved yet")?;
        crate::rules::read_from(app_data);
        Ok(Self {
            settings: crate::rules::with_rules(crate::settings::from_store_value(stored)),
        })
    }

    /// Use `key` for `provider` in this run only (never saved).
    pub fn use_api_key(&mut self, provider: &str, key: &str) {
        self.settings
            .post_process_api_keys
            .insert(provider.to_string(), key.trim().to_string());
    }

    /// Stop the local cleanup model this run started, if any.
    pub fn stop_local_cleanup(&self) {
        crate::local_llm::stop();
    }

    pub fn vocabulary(&self) -> &[String] {
        &self.settings.custom_words
    }

    /// A cloud speech provider as dictation would use it, with your
    /// vocabulary; `model` picks the OpenRouter model.
    pub fn remote(&self, provider: &str, model: Option<&str>) -> Result<Remote, String> {
        let mut settings = self.settings.clone();
        if let Some(model) = model {
            settings.openrouter_transcription_model = model.to_string();
        }
        Remote::for_provider(&settings, provider, true)
    }

    /// The text rules a cloud transcript gets (fillers, normalising).
    pub fn finish_cloud_text(&self, raw: String) -> String {
        crate::managers::transcription::finish_cloud_text_with(&self.settings, raw)
    }

    /// Transcribe with a model on this Mac as a dictation would.
    pub fn transcribe_local(&self, session: &mut Session, audio: &[f32]) -> Result<String, String> {
        crate::managers::transcription::transcribe_like_dictation(&self.settings, session, audio)
            .map_err(|e| e.to_string())
    }

    /// What would be pasted into `app` (`bundle_id`): rules, the cleanup
    /// level's AI pass with your provider and styling. Also whether the AI
    /// pass was used. Not concurrent with another app's cleanup.
    pub async fn clean(
        &self,
        text: &str,
        app: Option<&str>,
        bundle_id: Option<&str>,
    ) -> (String, bool) {
        crate::app_context::replace_current(crate::app_context::DictationContext {
            app_name: app.map(str::to_string),
            bundle_id: bundle_id.map(str::to_string),
            ..Default::default()
        });
        crate::actions::clean_like_dictation(&self.settings, text).await
    }

    /// Which cleanup runs: provider, level and whether it's on.
    pub fn cleanup_label(&self) -> String {
        format!(
            "{} {:?}{}",
            self.settings.post_process_provider_id,
            self.settings.cleanup_level,
            if self.settings.post_process_enabled {
                ""
            } else {
                " (off)"
            }
        )
    }

    /// Best guess at what was said, from what the models heard, with the
    /// assistant model on your ChatGPT plan.
    pub async fn reference(&self, record: &Record, heard: Vec<Heard>) -> Result<Guess, String> {
        let llm = crate::meetings::llm::Llm::Chatgpt {
            model: self.settings.assistant_model.clone(),
        };
        crate::benchmark::reconcile(&llm, &self.settings.custom_words, record, heard).await
    }
}
