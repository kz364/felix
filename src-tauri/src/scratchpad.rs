//! The scratchpad: a deterministic staging step between transcription and
//! paste.
//!
//! Text lands here before it goes anywhere near the focused app. Rule-based
//! passes run first, so they are predictable and fast, and so the optional LLM
//! cleanup never sees (and can't mangle) voice commands:
//!
//! 1. Canonical spelling of technical vocabulary ("G L M five point three" →
//!    "GLM-5.3"), then the user's correction rules ("cube cuddle" → "kubectl").
//! 2. End-of-dictation voice triggers ("… press enter"), which are stripped
//!    from the text and turned into a key press after the paste.
//! 3. Spoken line breaks ("new line", "new paragraph").

use crate::settings::{AppSettings, AutoSubmitKey, TextReplacement, VoiceTrigger};
use once_cell::sync::Lazy;
use regex::{NoExpand, Regex, RegexBuilder};

/// Result of the rule-based scratchpad passes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchpadOutput {
    pub text: String,
    /// Key to press once the text has been pasted (from a voice trigger).
    pub submit_key: Option<AutoSubmitKey>,
    /// App to bring to the front instead of pasting ("go to Claude").
    pub switch_to_app: Option<String>,
}

pub fn run_rules(text: &str, settings: &AppSettings) -> ScratchpadOutput {
    // A whole-dictation app-switch command replaces the paste entirely.
    if let Some(app) = crate::app_switcher::detect(text, settings) {
        return ScratchpadOutput {
            text: String::new(),
            submit_key: None,
            switch_to_app: Some(app),
        };
    }

    let text = crate::vocabulary::apply_canonical_forms(text, &settings.custom_words);
    let text = crate::vocab_teach::apply_taught_rules(&text, settings);
    let mut text = apply_text_replacements(&text, &settings.text_replacements);
    let mut submit_key = None;

    if settings.voice_control_enabled {
        let (stripped, key) = apply_voice_triggers(&text, &settings.voice_triggers);
        text = apply_spoken_symbols(&apply_spoken_breaks(&stripped));
        submit_key = key;
    }

    ScratchpadOutput {
        text,
        submit_key,
        switch_to_app: None,
    }
}

/// Word tokens with their byte ranges. Apostrophes stay inside words so
/// "don't" is one token.
static WORD_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[\p{L}\p{N}]+(?:'[\p{L}\p{N}]+)*").unwrap());

/// Curly apostrophes from some ASR models would otherwise split "don’t".
fn normalize_apostrophes(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains(['\u{2019}', '\u{2018}']) {
        std::borrow::Cow::Owned(text.replace(['\u{2019}', '\u{2018}'], "'"))
    } else {
        std::borrow::Cow::Borrowed(text)
    }
}

fn phrase_words(phrase: &str) -> Vec<String> {
    WORD_RE
        .find_iter(&normalize_apostrophes(phrase))
        .map(|m| m.as_str().to_lowercase())
        .collect()
}

/// If `text` ends with one of the trigger phrases, strip it and return the
/// trigger's key. Matching is on lowercased word tokens, so capitalization and
/// any punctuation between or after the words ("Press-Enter!", "press… enter.")
/// don't matter. The longest matching phrase wins, so "press command enter"
/// beats a shorter "enter" trigger.
pub fn apply_voice_triggers(
    text: &str,
    triggers: &[VoiceTrigger],
) -> (String, Option<AutoSubmitKey>) {
    let normalized = normalize_apostrophes(text);
    let text = normalized.as_ref();
    let words: Vec<_> = WORD_RE.find_iter(text).collect();

    let best = triggers
        .iter()
        .filter_map(|trigger| {
            let phrase = phrase_words(&trigger.phrase);
            if phrase.is_empty() || phrase.len() > words.len() {
                return None;
            }
            let tail = &words[words.len() - phrase.len()..];
            let matches = tail
                .iter()
                .zip(&phrase)
                .all(|(word, expected)| word.as_str().to_lowercase() == *expected);
            matches.then(|| (phrase.len(), tail[0].start(), trigger.key))
        })
        .max_by_key(|(len, _, _)| *len);

    match best {
        Some((_, cut_at, key)) => {
            // Drop separators and any opening quote/bracket the trigger sat in.
            let kept = text[..cut_at].trim_end_matches(|c: char| {
                c.is_whitespace()
                    || [
                        ',', ';', ':', '-', '–', '—', '"', '“', '\'', '‘', '(', '[', '{',
                    ]
                    .contains(&c)
            });
            (kept.to_string(), Some(key))
        }
        None => (text.to_string(), None),
    }
}

static NEW_PARAGRAPH_RE: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(r"[ \t]*,?[ \t]*\b(?:new|next) paragraph\b[.,;:!]?[ \t]*")
        .case_insensitive(true)
        .build()
        .unwrap()
});

static NEW_LINE_RE: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(r"[ \t]*,?[ \t]*\b(?:new line|next line|line break)\b[.,;:!]?[ \t]*")
        .case_insensitive(true)
        .build()
        .unwrap()
});

/// Turn spoken "new paragraph" / "new line" into line breaks and capitalise
/// the first letter after each break.
pub fn apply_spoken_breaks(text: &str) -> String {
    let text = NEW_PARAGRAPH_RE.replace_all(text, "\n\n");
    let text = NEW_LINE_RE.replace_all(&text, "\n");
    if !text.contains('\n') {
        return text.into_owned();
    }

    let mut out = String::with_capacity(text.len());
    let mut capitalize_next = false;
    for c in text.chars() {
        if c == '\n' {
            capitalize_next = true;
            out.push(c);
        } else if capitalize_next && c.is_alphabetic() {
            out.extend(c.to_uppercase());
            capitalize_next = false;
        } else {
            if !c.is_whitespace() {
                capitalize_next = false;
            }
            out.push(c);
        }
    }
    out.trim_matches(|c: char| c == ' ' || c == '\t')
        .to_string()
}

static DASH_DASH_RE: Lazy<Regex> = Lazy::new(|| {
    RegexBuilder::new(r"\bdash[\s-]+dash\s+([\p{L}\p{N}])")
        .case_insensitive(true)
        .build()
        .unwrap()
});

/// Spoken command-line flags: "dash dash save" → "--save". The on-device
/// cleanup model gets this wrong, and it is unambiguous as a rule.
pub fn apply_spoken_symbols(text: &str) -> String {
    DASH_DASH_RE.replace_all(text, "--$1").into_owned()
}

/// Case-insensitive replacements, applied in order.
///
/// * `from` written as `/pattern/` is a regular expression; `to` may use
///   capture groups (`$1`, `${name}`).
/// * Otherwise `from` is a whole-phrase literal. Whitespace inside it matches
///   any run of whitespace, so "cube  cuddle" still hits; `to` is literal.
pub fn apply_text_replacements(text: &str, replacements: &[TextReplacement]) -> String {
    let mut text = text.to_string();
    for replacement in replacements {
        let from = replacement.from.trim();
        if from.is_empty() {
            continue;
        }
        if let Some(pattern) = regex_rule(from) {
            match RegexBuilder::new(pattern).case_insensitive(true).build() {
                Ok(re) => text = re.replace_all(&text, replacement.to.as_str()).into_owned(),
                Err(e) => log::warn!("Skipping regex replacement '{from}': {e}"),
            }
            continue;
        }
        let body = from
            .split_whitespace()
            .map(regex::escape)
            .collect::<Vec<_>>()
            .join(r"\s+");
        let starts_word = from.chars().next().is_some_and(char::is_alphanumeric);
        let ends_word = from.chars().last().is_some_and(char::is_alphanumeric);
        let pattern = format!(
            "{}{}{}",
            if starts_word { r"\b" } else { "" },
            body,
            if ends_word { r"\b" } else { "" }
        );
        match RegexBuilder::new(&pattern).case_insensitive(true).build() {
            Ok(re) => {
                text = re
                    .replace_all(&text, NoExpand(&replacement.to))
                    .into_owned()
            }
            Err(e) => log::warn!("Skipping text replacement '{from}': {e}"),
        }
    }
    text
}

/// `/pattern/` → `pattern`; anything else is a literal rule.
fn regex_rule(from: &str) -> Option<&str> {
    from.strip_prefix('/')
        .and_then(|rest| rest.strip_suffix('/'))
        .filter(|pattern| !pattern.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::default_voice_triggers;

    fn triggers() -> Vec<VoiceTrigger> {
        default_voice_triggers()
    }

    #[test]
    fn strips_trailing_press_enter() {
        let (text, key) = apply_voice_triggers("Sounds good. Press enter.", &triggers());
        assert_eq!(text, "Sounds good.");
        assert_eq!(key, Some(AutoSubmitKey::Enter));
    }

    #[test]
    fn strips_unpunctuated_trigger_and_dangling_comma() {
        let (text, key) = apply_voice_triggers("sounds good, press enter", &triggers());
        assert_eq!(text, "sounds good");
        assert_eq!(key, Some(AutoSubmitKey::Enter));
    }

    #[test]
    fn trigger_alone_leaves_empty_text() {
        let (text, key) = apply_voice_triggers("Press Enter.", &triggers());
        assert_eq!(text, "");
        assert_eq!(key, Some(AutoSubmitKey::Enter));
    }

    #[test]
    fn trigger_mid_sentence_is_kept() {
        let input = "Press enter to continue, then wait.";
        let (text, key) = apply_voice_triggers(input, &triggers());
        assert_eq!(text, input);
        assert_eq!(key, None);
    }

    #[test]
    fn trigger_tolerates_case_and_punctuation() {
        for input in [
            "Sounds good. PRESS ENTER!!!",
            "Sounds good. Press-Enter.",
            "Sounds good. Press… enter?",
            "Sounds good. \"Press enter.\"",
            "Sounds good. (press enter)",
            "Sounds good.\nPress Enter.\n",
            "Sounds good. Press   enter .",
        ] {
            let (text, key) = apply_voice_triggers(input, &triggers());
            assert_eq!(text, "Sounds good.", "input: {input:?}");
            assert_eq!(key, Some(AutoSubmitKey::Enter), "input: {input:?}");
        }
    }

    #[test]
    fn curly_apostrophes_match_straight_ones() {
        let triggers = vec![VoiceTrigger {
            phrase: "that's it".into(),
            key: AutoSubmitKey::Enter,
        }];
        let (text, key) = apply_voice_triggers("Ship it, that\u{2019}s it.", &triggers);
        assert_eq!(text, "Ship it");
        assert_eq!(key, Some(AutoSubmitKey::Enter));
    }

    #[test]
    fn longest_trigger_wins() {
        let (text, key) = apply_voice_triggers("Ship it. Press command enter.", &triggers());
        assert_eq!(text, "Ship it.");
        assert_eq!(key, Some(AutoSubmitKey::CmdEnter));
    }

    #[test]
    fn partial_word_does_not_trigger() {
        let (text, key) = apply_voice_triggers("We need to repress enter", &triggers());
        assert_eq!(text, "We need to repress enter");
        assert_eq!(key, None);
    }

    #[test]
    fn spoken_new_line_keeps_sentence_punctuation() {
        assert_eq!(
            apply_spoken_breaks("Hello there. New line. how are you?"),
            "Hello there.\nHow are you?"
        );
    }

    #[test]
    fn spoken_new_line_without_punctuation() {
        assert_eq!(
            apply_spoken_breaks("when is reading club new line should be tomorrow"),
            "when is reading club\nShould be tomorrow"
        );
    }

    #[test]
    fn spoken_new_paragraph() {
        assert_eq!(
            apply_spoken_breaks("First point, new paragraph, second point."),
            "First point\n\nSecond point."
        );
    }

    #[test]
    fn text_without_breaks_is_unchanged() {
        let input = "  Nothing to see here.";
        assert_eq!(apply_spoken_breaks(input), input);
    }

    #[test]
    fn replacements_are_case_insensitive_whole_phrases() {
        let replacements = vec![
            TextReplacement {
                from: "cube cuddle".into(),
                to: "kubectl".into(),
            },
            TextReplacement {
                from: "hand e".into(),
                to: "Handy".into(),
            },
        ];
        assert_eq!(
            apply_text_replacements("Run Cube  cuddle apply in hand e.", &replacements),
            "Run kubectl apply in Handy."
        );
        // Not inside other words.
        assert_eq!(
            apply_text_replacements("handed it over", &replacements),
            "handed it over"
        );
    }

    #[test]
    fn spoken_double_dash_becomes_flag() {
        assert_eq!(
            apply_spoken_symbols("run npm install dash dash save-dev and Dash-dash force"),
            "run npm install --save-dev and --force"
        );
        assert_eq!(apply_spoken_symbols("a dash of salt"), "a dash of salt");
    }

    #[test]
    fn regex_rules_support_captures_and_ignore_case() {
        let replacements = vec![
            TextReplacement {
                from: r"/\bcasp[ae]r\b/".into(),
                to: "Kaspar".into(),
            },
            TextReplacement {
                from: r"/\b(\d+) percent\b/".into(),
                to: "$1%".into(),
            },
        ];
        assert_eq!(
            apply_text_replacements("Ask CASPER about the 20 percent drop.", &replacements),
            "Ask Kaspar about the 20% drop."
        );
    }

    #[test]
    fn invalid_regex_rule_is_skipped() {
        let replacements = vec![TextReplacement {
            from: "/(unclosed/".into(),
            to: "x".into(),
        }];
        assert_eq!(
            apply_text_replacements("keep (unclosed", &replacements),
            "keep (unclosed"
        );
    }

    #[test]
    fn slash_alone_is_a_literal() {
        let replacements = vec![TextReplacement {
            from: "/".into(),
            to: " slash ".into(),
        }];
        assert_eq!(
            apply_text_replacements("a / b", &replacements),
            "a  slash  b"
        );
    }

    #[test]
    fn replacement_target_is_literal() {
        let replacements = vec![TextReplacement {
            from: "dollar sign".into(),
            to: "$1".into(),
        }];
        assert_eq!(
            apply_text_replacements("a dollar sign here", &replacements),
            "a $1 here"
        );
    }

    #[test]
    fn run_rules_respects_toggle() {
        let mut settings = crate::settings::get_default_settings();
        settings.voice_control_enabled = false;
        let out = run_rules("hello new line world press enter", &settings);
        assert_eq!(out.text, "hello new line world press enter");
        assert_eq!(out.submit_key, None);

        settings.voice_control_enabled = true;
        let out = run_rules("hello new line world press enter", &settings);
        assert_eq!(out.text, "hello\nWorld");
        assert_eq!(out.submit_key, Some(AutoSubmitKey::Enter));
    }

    #[test]
    fn app_switch_command_replaces_paste() {
        let mut settings = crate::settings::get_default_settings();
        settings.app_switch_any_installed = false;
        settings.app_aliases = vec![crate::settings::AppAlias {
            phrase: "chat".into(),
            app_path: "/Applications/Claude.app".into(),
        }];
        let out = run_rules("Go to chat.", &settings);
        assert_eq!(out.text, "");
        assert_eq!(
            out.switch_to_app.as_deref(),
            Some("/Applications/Claude.app")
        );

        let out = run_rules("Go to the chat room later.", &settings);
        assert_eq!(out.switch_to_app, None);
        assert_eq!(out.text, "Go to the chat room later.");
    }
}
