//! Stream a WAV through the live draft model the way a dictation feeds it
//! (real-time sized chunks) and print each draft with how long it took.
//!
//!     cargo run --release --example draft_eval -- <model.gguf> <file.wav>

use handy_app_lib::audio_toolkit::audio::read_wav_samples;
use std::time::Instant;
use transcribe_cpp::{Model, RunOptions, StreamOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let started = Instant::now();
    let model = Model::load(args.next().expect("model path"))?;
    let mut session = model.session()?;
    println!("loaded in {:?}", started.elapsed());
    let samples = read_wav_samples(args.next().expect("wav path"))?;
    let mut stream = session.stream(&RunOptions::default(), &StreamOptions::default())?;
    let mut compute = std::time::Duration::ZERO;
    for (i, chunk) in samples.chunks(480).enumerate() {
        let t = Instant::now();
        let update = stream.feed(chunk)?;
        compute += t.elapsed();
        if update.committed_changed || update.tentative_changed {
            let text = stream.text();
            println!(
                "{:>5.2}s  {}{}",
                (i + 1) as f32 * 0.03,
                text.committed,
                text.tentative
            );
        }
    }
    stream.finalize()?;
    println!("final: {}", stream.text().full);
    println!(
        "audio {:.1}s, compute {:?}",
        samples.len() as f32 / 16000.0,
        compute
    );
    Ok(())
}
