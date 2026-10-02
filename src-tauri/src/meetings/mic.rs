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

/// The system default input's name.
pub fn default_name() -> Option<String> {
    crate::audio_toolkit::get_cpal_host()
        .default_input_device()
        .and_then(|d| d.name().ok())
}

/// The system default input, unless it's a dictation mic (the DJI): then
/// the Mac's own mic, or else any other input.
pub fn call_default() -> Option<cpal::Device> {
    use super::voiceprint::is_dictation_mic;
    let host = crate::audio_toolkit::get_cpal_host();
    let default = host.default_input_device();
    if let Some(d) = default.filter(|d| !d.name().is_ok_and(|n| is_dictation_mic(&n))) {
        return Some(d);
    }
    let devices = crate::audio_toolkit::list_input_devices().ok()?;
    let others: Vec<_> = devices
        .into_iter()
        .filter(|d| !is_dictation_mic(&d.name))
        .collect();
    let built_in = others
        .iter()
        .position(|d| d.name.contains("MacBook") || d.name.contains("Built-in"));
    others
        .into_iter()
        .nth(built_in.unwrap_or(0))
        .map(|d| d.device)
}

/// Open the microphone (`None`: the system default input, see [`call_default`]).
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
                    None => call_default()
                        .ok_or("No microphone found (dictation mics aren't used for meetings)")?,
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

/// The input volume the Mac has set for a mic (`None`: the default input),
/// in dB. `None` when the device has no volume control. Call apps (Zoom,
/// Teams) move this slider on their own during calls.
#[cfg(target_os = "macos")]
pub fn input_volume_db(device_name: Option<&str>) -> Option<f32> {
    use objc2::rc::Retained;
    use objc2_core_audio::{
        kAudioDevicePropertyVolumeDecibels, kAudioHardwarePropertyDefaultInputDevice,
        kAudioHardwarePropertyDevices, kAudioObjectPropertyElementMain, kAudioObjectPropertyName,
        kAudioObjectPropertyScopeGlobal, kAudioObjectPropertyScopeInput, kAudioObjectSystemObject,
        kAudioObjectUnknown, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
        AudioObjectHasProperty, AudioObjectID, AudioObjectPropertyAddress,
    };
    use objc2_foundation::NSString;
    use std::mem::size_of;
    use std::ptr::NonNull;

    let address = |selector, scope, element| AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: element,
    };
    // SAFETY: each call passes a valid address and a buffer of the size it
    // says; the name is a +1 CFString handed to `Retained`.
    unsafe {
        let get =
            |object: AudioObjectID, addr: &AudioObjectPropertyAddress, out: *mut u8, size: u32| {
                let mut size = size;
                AudioObjectGetPropertyData(
                    object,
                    NonNull::from(addr),
                    0,
                    std::ptr::null(),
                    NonNull::from(&mut size),
                    NonNull::new_unchecked(out as *mut std::ffi::c_void),
                ) == 0
                    && size > 0
            };
        let system = kAudioObjectSystemObject as AudioObjectID;
        let global = |sel| {
            address(
                sel,
                kAudioObjectPropertyScopeGlobal,
                kAudioObjectPropertyElementMain,
            )
        };
        let device = match device_name {
            None => {
                let mut id: AudioObjectID = kAudioObjectUnknown;
                get(
                    system,
                    &global(kAudioHardwarePropertyDefaultInputDevice),
                    &mut id as *mut _ as *mut u8,
                    size_of::<AudioObjectID>() as u32,
                )
                .then_some(id)?
            }
            Some(name) => {
                let addr = global(kAudioHardwarePropertyDevices);
                let mut size = 0u32;
                if AudioObjectGetPropertyDataSize(
                    system,
                    NonNull::from(&addr),
                    0,
                    std::ptr::null(),
                    NonNull::from(&mut size),
                ) != 0
                {
                    return None;
                }
                let mut ids = vec![0 as AudioObjectID; size as usize / size_of::<AudioObjectID>()];
                if ids.is_empty() || !get(system, &addr, ids.as_mut_ptr() as *mut u8, size) {
                    return None;
                }
                ids.into_iter().find(|&id| {
                    let mut cf: *mut NSString = std::ptr::null_mut();
                    get(
                        id,
                        &global(kAudioObjectPropertyName),
                        &mut cf as *mut _ as *mut u8,
                        size_of::<*mut NSString>() as u32,
                    ) && Retained::from_raw(cf).is_some_and(|n| n.to_string() == name)
                })?
            }
        };
        if device == kAudioObjectUnknown {
            return None;
        }
        // The main element, or else the first channel.
        [kAudioObjectPropertyElementMain, 1]
            .into_iter()
            .find_map(|element| {
                let addr = address(
                    kAudioDevicePropertyVolumeDecibels,
                    kAudioObjectPropertyScopeInput,
                    element,
                );
                if !AudioObjectHasProperty(device, NonNull::from(&addr)) {
                    return None;
                }
                let mut db = 0f32;
                get(
                    device,
                    &addr,
                    &mut db as *mut f32 as *mut u8,
                    size_of::<f32>() as u32,
                )
                .then_some(db)
                .filter(|db| db.is_finite())
            })
    }
}

#[cfg(not(target_os = "macos"))]
pub fn input_volume_db(_device_name: Option<&str>) -> Option<f32> {
    None
}

/// Gain that undoes a change to the mic's input volume since `start_db`,
/// kept to ±18 dB so a slider pulled to the bottom doesn't turn into noise.
pub fn undo_volume_change(start_db: f32, now_db: f32) -> f32 {
    10f32.powf(((start_db - now_db) / 20.0).clamp(-18.0 / 20.0, 18.0 / 20.0))
}

#[cfg(test)]
mod tests {
    use super::undo_volume_change;

    #[test]
    fn a_lowered_slider_is_made_up_within_limits() {
        assert!((undo_volume_change(0.0, 0.0) - 1.0).abs() < 1e-6);
        assert!((undo_volume_change(0.0, -6.0) - 1.995).abs() < 0.01);
        assert!((undo_volume_change(-6.0, 0.0) - 0.501).abs() < 0.01);
        assert!((undo_volume_change(0.0, -40.0) - 7.94).abs() < 0.05);
    }
}
