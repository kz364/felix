//! Taking the call out of the mic: with the call on the laptop's speakers,
//! the mic hears the other side again, which gets transcribed twice and
//! told apart as one more person in the room. WebRTC's echo canceller
//! (AEC3, the one in browsers and Granola) learns how the speakers reach
//! the mic from the system track and subtracts it, before the speech is
//! found. Done on the saved tracks after the call, so it works on any
//! meeting, including ones transcribed again.

use super::track::SAMPLE_RATE;
use aec3::nodes::audio::AudioFormat;
use aec3::pipelines::linear;
use std::path::Path;

/// The mic with the call taken out, kept next to the tracks.
pub const FILE: &str = "mic_clean.wav";
/// The same, made while recording for the live transcript.
pub const LIVE_FILE: &str = "mic_live_clean.wav";

fn open(path: &Path) -> Result<hound::WavReader<std::io::BufReader<std::fs::File>>, String> {
    let reader = hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE || spec.channels != 1 || spec.bits_per_sample != 16 {
        return Err(format!("{} isn't 16 kHz mono 16-bit", path.display()));
    }
    Ok(reader)
}

/// Write `mic` with `system`'s echo cancelled to `out` (16 kHz mono 16-bit,
/// the same length as `mic`), a frame at a time.
pub fn cancel(mic: &Path, system: &Path, out: &Path) -> Result<(), String> {
    let mut mic = open(mic)?;
    let mut system = open(system)?;
    let mut canceller = Canceller::new()?;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let tmp = out.with_extension("tmp");
    let mut w = hound::WavWriter::create(&tmp, spec).map_err(|e| e.to_string())?;
    let mut mic = mic.samples::<i16>().map(|s| s.map(|v| v as f32 / 32768.0));
    let mut system = system
        .samples::<i16>()
        .map(|s| s.map(|v| v as f32 / 32768.0));
    let n = canceller.frame;
    let (mut m, mut s) = (Vec::with_capacity(n), Vec::with_capacity(n));
    loop {
        m.clear();
        s.clear();
        for x in mic.by_ref().take(n) {
            m.push(x.map_err(|e| e.to_string())?);
        }
        if m.is_empty() {
            break;
        }
        for x in system.by_ref().take(n) {
            s.push(x.map_err(|e| e.to_string())?);
        }
        for y in canceller.process(&m, &s)? {
            w.write_sample((y.clamp(-1.0, 1.0) * 32767.0) as i16)
                .map_err(|e| e.to_string())?;
        }
    }
    w.finalize().map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, out).map_err(|e| e.to_string())
}

/// Echo cancelling a frame at a time ([`Canceller::FRAME`] samples).
pub struct Canceller {
    pipeline: linear::LinearPipeline,
    frame: usize,
    processed: Vec<f32>,
}

impl Canceller {
    pub const FRAME: usize = SAMPLE_RATE as usize / 100;

    pub fn new() -> Result<Self, String> {
        let format = AudioFormat::ten_ms(SAMPLE_RATE, 1);
        // Only the echo canceller (and the high-pass it expects): noise
        // suppression and gain would change the voices the speaker model hears.
        let pipeline = linear::builder(format, format)
            .enable_noise_suppression(false)
            .enable_gain_controller2(false)
            .build()
            .map_err(|e| format!("Couldn't start the echo canceller: {e:?}"))?;
        let frame = format.sample_count();
        Ok(Canceller {
            pipeline,
            frame,
            processed: vec![0.0; frame],
        })
    }

    /// One 10 ms frame (shorter at the end) of mic and what played then.
    pub fn process(&mut self, mic: &[f32], system: &[f32]) -> Result<Vec<f32>, String> {
        let pad = |x: &[f32]| {
            let mut f = x.to_vec();
            f.resize(self.frame, 0.0);
            f
        };
        self.pipeline
            .handle_render_frame(&pad(system))
            .map_err(|e| format!("Echo canceller: {e:?}"))?;
        let produced = self
            .pipeline
            .process_capture_frame(&pad(mic), &mut self.processed)
            .map_err(|e| format!("Echo canceller: {e:?}"))?;
        Ok(if produced {
            self.processed[..mic.len()].to_vec()
        } else {
            vec![0.0; mic.len()]
        })
    }
}
