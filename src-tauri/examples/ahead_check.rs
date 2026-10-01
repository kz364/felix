//! Do the chunks planned while recording (`live`) match the final pass's,
//! and how much of a finished meeting's transcript would they have saved?
//! Feeds a meeting's tracks to `pipeline::Listener` in 20 s rounds, as the
//! live pass does, and compares with `pipeline::analyze` and the meeting's
//! transcript.
//!
//! cargo run --release --example ahead_check -- <meeting dir>

use handy_app_lib::meetings::live::chunk_key;
use handy_app_lib::meetings::pipeline;
use handy_app_lib::meetings::transcript::{self, Source};
use std::collections::BTreeSet;
use std::path::Path;

fn main() -> Result<(), String> {
    let dir = std::env::args().nth(1).ok_or("Give a meeting folder")?;
    let dir = Path::new(&dir);
    let vad = Path::new("resources/models/silero_vad_v4.onnx");
    let t = pipeline::load(dir).ok_or("No transcript")?;
    for source in [Source::Mic, Source::System] {
        let wav = dir.join(source.file());
        if !wav.is_file() {
            continue;
        }
        let audio =
            handy_app_lib::audio_toolkit::read_wav_samples(&wav).map_err(|e| e.to_string())?;
        // Live: 20 s rounds.
        let mut listener = pipeline::Listener::new(vad, false)?;
        let mut ahead = BTreeSet::new();
        for piece in audio.chunks(20 * 16_000) {
            listener.push(piece)?;
            let speech = &listener.analysis.speech;
            let heard = speech.len() as u64 * transcript::FRAME_MS;
            for c in transcript::plan_chunks(speech) {
                if c.end_ms + transcript::PAUSE_MS <= heard {
                    ahead.insert(chunk_key(source, c));
                }
            }
        }
        let full = pipeline::analyze(&wav, vad, false)?;
        let final_chunks: BTreeSet<String> = transcript::plan_chunks(&full.speech)
            .into_iter()
            .map(|c| chunk_key(source, c))
            .collect();
        let same_speech = full.speech == listener.analysis.speech;
        let segs: Vec<_> = t.segments.iter().filter(|s| s.source == source).collect();
        let reused: Vec<_> = segs
            .iter()
            .filter(|s| {
                ahead.contains(&chunk_key(
                    source,
                    transcript::Chunk {
                        start_ms: s.start_ms,
                        end_ms: s.end_ms,
                    },
                ))
            })
            .collect();
        let secs = |v: &[&&transcript::Segment]| {
            v.iter().map(|s| s.end_ms - s.start_ms).sum::<u64>() / 1000
        };
        println!(
            "{source:?}: VAD identical {same_speech}; {} chunks planned live, {} of the final plan's {} ({} missing); transcript reuse {} of {} segments, {} of {} s",
            ahead.len(),
            ahead.intersection(&final_chunks).count(),
            final_chunks.len(),
            final_chunks.difference(&ahead).count(),
            reused.len(),
            segs.len(),
            secs(&reused),
            secs(&segs.iter().collect::<Vec<_>>()),
        );
    }
    Ok(())
}
