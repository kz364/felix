//! The meeting's microphone: its own cpal stream, separate from dictation's,
//! recorded raw (no gain, no VAD) and mixed down to mono. Both streams can
//! be open on the same device at once, so dictation keeps working (with its
//! own gain and VAD) during a meeting.

use super::capture::Source;
use crate::audio_toolkit::audio::AudioRecorder;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SizedSample};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::Duration;

const RING_SECONDS: usize = 10;

/// Keeps the mic stream running; stops it on drop.
pub struct MicStream {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for MicStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::SupportedStreamConfig,
    mut producer: rtrb::Producer<f32>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::BuildStreamError>
where
    T: Sample + SizedSample + Send + 'static,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels().max(1) as usize;
    device.build_input_stream(
        &config.clone().into(),
        move |data: &[T], _| {
            for frame in data.chunks_exact(channels) {
                let sum: f32 = frame.iter().map(|s| s.to_sample::<f32>()).sum();
                let _ = producer.push(sum / channels as f32);
            }
        },
        move |_| failed.store(true, Ordering::Release),
        None,
    )
}

/// Open the microphone (`None`: the system default input).
pub fn start(device: Option<cpal::Device>) -> Result<(Source, MicStream), String> {
    let stop = Arc::new(AtomicBool::new(false));
    let (init_tx, init_rx) = mpsc::sync_channel(1);
    let thread_stop = stop.clone();
    // cpal streams stay on the thread that built them.
    let thread = std::thread::Builder::new()
        .name("meeting-mic".into())
        .spawn(move || {
            let result = (|| -> Result<(cpal::Stream, Source), String> {
                let device = match device {
                    Some(d) => d,
                    None => crate::audio_toolkit::get_cpal_host()
                        .default_input_device()
                        .ok_or("No microphone found")?,
                };
                let label = device.name().unwrap_or_else(|_| "Microphone".into());
                let config = AudioRecorder::get_preferred_config(&device)
                    .map_err(|e| format!("Couldn't read the microphone's format: {e}"))?;
                let rate = config.sample_rate().0;
                let (producer, consumer) = rtrb::RingBuffer::new(rate as usize * RING_SECONDS);
                let failed = Arc::new(AtomicBool::new(false));
                let stream = match config.sample_format() {
                    cpal::SampleFormat::F32 => build::<f32>(&device, &config, producer, failed),
                    cpal::SampleFormat::I16 => build::<i16>(&device, &config, producer, failed),
                    cpal::SampleFormat::I32 => build::<i32>(&device, &config, producer, failed),
                    cpal::SampleFormat::U8 => build::<u8>(&device, &config, producer, failed),
                    cpal::SampleFormat::I8 => build::<i8>(&device, &config, producer, failed),
                    other => return Err(format!("Unsupported microphone format {other:?}")),
                }
                .map_err(|e| format!("Couldn't open the microphone: {e}"))?;
                stream
                    .play()
                    .map_err(|e| format!("Couldn't start the microphone: {e}"))?;
                log::info!("Meeting microphone: {label} at {rate} Hz");
                Ok((
                    stream,
                    Source {
                        rate,
                        consumer,
                        label,
                    },
                ))
            })();
            match result {
                Ok((stream, source)) => {
                    let _ = init_tx.send(Ok(source));
                    while !thread_stop.load(Ordering::Acquire) {
                        std::thread::sleep(Duration::from_millis(50));
                    }
                    drop(stream);
                }
                Err(e) => {
                    let _ = init_tx.send(Err(e));
                }
            }
        })
        .map_err(|e| e.to_string())?;
    let source = match init_rx.recv_timeout(Duration::from_secs(10)) {
        Ok(result) => result?,
        Err(_) => {
            stop.store(true, Ordering::Release);
            return Err("The microphone didn't open".into());
        }
    };
    Ok((
        source,
        MicStream {
            stop,
            thread: Some(thread),
        },
    ))
}
