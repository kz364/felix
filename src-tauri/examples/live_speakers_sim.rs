//! Would telling speakers apart while recording let the final pass reuse
//! the live text? Replays a meeting's track: every EVERY seconds (default
//! 120) the voices are worked out on the audio so far, and chunks no more
//! speech can join are cut at the speaker changes found then (what the
//! live pass would transcribe). Then the final pass's pieces, from the
//! whole track, are compared: a piece transcribed live with the same
//! bounds is reused.
//!
//! CUT=turns cuts chunks where the segmentation model hears a new person
//! (local, so the same live and after) instead of where the voice labels
//! change, and reports how mixed the pieces are (share of each piece's
//! labelled frames not its main voice, by the final labels).
//!
//! cargo run --release --example live_speakers_sim -- <speaker model> <meeting dir> [mic|system]

use handy_app_lib::meetings::live::chunk_key;
use handy_app_lib::meetings::transcript::{self, Chunk, Source};
use handy_app_lib::meetings::{diarize, pipeline, segment};
use std::collections::BTreeSet;
use std::path::Path;
use std::time::Instant;

/// Pieces shorter than this join the one before (or after).
const MIN_PIECE_MS: u64 = 1_500;

fn turn_cuts(c: Chunk, change: &[bool]) -> Vec<Chunk> {
    let f = |ms: u64| (ms / transcript::FRAME_MS) as usize;
    let mut bounds = vec![c.start_ms];
    for i in f(c.start_ms) + 1..f(c.end_ms).min(change.len()) {
        if change[i] {
            bounds.push(i as u64 * transcript::FRAME_MS);
        }
    }
    bounds.push(c.end_ms);
    let mut out: Vec<Chunk> = Vec::new();
    for w in bounds.windows(2) {
        out.push(Chunk {
            start_ms: w[0],
            end_ms: w[1],
        });
    }
    // Short pieces join a neighbour (by position only, so it's stable).
    let mut i = 0;
    while i < out.len() && out.len() > 1 {
        if out[i].end_ms - out[i].start_ms < MIN_PIECE_MS {
            if i > 0 {
                out[i - 1].end_ms = out[i].end_ms;
            } else {
                out[1].start_ms = out[0].start_ms;
            }
            out.remove(i);
        } else {
            i += 1;
        }
    }
    out
}

fn pieces(
    speech: &[bool],
    labels: &[Option<u32>],
    change: Option<&[bool]>,
    upto_ms: Option<u64>,
) -> Vec<(Chunk, Chunk)> {
    transcript::plan_chunks(speech)
        .into_iter()
        .filter(|c| upto_ms.is_none_or(|t| c.end_ms + transcript::PAUSE_MS <= t))
        .flat_map(|c| {
            let ps: Vec<Chunk> = match change {
                Some(ch) => turn_cuts(c, ch),
                None => diarize::split_by_speaker(c, labels)
                    .into_iter()
                    .map(|(p, _)| p)
                    .collect(),
            };
            ps.into_iter().map(move |p| (c, p))
        })
        .collect()
}

/// Share of labelled frames in the pieces that aren't their piece's main voice.
fn mixed(pieces: &[(Chunk, Chunk)], labels: &[Option<u32>]) -> f64 {
    let (mut other, mut all) = (0usize, 0usize);
    for (_, p) in pieces {
        let f = |ms: u64| ((ms / transcript::FRAME_MS) as usize).min(labels.len());
        let mut counts = std::collections::BTreeMap::<u32, usize>::new();
        for l in labels[f(p.start_ms)..f(p.end_ms)].iter().flatten() {
            *counts.entry(*l).or_default() += 1;
        }
        let n: usize = counts.values().sum();
        other += n - counts.values().max().copied().unwrap_or(0);
        all += n;
    }
    other as f64 / all.max(1) as f64
}

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let model = args.next().ok_or("speaker model")?;
    let dir = args.next().ok_or("meeting dir")?;
    let source = match args.next().as_deref() {
        Some("mic") => Source::Mic,
        _ => Source::System,
    };
    let every: u64 = std::env::var("EVERY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(120);
    let model = Path::new(&model);
    let seg = segment::model_beside(model);
    let wav = Path::new(&dir).join(source.file());
    let vad = Path::new("resources/models/silero_vad_v4.onnx");
    let samples =
        handy_app_lib::audio_toolkit::read_wav_samples(&wav).map_err(|e| e.to_string())?;
    let speech = pipeline::analyze(&wav, vad, false)?.speech;
    let by_turns = std::env::var("CUT").is_ok_and(|v| v == "turns");
    let labels = |upto: usize| -> Result<(Vec<Option<u32>>, Vec<bool>), String> {
        let frames = upto.min(speech.len());
        let s = &speech[..frames];
        let n = (frames * 480).min(samples.len());
        let (wins, embs, turns) =
            diarize::fingerprint_samples(&samples[..n], s, model, seg.as_deref())?;
        let change = turns
            .map(|t| t.change)
            .unwrap_or_else(|| vec![false; frames]);
        Ok((
            diarize::label_with(s, &wins, &embs, None, &[]).labels,
            change,
        ))
    };

    // Live.
    let mut done_chunks: BTreeSet<String> = BTreeSet::new();
    let mut live: BTreeSet<String> = BTreeSet::new();
    let (mut rounds, mut busy) = (0, 0.0);
    let total_ms = speech.len() as u64 * transcript::FRAME_MS;
    let mut t = every * 1000;
    while t < total_ms + every * 1000 {
        let at = t.min(total_ms);
        let started = Instant::now();
        let (l, ch) = labels((at / transcript::FRAME_MS) as usize)?;
        busy += started.elapsed().as_secs_f64();
        rounds += 1;
        let ch = by_turns.then_some(ch.as_slice());
        for (c, p) in pieces(
            &speech[..(at / transcript::FRAME_MS) as usize],
            &l,
            ch,
            Some(at),
        ) {
            let ck = chunk_key(source, c);
            // Each chunk is transcribed once, when it's ready.
            if done_chunks.contains(&ck) {
                continue;
            }
            live.insert(chunk_key(source, p));
        }
        for c in transcript::plan_chunks(&speech[..(at / transcript::FRAME_MS) as usize]) {
            if c.end_ms + transcript::PAUSE_MS <= at {
                done_chunks.insert(chunk_key(source, c));
            }
        }
        if at == total_ms {
            break;
        }
        t += every * 1000;
    }
    // Final.
    let started = Instant::now();
    let (fin, fin_change) = labels(speech.len())?;
    let final_secs = started.elapsed().as_secs_f64();
    let final_pieces = pieces(
        &speech,
        &fin,
        by_turns.then_some(fin_change.as_slice()),
        None,
    );
    let by_labels = pieces(&speech, &fin, None, None);
    let len = |p: &Chunk| (p.end_ms - p.start_ms) as f64 / 1000.0;
    let total: f64 = final_pieces.iter().map(|(_, p)| len(p)).sum();
    let reused: Vec<&Chunk> = final_pieces
        .iter()
        .map(|(_, p)| p)
        .filter(|p| live.contains(&chunk_key(source, **p)))
        .collect();
    let reused_s: f64 = reused.iter().map(|p| len(p)).sum();
    // Without live speakers (whole chunks only), for comparison.
    let whole: BTreeSet<String> = transcript::plan_chunks(&speech)
        .into_iter()
        .map(|c| chunk_key(source, c))
        .collect();
    let whole_reused: f64 = final_pieces
        .iter()
        .filter(|(_, p)| whole.contains(&chunk_key(source, *p)))
        .map(|(_, p)| len(p))
        .sum();
    println!(
        "pieces mixed: {:.1}% (cut by labels: {:.1}%, {} pieces)",
        100.0 * mixed(&final_pieces, &fin),
        100.0 * mixed(&by_labels, &fin),
        by_labels.len()
    );
    println!(
        "{source:?} {:.0} min: {} final pieces ({total:.0} s). Reused with live speakers: {} pieces, {reused_s:.0} s ({:.0}%); whole chunks only: {whole_reused:.0} s ({:.0}%). Live speaker work: {rounds} rounds, {busy:.0} s total; final speaker step {final_secs:.0} s",
        total_ms as f64 / 60_000.0,
        final_pieces.len(),
        reused.len(),
        100.0 * reused_s / total,
        100.0 * whole_reused / total,
    );
    Ok(())
}
