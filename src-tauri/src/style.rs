//! Deterministic output styling, applied after (optional) AI cleanup.
//!
//! Small on-device models are unreliable at layout, and formality is purely
//! mechanical, so these passes are rules rather than prompt instructions:
//!
//! * numbered lists put one item per line;
//! * in email, a spoken greeting and sign-off get their own lines;
//! * formality per app category: Formal (as cleaned), Casual (no trailing
//!   period, no comma after an opening "hey"), Very casual (Casual plus
//!   lowercase sentence starts).
//!
//! Formality must run after the LLM, which would otherwise restore the
//! capitals and punctuation it removes.

use crate::settings::{AppCategory, Formality};
use once_cell::sync::Lazy;
use regex::Regex;

/// Put inline numbered lists ("… are: 1. Foo 2. Bar 3. Baz") one item per
/// line. Only sequences numbered 1, 2, … with at least two items qualify.
pub fn format_lists(text: &str) -> String {
    static ITEM: Lazy<Regex> = Lazy::new(|| Regex::new(r"(^|[\s:,;])(\d{1,2})\.\s+").unwrap());
    let items: Vec<(usize, usize, u32)> = ITEM
        .captures_iter(text)
        .filter_map(|c| {
            let whole = c.get(0)?;
            let lead = c.get(1)?.as_str().len();
            let n: u32 = c.get(2)?.as_str().parse().ok()?;
            Some((whole.start() + lead, whole.end(), n))
        })
        .collect();

    // Find the first run 1, 2, 3, … of length >= 2.
    let Some(first) = items.iter().position(|&(_, _, n)| n == 1) else {
        return text.to_string();
    };
    let mut run = vec![items[first]];
    for &item in &items[first + 1..] {
        if item.2 == run.last().unwrap().2 + 1 {
            run.push(item);
        }
    }
    if run.len() < 2 {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len() + run.len() * 2);
    let mut cursor = 0;
    for (i, &(start, end, n)) in run.iter().enumerate() {
        let before = text[cursor..start].trim_end_matches([' ', ',', ';', '\n']);
        out.push_str(before);
        if i == 0 && !before.is_empty() && !before.ends_with([':', '\n']) {
            out.push(':');
        }
        out.push('\n');
        out.push_str(&format!("{n}. "));
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    // Item texts: drop the separators the LLM left between inline items and
    // capitalize each item's first letter.
    let lines: Vec<String> = out
        .lines()
        .map(|line| {
            static NUMBERED: Lazy<Regex> =
                Lazy::new(|| Regex::new(r"^(\d{1,2}\. )(.*?)[,;]?\s*$").unwrap());
            match NUMBERED.captures(line) {
                Some(c) => format!("{}{}", &c[1], capitalize_first(&c[2])),
                None => line.to_string(),
            }
        })
        .collect();
    lines.join("\n")
}

static GREETING: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^\s*((?:hi|hello|hey|dear|good (?:morning|afternoon|evening))(?:\s+(?:mr|mrs|ms|dr|prof)\.)?(?:\s+[\p{L}'-]+){0,3}?)\s*[,.!]\s*(\S)",
    )
    .unwrap()
});

static SIGN_OFF: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)(^|[.!?,]\s*|\n\s*)((?:thanks(?: again| so much)?|thank you|many thanks|best(?: regards| wishes)?|kind regards|warm regards|regards|cheers|sincerely|all the best|talk soon)[.!,]?)(?:\s+(\p{Lu}[\p{L}'-]*(?:\s+\p{Lu}[\p{L}'-]*)?))?[.!]?\s*$",
    )
    .unwrap()
});

/// Email layout: greeting on its own line, sign-off in its own paragraph.
/// Nothing is added that wasn't spoken.
pub fn format_email(text: &str) -> String {
    let mut body = text.trim().to_string();
    let mut greeting = None;
    if let Some(c) = GREETING.captures(&body) {
        let head = c.get(1).unwrap().as_str().trim().to_string();
        let rest_start = c.get(2).unwrap().start();
        greeting = Some(format!("{},", head.trim_end_matches([',', '.', '!'])));
        body = capitalize_first(&body[rest_start..]);
    }

    let mut sign_off = None;
    if let Some(c) = SIGN_OFF.captures(&body) {
        let word = c
            .get(2)
            .unwrap()
            .as_str()
            .trim_end_matches(['.', '!', ','])
            .to_string();
        let name = c.get(3).map(|m| m.as_str().to_string());
        let cut = c.get(0).unwrap().start() + c.get(1).unwrap().as_str().len();
        let remaining = body[..cut].trim_end().to_string();
        // A lone "Thanks." email is a message, not a sign-off.
        if !remaining.is_empty() {
            let word = capitalize_first(&word);
            sign_off = Some(match name {
                Some(name) => format!("{word},\n{name}"),
                None if word.starts_with("Thank") => format!("{word}!"),
                None => format!("{word},"),
            });
            let mut remaining = remaining.trim_end_matches(',').to_string();
            if !remaining.ends_with(['.', '!', '?', ':']) {
                remaining.push('.');
            }
            body = remaining;
        }
    }

    let mut out = String::new();
    if let Some(g) = greeting {
        out.push_str(&capitalize_first(&g));
        out.push_str("\n\n");
    }
    out.push_str(body.trim());
    if let Some(s) = sign_off {
        out.push_str("\n\n");
        out.push_str(&s);
    }
    out
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

static LEADING_INTERJECTION_COMMA: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)^(hey|hi|hello|yeah|yes|no|ok|okay|oh|so|well|thanks)(\s+\p{Lu}[\p{L}'-]*)?,\s",
    )
    .unwrap()
});

/// Apply the category's formality.
pub fn apply_formality(text: &str, formality: Formality, vocabulary: &[String]) -> String {
    if formality == Formality::Formal || text.is_empty() {
        return text.to_string();
    }
    // Casual: drop a single trailing period and the comma after an opening
    // interjection ("Hey, are you…" → "Hey are you…").
    let mut out = LEADING_INTERJECTION_COMMA
        .replace(text, |c: &regex::Captures| {
            format!("{}{} ", &c[1], c.get(2).map_or("", |m| m.as_str()))
        })
        .into_owned();
    if out.ends_with('.') && !out.ends_with("..") {
        out.pop();
    }
    if formality == Formality::Casual {
        return out;
    }
    lowercase_sentence_starts(&out, vocabulary)
}

/// Very casual: lowercase the first letter of each sentence, except "I"
/// forms, acronyms and vocabulary terms.
fn lowercase_sentence_starts(text: &str, vocabulary: &[String]) -> String {
    static SENTENCE_START: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"(^|[.!?]\s+|\n\s*)(\p{Lu}[\p{L}'’-]*)").unwrap());
    SENTENCE_START
        .replace_all(text, |c: &regex::Captures| {
            let lead = &c[1];
            let word = &c[2];
            let keep = word == "I"
                || word.starts_with("I'")
                || word.starts_with("I’")
                || word.chars().skip(1).any(char::is_uppercase)
                || vocabulary
                    .iter()
                    .any(|v| v.split_whitespace().next() == Some(word));
            if keep {
                format!("{lead}{word}")
            } else {
                let mut chars = word.chars();
                let first = chars.next().unwrap();
                format!("{lead}{}{}", first.to_lowercase(), chars.as_str())
            }
        })
        .into_owned()
}

/// All deterministic styling for a destination category.
pub fn apply(
    text: &str,
    category: AppCategory,
    formality: Formality,
    vocabulary: &[String],
) -> String {
    let mut out = format_lists(text);
    if category == AppCategory::Email {
        out = format_email(&out);
    }
    apply_formality(&out, formality, vocabulary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_lists_become_lines() {
        assert_eq!(
            format_lists("The plan for this week is: 1. Finish the migration 2. Write the docs 3. Get the demo ready for Friday."),
            "The plan for this week is:\n1. Finish the migration\n2. Write the docs\n3. Get the demo ready for Friday."
        );
        assert_eq!(
            format_lists("The plan is 1. finish the migration, 2. write the docs, 3. ship"),
            "The plan is:\n1. Finish the migration\n2. Write the docs\n3. Ship"
        );
    }

    #[test]
    fn non_lists_are_untouched() {
        for text in [
            "Version 2. That's it.",
            "We moved from 1. to 3. in a day.",
            "Already:\n1. One\n2. Two",
        ] {
            let out = format_lists(text);
            if text.contains('\n') {
                assert_eq!(out, text);
            } else {
                assert!(!out.contains('\n'), "{text:?} -> {out:?}");
            }
        }
    }

    #[test]
    fn email_greeting_and_sign_off() {
        assert_eq!(
            format_email("Hi Sarah, thanks for sending over the contract. Can we set up a call this week? Best, Sam"),
            "Hi Sarah,\n\nThanks for sending over the contract. Can we set up a call this week?\n\nBest,\nSam"
        );
        assert_eq!(
            format_email("Hello everyone, the office is closed Monday. Thanks."),
            "Hello everyone,\n\nThe office is closed Monday.\n\nThanks!"
        );
        assert_eq!(
            format_email("Dear Mr. Thompson, I'm confirming Tuesday at 3 PM. Kind regards"),
            "Dear Mr. Thompson,\n\nI'm confirming Tuesday at 3 PM.\n\nKind regards,"
        );
    }

    #[test]
    fn email_without_greeting_or_sign_off_is_unchanged() {
        let text = "The invoice went out yesterday. Let me know if the client needs anything.";
        assert_eq!(format_email(text), text);
        assert_eq!(format_email("Thanks."), "Thanks.");
    }

    #[test]
    fn formality_levels() {
        let text = "Hey, are you free for lunch tomorrow? Let's do 12 if that works for you.";
        assert_eq!(apply_formality(text, Formality::Formal, &[]), text);
        assert_eq!(
            apply_formality(text, Formality::Casual, &[]),
            "Hey are you free for lunch tomorrow? Let's do 12 if that works for you"
        );
        assert_eq!(
            apply_formality(text, Formality::VeryCasual, &[]),
            "hey are you free for lunch tomorrow? let's do 12 if that works for you"
        );
    }

    #[test]
    fn very_casual_keeps_i_acronyms_and_vocabulary() {
        let vocab = vec!["Sam".to_string()];
        assert_eq!(
            apply_formality(
                "I think so. PR is up. Sam will review. Sounds good.",
                Formality::VeryCasual,
                &vocab
            ),
            "I think so. PR is up. Sam will review. sounds good"
        );
    }
}
