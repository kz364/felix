//! Dictation rules kept in a file (`rules.toml` in Handy's app data folder)
//! instead of in the app: vocabulary, corrections, sound-alikes and test
//! cases. Handy checks the file at every dictation and uses a saved change
//! straight away, so fixing a mishearing needs no rebuild or restart. A file
//! with a mistake is reported in the log and the last good version is kept.
//!
//! The rules add to what's set in the app: vocabulary and corrections are
//! appended to Settings → Vocabulary's, and a sound-alike here replaces the
//! app's entry for the same heard word.
//!
//! `[[test]]` cases are checked by `cargo run --example rules_check` and when
//! a proposed change is shown in the app (see `propose`).

use crate::settings::{AppSettings, Soundalike, TextReplacement};
use once_cell::sync::{Lazy, OnceCell};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

pub const FILE: &str = "rules.toml";

/// How to read the file, for AI agents and other apps: at the top of every
/// rules file.
macro_rules! agent_section {
    () => {
        r#"# ---------------------------------------------------------------------
# For AI agents and other apps reading this file
#
# This is plain TOML describing how one person's dictation should be
# corrected. To apply it to a raw speech-to-text transcript, in this order:
#  1. soundalike: for each [[soundalike]], occurrences of `heard` (whole
#     words, any case) become `word` when the sentence is about the name:
#     always when followed by one of `compounds`; never right after a word in
#     `ordinary_after` (default: the, my, our, a, an, your, their, this,
#     that), unless the text is going into an app in `name_in_apps`, or
#     `name_if_on_screen` is true and the name is visible on screen. Other
#     cases are a judgment call from `meaning`.
#  2. vocabulary: any word here that was transcribed with different casing
#     or spacing is rewritten to exactly this spelling. Speech models can
#     also be biased toward these words.
#  3. replace: each [[replace]] in file order. `from` matches a whole phrase,
#     case-insensitively; `/…/` is a regex, and `to` may use $1, $2.
#  [[test]] entries are examples: `said` is the raw transcript, `expect` is
#  the right result (in `app`, or with `screen` text visible, if given).
#  Use them to check your reading of the rules.
#
# Editing: keep it valid TOML and keep these comments. Add words to the
# existing `vocabulary` list rather than a second one. Felix re-reads the
# file before each dictation and keeps the last good version if it breaks.
# ---------------------------------------------------------------------
"#
    };
}
const AGENT_SECTION: &str = agent_section!();
/// Its first line, to tell whether a file has it.
const AGENT_MARKER: &str = "# For AI agents and other apps reading this file";

/// The file as first written: the format, explained, and the Claude rule.
pub const STARTER: &str = concat!(
    r#"# Felix dictation rules. Saved changes apply to your next dictation; no
# restart needed. They add to Settings → Vocabulary.
#
"#,
    agent_section!(),
    r#"#
# vocabulary: words and names to recognise and spell exactly like this.
# [[replace]]: fix a mishearing. `from` is a whole phrase, any case; write it
#   as /regex/ for a pattern (then `to` may use $1). Applied in order.
# [[soundalike]]: a name transcribed as an ordinary word that sounds like it
#   ("Claude" as "cloud"). Each occurrence is decided from its sentence:
#   followed by a compound (`compounds`) it's the name; right after a word in
#   `ordinary_after` (default: the, my, our, a…) it's the ordinary word,
#   unless dictating into one of `name_in_apps` or, with `name_if_on_screen`,
#   the name is on screen, in which case the local model decides, as it does
#   everywhere else.
# [[test]]: what was transcribed (`said`) and what should come out
#   (`expect`), optionally in an app (`app`) or with text on screen
#   (`screen`). Checked when rules are changed.

vocabulary = []

[[soundalike]]
heard = "cloud"
word = "Claude"
meaning = "Anthropic's AI assistant and its products, such as Claude Code, the Claude app and Claude models"
compounds = ["Claude Code"]
name_in_apps = ["Claude", "Codex"]
name_if_on_screen = true

[[test]]
said = "open cloud code in the repo"
expect = "open Claude Code in the repo"

[[test]]
said = "our cloud bill went up"
expect = "our cloud bill went up"

[[test]]
said = "restart my cloud instance"
app = "Codex"
expect = "restart my Claude instance"
"#
);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rules {
    #[serde(default)]
    pub vocabulary: Vec<String>,
    #[serde(default)]
    pub replace: Vec<TextReplacement>,
    #[serde(default, deserialize_with = "strict_soundalikes")]
    pub soundalike: Vec<Soundalike>,
    #[serde(default)]
    pub test: Vec<TestCase>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestCase {
    /// As transcribed.
    pub said: String,
    /// What should come out.
    pub expect: String,
    /// The app dictated into (name or bundle id).
    #[serde(default)]
    pub app: Option<String>,
    /// Text on screen there.
    #[serde(default)]
    pub screen: Option<String>,
}

/// `Soundalike` as written in the file, where a misspelt field is an error
/// (the app's settings are more forgiving).
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictSoundalike {
    heard: String,
    word: String,
    meaning: String,
    #[serde(default)]
    compounds: Vec<String>,
    #[serde(default)]
    ordinary_after: Vec<String>,
    #[serde(default)]
    name_in_apps: Vec<String>,
    #[serde(default)]
    name_if_on_screen: bool,
}

fn strict_soundalikes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Soundalike>, D::Error> {
    let entries: Vec<StrictSoundalike> = Vec::deserialize(d)?;
    Ok(entries
        .into_iter()
        .map(|e| Soundalike {
            heard: e.heard,
            word: e.word,
            meaning: e.meaning,
            compounds: e.compounds,
            ordinary_after: e.ordinary_after,
            name_in_apps: e.name_in_apps,
            name_if_on_screen: e.name_if_on_screen,
        })
        .collect())
}

pub fn parse(text: &str) -> Result<Rules, String> {
    toml::from_str(text).map_err(|e| e.to_string())
}

static PATH: OnceCell<PathBuf> = OnceCell::new();

/// Where the rules live; writes the starter file the first time.
pub fn init(app_data_dir: &Path) {
    let path = app_data_dir.join(FILE);
    if !path.exists() {
        if let Err(e) = std::fs::write(&path, STARTER) {
            log::warn!("Couldn't create {}: {e}", path.display());
        }
    }
    let _ = PATH.set(path.clone());
    // Files from before the section for agents get it added on top.
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Some(new) = with_agent_section(&text) {
            if let Err(e) = save(&new) {
                log::warn!("Couldn't add the agent notes to {}: {e}", path.display());
            }
        }
    }
}

/// `text` with the section for agents added on top, or `None` if it has
/// one already.
fn with_agent_section(text: &str) -> Option<String> {
    (!text.contains(AGENT_MARKER)).then(|| format!("{AGENT_SECTION}\n{text}"))
}

/// Rules in a form other apps can load: the file's contents as JSON, with
/// how to apply them, plus the words taught by voice for the current model.
pub fn export_json(settings: &AppSettings) -> Result<String, String> {
    let rules = current();
    let taught: Vec<serde_json::Value> = settings
        .taught_words
        .iter()
        .filter_map(|t| {
            let entry = t
                .by_model
                .iter()
                .find(|m| m.model_id == settings.selected_model)
                .or_else(|| t.by_model.last())?;
            let heard_as: Vec<&String> = entry
                .variants
                .iter()
                .filter(|v| !entry.excluded.contains(v))
                .collect();
            Some(serde_json::json!({"word": t.word, "heard_as": heard_as}))
        })
        .collect();
    let json = serde_json::json!({
        "format": "felix-dictation-rules",
        "version": 1,
        "how_to_apply": "Apply to a raw transcript in this order: soundalike (heard -> word when the sentence is about the name), vocabulary (rewrite to these exact spellings; can also bias speech models), replace (in order; `from` is a whole phrase, any case, or /regex/ with $1 in `to`), then taught (each heard_as phrase -> word). `test` entries are examples: said -> expect.",
        "vocabulary": rules.vocabulary,
        "replace": rules.replace,
        "soundalike": rules.soundalike,
        "taught": taught,
        "test": rules.test,
    });
    serde_json::to_string_pretty(&json).map_err(|e| e.to_string())
}

pub fn path() -> Option<&'static Path> {
    PATH.get().map(PathBuf::as_path)
}

/// The last good rules and the file time they were read at.
static CACHE: Lazy<Mutex<(Option<SystemTime>, Rules)>> =
    Lazy::new(|| Mutex::new((None, Rules::default())));

/// The current rules, re-read if the file changed.
pub fn current() -> Rules {
    let Some(path) = path() else {
        return Rules::default();
    };
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if modified.is_some() && cache.0 != modified {
        cache.0 = modified;
        match std::fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|t| parse(&t))
        {
            Ok(rules) => {
                log::info!(
                    "Loaded {}: {} words, {} corrections, {} sound-alikes",
                    FILE,
                    rules.vocabulary.len(),
                    rules.replace.len(),
                    rules.soundalike.len()
                );
                cache.1 = rules;
            }
            Err(e) => log::error!("{FILE} has a mistake, keeping the last good rules: {e}"),
        }
    }
    cache.1.clone()
}

/// Add the rules to the app's settings, for one dictation.
pub(crate) fn add_to(settings: &mut AppSettings, rules: &Rules) {
    for word in &rules.vocabulary {
        let word = word.trim();
        if !word.is_empty() && !settings.custom_words.iter().any(|w| w.trim() == word) {
            settings.custom_words.push(word.to_string());
        }
    }
    settings
        .text_replacements
        .extend(rules.replace.iter().cloned());
    for entry in &rules.soundalike {
        settings
            .soundalikes
            .retain(|s| !s.heard.eq_ignore_ascii_case(&entry.heard));
        settings.soundalikes.push(entry.clone());
    }
}

/// The settings with the current rules file added.
pub(crate) fn with_rules(mut settings: AppSettings) -> AppSettings {
    add_to(&mut settings, &current());
    settings
}

/// How a test case came out.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct TestResult {
    pub said: String,
    pub expect: String,
    pub got: String,
    pub passed: bool,
    /// The local model decides this one; `got` is what it gives if it
    /// agrees with the test.
    pub model_decides: bool,
}

/// The rule steps of a dictation (no transcription model, no cleanup):
/// sound-alikes, vocabulary spelling, corrections. Sound-alike occurrences
/// the local model would decide pass if either answer gives `expect`.
pub(crate) fn check(rules: &Rules, base: &AppSettings) -> Vec<TestResult> {
    let mut settings = base.clone();
    add_to(&mut settings, rules);
    let run = |text: &str, place: &crate::soundalikes::Place, model_says_name: bool| {
        let text = crate::soundalikes::resolve_offline(
            text,
            &settings.soundalikes,
            place,
            model_says_name,
        );
        let text = crate::vocabulary::apply_canonical_forms(&text, &settings.custom_words);
        crate::scratchpad::apply_text_replacements(&text, &settings.text_replacements)
    };
    rules
        .test
        .iter()
        .map(|t| {
            let place = crate::soundalikes::Place {
                app_name: t.app.clone(),
                bundle_id: t.app.clone(),
                screen: t.screen.clone(),
            };
            let as_name = run(&t.said, &place, true);
            let as_heard = run(&t.said, &place, false);
            let expect = t.expect.as_str();
            let got = if same_text(&as_heard, expect) {
                as_heard.clone()
            } else {
                as_name.clone()
            };
            TestResult {
                said: t.said.clone(),
                expect: t.expect.clone(),
                passed: same_text(&got, expect),
                model_decides: as_name != as_heard,
                got,
            }
        })
        .collect()
}

/// Whether a test's result matches, ignoring what rules never change and
/// cleanup does: the first letter's case and a final full stop, ? or !.
fn same_text(got: &str, expect: &str) -> bool {
    fn norm(s: &str) -> String {
        let s = s.trim().trim_end_matches(['.', '!', '?']);
        let mut chars = s.chars();
        match chars.next() {
            Some(first) => first.to_lowercase().chain(chars).collect(),
            None => String::new(),
        }
    }
    norm(got) == norm(expect)
}

/// Check a rules file against the app's saved vocabulary and corrections
/// (read from `settings_store.json`, if given). For `examples/rules_check`.
pub fn check_file(rules: &Path, settings_store: Option<&Path>) -> Result<Vec<TestResult>, String> {
    let text = std::fs::read_to_string(rules).map_err(|e| format!("{}: {e}", rules.display()))?;
    let rules = parse(&text)?;
    let mut base = crate::settings::get_default_settings();
    if let Some(store) = settings_store.filter(|p| p.exists()) {
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(store).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        let saved = &json["settings"];
        if let Ok(words) = serde_json::from_value(saved["custom_words"].clone()) {
            base.custom_words = words;
        }
        if let Ok(replacements) = serde_json::from_value(saved["text_replacements"].clone()) {
            base.text_replacements = replacements;
        }
        if let Ok(soundalikes) = serde_json::from_value(saved["soundalikes"].clone()) {
            base.soundalikes = soundalikes;
        }
    }
    Ok(check(&rules, &base))
}

// ---- Proposing rules from a report ----------------------------------------

/// A line of a before/after comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Type)]
pub struct DiffLine {
    /// "same", "added" or "removed".
    pub kind: String,
    pub text: String,
}

/// Line diff (longest common subsequence); rules files are small.
pub fn diff(old: &str, new: &str) -> Vec<DiffLine> {
    let (a, b): (Vec<&str>, Vec<&str>) = (old.lines().collect(), new.lines().collect());
    let mut lcs = vec![vec![0u32; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let line = |kind: &str, text: &str| DiffLine {
        kind: kind.into(),
        text: text.into(),
    };
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            out.push(line("same", a[i]));
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || lcs[i][j + 1] >= lcs[i + 1][j]) {
            out.push(line("added", b[j]));
            j += 1;
        } else {
            out.push(line("removed", a[i]));
            i += 1;
        }
    }
    out
}

/// A change to the rules file, suggested from the user's report.
#[derive(Debug, Clone, Serialize, Type)]
pub struct Proposal {
    /// The report's entry in `rule_reports.jsonl`.
    pub id: String,
    /// What went wrong and what the change does, in plain words.
    pub explanation: String,
    /// The fix needs a change to Handy itself, not only rules.
    pub needs_code_change: bool,
    /// The whole new file.
    pub rules: String,
    pub diff: Vec<DiffLine>,
    pub tests: Vec<TestResult>,
    /// The new file doesn't parse.
    pub error: Option<String>,
}

const PROPOSE_INSTRUCTIONS: &str = r#"You maintain the dictation rules of Felix, a speech-to-text app. The user reports something dictation got wrong. Reply with the rules to ADD that fix it; existing rules stay as they are.

Kinds of rule, narrowest first:
- "vocabulary": a word or name to recognise and spell exactly like this ("kubectl", "GitHub", "LLM"). It fixes the case and spelling of the word when it was transcribed as that word ("github" → "GitHub"), and joins spelled-out letters ("l l m" → "LLM"). It does NOT turn different words into it: "super base" stays "super base", so that needs a "replace" (add the word to vocabulary too, which helps the speech model hear it).
- "replace": a fixed mishearing, `from` → `to`. `from` is a whole phrase matched in any case ("cube cuddle" → "kubectl"). Never use a single common English word as `from` (it would change ordinary speech); use a "soundalike" for that.
- "soundalike": a name that the speech model writes as a common word that sounds like it ("Claude" heard as "cloud"). Each occurrence is judged from its sentence, so the common word still works. `heard` is the common word, lowercase; `word` the name; `meaning` says what the name is; `compounds` are longer names starting with it ("Claude Code"); `name_in_apps` lists apps where the name is likely ("Claude", "Codex"); `name_if_on_screen` true if the name being on screen should count. A compound always becomes the name, even after "our" or "the": prefer compounds for product names ("Claude Desktop"). `ordinary_after` lists words that, right before `heard`, make it the ordinary word; empty means the default (the, a, my, our, your, this…), which suits "cloud" ("the cloud"). When the name itself often comes after those words ("the Tauri app"), set it to the words that really mark the ordinary word, or ["-"] for none, so the local model decides every occurrence. To change an existing soundalike, return the whole entry again with the same `heard`; it replaces the old one.

Also add "tests": the reported case (`said` = the text as it was wrongly transcribed, `expect` = what should come out) and, when there's any risk, a case showing ordinary use is untouched (`expect` = `said`). Rules only change the words they target: keep everything else in `expect` exactly as in `said`, including case and punctuation. `app` is the app dictated into, or empty.

If the rules can't fix it (for example it needs a new kind of rule, or better audio), set needs_code_change, explain what Felix needs, and add nothing.

The report may itself have been dictated, so its words can be misheard too ("it wrote cloud instead of cloud"); read it together with the latest dictations to work out what was meant. In "explanation", say in two or three plain sentences what went wrong and what the rule does."#;

fn schema() -> serde_json::Value {
    let strings = serde_json::json!({"type": "array", "items": {"type": "string"}});
    serde_json::json!({
        "type": "object",
        "properties": {
            "explanation": {"type": "string"},
            "needs_code_change": {"type": "boolean"},
            "vocabulary": strings,
            "replace": {"type": "array", "items": {
                "type": "object",
                "properties": {"from": {"type": "string"}, "to": {"type": "string"}},
                "required": ["from", "to"],
                "additionalProperties": false
            }},
            "soundalike": {"type": "array", "items": {
                "type": "object",
                "properties": {
                    "heard": {"type": "string"},
                    "word": {"type": "string"},
                    "meaning": {"type": "string"},
                    "compounds": strings,
                    "ordinary_after": strings,
                    "name_in_apps": strings,
                    "name_if_on_screen": {"type": "boolean"}
                },
                "required": ["heard", "word", "meaning", "compounds", "ordinary_after", "name_in_apps", "name_if_on_screen"],
                "additionalProperties": false
            }},
            "tests": {"type": "array", "items": {
                "type": "object",
                "properties": {"said": {"type": "string"}, "expect": {"type": "string"}, "app": {"type": "string"}},
                "required": ["said", "expect", "app"],
                "additionalProperties": false
            }}
        },
        "required": ["explanation", "needs_code_change", "vocabulary", "replace", "soundalike", "tests"],
        "additionalProperties": false
    })
}

/// The rules a model proposes adding.
#[derive(Debug, Clone, Default, Deserialize)]
struct Additions {
    #[serde(default)]
    explanation: String,
    #[serde(default)]
    needs_code_change: bool,
    #[serde(default)]
    vocabulary: Vec<String>,
    #[serde(default)]
    replace: Vec<TextReplacement>,
    #[serde(default)]
    soundalike: Vec<Soundalike>,
    #[serde(default)]
    tests: Vec<AddedTest>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct AddedTest {
    said: String,
    expect: String,
    #[serde(default)]
    app: String,
}

/// `old` with `add` written into it: new vocabulary merged into the
/// `vocabulary = [...]` line (or a new one), new entries appended.
fn render(old: &str, add: &Additions) -> Result<String, String> {
    let current = parse(old)?;
    let mut text = old.trim_end().to_string();
    let new_words: Vec<String> = add
        .vocabulary
        .iter()
        .map(|w| w.trim().to_string())
        .filter(|w| !w.is_empty() && !current.vocabulary.contains(w))
        .fold(Vec::new(), |mut acc, w| {
            if !acc.contains(&w) {
                acc.push(w);
            }
            acc
        });
    if !new_words.is_empty() {
        let mut all = current.vocabulary.clone();
        all.extend(new_words);
        let line = format!("vocabulary = {}", toml::Value::from(all));
        text = match vocabulary_span(&text) {
            Some((start, end)) => format!("{}{line}{}", &text[..start], &text[end..]),
            None => format!("{line}\n\n{text}"),
        };
    }
    #[derive(Serialize)]
    struct Entries<'a> {
        #[serde(skip_serializing_if = "<[_]>::is_empty")]
        replace: &'a [TextReplacement],
        #[serde(skip_serializing_if = "<[_]>::is_empty")]
        soundalike: &'a [Soundalike],
        #[serde(skip_serializing_if = "<[_]>::is_empty")]
        test: &'a [TestCase],
    }
    let tests: Vec<TestCase> = add
        .tests
        .iter()
        .filter(|t| !t.said.trim().is_empty())
        .map(|t| TestCase {
            said: t.said.clone(),
            expect: t.expect.clone(),
            app: Some(t.app.trim().to_string()).filter(|a| !a.is_empty()),
            screen: None,
        })
        .collect();
    let entries = toml::to_string(&Entries {
        replace: &add.replace,
        soundalike: &add.soundalike,
        test: &tests,
    })
    .map_err(|e| e.to_string())?;
    if !entries.trim().is_empty() {
        text.push_str("\n\n");
        text.push_str(entries.trim_end());
    }
    text.push('\n');
    parse(&text)?;
    Ok(text)
}

/// Where the top-level `vocabulary = [...]` is, possibly over several lines.
fn vocabulary_span(text: &str) -> Option<(usize, usize)> {
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with('[') {
            return None; // past the top level
        }
        if line.trim_start().starts_with("vocabulary") && line.contains('=') {
            let start = offset + line.find("vocabulary")?;
            let open = offset + line.find('[')?;
            let mut in_string = false;
            let mut escaped = false;
            for (i, c) in text[open..].char_indices() {
                match c {
                    _ if escaped => escaped = false,
                    '\\' if in_string => escaped = true,
                    '"' => in_string = !in_string,
                    ']' if !in_string => return Some((start, open + i + 1)),
                    _ => {}
                }
            }
            return None;
        }
        offset += line.len();
    }
    None
}

/// A proposal, and the model's attempts at it.
pub struct Draft {
    pub proposal: Proposal,
    /// 2 when the first answer was sent back for another try.
    pub attempts: u32,
}

/// Ask `llm` for the rules that fix `report`, tested against `base`. An
/// answer that doesn't work (a mistake in it, or its own tests failing) is
/// sent back once with what went wrong.
pub(crate) async fn draft_with(
    llm: &crate::meetings::llm::Llm,
    effort: &str,
    old: &str,
    report: &str,
    recent: &[(String, String)],
    base: &AppSettings,
) -> Result<Draft, String> {
    let mut input = format!(
        "The report:\n{}\n\nThe rules file now:\n```toml\n{old}\n```\n",
        report.trim()
    );
    if !base.custom_words.is_empty() {
        input.push_str(&format!(
            "\nVocabulary already set in the app: {}\n",
            base.custom_words.join(", ")
        ));
    }
    if !recent.is_empty() {
        input.push_str(
            "\nThe user's latest dictations (newest first), as transcribed → as pasted:\n",
        );
        for (raw, pasted) in recent {
            input.push_str(&format!("- {raw} → {pasted}\n"));
        }
    }
    let mut attempts = 0;
    loop {
        attempts += 1;
        let add: Result<Additions, String> = llm
            .ask_json(PROPOSE_INSTRUCTIONS, &input, &schema(), effort)
            .await
            .and_then(|reply| {
                serde_json::from_value(reply)
                    .map_err(|e| format!("The model's answer wasn't in the expected form: {e}"))
            });
        let add = match add {
            Ok(add) => add,
            Err(e) if attempts < 2 && !e.contains("Sign in") => {
                log::warn!("Rules proposal: {e}; asking again");
                continue;
            }
            Err(e) => return Err(e),
        };
        let (new, error) = match render(old, &add) {
            Ok(new) => (new, None),
            Err(e) => (old.to_string(), Some(e)),
        };
        let tests = match &error {
            None => check(&parse(&new)?, base),
            Some(_) => Vec::new(),
        };
        let proposal = Proposal {
            id: String::new(),
            explanation: add.explanation.trim().to_string(),
            needs_code_change: add.needs_code_change,
            diff: diff(old, &new),
            rules: new,
            tests,
            error,
        };
        let failing: Vec<&TestResult> = proposal.tests.iter().filter(|t| !t.passed).collect();
        if attempts >= 2 || (proposal.error.is_none() && failing.is_empty()) {
            return Ok(Draft { proposal, attempts });
        }
        let mut problem = String::from("\n\nYour previous answer didn't work:\n");
        if let Some(e) = &proposal.error {
            problem.push_str(&format!("- The rules had a mistake: {e}\n"));
        }
        for t in failing {
            problem.push_str(&format!(
                "- Test {:?} gave {:?}, not {:?}\n",
                t.said, t.got, t.expect
            ));
        }
        problem.push_str("Answer again with rules and tests that work. A test expecting a change the rules can't make is wrong; fix the rule or the test.\n");
        input.push_str(&problem);
    }
}

/// `draft_with` against the default settings, for `examples/rules_eval`.
pub async fn draft(
    llm: &crate::meetings::llm::Llm,
    effort: &str,
    old: &str,
    report: &str,
    recent: &[(String, String)],
) -> Result<Draft, String> {
    draft_with(
        llm,
        effort,
        old,
        report,
        recent,
        &crate::settings::get_default_settings(),
    )
    .await
}

/// Check `rules` (a whole file) against cases it wasn't written with.
pub fn check_against(rules: &str, cases: &[TestCase]) -> Result<Vec<TestResult>, String> {
    let mut rules = parse(rules)?;
    rules.test = cases.to_vec();
    Ok(check(&rules, &crate::settings::get_default_settings()))
}

/// A recent dictation, for a mistake report.
#[derive(Debug, Clone, Default)]
pub struct Recent {
    pub transcribed: String,
    pub pasted: String,
    /// The speech model that transcribed it.
    pub model: Option<String>,
    /// History's name for its recording ("handy-….wav").
    pub file_name: String,
    /// The recording on disk, if history kept it.
    pub audio_path: Option<PathBuf>,
}

/// Ask a strong remote model for the rules change that fixes `report`.
/// `recent` is the last dictations, newest first. The report, the dictation
/// it's most likely about, its transcription model and its audio are kept
/// (see `REPORTS_FILE`, `REPORTS_AUDIO_DIR`), so mistakes can be traced to a
/// model and replayed against other models later.
pub(crate) async fn propose(
    settings: &AppSettings,
    report: &str,
    recent: &[Recent],
    source: &str,
) -> Result<Proposal, String> {
    let id = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let pairs: Vec<(String, String)> = recent
        .iter()
        .map(|r| (r.transcribed.clone(), r.pasted.clone()))
        .collect();
    let result = ask_for_rules(settings, report, &pairs, id.clone()).await;
    let last = recent.first();
    let audio = last.and_then(|r| save_report_audio(&id, r));
    let mut entry = serde_json::json!({
        "id": id,
        "event": "reported",
        "at": chrono::Utc::now().to_rfc3339(),
        "source": source,
        "report": report.trim(),
        "last_dictation": last.map(|r| serde_json::json!({
            "transcribed": r.transcribed,
            "pasted": r.pasted,
            "model": r.model,
            "recording": r.file_name,
        })),
        "audio": audio,
    });
    match &result {
        Ok(p) => {
            entry["explanation"] = p.explanation.clone().into();
            entry["needs_code_change"] = p.needs_code_change.into();
            entry["change"] = change_lines(p).into();
            entry["tests_failing"] = p.tests.iter().filter(|t| !t.passed).count().into();
        }
        Err(e) => entry["error"] = e.clone().into(),
    }
    log_report(&entry);
    result
}

fn change_lines(p: &Proposal) -> Vec<String> {
    p.diff
        .iter()
        .filter(|l| l.kind != "same" && !l.text.trim().is_empty())
        .map(|l| format!("{} {}", if l.kind == "added" { "+" } else { "-" }, l.text))
        .collect()
}

/// Reported dictations' audio, one WAV per report, for benchmarking models
/// on the mistakes. Local only.
pub const REPORTS_AUDIO_DIR: &str = "report_audio";

/// Recent dictations' audio, kept in memory whatever history keeps.
/// (history file name, samples), newest first.
type RecentAudio = std::collections::VecDeque<(String, Vec<f32>)>;
static RECENT_AUDIO: Lazy<Mutex<RecentAudio>> =
    Lazy::new(|| Mutex::new(std::collections::VecDeque::new()));
const RECENT_AUDIO_KEPT: usize = 3;

/// Hold a dictation's audio for a while, in case it's reported.
pub fn keep_recent_audio(file_name: &str, samples: &[f32]) {
    let mut recent = RECENT_AUDIO.lock().unwrap_or_else(|e| e.into_inner());
    recent.push_front((file_name.to_string(), samples.to_vec()));
    recent.truncate(RECENT_AUDIO_KEPT);
}

/// Save the reported dictation's audio; its path relative to the rules
/// folder.
fn save_report_audio(id: &str, dictation: &Recent) -> Option<String> {
    let dir = path()?.with_file_name(REPORTS_AUDIO_DIR);
    let _ = std::fs::create_dir_all(&dir);
    let target = dir.join(format!("{id}.wav"));
    let in_memory = RECENT_AUDIO
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(name, _)| *name == dictation.file_name)
        .map(|(_, samples)| samples.clone());
    let saved = match (in_memory, &dictation.audio_path) {
        (Some(samples), _) => {
            crate::audio_toolkit::save_wav_file(&target, &samples).map_err(|e| e.to_string())
        }
        (None, Some(disk)) if disk.exists() => std::fs::copy(disk, &target)
            .map(|_| ())
            .map_err(|e| e.to_string()),
        _ => return None,
    };
    match saved {
        Ok(()) => Some(format!("{REPORTS_AUDIO_DIR}/{id}.wav")),
        Err(e) => {
            log::warn!("Couldn't keep the reported dictation's audio: {e}");
            None
        }
    }
}

/// Every report, what was proposed and what was applied, one JSON object a
/// line, next to the rules file. Kept for reference; never sent anywhere.
pub const REPORTS_FILE: &str = "rule_reports.jsonl";

fn log_report(entry: &serde_json::Value) {
    let Some(path) = path().map(|p| p.with_file_name(REPORTS_FILE)) else {
        return;
    };
    use std::io::Write;
    let written = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{entry}"));
    if let Err(e) = written {
        log::warn!("Couldn't write {}: {e}", path.display());
    }
}

/// Note that a proposal was applied (or undone), in the reports file.
pub fn log_applied(id: Option<&str>, event: &str) {
    log_report(&serde_json::json!({
        "id": id,
        "event": event,
        "at": chrono::Utc::now().to_rfc3339(),
    }));
}

/// A proposal that's safe to apply without the user looking: it parses,
/// changes something, needs no code change and passes every test.
pub fn safe_to_apply(p: &Proposal) -> bool {
    p.error.is_none()
        && !p.needs_code_change
        && p.diff.iter().any(|l| l.kind != "same")
        && p.tests.iter().all(|t| t.passed)
}

/// Put back the rules file from before the last save.
pub fn undo() -> Result<(), String> {
    let path = path().ok_or("The rules file isn't set up")?;
    let backup = path.with_extension("toml.bak");
    if !backup.exists() {
        return Err("There's no earlier version to go back to".into());
    }
    let applied = last_applied_id();
    std::fs::rename(&backup, path).map_err(|e| e.to_string())?;
    log_applied(applied.as_deref(), "undone");
    Ok(())
}

/// The report whose change was applied last (what an undo takes back).
fn last_applied_id() -> Option<String> {
    let path = path()?.with_file_name(REPORTS_FILE);
    let text = std::fs::read_to_string(path).ok()?;
    last_applied_in(&text)
}

fn last_applied_in(reports: &str) -> Option<String> {
    reports.lines().rev().find_map(|line| {
        let entry: serde_json::Value = serde_json::from_str(line).ok()?;
        (entry["event"] == "applied")
            .then(|| entry["id"].as_str().map(str::to_string))
            .flatten()
    })
}

async fn ask_for_rules(
    settings: &AppSettings,
    report: &str,
    recent: &[(String, String)],
    id: String,
) -> Result<Proposal, String> {
    let path = path().ok_or("The rules file isn't set up")?;
    let old = std::fs::read_to_string(path).unwrap_or_else(|_| STARTER.to_string());
    if crate::chatgpt::signed_in_as().is_none() {
        return Err("Sign in with ChatGPT (Felix settings) to have rules written for you".into());
    }
    let llm = crate::meetings::llm::Llm::Chatgpt {
        model: settings.assistant_model.clone(),
    };
    let draft = draft_with(&llm, "medium", &old, report, recent, settings).await?;
    Ok(Proposal {
        id,
        ..draft.proposal
    })
}

// ---- What Handy has learned, and the reports it came from ----------------

/// The rules file's entries, for the Vocabulary page.
#[derive(Debug, Clone, Default, Serialize, Type)]
pub struct Learned {
    pub words: Vec<String>,
    pub corrections: Vec<TextReplacement>,
    pub soundalikes: Vec<Soundalike>,
    /// The file has a mistake (the list is from the last good version).
    pub error: Option<String>,
}

pub fn learned() -> Learned {
    let rules = current();
    let error = path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| parse(&t).err());
    Learned {
        words: rules.vocabulary,
        corrections: rules.replace,
        soundalikes: rules.soundalike,
        error,
    }
}

/// Add a word to the vocabulary (no-op if it's already there).
pub fn add_word(word: &str) -> Result<(), String> {
    let word = word.trim();
    if word.is_empty() {
        return Err("Type a word first".into());
    }
    let path = path().ok_or("The rules file isn't set up")?;
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    if parse(&text)?.vocabulary.iter().any(|w| w == word) {
        return Ok(());
    }
    let add = Additions {
        vocabulary: vec![word.to_string()],
        ..Default::default()
    };
    save(&render(&text, &add)?)?;
    log_applied(None, &format!("added word {word}"));
    Ok(())
}

/// Remove one entry ("word", "correction" or "soundalike", by position),
/// keeping the file's comments. Tests that only passed because of it go too.
pub fn forget(kind: &str, index: usize) -> Result<(), String> {
    let path = path().ok_or("The rules file isn't set up")?;
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let new = forget_in(&text, kind, index, &crate::settings::get_default_settings())?;
    save(&new)?;
    log_applied(None, &format!("forgot {kind} {index}"));
    Ok(())
}

fn forget_in(text: &str, kind: &str, index: usize, base: &AppSettings) -> Result<String, String> {
    let before = parse(text)?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    let missing = || format!("There's no such {kind}");
    match kind {
        "word" => {
            let words = doc
                .get_mut("vocabulary")
                .and_then(|v| v.as_array_mut())
                .ok_or_else(missing)?;
            if index >= words.len() {
                return Err(missing());
            }
            words.remove(index);
        }
        "correction" | "soundalike" => {
            let key = if kind == "correction" {
                "replace"
            } else {
                "soundalike"
            };
            let tables = doc
                .get_mut(key)
                .and_then(|v| v.as_array_of_tables_mut())
                .ok_or_else(missing)?;
            if index >= tables.len() {
                return Err(missing());
            }
            tables.remove(index);
        }
        _ => return Err(format!("Unknown kind of rule: {kind}")),
    }
    let after = parse(&doc.to_string())?;
    let passed_before = check(&before, base);
    let passes_after = check(&after, base);
    let orphaned: Vec<usize> = passed_before
        .iter()
        .zip(&passes_after)
        .enumerate()
        .filter(|(_, (b, a))| b.passed && !a.passed)
        .map(|(i, _)| i)
        .collect();
    if let Some(tests) = doc.get_mut("test").and_then(|v| v.as_array_of_tables_mut()) {
        for i in orphaned.into_iter().rev() {
            tests.remove(i);
        }
    }
    let new = doc.to_string();
    parse(&new)?;
    Ok(new)
}

/// A mistake report, as kept in `REPORTS_FILE`.
#[derive(Debug, Clone, Default, Serialize, Type)]
pub struct Report {
    pub id: String,
    /// RFC 3339.
    pub at: String,
    /// "typed" or "voice".
    pub source: String,
    pub report: String,
    pub transcribed: Option<String>,
    pub pasted: Option<String>,
    pub transcription_model: Option<String>,
    pub has_audio: bool,
    pub explanation: String,
    /// Lines added or removed in the rules file ("+ …", "- …").
    pub change: Vec<String>,
    /// "applied", "proposed" (not applied), "no change" or "failed".
    pub status: String,
    pub error: Option<String>,
}

/// The reports, newest first.
pub fn reports() -> Vec<Report> {
    let Some(file) = path().map(|p| p.with_file_name(REPORTS_FILE)) else {
        return Vec::new();
    };
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let mut reports: Vec<Report> = Vec::new();
    let mut applied = std::collections::HashSet::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let str_of = |key: &str| v[key].as_str().map(str::to_string);
        match v["event"].as_str() {
            Some("reported") => {
                let last = &v["last_dictation"];
                let change: Vec<String> = v["change"]
                    .as_array()
                    .map(|a| {
                        a.iter()
                            .filter_map(|l| l.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                let error = str_of("error");
                let status = if error.is_some() {
                    "failed"
                } else if change.is_empty() {
                    "no change"
                } else {
                    "proposed"
                };
                reports.push(Report {
                    id: str_of("id").unwrap_or_default(),
                    at: str_of("at").unwrap_or_default(),
                    source: str_of("source").unwrap_or_default(),
                    report: str_of("report").unwrap_or_default(),
                    transcribed: last["transcribed"].as_str().map(str::to_string),
                    pasted: last["pasted"].as_str().map(str::to_string),
                    transcription_model: last["model"].as_str().map(str::to_string),
                    has_audio: v["audio"].is_string(),
                    explanation: str_of("explanation").unwrap_or_default(),
                    change,
                    status: status.into(),
                    error,
                });
            }
            Some("applied") => {
                if let Some(id) = str_of("id") {
                    applied.insert(id);
                }
            }
            _ => {}
        }
    }
    for r in &mut reports {
        if applied.contains(&r.id) {
            r.status = "applied".into();
        }
    }
    reports.reverse();
    reports
}

/// Delete a report and its audio (not the rules it added).
pub fn forget_report(id: &str) -> Result<(), String> {
    let file = path()
        .map(|p| p.with_file_name(REPORTS_FILE))
        .ok_or("The rules file isn't set up")?;
    let text = std::fs::read_to_string(&file).unwrap_or_default();
    let kept: String = text
        .lines()
        .filter(|line| {
            serde_json::from_str::<serde_json::Value>(line)
                .map(|v| v["id"].as_str() != Some(id))
                .unwrap_or(true)
        })
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&file, kept).map_err(|e| e.to_string())?;
    if let Some(dir) = path().map(|p| p.with_file_name(REPORTS_AUDIO_DIR)) {
        let _ = std::fs::remove_file(dir.join(format!("{id}.wav")));
    }
    Ok(())
}

/// Move vocabulary, corrections and sound-alikes kept in the app's settings
/// (from before the rules file) into the rules file, once, so the file is
/// the only place they live. Returns the settings to save, if any moved.
pub(crate) fn take_from_settings(mut settings: AppSettings) -> Result<Option<AppSettings>, String> {
    if settings.custom_words.is_empty()
        && settings.text_replacements.is_empty()
        && settings.soundalikes.is_empty()
    {
        return Ok(None);
    }
    let path = path().ok_or("The rules file isn't set up")?;
    let old = std::fs::read_to_string(path).unwrap_or_else(|_| STARTER.to_string());
    let current = parse(&old)?;
    let add = Additions {
        vocabulary: settings.custom_words.clone(),
        replace: settings
            .text_replacements
            .iter()
            .filter(|r| !current.replace.contains(r))
            .cloned()
            .collect(),
        soundalike: settings
            .soundalikes
            .iter()
            .filter(|s| {
                !current
                    .soundalike
                    .iter()
                    .any(|c| c.heard.eq_ignore_ascii_case(&s.heard))
            })
            .cloned()
            .collect(),
        ..Default::default()
    };
    let new = render(&old, &add)?;
    if new != old {
        save(&new)?;
    }
    log::info!(
        "Moved {} words, {} corrections and {} sound-alikes from settings into {FILE}",
        settings.custom_words.len(),
        add.replace.len(),
        add.soundalike.len()
    );
    settings.custom_words.clear();
    settings.text_replacements.clear();
    settings.soundalikes.clear();
    Ok(Some(settings))
}

/// Save a new rules file (the old one is kept as `rules.toml.bak`).
pub fn save(text: &str) -> Result<(), String> {
    parse(text)?;
    let path = path().ok_or("The rules file isn't set up")?;
    if path.exists() {
        let _ = std::fs::copy(path, path.with_extension("toml.bak"));
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn older_files_get_the_agent_notes_once() {
        assert!(STARTER.contains(AGENT_MARKER));
        assert_eq!(with_agent_section(STARTER), None);
        let old = "vocabulary = [\"kubectl\"]\n";
        let new = with_agent_section(old).unwrap();
        assert!(new.ends_with(old));
        assert_eq!(parse(&new).unwrap().vocabulary, vec!["kubectl"]);
        assert_eq!(with_agent_section(&new), None);
    }

    #[test]
    fn the_starter_file_parses_and_its_tests_pass() {
        let rules = parse(STARTER).unwrap();
        assert_eq!(rules.soundalike.len(), 1);
        let results = check(&rules, &crate::settings::get_default_settings());
        assert_eq!(results.len(), 3);
        for r in &results {
            assert!(r.passed, "{r:?}");
        }
        assert!(results[2].model_decides);
        assert!(!results[1].model_decides);
    }

    #[test]
    fn rules_add_to_the_apps_settings() {
        let rules = parse(
            r#"
vocabulary = ["kubectl", "Handy"]
[[replace]]
from = "cube cuddle"
to = "kubectl"
[[soundalike]]
heard = "cloud"
word = "Claude"
meaning = "the assistant"
[[test]]
said = "run cube cuddle apply"
expect = "run kubectl apply"
[[test]]
said = "run cube cuddle apply"
expect = "run cube cuddle apply"
"#,
        )
        .unwrap();
        let mut settings = crate::settings::get_default_settings();
        settings.custom_words = vec!["Handy".into()];
        add_to(&mut settings, &rules);
        assert_eq!(settings.custom_words, vec!["Handy", "kubectl"]);
        assert_eq!(settings.soundalikes.len(), 1);
        assert_eq!(settings.soundalikes[0].meaning, "the assistant");
        let results = check(&rules, &crate::settings::get_default_settings());
        assert!(results[0].passed);
        assert!(!results[1].passed);
        assert_eq!(results[1].got, "run kubectl apply");
    }

    #[test]
    fn typos_in_the_file_are_reported() {
        let e =
            parse("[[soundalike]]\nheard = \"a\"\nword = \"A\"\nmeaning = \"x\"\ncompund = []\n")
                .unwrap_err();
        assert!(e.contains("compund"), "{e}");
        let e = parse("[[replaces]]\nfrom = \"a\"\nto = \"b\"\n").unwrap_err();
        assert!(e.contains("replaces"), "{e}");
    }

    #[test]
    fn diff_shows_added_and_removed_lines() {
        let d = diff("a\nb\nc", "a\nc\nd");
        let kinds: Vec<(&str, &str)> = d
            .iter()
            .map(|l| (l.kind.as_str(), l.text.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("same", "a"),
                ("removed", "b"),
                ("same", "c"),
                ("added", "d")
            ]
        );
    }

    #[test]
    fn additions_are_written_into_the_file() {
        let add = Additions {
            vocabulary: vec!["kubectl".into(), "kubectl".into()],
            replace: vec![TextReplacement {
                from: "cube cuddle".into(),
                to: "kubectl".into(),
            }],
            tests: vec![AddedTest {
                said: "run cube cuddle apply".into(),
                expect: "run kubectl apply".into(),
                app: String::new(),
            }],
            ..Default::default()
        };
        let new = render(STARTER, &add).unwrap();
        assert!(
            new.starts_with(STARTER.lines().next().unwrap()),
            "comments kept"
        );
        let rules = parse(&new).unwrap();
        assert_eq!(rules.vocabulary, vec!["kubectl"]);
        assert_eq!(rules.replace.len(), 1);
        assert_eq!(rules.test.len(), 4);
        assert!(check(&rules, &crate::settings::get_default_settings())
            .iter()
            .all(|t| t.passed));

        // A multi-line list the user wrote, and a file without one.
        let more = Additions {
            vocabulary: vec!["Anthropic".into()],
            ..Default::default()
        };
        let edited = render("vocabulary = [\n  \"a]b\",\n  \"c\",\n]\n", &more).unwrap();
        assert_eq!(
            parse(&edited).unwrap().vocabulary,
            vec!["a]b", "c", "Anthropic"]
        );
        let fresh = render("[[replace]]\nfrom = \"x y\"\nto = \"z\"\n", &more).unwrap();
        assert_eq!(parse(&fresh).unwrap().vocabulary, vec!["Anthropic"]);
    }

    #[test]
    fn forgetting_a_rule_keeps_comments_and_drops_its_tests() {
        let text = format!(
            "{STARTER}\n[[replace]]\nfrom = \"cube cuddle\"\nto = \"kubectl\"\n\n[[test]]\nsaid = \"run cube cuddle\"\nexpect = \"run kubectl\"\n"
        );
        let base = crate::settings::get_default_settings();
        let mut no_soundalikes = base.clone();
        no_soundalikes.soundalikes.clear();
        let new = forget_in(&text, "correction", 0, &no_soundalikes).unwrap();
        assert!(new.starts_with("# Felix dictation rules"));
        let rules = parse(&new).unwrap();
        assert!(rules.replace.is_empty());
        assert!(!rules.test.iter().any(|t| t.said == "run cube cuddle"));
        assert_eq!(rules.test.len(), 3, "the other tests stay");

        let new = forget_in(&new, "soundalike", 0, &no_soundalikes).unwrap();
        let rules = parse(&new).unwrap();
        assert!(rules.soundalike.is_empty());
        // "our cloud bill" still passes without the rule; the others go.
        assert_eq!(rules.test.len(), 1);
        assert!(forget_in(&new, "word", 0, &no_soundalikes).is_err());
    }

    #[test]
    fn settings_lists_move_into_the_file_once() {
        let dir = std::env::temp_dir().join(format!("handy-rules-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join(FILE);
        std::fs::write(&file, STARTER).unwrap();
        let _ = PATH.set(file.clone());
        if path() != Some(file.as_path()) {
            return; // another test set the path first
        }
        let mut settings = crate::settings::get_default_settings();
        settings.custom_words = vec!["kubectl".into()];
        settings.text_replacements = vec![TextReplacement {
            from: "cube cuddle".into(),
            to: "kubectl".into(),
        }];
        let moved = take_from_settings(settings).unwrap().unwrap();
        assert!(moved.custom_words.is_empty() && moved.soundalikes.is_empty());
        let rules = parse(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(rules.vocabulary, vec!["kubectl"]);
        assert_eq!(rules.replace.len(), 1);
        assert_eq!(
            rules.soundalike.len(),
            1,
            "the default cloud rule isn't doubled"
        );
        assert!(take_from_settings(moved).unwrap().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn undo_names_the_last_applied_report() {
        let log = r#"{"id":"a","report":"x"}
{"id":"a","event":"applied","at":"t"}
{"id":"b","event":"applied","at":"t"}
{"id":null,"event":"undone","at":"t"}"#;
        assert_eq!(last_applied_in(log).as_deref(), Some("b"));
        assert_eq!(last_applied_in(""), None);
    }
}
