//! Does handing the live pass's work to the final pass give the same answers
//! and save time? Replays a meeting's track as the live pass hears it (20 s
//! at a time, the voices told apart every 60 s from the kept model answers,
//! ready chunks cut where the speaker changes, each once), then carries on
//! to the end as the final pass does and compares with starting over: the
//! speech and the voice labels must come out the same.
//!
//! cargo run --release --example live_handover_check -- <speaker model> <meeting dir> [mic|system]

use handy_app_lib::meetings::diarize::{self, FingerprintCache};
use handy_app_lib::meetings::live::chunk_key;
use handy_app_lib::meetings::transcript::{self, Chunk, Source};
use handy_app_lib::meetings::{pipeline, segment};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Instant;

const EVERY_S: usize = 20;
const VOICES_EVERY_S: usize = 60;
const RATE: usize = 16_000;

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let model = args.next().ok_or("speaker model")?;
    let dir = args.next().ok_or("meeting dir")?;
    let source = match args.next().as_deref() {
        Some("mic") => Source::Mic,
        _ => Source::System,
    };
    let model = Path::new(&model);
    let seg = segment::model_beside(model);
    let wav = Path::new(&dir).join(source.file());
    let vad = Path::new("resources/models/silero_vad_v4.onnx");
    let samples = diarize::read_samples(&wav)?;

    // Live.
    let mut listener = pipeline::Listener::new(vad, false)?;
    let mut cache = FingerprintCache::default();
    let mut labels: Option<Vec<Option<u32>>> = None;
    let mut cuts: HashMap<String, Vec<Chunk>> = HashMap::new();
    let mut live: BTreeSet<String> = BTreeSet::new();
    let (mut heard, mut since_voices, mut busy, mut rounds) = (0usize, VOICES_EVERY_S, 0.0, 0);
    while heard < samples.len() {
        let next = (heard + EVERY_S * RATE).min(samples.len());
        listener.push(&samples[heard..next])?;
        heard = next;
        since_voices += EVERY_S;
        let speech = &listener.analysis.speech;
        if since_voices >= VOICES_EVERY_S {
            since_voices = 0;
            let started = Instant::now();
            match source {
                Source::Mic => {
                    diarize::fingerprint_cached(
                        &samples[..heard],
                        speech,
                        model,
                        seg.as_deref(),
                        &mut cache,
                    )?;
                }
                Source::System => {
                    labels = Some(
                        diarize::speakers_cached(
                            &samples[..heard],
                            speech,
                            model,
                            None,
                            &[],
                            &mut cache,
                        )?
                        .labels,
                    );
                }
            }
            busy += started.elapsed().as_secs_f64();
            rounds += 1;
        }
        let heard_ms = speech.len() as u64 * transcript::FRAME_MS;
        for c in transcript::plan_chunks(speech) {
            if c.end_ms + transcript::PAUSE_MS > heard_ms {
                continue;
            }
            let key = chunk_key(source, c);
            if cuts.contains_key(&key) {
                continue;
            }
            let pieces = match (&labels, source) {
                (_, Source::Mic) => vec![c],
                (Some(l), _) if (c.end_ms / transcript::FRAME_MS) as usize <= l.len() => {
                    diarize::split_by_speaker(c, l)
                        .into_iter()
                        .map(|(c, _)| c)
                        .collect()
                }
                _ => continue,
            };
            live.extend(pieces.iter().map(|p| chunk_key(source, *p)));
            cuts.insert(key, pieces);
        }
    }
    // The last few seconds the live pass hadn't heard when it stopped.
    let stop_at = samples.len().saturating_sub(7 * RATE);
    let mut handed = pipeline::Listener::new(vad, false)?;
    handed.push(&samples[..stop_at])?;
    handed.push(&samples[stop_at..])?;

    // Final, carried on.
    let started = Instant::now();
    let speech = listener.analysis.speech.clone();
    let carried = diarize::speakers_cached(&samples, &speech, model, None, &[], &mut cache)?;
    let carried_s = started.elapsed().as_secs_f64();
    // Final, from scratch.
    let started = Instant::now();
    let fresh_speech = pipeline::analyze(&wav, vad, false)?.speech;
    let vad_s = started.elapsed().as_secs_f64();
    let started = Instant::now();
    let fresh = diarize::speakers_with(&wav, &fresh_speech, model, None, &[])?;
    let fresh_s = started.elapsed().as_secs_f64();

    let final_pieces: Vec<Chunk> = transcript::plan_chunks(&fresh_speech)
        .into_iter()
        .flat_map(|c| match source {
            Source::Mic => vec![c],
            Source::System => diarize::split_by_speaker(c, &fresh.labels)
                .into_iter()
                .map(|(c, _)| c)
                .collect(),
        })
        .collect();
    let len = |p: &Chunk| (p.end_ms - p.start_ms) as f64 / 1000.0;
    let total: f64 = final_pieces.iter().map(len).sum();
    let reused: f64 = final_pieces
        .iter()
        .filter(|p| live.contains(&chunk_key(source, **p)))
        .map(len)
        .sum();
    let transcribed_live: f64 = live
        .iter()
        .filter_map(|k| {
            let mut it = k.rsplitn(3, '-');
            let end: u64 = it.next()?.parse().ok()?;
            let start: u64 = it.next()?.parse().ok()?;
            Some((end - start) as f64 / 1000.0)
        })
        .sum();
    println!(
        "{source:?} {:.0} min: speech same {} (stopped and handed over: {}); labels same {} (windows {} vs {}); reuse {reused:.0} of {total:.0} s ({:.0}%), {transcribed_live:.0} s transcribed live; live voices {rounds} rounds {busy:.1} s; final voices carried on {carried_s:.1} s vs from scratch {fresh_s:.1} s (+ VAD {vad_s:.1} s saved)",
        samples.len() as f64 / RATE as f64 / 60.0,
        speech == fresh_speech,
        handed.analysis.speech == fresh_speech,
        carried.labels == fresh.labels,
        carried.windows.len(),
        fresh.windows.len(),
        100.0 * reused / total.max(1e-9),
    );
    Ok(())
}
