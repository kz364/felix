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
    use objc2_core_audio::kAudioProcessPropertyIsRunningInput;

    super::system_audio::process_objects()
        .into_iter()
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
        .filter_map(super::system_audio::bundle_id)
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
