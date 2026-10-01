//! Clues in what people say about who's talking: "Hi, I'm Priya", "Sam,
//! what do you think?" (the next voice is probably Sam's), "thanks, Sam"
//! (the last one probably was), "as Sam said" (this one isn't Sam). The
//! meeting's language model finds them; each must quote the paragraph it
//! comes from, word for word with the name in it, or it's dropped. They're
//! weak evidence (0.4–0.6) that the resolver weighs with the rest. Found
//! once per transcript, with the summary (so not when there's no model).
//!
//! The same pass looks for turns the voices got wrong: a paragraph that
//! answers a question put under the same label, or a back-and-forth whose
//! labels don't alternate. Those paragraphs get the label the conversation
//! says (`turns.json`, under the user's own fixes) and are marked unsure.

use super::llm::Llm;
use super::transcript::{Paragraph, Source};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const FILE: &str = "clues.json";
/// Paragraphs given another speaker from the flow of the conversation, by
/// paragraph key.
pub const TURNS_FILE: &str = "turns.json";
/// More of a batch than this relabelled means the model lost the thread;
/// its turns are dropped.
const MAX_TURNS_SHARE: f32 = 0.25;

/// What a clue says about the speaker of a stretch of a track.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clue {
    pub source: Source,
    pub start_ms: u64,
    pub end_ms: u64,
    pub name: String,
    /// True: this is them. False: this isn't them.
    pub is: bool,
    pub strength: f32,
    pub quote: String,
}

const SELF: f32 = 0.6;
const ADDRESSED: f32 = 0.4;
const THANKED: f32 = 0.4;
const NOT: f32 = 0.5;

const INSTRUCTIONS: &str = "You read a meeting transcript and find clues to who is speaking. Each paragraph has a number and a speaker label; labels are guesses and may be wrong, so don't trust them. Find only these kinds of clue, where a person's name is said out loud:\n- \"self\": the speaker gives their own name (\"I'm Priya\", \"Sam here\").\n- \"addressed\": the speaker hands over to or asks someone by name (\"Sam, what do you think?\"), so the next speaker is probably that person and this speaker isn't.\n- \"thanked\": the speaker thanks or answers someone by name right after they spoke (\"thanks, Sam\"), so the previous speaker probably was that person and this one isn't.\n- \"not\": the speaker talks about someone by name in the third person (\"as Sam said\"), so this speaker isn't that person.\nFor each clue give the paragraph number, the name as said, the kind, and a short quote copied exactly from that paragraph that contains the name. Skip anything uncertain. Most paragraphs have no clue; an empty list is fine.\n\nAlso list \"turns\": paragraphs the conversation shows were said by someone other than their label, because voices that sound alike get mixed up. Signs: a paragraph answers a question asked in the paragraph just before under the same label (people don't answer themselves); a back-and-forth between two people where the labels don't alternate; someone named and asked a question replies. Give the paragraph number and the label of who most likely said it, written exactly as one of the labels in the transcript. Only where the conversation makes it clear; most labels are right, and an empty list is fine.";

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "clues": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "paragraph": { "type": "integer" },
                        "name": { "type": "string" },
                        "kind": { "type": "string", "enum": ["self", "addressed", "thanked", "not"] },
                        "quote": { "type": "string" }
                    },
                    "required": ["paragraph", "name", "kind", "quote"],
                    "additionalProperties": false
                }
            },
            "turns": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "paragraph": { "type": "integer" },
                        "speaker": { "type": "string" }
                    },
                    "required": ["paragraph", "speaker"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["clues", "turns"],
        "additionalProperties": false
    })
}

fn norm(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap_or(c)
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The nearest paragraph before or after `i` on the same track by someone
/// labelled differently.
fn neighbour(paragraphs: &[Paragraph], i: usize, forward: bool) -> Option<&Paragraph> {
    let here = &paragraphs[i];
    let differs = |p: &&Paragraph| p.speaker != here.speaker || p.source != here.source;
    if forward {
        paragraphs[i + 1..].iter().take(3).find(differs)
    } else {
        paragraphs[..i].iter().rev().take(3).find(differs)
    }
}

/// Turn the model's reply into clues, keeping only those whose quote is in
/// the paragraph and names the person.
pub fn from_reply(reply: &Value, paragraphs: &[Paragraph], numbers: &[usize]) -> Vec<Clue> {
    let mut out = Vec::new();
    let Some(items) = reply["clues"].as_array() else {
        return out;
    };
    for item in items {
        let (Some(n), Some(name), Some(kind), Some(quote)) = (
            item["paragraph"].as_u64(),
            item["name"].as_str(),
            item["kind"].as_str(),
            item["quote"].as_str(),
        ) else {
            continue;
        };
        let Some(&i) = numbers.get(n as usize) else {
            continue;
        };
        let p = &paragraphs[i];
        let (q, name_n) = (norm(quote), norm(name));
        if q.is_empty() || name_n.is_empty() || !norm(&p.text).contains(&q) {
            continue;
        }
        if !format!(" {q} ").contains(&format!(" {name_n} ")) {
            continue;
        }
        let name = name.trim().to_string();
        let clue = |p: &Paragraph, is: bool, strength: f32| Clue {
            source: p.source,
            start_ms: p.start_ms,
            end_ms: p.end_ms,
            name: name.clone(),
            is,
            strength,
            quote: quote.to_string(),
        };
        match kind {
            "self" => out.push(clue(p, true, SELF)),
            "addressed" => {
                out.push(clue(p, false, NOT));
                if let Some(next) = neighbour(paragraphs, i, true) {
                    out.push(clue(next, true, ADDRESSED));
                }
            }
            "thanked" => {
                out.push(clue(p, false, NOT));
                if let Some(prev) = neighbour(paragraphs, i, false) {
                    out.push(clue(prev, true, THANKED));
                }
            }
            "not" => out.push(clue(p, false, NOT)),
            _ => {}
        }
    }
    out
}

/// The turns in the model's reply: paragraph key → speaker, for labels
/// that name a voice heard on the same track, different from the one the
/// paragraph has. None of them if the model relabelled too much.
pub fn turns_from_reply(
    reply: &Value,
    paragraphs: &[Paragraph],
    numbers: &[usize],
    label: &dyn Fn(&Paragraph) -> String,
) -> BTreeMap<String, u32> {
    let mut voices: BTreeMap<(bool, String), u32> = BTreeMap::new();
    for p in paragraphs {
        if let Some(v) = p.speaker {
            voices
                .entry((p.source == Source::Mic, label(p)))
                .or_insert(v);
        }
    }
    let mut out = BTreeMap::new();
    for item in reply["turns"].as_array().into_iter().flatten() {
        let (Some(n), Some(who)) = (item["paragraph"].as_u64(), item["speaker"].as_str()) else {
            continue;
        };
        let Some(p) = numbers.get(n as usize).map(|&i| &paragraphs[i]) else {
            continue;
        };
        let Some(&v) = voices.get(&(p.source == Source::Mic, who.trim().to_string())) else {
            continue;
        };
        if p.speaker.is_some() && p.speaker != Some(v) {
            out.insert(super::summary::paragraph_key(p), v);
        }
    }
    if out.len() as f32 > numbers.len() as f32 * MAX_TURNS_SHARE {
        log::info!("Dropped {} speaker turns: too many to trust", out.len());
        return BTreeMap::new();
    }
    out
}

/// Ask the model for clues and turns across the meeting, in pieces it can
/// take.
pub async fn find(
    llm: &Llm,
    paragraphs: &[Paragraph],
    label: &dyn Fn(&Paragraph) -> String,
) -> Result<(Vec<Clue>, BTreeMap<String, u32>), String> {
    let budget = llm.budget().summary.min(40_000);
    let (mut out, mut turns) = (Vec::new(), BTreeMap::new());
    for batch in super::summary::batches(paragraphs, budget) {
        let input: String = batch
            .iter()
            .enumerate()
            .map(|(n, &i)| format!("[{n}] {}: {}\n", label(&paragraphs[i]), paragraphs[i].text))
            .collect();
        let reply = llm.ask_json(INSTRUCTIONS, &input, &schema(), "low").await?;
        out.extend(from_reply(&reply, paragraphs, &batch));
        turns.extend(turns_from_reply(&reply, paragraphs, &batch, label));
    }
    Ok((out, turns))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(start_s: u64, speaker: u32, text: &str) -> Paragraph {
        Paragraph {
            source: Source::System,
            start_ms: start_s * 1000,
            end_ms: start_s * 1000 + 4000,
            speaker: Some(speaker),
            text: text.into(),
            raw: None,
        }
    }

    #[test]
    fn clues_must_quote_their_paragraph_and_name_the_person() {
        let ps = vec![
            para(0, 100, "Hi everyone, I'm Priya and I run pricing."),
            para(5, 100, "Sam, what do you make of the table?"),
            para(10, 101, "I think it's too busy."),
        ];
        let reply = json!({"clues": [
            {"paragraph": 0, "name": "Priya", "kind": "self", "quote": "I'm Priya"},
            {"paragraph": 1, "name": "Sam", "kind": "addressed", "quote": "Sam, what do you make"},
            // Made up: not in the paragraph.
            {"paragraph": 2, "name": "Sam", "kind": "self", "quote": "I'm Sam"},
            // The quote is there but doesn't name anyone.
            {"paragraph": 2, "name": "Jo", "kind": "not", "quote": "too busy"}
        ]});
        let clues = from_reply(&reply, &ps, &[0, 1, 2]);
        assert_eq!(clues.len(), 3);
        assert!(clues[0].is && clues[0].name == "Priya" && clues[0].start_ms == 0);
        // "Sam, …?" says this speaker isn't Sam and the next one probably is.
        assert!(!clues[1].is && clues[1].start_ms == 5_000);
        assert!(clues[2].is && clues[2].name == "Sam" && clues[2].start_ms == 10_000);
    }

    #[test]
    fn turns_move_a_paragraph_to_a_voice_heard_on_its_track() {
        let mut ps: Vec<Paragraph> = (0..8)
            .map(|i| para(i * 5, 100 + (i % 2) as u32, "words"))
            .collect();
        ps[3].speaker = Some(100);
        let label = |p: &Paragraph| format!("Speaker {}", p.speaker.unwrap_or(0));
        let numbers: Vec<usize> = (0..ps.len()).collect();
        let reply = json!({"turns": [
            {"paragraph": 3, "speaker": "Speaker 101"},
            // Nobody by that label: ignored.
            {"paragraph": 4, "speaker": "Priya"},
            // Already that speaker: nothing to do.
            {"paragraph": 5, "speaker": "Speaker 101"}
        ]});
        let turns = turns_from_reply(&reply, &ps, &numbers, &label);
        assert_eq!(turns, BTreeMap::from([("system-15000".to_string(), 101)]));
        // Relabelling most of the batch isn't trusted.
        let wild = json!({"turns": (0..4).map(|i| json!({"paragraph": i * 2, "speaker": "Speaker 101"})).collect::<Vec<_>>()});
        assert!(turns_from_reply(&wild, &ps, &numbers, &label).is_empty());
    }
}
