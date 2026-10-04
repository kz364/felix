//! Which languages a meeting is in, and which model can transcribe them.
//! A meeting held in Indonesian and English goes to a model that knows both,
//! not the dictation model that only knows English.

use crate::managers::model::{EngineType, ModelInfo};

/// How many stretches of the recording to listen to when finding its
/// languages, and how long each is.
pub const SAMPLES: usize = 8;
pub const SAMPLE_SECONDS: usize = 15;
/// A language counts once it's heard in this many samples (or it's the only
/// one heard).
const MIN_HITS: usize = 2;

/// The languages heard in `hits` (one detected language per sample), most
/// heard first.
pub fn languages_heard(hits: &[String]) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for h in hits {
        match counts.iter_mut().find(|(l, _)| l == h) {
            Some((_, n)) => *n += 1,
            None => counts.push((h.clone(), 1)),
        }
    }
    counts.sort_by_key(|c| std::cmp::Reverse(c.1));
    let only_one = counts.len() == 1;
    counts
        .into_iter()
        .filter(|(_, n)| *n >= MIN_HITS || only_one)
        .map(|(l, _)| l)
        .collect()
}

/// Whether a model knows every language in `needed`. A model that doesn't
/// list its languages is taken to know only English.
pub fn covers(model: &ModelInfo, needed: &[String]) -> bool {
    let known = |l: &str| {
        if model.supported_languages.is_empty() {
            return l == "en";
        }
        model
            .supported_languages
            .iter()
            .any(|m| m == l || m.split(['-', '_']).next() == Some(l))
    };
    needed.iter().all(|l| known(l))
}

/// Where a meeting in `needed` languages should be transcribed.
#[derive(Debug, PartialEq)]
pub enum Route {
    /// The dictation model knows them.
    Current,
    /// A downloaded model that does, the most accurate one.
    Other(String),
    /// No downloaded model knows them all.
    NoModel,
}

pub fn route(models: &[ModelInfo], current_id: &str, needed: &[String]) -> Route {
    if needed.is_empty() {
        return Route::Current;
    }
    if models
        .iter()
        .any(|m| m.id == current_id && covers(m, needed))
    {
        return Route::Current;
    }
    models
        .iter()
        .filter(|m| m.is_downloaded && matches!(m.engine_type, EngineType::TranscribeCpp))
        .filter(|m| covers(m, needed))
        .max_by(|a, b| a.accuracy_score.total_cmp(&b.accuracy_score))
        .map_or(Route::NoModel, |m| Route::Other(m.id.clone()))
}

/// The language to hold a meeting's chunks to: its only one, if it has just
/// one. Unpinned, a model guesses per chunk, and a 1 s "uh" can come out as
/// another language.
pub fn pinned(languages: &[String]) -> Option<&str> {
    match languages {
        [only] => Some(only.as_str()),
        _ => None,
    }
}

/// Languages written in the Latin alphabet.
const LATIN: &[&str] = &[
    "af", "ca", "cs", "cy", "da", "de", "en", "es", "et", "eu", "fi", "fr", "ga", "gl", "hr", "hu",
    "id", "is", "it", "lt", "lv", "ms", "nb", "nl", "nn", "no", "pl", "pt", "ro", "sk", "sl", "sq",
    "sv", "sw", "tl", "tr", "vi",
];

/// Text with letters, none of them Latin, in a meeting held only in
/// Latin-alphabet languages: a filler or laugh the model wrote in another
/// language ("嗯。"), not something that was said.
pub fn wrong_script(text: &str, languages: &[String]) -> bool {
    if languages.is_empty() || !languages.iter().all(|l| LATIN.contains(&l.as_str())) {
        return false;
    }
    let mut letters = text.chars().filter(|c| c.is_alphabetic()).peekable();
    letters.peek().is_some() && letters.all(|c| !c.is_ascii() && !matches!(c, '\u{c0}'..='\u{24f}'))
}

/// Blank the segments in the wrong script (see [`wrong_script`]), as if
/// nothing was heard there. Returns how many.
pub fn blank_wrong_script(
    segments: &mut [super::transcript::Segment],
    languages: &[String],
) -> usize {
    let mut n = 0;
    for s in segments
        .iter_mut()
        .filter(|s| wrong_script(&s.text, languages))
    {
        s.text.clear();
        n += 1;
    }
    n
}

/// A downloaded model that can find the spoken language, for listening to
/// a recording whose languages weren't chosen: the most accurate one.
pub fn detector(models: &[ModelInfo]) -> Option<&ModelInfo> {
    models
        .iter()
        .filter(|m| {
            m.is_downloaded
                && matches!(m.engine_type, EngineType::TranscribeCpp)
                && m.supports_language_detection
        })
        .max_by(|a, b| a.accuracy_score.total_cmp(&b.accuracy_score))
}

/// Where to listen: `SAMPLES` stretches spread over the recording, each the
/// loudest of a few nearby candidates so silence isn't sampled.
pub fn sample_spans(audio: &[f32]) -> Vec<std::ops::Range<usize>> {
    let len = SAMPLE_SECONDS * 16_000;
    if audio.len() <= len {
        return if audio.is_empty() {
            vec![]
        } else {
            std::iter::once(0..audio.len()).collect()
        };
    }
    let rms = |r: &std::ops::Range<usize>| {
        let s = &audio[r.clone()];
        (s.iter().step_by(8).map(|v| v * v).sum::<f32>() / (s.len() / 8).max(1) as f32).sqrt()
    };
    let slot = audio.len() / SAMPLES;
    (0..SAMPLES)
        .filter_map(|i| {
            let candidates: Vec<std::ops::Range<usize>> = (0..4)
                .map(|k| i * slot + k * slot / 4)
                .filter(|&from| from + len <= audio.len())
                .map(|from| from..from + len)
                .collect();
            candidates
                .into_iter()
                .max_by(|a, b| rms(a).total_cmp(&rms(b)))
                .filter(|r| rms(r) > 0.003)
        })
        .collect()
}

/// A line for a speech model's prompt when a meeting mixes languages, so it
/// keeps each in its own language instead of translating.
pub fn prompt_for(languages: &[String]) -> Option<String> {
    if languages.len() < 2 {
        return None;
    }
    let names: Vec<&str> = languages.iter().map(|l| name(l)).collect();
    Some(format!(
        "The speakers switch between {}.",
        match names.split_last() {
            Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
            None => String::new(),
        }
    ))
}

fn name(code: &str) -> &str {
    match code {
        "en" => "English",
        "id" => "Indonesian",
        "ms" => "Malay",
        "de" => "German",
        "fr" => "French",
        "es" => "Spanish",
        "it" => "Italian",
        "pt" => "Portuguese",
        "nl" => "Dutch",
        "zh" => "Chinese",
        "ja" => "Japanese",
        "ko" => "Korean",
        "vi" => "Vietnamese",
        "th" => "Thai",
        "tl" => "Tagalog",
        "hi" => "Hindi",
        "ar" => "Arabic",
        "ru" => "Russian",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn langs(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    fn model(id: &str, langs: &[&str], acc: f32, downloaded: bool) -> ModelInfo {
        let mut m: ModelInfo = serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "description": "", "filename": "", "source": {"Url": {"url": "", "sha256": null}},
            "size_mb": 0, "is_downloaded": downloaded, "is_downloading": false,
            "partial_size": 0, "is_directory": false, "engine_type": "TranscribeCpp",
            "accuracy_score": acc, "speed_score": 0.5, "supports_translation": false,
            "is_recommended": false, "supported_languages": [], "supports_language_selection": true,
            "is_custom": false, "supports_streaming": false, "supports_language_detection": true
        }))
        .unwrap();
        m.supported_languages = langs.iter().map(|l| l.to_string()).collect();
        m
    }

    #[test]
    fn a_meeting_goes_to_a_model_that_knows_its_languages() {
        let models = vec![
            model("cohere", &["en", "de", "fr"], 0.9, true),
            model("whisper", &["en", "id", "ms"], 0.8, true),
            model("qwen", &["en", "id"], 0.85, false),
        ];
        assert_eq!(route(&models, "cohere", &langs(&["en"])), Route::Current);
        assert_eq!(
            route(&models, "cohere", &langs(&["id", "en"])),
            Route::Other("whisper".into())
        );
        assert_eq!(route(&models, "cohere", &langs(&["th"])), Route::NoModel);
        assert_eq!(route(&models, "cohere", &[]), Route::Current);
    }

    #[test]
    fn languages_heard_twice_count() {
        let hits: Vec<String> = ["id", "en", "id", "is", "en", "id"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(languages_heard(&hits), vec!["id", "en"]);
        assert_eq!(languages_heard(&["de".to_string()]), vec!["de"]);
        assert_eq!(
            prompt_for(&["id".into(), "en".into()]).as_deref(),
            Some("The speakers switch between Indonesian and English.")
        );
    }

    #[test]
    fn fillers_in_another_script_are_caught_only_in_latin_meetings() {
        let en = langs(&["en"]);
        assert!(wrong_script("嗯。", &en));
        assert!(wrong_script("الله، الله.", &en));
        assert!(!wrong_script("Uh...", &en));
        assert!(!wrong_script("Café crème", &langs(&["fr"])));
        assert!(!wrong_script("…", &en));
        assert!(!wrong_script("嗯。", &langs(&["zh"])));
        assert!(!wrong_script("嗯。", &langs(&["en", "zh"])));
        assert!(!wrong_script("嗯。", &[]));
    }

    #[test]
    fn only_a_single_language_is_pinned() {
        assert_eq!(pinned(&langs(&["en"])), Some("en"));
        assert_eq!(pinned(&langs(&["en", "id"])), None);
        assert_eq!(pinned(&[]), None);
    }
}
