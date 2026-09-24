//! Canonical spelling for technical vocabulary terms.
//!
//! Biasing gets the model to *hear* a term, but its surface form still drifts:
//! "GLM 5.3" or "G L M five point three" for GLM-5.3, "Qwen3 ASR" for
//! Qwen3-ASR, "M4 MAX" for M4 Max. For every custom word that looks like an
//! identifier (digits, internal punctuation or several capitals) we build a
//! matcher that tolerates case, separators, spelled-out acronym letters and
//! spoken numbers, and rewrite matches to the exact vocabulary spelling.
//!
//! Plain words ("Handy", "Priya") are skipped: matching them case-insensitively
//! would recase ordinary English ("handy") and biasing already handles them.

use once_cell::sync::Lazy;
use regex::{NoExpand, Regex, RegexBuilder};
use std::sync::Mutex;

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// Spoken forms of a digit run: the digits themselves, plus number words for
/// 0–99 and digit-by-digit reading for longer runs ("one two eight").
fn digits_pattern(digits: &str) -> String {
    let mut alternatives = vec![regex::escape(digits)];
    if let Ok(n) = digits.parse::<usize>() {
        if digits.len() <= 2 || !digits.starts_with('0') {
            if n < 20 {
                alternatives.push(ONES[n].to_string());
            } else if n < 100 {
                let tens = TENS[n / 10];
                alternatives.push(if n % 10 == 0 {
                    tens.to_string()
                } else {
                    format!(r"{tens}[\s\-]?{}", ONES[n % 10])
                });
            }
        }
        if n == 0 {
            alternatives.push("oh".to_string());
        }
    }
    if digits.len() > 1 {
        let spelled: Vec<&str> = digits
            .chars()
            .filter_map(|c| c.to_digit(10))
            .map(|d| ONES[d as usize])
            .collect();
        alternatives.push(spelled.join(r"[\s\-]+"));
    }
    format!("(?:{})", alternatives.join("|"))
}

#[derive(Debug, PartialEq)]
enum Chunk {
    Letters(String),
    Digits(String),
    Dot,
    Sep,
    Other(char),
}

fn chunks(term: &str) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    for c in term.chars() {
        let next = if c.is_alphabetic() {
            if let Some(Chunk::Letters(s)) = out.last_mut() {
                s.push(c);
                continue;
            }
            Chunk::Letters(c.to_string())
        } else if c.is_ascii_digit() {
            if let Some(Chunk::Digits(s)) = out.last_mut() {
                s.push(c);
                continue;
            }
            Chunk::Digits(c.to_string())
        } else if c == '.' {
            Chunk::Dot
        } else if c.is_whitespace() || c == '-' || c == '_' {
            if matches!(out.last(), Some(Chunk::Sep)) {
                continue;
            }
            Chunk::Sep
        } else {
            Chunk::Other(c)
        };
        out.push(next);
    }
    out
}

/// Whether a term is identifier-like enough to normalize.
fn is_technical(term: &str) -> bool {
    let uppercase = term.chars().filter(|c| c.is_uppercase()).count();
    term.chars().any(|c| c.is_ascii_digit())
        || term.trim().contains(['-', '.', '_', '/', '+'])
        || uppercase >= 2
}

/// Regex matching the spoken/written variants of `term`, or `None` for plain
/// words.
pub fn term_pattern(term: &str) -> Option<String> {
    let term = term.trim();
    if term.is_empty() || !is_technical(term) {
        return None;
    }
    let parts = chunks(term);
    let mut pattern = String::new();
    for (i, part) in parts.iter().enumerate() {
        let prev = i.checked_sub(1).and_then(|j| parts.get(j));
        let next = parts.get(i + 1);
        // Adjacent letter/digit runs with no separator in the term ("Qwen3",
        // "M4", "4o") may still be spoken or written apart.
        let glued = matches!(
            (prev, part),
            (Some(Chunk::Letters(_)), Chunk::Digits(_))
                | (Some(Chunk::Digits(_)), Chunk::Letters(_))
        );
        if glued {
            pattern.push_str(r"[\s\-]?");
        }
        match part {
            Chunk::Letters(letters) => {
                let is_acronym =
                    letters.chars().count() <= 5 && letters.chars().all(|c| c.is_uppercase());
                if is_acronym {
                    // "GLM", "G L M", "G-L-M", "G.L.M."
                    let spelled: Vec<String> = letters
                        .chars()
                        .map(|c| regex::escape(&c.to_string()))
                        .collect();
                    pattern.push_str(&spelled.join(r"[\s.\-]?"));
                    pattern.push_str(r"\.?");
                } else {
                    pattern.push_str(&regex::escape(letters));
                }
            }
            Chunk::Digits(digits) => pattern.push_str(&digits_pattern(digits)),
            Chunk::Dot => {
                let between_digits = matches!(prev, Some(Chunk::Digits(_)))
                    && matches!(next, Some(Chunk::Digits(_)));
                pattern.push_str(if between_digits {
                    r"(?:\s*\.\s*|\s+(?:point|dot)\s+)"
                } else {
                    r"\.?"
                });
            }
            Chunk::Sep => pattern.push_str(r"[\s\-_]*"),
            Chunk::Other(c) => {
                pattern.push_str(r"\s*");
                pattern.push_str(&regex::escape(&c.to_string()));
                pattern.push_str(r"\s*");
            }
        }
    }
    let starts_word = term.chars().next().is_some_and(char::is_alphanumeric);
    let ends_word = term.chars().last().is_some_and(char::is_alphanumeric);
    Some(format!(
        "{}{}{}",
        if starts_word { r"\b" } else { "" },
        pattern,
        if ends_word { r"\b" } else { "" }
    ))
}

/// Compiled matchers for the current vocabulary, rebuilt when it changes.
static CACHE: Lazy<Mutex<(Vec<String>, Vec<(Regex, String)>)>> =
    Lazy::new(|| Mutex::new((Vec::new(), Vec::new())));

fn matchers(custom_words: &[String]) -> Vec<(Regex, String)> {
    let mut cache = CACHE.lock().unwrap();
    if cache.0 != custom_words {
        // Longer terms first so "Claude Sonnet 4.5" wins over "Sonnet 4.5".
        let mut terms: Vec<&String> = custom_words.iter().collect();
        terms.sort_by_key(|t| std::cmp::Reverse(t.len()));
        let compiled = terms
            .into_iter()
            .filter_map(|term| {
                let pattern = term_pattern(term)?;
                match RegexBuilder::new(&pattern).case_insensitive(true).build() {
                    Ok(re) => Some((re, term.trim().to_string())),
                    Err(e) => {
                        log::warn!("Vocabulary term '{term}' has no matcher: {e}");
                        None
                    }
                }
            })
            .collect();
        *cache = (custom_words.to_vec(), compiled);
    }
    cache.1.clone()
}

/// Rewrite spoken or drifted forms of technical vocabulary terms to their
/// exact spelling.
pub fn apply_canonical_forms(text: &str, custom_words: &[String]) -> String {
    let mut text = text.to_string();
    for (re, term) in matchers(custom_words) {
        text = re.replace_all(&text, NoExpand(&term)).into_owned();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon(text: &str, vocab: &[&str]) -> String {
        let vocab: Vec<String> = vocab.iter().map(|s| s.to_string()).collect();
        apply_canonical_forms(text, &vocab)
    }

    #[test]
    fn glm_variants() {
        for spoken in [
            "compare GLM 5.3 today",
            "compare GLM5.3 today",
            "compare G L M five point three today",
            "compare g-l-m 5 point 3 today",
            "compare glm-5.3 today",
        ] {
            assert_eq!(
                canon(spoken, &["GLM-5.3"]),
                "compare GLM-5.3 today",
                "{spoken}"
            );
        }
    }

    #[test]
    fn mixed_identifiers() {
        let vocab = [
            "Qwen3-ASR",
            "M4 Max",
            "v2.1.22",
            "GPT-4o",
            "Claude Sonnet 4.5",
        ];
        assert_eq!(
            canon("We deployed Qwen3 ASR on the M4 MAX.", &vocab),
            "We deployed Qwen3-ASR on the M4 Max."
        );
        assert_eq!(
            canon("bump to v two point one point twenty two", &vocab),
            "bump to v2.1.22"
        );
        assert_eq!(
            canon("try GPT 4o and gpt-4o", &vocab),
            "try GPT-4o and GPT-4o"
        );
        assert_eq!(
            canon("use claude sonnet four point five", &vocab),
            "use Claude Sonnet 4.5"
        );
    }

    #[test]
    fn plain_words_are_left_alone() {
        assert_eq!(
            canon("that's handy, thanks", &["Handy"]),
            "that's handy, thanks"
        );
        assert_eq!(term_pattern("Priya"), None);
    }

    #[test]
    fn partial_matches_do_not_rewrite() {
        let vocab = ["GLM-5.3"];
        assert_eq!(canon("GLM is great", &vocab), "GLM is great");
        assert_eq!(canon("GLM 5.35", &vocab), "GLM 5.35");
        assert_eq!(canon("XGLM 5.3", &vocab), "XGLM 5.3");
    }

    #[test]
    fn longer_terms_win() {
        let vocab = ["Sonnet 4.5", "Claude Sonnet 4.5"];
        assert_eq!(
            canon("ask claude sonnet 4.5", &vocab),
            "ask Claude Sonnet 4.5"
        );
    }
}
