//! Does cancelling the call's echo in the mic help? For a call recorded on
//! the laptop's speakers: cancels it (to the scratch path given), then
//! compares the mic before and after: how much mic speech the VAD finds
//! while the call talks (mostly echo) and while it doesn't (the room), and
//! how many voices the mic comes out as.
//!
//! cargo run --release --example aec_check -- <speaker model> <meeting dir> <out.wav>

use handy_app_lib::meetings::transcript::Source;
use handy_app_lib::meetings::{aec, diarize, pipeline};
use std::path::Path;
use std::time::Instant;

fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let model = args.next().ok_or("speaker model")?;
    let dir = args.next().ok_or("meeting dir")?;
    let out = args.next().ok_or("out.wav")?;
    let (model, dir, out) = (Path::new(&model), Path::new(&dir), Path::new(&out));
    let vad = Path::new("resources/models/silero_vad_v4.onnx");
    let mic = dir.join(Source::Mic.file());
    let system = dir.join(Source::System.file());
    let t = Instant::now();
    aec::cancel(&mic, &system, out)?;
    println!("cancelled in {:.0?}", t.elapsed());
    let sys = pipeline::analyze(&system, vad, false)?.speech;
    for (what, wav) in [("before", mic.as_path()), ("after", out)] {
        let a = pipeline::analyze(wav, vad, false)?;
        let n = a.speech.len().min(sys.len());
        let over = (0..n).filter(|&i| a.speech[i] && sys[i]).count() as f32 * 0.03;
        let alone = (0..n).filter(|&i| a.speech[i] && !sys[i]).count() as f32 * 0.03;
        let v = diarize::speakers_with(wav, &a.speech, model, None, &[])?;
        let mut talk = std::collections::BTreeMap::<u32, (f32, f32)>::new();
        for (i, l) in v.labels.iter().enumerate() {
            if let Some(l) = l {
                let e = talk.entry(*l).or_default();
                e.0 += 0.03;
                if sys.get(i) == Some(&true) {
                    e.1 += 0.03;
                }
            }
        }
        println!(
            "{what}: mic speech while the call talks {over:.0} s, otherwise {alone:.0} s; voices (s, % over the call): {:?}",
            talk.values()
                .map(|(t, o)| (t.round() as u32, (100.0 * o / t.max(0.01)).round() as u32))
                .collect::<Vec<_>>()
        );
    }
    Ok(())
}
