//! Words spelled out letter by letter ("Rivera, R I V E R A").
//!
//! Speech recognizers write spelled letters in many ways ("S J O G R E N",
//! "S-J-O-G-R-E-N", "Z. H. O. U.", "p e n g u i n", "double L"), so this works
//! on the text alone, whatever model produced it. A run of three or more
//! single letters becomes one word. When the speaker spells the word they just
//! said (often misheard: "Casper. C A S P A R."), the spelling replaces that
//! word instead of repeating it.

use once_cell::sync::Lazy;
use regex::Regex;

/// Fewer letters than this is normal speech ("plan A or B", "I").
const MIN_LETTERS: usize = 3;

static TOKEN_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[\p{L}\p{N}]+(?:['’][\p{L}\p{N}]+)*").unwrap());

/// Words that introduce a spelling of the word before them.
const CONNECTORS: &[&str] = &[
    "spelled", "spelt", "spelling", "that's", "thats", "that", "is",
];

#[derive(Debug)]
struct Token<'a> {
    text: &'a str,
    start: usize,
    end: usize,
}

fn single_letter(token: &str) -> Option<char> {
    let mut chars = token.chars();
    let c = chars.next()?;
    (chars.next().is_none() && c.is_ascii_alphabetic()).then_some(c)
}

/// What sits between two tokens of a run: spaces and the punctuation
/// recognizers put between spelled letters.
fn run_gap(gap: &str) -> bool {
    gap.chars()
        .all(|c| c.is_whitespace() || matches!(c, '.' | ',' | '-'))
}

/// A spelled run starting at token `i`: (index after the run, word, letters).
fn read_run(tokens: &[Token], text: &str, i: usize) -> Option<(usize, String, usize)> {
    let mut word = String::new();
    let mut letters = 0;
    let mut j = i;
    let mut pending_case: Option<bool> = None; // Some(true) = capital
    let mut repeat = 1;
    let mut last_letter_end = i;
    while j < tokens.len() {
        if j > i {
            let gap = &text[tokens[j - 1].end..tokens[j].start];
            // A period only separates letters when every gap has one
            // ("Z. H. O. U."); otherwise it ends a sentence ("R E A D M E. M D").
            let dotted = text[tokens[i].end..tokens[i + 1].start].contains('.');
            if !run_gap(gap) || (gap.contains('.') && !dotted) {
                break;
            }
        }
        let lower = tokens[j].text.to_lowercase();
        if let Some(c) = single_letter(tokens[j].text) {
            let c = match pending_case.take() {
                Some(true) => c.to_ascii_uppercase(),
                Some(false) => c.to_ascii_lowercase(),
                None => c,
            };
            for _ in 0..repeat {
                word.push(c);
            }
            letters += repeat;
            repeat = 1;
            j += 1;
            last_letter_end = j;
            continue;
        }
        // Modifiers only count when a letter follows them.
        let next_is_letter = tokens
            .get(j + 1)
            .is_some_and(|t| single_letter(t.text).is_some())
            && run_gap(&text[tokens[j].end..tokens.get(j + 1).map_or(0, |t| t.start)]);
        match lower.as_str() {
            "double" | "triple" if next_is_letter => {
                repeat = if lower == "double" { 2 } else { 3 };
            }
            "capital" | "uppercase" | "cap" if next_is_letter => pending_case = Some(true),
            "lowercase" | "small" if next_is_letter => pending_case = Some(false),
            "apostrophe" if next_is_letter && letters > 0 => word.push('\''),
            // Recognizers hear "…N apostrophe S" as "…N and apostrophe S".
            "and" | "an"
                if letters > 0
                    && tokens
                        .get(j + 1)
                        .is_some_and(|t| t.text.eq_ignore_ascii_case("apostrophe")) => {}
            "dash" | "hyphen" if next_is_letter && letters > 0 => word.push('-'),
            _ => break,
        }
        j += 1;
    }
    (letters >= MIN_LETTERS).then_some((last_letter_end, word, letters))
}

/// Normalized form for comparing a spoken word with its spelling.
fn fold(word: &str) -> String {
    word.chars()
        .filter_map(|c| match c {
            'à' | 'á' | 'â' | 'ä' | 'ã' | 'å' => Some('a'),
            'è' | 'é' | 'ê' | 'ë' => Some('e'),
            'ì' | 'í' | 'î' | 'ï' => Some('i'),
            'ò' | 'ó' | 'ô' | 'ö' | 'õ' | 'ø' => Some('o'),
            'ù' | 'ú' | 'û' | 'ü' => Some('u'),
            'ç' => Some('c'),
            'ñ' => Some('n'),
            c if c.is_alphanumeric() => Some(c.to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

fn is_capitalized(word: &str) -> bool {
    word.chars().next().is_some_and(char::is_uppercase)
}

/// The word is the first of its sentence (so its capital means nothing).
fn starts_sentence(text: &str, start: usize) -> bool {
    text[..start]
        .trim_end()
        .chars()
        .last()
        .is_none_or(|c| matches!(c, '.' | '!' | '?' | '\n'))
}

/// Casing for a spelled word. Letters the recognizer wrote in mixed case are
/// kept; all lowercase stays lowercase; all capitals become a name, or stay
/// capitals when short (an acronym) or when the word replaced was capitals.
fn apply_case(word: &str, like: Option<&str>) -> String {
    let has_upper = word.chars().any(|c| c.is_ascii_uppercase());
    let has_lower = word.chars().any(|c| c.is_ascii_lowercase());
    if has_upper && has_lower {
        return word.to_string();
    }
    let title = || {
        let lower = word.to_lowercase();
        let mut chars = lower.chars();
        chars
            .next()
            .map(|c| c.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    };
    match like {
        Some(like) if like.chars().all(|c| !c.is_alphabetic() || c.is_uppercase()) => {
            word.to_uppercase()
        }
        // People spell names; a lowercase word before capital letters was
        // usually a misheard name ("chagrins, S J O G R E N").
        Some(_) => title(),
        None if !has_upper => word.to_string(),
        None if word.chars().filter(char::is_ascii_alphabetic).count() <= 4 => word.to_string(),
        None => title(),
    }
}

fn similarity(previous: &str, spelled: &str) -> f64 {
    let (a, b) = (fold(previous), fold(spelled));
    if a.is_empty() {
        return 0.0;
    }
    strsim::normalized_levenshtein(&a, &b)
}

/// Letters that recognizers swap for the same sound ("Carla"/"Karla").
fn same_sound(a: char, b: char) -> bool {
    const GROUPS: &[&str] = &["ckq", "csz", "jg", "fp", "iy", "aeiou"];
    a == b || GROUPS.iter().any(|g| g.contains(a) && g.contains(b))
}

/// Whether the word before a spelling is the word being spelled. A loose match
/// must also start with the same sound, so short everyday words don't match
/// by chance ("named" / "R E A D M E M D").
fn spells(previous: &str, spelled: &str) -> bool {
    let sim = similarity(previous, spelled);
    let first = |w: &str| fold(w).chars().next();
    let same_start = match (first(previous), first(spelled)) {
        (Some(a), Some(b)) => same_sound(a, b),
        _ => false,
    };
    sim >= 0.6 || (sim >= 0.5 && same_start)
}

pub fn apply_spelled_words(text: &str) -> String {
    let tokens: Vec<Token> = TOKEN_RE
        .find_iter(text)
        .map(|m| Token {
            text: m.as_str(),
            start: m.start(),
            end: m.end(),
        })
        .collect();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0; // bytes of `text` already handled
    let mut emitted = 0; // tokens already handled
    let mut i = 0;
    while i < tokens.len() {
        let Some((end, spelled, _)) = read_run(&tokens, text, i) else {
            i += 1;
            continue;
        };
        let run_start = tokens[i].start;
        let run_end = tokens[end - 1].end;

        // The word being spelled: the one before the run, skipping a connector
        // ("Siobhan, that's S I O B H A N"), with only punctuation between.
        let mut k = i;
        while k > emitted
            && CONNECTORS.contains(&tokens[k - 1].text.to_lowercase().as_str())
            && text[tokens[k - 1].end..tokens[k].start]
                .chars()
                .all(|c| c.is_whitespace() || matches!(c, ',' | '.' | ':' | '-'))
        {
            k -= 1;
        }
        let previous = (k > emitted
            && text[tokens[k - 1].end..tokens[k].start]
                .chars()
                .all(|c| c.is_whitespace() || matches!(c, ',' | '.' | ':' | '-' | '(')))
        .then(|| &tokens[k - 1]);
        let target = previous.filter(|p| {
            let named = is_capitalized(p.text) && p.text != "I" && !starts_sentence(text, p.start);
            // A name right before a long spelling is the word being spelled,
            // even when misheard ("Win N G U Y E N"). Short runs need a close
            // match, since they may be acronyms ("Dana A S A P").
            spells(p.text, &spelled)
                || (named && spelled.chars().filter(char::is_ascii_alphabetic).count() >= 5)
        });

        // Whisper writes a spelled possessive as "S-J-O-G-R-E-N'".
        let mut run_end = run_end;
        let mut possessive = false;
        if text[run_end..].starts_with(['\'', '’'])
            && !text[run_end..]
                .chars()
                .nth(1)
                .is_some_and(char::is_alphanumeric)
        {
            run_end += text[run_end..].chars().next().map_or(0, char::len_utf8);
            possessive = true;
        }

        match target {
            Some(p) => {
                // Keep the possessive of the word being replaced.
                let lower = p.text.to_lowercase();
                let possessive = possessive || lower.ends_with("'s") || lower.ends_with("’s");
                let mut word = apply_case(&spelled, Some(p.text));
                if possessive && !word.to_lowercase().ends_with("'s") {
                    word.push_str("'s");
                }
                out.push_str(&text[copied..p.start]);
                out.push_str(&word);
            }
            None => {
                out.push_str(&text[copied..run_start]);
                out.push_str(&apply_case(&spelled, None));
                if possessive {
                    out.push_str("'s");
                }
            }
        }
        copied = run_end;
        emitted = end;
        i = end;
    }
    out.push_str(&text[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::apply_spelled_words as f;

    #[test]
    fn spelling_replaces_the_word_it_spells() {
        assert_eq!(
            f("likely Sjogren's, S J O G R E N apostrophe S, and just"),
            "likely Sjogren's, and just"
        );
        assert_eq!(f("My name is Casper. C A S P A R."), "My name is Caspar.");
        assert_eq!(f("Book it under Win N G U Y E N."), "Book it under Nguyen.");
        assert_eq!(
            f("She likely has shagrens, S-J-O-G-R-E-N' and she gets tired."),
            "She likely has Sjogren's and she gets tired."
        );
        assert_eq!(
            f("has showgrins, S, J, O, G, R, E, N, and apostrophe S, and she"),
            "has Sjogren's, and she"
        );
        assert_eq!(
            f("The file is named R E A D M E. M D."),
            "The file is named Readme. M D."
        );
        assert_eq!(f("My name is Rivera. R.I.V.E.R.A."), "My name is Rivera.");
        // Too short to trust a mismatch: joined, not replaced.
        assert_eq!(
            f("My last name is Shaw. X. I. A. O."),
            "My last name is Shaw. XIAO."
        );
        assert_eq!(
            f("The company is called Lumenfold, spelled L-U-M-E-N-F-O-L-D."),
            "The company is called Lumenfold."
        );
        assert_eq!(
            f("Send it to Chavorn. That's S I O B H A N."),
            "Send it to Siobhan."
        );
        assert_eq!(f("It's Philip. P. H. I. Double L. I. P."), "It's Phillip.");
        assert_eq!(
            f("Call Leo. L. E. O. Tomorrow morning."),
            "Call Leo. Tomorrow morning."
        );
    }

    #[test]
    fn spelling_on_its_own_becomes_a_word() {
        assert_eq!(
            f("The password hint is p e n g u i n."),
            "The password hint is penguin."
        );
        assert_eq!(
            f("Search for a user called M A R L O W E in the admin panel."),
            "Search for a user called Marlowe in the admin panel."
        );
        assert_eq!(
            f("We moved to the U S A last year."),
            "We moved to the USA last year."
        );
        assert_eq!(f("Tell Dana A S A P."), "Tell Dana ASAP.");
    }

    #[test]
    fn letters_in_normal_speech_are_left_alone() {
        for text in [
            "Should we pick plan A, B, or C?",
            "Press A, then B, then C.",
            "Grade A eggs are on aisle B.",
            "Vitamin C and D are important in winter.",
            "The T in the logo should be blue.",
            "I think I need a break.",
            "Is it option A or option B?",
            "Row A seats one through four.",
        ] {
            assert_eq!(f(text), text);
        }
    }
}
