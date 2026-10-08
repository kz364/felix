//! System audio (the other people on a call) through a Core Audio process
//! tap, macOS 14.2+. The tap mixes every app's output except Felix's own
//! (feedback sounds) down to mono, and leaves out the browser too while the
//! Felix Meetings extension sends the call tab's own audio (see
//! [`super::call_audio`]); a private aggregate device wraps it so an
//! IOProc can read it. Needs the "System Audio Recording" permission
//! (`NSAudioCaptureUsageDescription`), not Screen Recording. Without it the
//! tap delivers silence.

use super::capture::Source;
use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceIsStackedKey,
    kAudioAggregateDeviceMainSubDeviceKey, kAudioAggregateDeviceNameKey,
    kAudioAggregateDeviceSubDeviceListKey, kAudioAggregateDeviceTapAutoStartKey,
    kAudioAggregateDeviceTapListKey, kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyNominalSampleRate, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioObjectUnknown,
    kAudioProcessPropertyBundleID, kAudioProcessPropertyPID, kAudioSubDeviceUIDKey,
    kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey, kAudioTapPropertyDescription,
    kAudioTapPropertyFormat, AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID,
    AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop, AudioHardwareCreateAggregateDevice,
    AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress, AudioObjectPropertySelector,
    AudioObjectSetPropertyData, CATapDescription, CATapMuteBehavior,
};
use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSString, NSUUID};
use std::cell::UnsafeCell;
use std::ffi::{c_void, CStr};
use std::mem::size_of;
use std::ptr::NonNull;

/// Seconds of audio the ring holds if the writer falls behind.
const RING_SECONDS: usize = 10;

/// Read one fixed-size property of a Core Audio object.
pub(super) unsafe fn get_property<T: Copy>(
    object: AudioObjectID,
    selector: AudioObjectPropertySelector,
    qualifier: Option<&i32>,
    initial: T,
) -> Result<T, String> {
    let address = AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let mut value = initial;
    let mut size = size_of::<T>() as u32;
    let (qualifier_size, qualifier_ptr) = match qualifier {
        Some(q) => (size_of::<i32>() as u32, q as *const i32 as *const c_void),
        None => (0, std::ptr::null()),
    };
    let status = AudioObjectGetPropertyData(
        object,
        NonNull::from(&address),
        qualifier_size,
        qualifier_ptr,
        NonNull::from(&mut size),
        NonNull::from(&mut value).cast(),
    );
    if status != 0 {
        return Err(format!("property {selector:#x} failed ({status})"));
    }
    Ok(value)
}

fn key(k: &CStr) -> Retained<NSString> {
    NSString::from_str(k.to_str().unwrap_or_default())
}

/// The UID of the default output device, the aggregate's clock.
unsafe fn default_output_uid() -> Result<Retained<NSString>, String> {
    let device: AudioObjectID = get_property(
        kAudioObjectSystemObject as AudioObjectID,
        kAudioHardwarePropertyDefaultOutputDevice,
        None,
        kAudioObjectUnknown,
    )?;
    let uid: *mut NSString = get_property(
        device,
        kAudioDevicePropertyDeviceUID,
        None,
        std::ptr::null_mut(),
    )?;
    // The property returns a +1 CFString, toll-free bridged to NSString.
    Retained::from_raw(uid).ok_or_else(|| "The output device has no UID".into())
}

extern "C" {
    /// The app a helper process (XPC service) works for; libSystem, used by
    /// Activity Monitor. Returns the pid itself for an app.
    fn responsibility_get_pid_responsible_for_pid(pid: libc::pid_t) -> libc::pid_t;
}

/// Every process with audio open right now.
pub(super) fn process_objects() -> Vec<AudioObjectID> {
    let address = AudioObjectPropertyAddress {
        mSelector: kAudioHardwarePropertyProcessObjectList,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    };
    let system = kAudioObjectSystemObject as AudioObjectID;
    let mut size = 0u32;
    // SAFETY: valid address and out-pointer.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            system,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 || size == 0 {
        return Vec::new();
    }
    let mut ids = vec![0 as AudioObjectID; size as usize / size_of::<AudioObjectID>()];
    // SAFETY: `ids` holds `size` bytes.
    let status = unsafe {
        AudioObjectGetPropertyData(
            system,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(ids.as_mut_ptr()).cast(),
        )
    };
    if status != 0 {
        return Vec::new();
    }
    ids.truncate(size as usize / size_of::<AudioObjectID>());
    ids
}

/// A process object's bundle id, if it has one.
pub(super) fn bundle_id(process: AudioObjectID) -> Option<String> {
    // SAFETY: the property is a +1 CFString, toll-free bridged.
    let raw: *mut NSString = unsafe {
        get_property(
            process,
            kAudioProcessPropertyBundleID,
            None,
            std::ptr::null_mut(),
        )
    }
    .ok()?;
    let name = unsafe { Retained::from_raw(raw) }?.to_string();
    (!name.is_empty()).then_some(name)
}

/// The app a process works for: itself, or the app behind a helper.
fn owner(pid: libc::pid_t) -> libc::pid_t {
    // SAFETY: plain libSystem call.
    let owner = unsafe { responsibility_get_pid_responsible_for_pid(pid) };
    if owner > 0 {
        owner
    } else {
        pid
    }
}

/// Whether a process's sound stays out: Felix's own (as for its web view,
/// whose helper Felix is responsible for), and the browser's whose call
/// the extension sends.
fn left_out(owner_pid: i32, own_pid: i32, browser_pid: Option<i32>) -> bool {
    owner_pid == own_pid || Some(owner_pid) == browser_pid
}

/// The process objects whose sound the tap leaves out right now.
pub fn left_out_processes() -> Vec<AudioObjectID> {
    let own = std::process::id() as i32;
    let browser = super::call_audio::live_browser().map(owner);
    let mut ids: Vec<AudioObjectID> = process_objects()
        .into_iter()
        .filter(|&id| {
            // SAFETY: an i32 property of a process object.
            unsafe { get_property::<i32>(id, kAudioProcessPropertyPID, None, 0) }
                .is_ok_and(|pid| left_out(owner(pid), own, browser))
        })
        .collect();
    ids.sort_unstable();
    ids
}

/// Change which processes a running tap leaves out.
pub fn leave_out(tap: AudioObjectID, processes: &[AudioObjectID]) -> Result<(), String> {
    unsafe {
        let raw: *mut CATapDescription = get_property(
            tap,
            kAudioTapPropertyDescription,
            None,
            std::ptr::null_mut(),
        )?;
        let description = Retained::from_raw(raw).ok_or("The tap has no description")?;
        let list: Vec<Retained<NSNumber>> =
            processes.iter().map(|&id| NSNumber::new_u32(id)).collect();
        description.setProcesses(&NSArray::from_retained_slice(&list));
        let address = AudioObjectPropertyAddress {
            mSelector: kAudioTapPropertyDescription,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain,
        };
        let pointer = Retained::as_ptr(&description);
        let status = AudioObjectSetPropertyData(
            tap,
            NonNull::from(&address),
            0,
            std::ptr::null(),
            size_of::<*const CATapDescription>() as u32,
            NonNull::from(&pointer).cast(),
        );
        if status != 0 {
            return Err(format!(
                "Couldn't change what the tap leaves out ({status})"
            ));
        }
    }
    Ok(())
}

/// The UID of the output the Mac plays through right now.
pub fn output_uid() -> Option<String> {
    unsafe { default_output_uid() }
        .ok()
        .map(|uid| uid.to_string())
}

struct Callback {
    producer: UnsafeCell<rtrb::Producer<f32>>,
}

/// Real-time callback: average the tap's channels and push mono samples.
/// Allocation-, lock- and syscall-free.
unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let callback = &*(client as *const Callback);
    let producer = &mut *callback.producer.get();
    let list = input.as_ref();
    let buffers = std::slice::from_raw_parts(list.mBuffers.as_ptr(), list.mNumberBuffers as usize);
    let Some(first) = buffers.first() else {
        return 0;
    };
    if first.mData.is_null() {
        return 0;
    }
    if buffers.len() == 1 {
        // Interleaved (or mono): one buffer with every channel.
        let channels = first.mNumberChannels.max(1) as usize;
        let samples = std::slice::from_raw_parts(
            first.mData as *const f32,
            first.mDataByteSize as usize / size_of::<f32>(),
        );
        for frame in samples.chunks_exact(channels) {
            let _ = producer.push(frame.iter().sum::<f32>() / channels as f32);
        }
    } else {
        // Non-interleaved: one buffer per channel.
        let frames = first.mDataByteSize as usize / size_of::<f32>();
        for i in 0..frames {
            let mut sum = 0.0;
            for buffer in buffers {
                if !buffer.mData.is_null() && i * size_of::<f32>() < buffer.mDataByteSize as usize {
                    sum += *(buffer.mData as *const f32).add(i);
                }
            }
            let _ = producer.push(sum / buffers.len() as f32);
        }
    }
    0
}

/// A running tap; stops and tears down on drop.
pub struct SystemTap {
    pub tap: AudioObjectID,
    /// The processes it was started leaving out.
    pub left_out: Vec<AudioObjectID>,
    aggregate: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    callback: *mut Callback,
}

// The raw pointers are only touched on start/drop; the IOProc owns its side.
unsafe impl Send for SystemTap {}

impl Drop for SystemTap {
    fn drop(&mut self) {
        unsafe {
            if self.proc_id.is_some() {
                AudioDeviceStop(self.aggregate, self.proc_id);
                AudioDeviceDestroyIOProcID(self.aggregate, self.proc_id);
            }
            if self.aggregate != kAudioObjectUnknown {
                AudioHardwareDestroyAggregateDevice(self.aggregate);
            }
            if self.tap != kAudioObjectUnknown {
                AudioHardwareDestroyProcessTap(self.tap);
            }
            if !self.callback.is_null() {
                drop(Box::from_raw(self.callback));
            }
        }
    }
}

/// Start tapping every app's audio output except Felix's (and the call
/// browser's, while the extension sends its call).
pub fn start() -> Result<(Source, SystemTap), String> {
    unsafe { start_inner() }
}

unsafe fn start_inner() -> Result<(Source, SystemTap), String> {
    let mut session = SystemTap {
        tap: kAudioObjectUnknown,
        left_out: left_out_processes(),
        aggregate: kAudioObjectUnknown,
        proc_id: None,
        callback: std::ptr::null_mut(),
    };

    // A process object exists only once the process has used audio; one
    // that starts later is added by [`leave_out`].
    let excluded: Vec<Retained<NSNumber>> = session
        .left_out
        .iter()
        .map(|&id| NSNumber::new_u32(id))
        .collect();
    let excluded = NSArray::from_retained_slice(&excluded);

    let description = CATapDescription::initMonoGlobalTapButExcludeProcesses(
        CATapDescription::alloc(),
        &excluded,
    );
    let tap_uuid = NSUUID::new();
    description.setUUID(&tap_uuid);
    description.setName(&NSString::from_str("Felix meeting"));
    description.setPrivate(true);
    description.setMuteBehavior(CATapMuteBehavior::Unmuted);

    let status = AudioHardwareCreateProcessTap(Some(&description), &mut session.tap);
    if status != 0 || session.tap == kAudioObjectUnknown {
        return Err(format!("Couldn't create the system audio tap ({status})"));
    }

    let format: AudioStreamBasicDescription = get_property(
        session.tap,
        kAudioTapPropertyFormat,
        None,
        std::mem::zeroed(),
    )?;
    let rate = format.mSampleRate.round() as u32;
    if rate == 0 {
        return Err("The system audio tap has no sample rate".into());
    }

    let output_uid = default_output_uid()?;
    let sub_device: Retained<NSDictionary<NSString, NSObject>> =
        NSDictionary::from_retained_objects(
            &[&*key(kAudioSubDeviceUIDKey)],
            &[Retained::into_super(output_uid.clone())],
        );
    let sub_tap: Retained<NSDictionary<NSString, NSObject>> = NSDictionary::from_retained_objects(
        &[
            &*key(kAudioSubTapUIDKey),
            &*key(kAudioSubTapDriftCompensationKey),
        ],
        &[
            Retained::into_super(tap_uuid.UUIDString()),
            Retained::into_super(Retained::into_super(NSNumber::new_bool(true))),
        ],
    );
    let aggregate_uid = NSUUID::new().UUIDString();
    let yes = || Retained::into_super(Retained::into_super(NSNumber::new_bool(true)));
    let no = || Retained::into_super(Retained::into_super(NSNumber::new_bool(false)));
    let description: Retained<NSDictionary<NSString, NSObject>> =
        NSDictionary::from_retained_objects(
            &[
                &*key(kAudioAggregateDeviceNameKey),
                &*key(kAudioAggregateDeviceUIDKey),
                &*key(kAudioAggregateDeviceMainSubDeviceKey),
                &*key(kAudioAggregateDeviceIsPrivateKey),
                &*key(kAudioAggregateDeviceIsStackedKey),
                &*key(kAudioAggregateDeviceTapAutoStartKey),
                &*key(kAudioAggregateDeviceSubDeviceListKey),
                &*key(kAudioAggregateDeviceTapListKey),
            ],
            &[
                Retained::into_super(NSString::from_str("Felix meeting")),
                Retained::into_super(aggregate_uid),
                Retained::into_super(output_uid),
                yes(),
                no(),
                yes(),
                Retained::into_super(NSArray::from_retained_slice(&[sub_device])),
                Retained::into_super(NSArray::from_retained_slice(&[sub_tap])),
            ],
        );
    // NSDictionary is toll-free bridged to CFDictionary.
    let cf = &*(Retained::as_ptr(&description) as *const CFDictionary);
    let status = AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut session.aggregate));
    if status != 0 || session.aggregate == kAudioObjectUnknown {
        return Err(format!(
            "Couldn't create the tap's aggregate device ({status})"
        ));
    }

    // The tap's format can say 48 kHz while the output it's built on runs
    // slower (AirPods on a call: 24 kHz); the device's own rate is what
    // arrives.
    let rate = match get_property::<f64>(
        session.aggregate,
        kAudioDevicePropertyNominalSampleRate,
        None,
        0.0,
    ) {
        Ok(device_rate) if device_rate >= 8_000.0 && device_rate.round() as u32 != rate => {
            log::warn!(
                "System audio tap says {rate} Hz but its device runs at {device_rate} Hz; using the device's"
            );
            device_rate.round() as u32
        }
        _ => rate,
    };
    let (producer, consumer) = rtrb::RingBuffer::new(rate as usize * RING_SECONDS);
    session.callback = Box::into_raw(Box::new(Callback {
        producer: UnsafeCell::new(producer),
    }));
    let status = AudioDeviceCreateIOProcID(
        session.aggregate,
        Some(io_proc),
        session.callback as *mut c_void,
        NonNull::from(&mut session.proc_id),
    );
    if status != 0 || session.proc_id.is_none() {
        return Err(format!("Couldn't read the tap ({status})"));
    }
    let status = AudioDeviceStart(session.aggregate, session.proc_id);
    if status != 0 {
        return Err(format!("Couldn't start the tap ({status})"));
    }
    log::info!(
        "System audio tap started: {} Hz, {} channel(s), leaving out {} process(es)",
        rate,
        format.mChannelsPerFrame,
        session.left_out.len()
    );
    Ok((
        Source {
            rate,
            consumer,
            label: "System audio".into(),
        },
        session,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn felix_always_and_the_call_browser_while_it_sends_stay_out() {
        assert!(left_out(10, 10, None));
        assert!(left_out(20, 10, Some(20)));
        assert!(!left_out(20, 10, None));
        assert!(!left_out(30, 10, Some(20)));
    }
}
