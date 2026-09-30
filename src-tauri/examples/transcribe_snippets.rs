//! Transcribe stretches of a 16 kHz mono WAV and print the detected language:
//!   cargo run --release --example transcribe_snippets -- <model.gguf> <wav> <start_s:len_s>...
use std::path::Path;
use transcribe_cpp::{Model, RunOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let model = Model::load(&args[0])?;
    let mut session = model.session()?;
    let audio = handy_app_lib::audio_toolkit::read_wav_samples(Path::new(&args[1]))?;
    for span in &args[2..] {
        let (start, len) = span.split_once(':').expect("start:len");
        let from = (start.parse::<f64>()? * 16_000.0) as usize;
        let to = (from + (len.parse::<f64>()? * 16_000.0) as usize).min(audio.len());
        let t = session.run(&audio[from.min(to)..to], &RunOptions::default())?;
        println!("[{span}] ({:?}) {}", t.language, t.text.trim());
    }
    Ok(())
}
