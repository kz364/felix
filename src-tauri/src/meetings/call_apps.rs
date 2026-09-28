//! Which call app is using the microphone. Core Audio lists every process
//! with audio open and whether it's recording, so a call starting (or
//! ending) shows up as a call app taking (or letting go of) the mic, with no
//! permissions needed.

/// A call app, found by bundle id (browsers by prefix, since their audio
/// runs in helper processes such as `com.google.Chrome.helper`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallApp {
    pub bundle_id: &'static str,
    pub name: &'static str,
    /// A browser holds the mic for a call in a tab (Meet), but also for
    /// other things, so it's a weaker sign.
    pub browser: bool,
}

const fn app(bundle_id: &'static str, name: &'static str) -> CallApp {
    CallApp {
        bundle_id,
        name,
        browser: false,
    }
}

const fn browser(bundle_id: &'static str, name: &'static str) -> CallApp {
    CallApp {
        bundle_id,
        name,
        browser: true,
    }
}

pub const CALL_APPS: &[CallApp] = &[
    app("us.zoom.xos", "Zoom"),
    app("com.microsoft.teams2", "Teams"),
    app("com.microsoft.teams", "Teams"),
    app("com.tinyspeck.slackmacgap", "Slack"),
    app("com.cisco.webexmeetingsapp", "Webex"),
    app("com.webex.meetingmanager", "Webex"),
    app("com.apple.FaceTime", "FaceTime"),
    app("net.whatsapp.WhatsApp", "WhatsApp"),
    app("com.hnc.Discord", "Discord"),
    app("com.skype.skype", "Skype"),
    browser("com.google.Chrome", "Chrome"),
    browser("com.apple.Safari", "Safari"),
    browser("com.apple.WebKit", "Safari"),
    browser("company.thebrowser.Browser", "Arc"),
    browser("com.microsoft.edgemac", "Edge"),
    browser("org.mozilla.firefox", "Firefox"),
    browser("com.brave.Browser", "Brave"),
];

/// The call app a process belongs to, if it is one.
pub fn call_app(bundle_id: &str) -> Option<CallApp> {
    CALL_APPS
        .iter()
        .find(|a| {
            bundle_id == a.bundle_id
                || (a.browser && bundle_id.starts_with(&format!("{}.", a.bundle_id)))
        })
        .copied()
}

/// The call app holding the mic among these recording processes, preferring
/// a real call app over a browser.
pub fn holding_call_app(recording: &[String]) -> Option<CallApp> {
    let apps: Vec<CallApp> = recording.iter().filter_map(|b| call_app(b)).collect();
    apps.iter()
        .find(|a| !a.browser)
        .or_else(|| apps.first())
        .copied()
}

/// Bundle ids of the processes recording from any input right now.
#[cfg(target_os = "macos")]
pub fn recording_processes() -> Vec<String> {
    use objc2::rc::Retained;
    use objc2_core_audio::{
        kAudioHardwarePropertyProcessObjectList, kAudioObjectPropertyElementMain,
        kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioProcessPropertyBundleID,
        kAudioProcessPropertyIsRunningInput, AudioObjectGetPropertyData,
        AudioObjectGetPropertyDataSize, AudioObjectID, AudioObjectPropertyAddress,
    };
    use objc2_foundation::NSString;
    use std::ptr::NonNull;

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
    let mut ids = vec![0 as AudioObjectID; size as usize / std::mem::size_of::<AudioObjectID>()];
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
    ids.truncate(size as usize / std::mem::size_of::<AudioObjectID>());
    ids.into_iter()
        .filter(|&id| {
            // SAFETY: a u32 property of a process object.
            unsafe {
                super::system_audio::get_property::<u32>(
                    id,
                    kAudioProcessPropertyIsRunningInput,
                    None,
                    0,
                )
            }
            .is_ok_and(|running| running != 0)
        })
        .filter_map(|id| {
            // SAFETY: the property is a +1 CFString, toll-free bridged.
            let raw: *mut NSString = unsafe {
                super::system_audio::get_property(
                    id,
                    kAudioProcessPropertyBundleID,
                    None,
                    std::ptr::null_mut(),
                )
            }
            .ok()?;
            let name = unsafe { Retained::from_raw(raw) }?.to_string();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
pub fn recording_processes() -> Vec<String> {
    Vec::new()
}

/// The call app using the mic right now, if any.
pub fn current() -> Option<CallApp> {
    holding_call_app(&recording_processes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_apps_are_found_by_bundle_id() {
        assert_eq!(call_app("us.zoom.xos").unwrap().name, "Zoom");
        assert!(!call_app("us.zoom.xos").unwrap().browser);
        let chrome = call_app("com.google.Chrome.helper").unwrap();
        assert!(chrome.browser);
        assert_eq!(chrome.bundle_id, "com.google.Chrome");
        assert!(call_app("com.google.Chromeish").is_none());
        assert!(call_app("com.pais.handy").is_none());
    }

    #[test]
    fn a_call_app_wins_over_a_browser() {
        let holding = vec![
            "com.google.Chrome.helper".to_string(),
            "com.microsoft.teams2".to_string(),
        ];
        assert_eq!(holding_call_app(&holding).unwrap().name, "Teams");
        assert_eq!(
            holding_call_app(&["com.apple.Safari".to_string()])
                .unwrap()
                .name,
            "Safari"
        );
        assert!(holding_call_app(&["com.apple.VoiceMemos".to_string()]).is_none());
    }
}
