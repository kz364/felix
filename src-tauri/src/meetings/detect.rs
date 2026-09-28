//! Telling a call from a meeting in the room, for recordings started
//! without saying which. They record the call audio too, to be safe, and
//! the kind is decided when they stop, from:
//!
//! - a call app (Zoom, Teams, FaceTime…) holding the mic while recording;
//! - a call window: Zoom's meeting window, a Slack huddle, a Meet or Teams
//!   tab in a browser;
//! - people talking through the Mac's speakers for a real share of it.
//!
//! A call counted as in-person would lose the other side, while the other
//! way round only adds a quiet track, so any strong sign means a call.

use super::call_apps::{self, CallApp, CALL_APPS};
use super::MeetingMode;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// Speech from the speakers that makes it a call without other signs:
/// at least this much…
const MIN_SYSTEM_SOUND_SECS: f64 = 20.0;
/// …and at least this share of the recording (a video played in a long
/// meeting in the room isn't a call).
const MIN_SYSTEM_SOUND_SHARE: f64 = 0.05;
const FRAME: usize = 480;
/// A frame louder than this has sound (as in `live`).
const SOUND_RMS: f32 = 0.01;

/// What was seen during a recording.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Signals {
    /// A call app (not a browser) that held the mic.
    pub call_app: Option<&'static str>,
    /// A call window that was open, e.g. "Zoom Meeting".
    pub call_window: Option<String>,
}

impl Signals {
    fn strong(&self) -> bool {
        self.call_app.is_some() || self.call_window.is_some()
    }
}

/// By meeting id.
static SEEN: Lazy<Mutex<HashMap<String, Signals>>> = Lazy::new(|| Mutex::new(HashMap::new()));

/// Whether a window title belongs to a call in this app.
pub fn is_call_title(app: &CallApp, title: &str) -> bool {
    let t = title.to_lowercase();
    if app.browser {
        return t.contains("meet.google.com")
            || t.starts_with("meet - ")
            || t.starts_with("meet – ")
            || t.starts_with("meet: ")
            || t.contains("zoom meeting")
            || t.contains("jitsi meet")
            || t.contains("whereby")
            || t.contains("huddle")
            || (t.contains("microsoft teams") && (t.contains("meeting") || t.contains("call")));
    }
    match app.name {
        "Zoom" => t.contains("zoom meeting") || t.contains("zoom webinar"),
        "Teams" => t.contains("meeting") || t.contains("call with") || t.starts_with("call "),
        "Slack" => t.contains("huddle"),
        "Webex" => t.contains("meeting"),
        "Discord" => t.contains("voice connected") || t.contains("call"),
        // Their windows are the call.
        "FaceTime" => !t.is_empty(),
        _ => false,
    }
}

/// A call window open in any call app right now.
pub fn call_window() -> Option<String> {
    for app in CALL_APPS {
        let Some(pid) = crate::ax_tree::pid_of(app.bundle_id) else {
            continue;
        };
        if let Some(title) = crate::ax_tree::window_titles(pid)
            .into_iter()
            .find(|t| is_call_title(app, t))
        {
            return Some(format!("{}: {title}", app.name));
        }
    }
    None
}

/// Note what's going on now, for a recording whose kind is still open.
/// `holder` is the call app that has held the mic for a few seconds;
/// `look_at_windows` when it's time to check the windows (it's slower).
pub fn note(id: &str, holder: Option<CallApp>, look_at_windows: bool) {
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let signals = seen.entry(id.to_string()).or_default();
    if signals.strong() {
        return;
    }
    if let Some(app) = holder.filter(|a| !a.browser) {
        log::info!("Meeting {id}: {} holds the mic, so it's a call", app.name);
        signals.call_app = Some(app.name);
        return;
    }
    if look_at_windows {
        if let Some(window) = call_window() {
            log::info!("Meeting {id}: call window open ({window})");
            signals.call_window = Some(window);
        }
    }
}

/// Look now, when a recording starts: the call app holding the mic, for
/// reading names from its window.
pub fn start(id: &str) -> Option<CallApp> {
    let holder = call_apps::current();
    note(id, holder, true);
    holder
}

/// Seconds of sound in a WAV (16-bit mono).
pub fn sound_secs(path: &Path) -> f64 {
    let Ok(mut reader) = hound::WavReader::open(path) else {
        return 0.0;
    };
    let rate = reader.spec().sample_rate.max(1) as f64;
    let mut loud = 0usize;
    let mut sum = 0.0f32;
    let mut n = 0usize;
    for sample in reader.samples::<i16>().map_while(Result::ok) {
        let v = sample as f32 / i16::MAX as f32;
        sum += v * v;
        n += 1;
        if n == FRAME {
            if (sum / FRAME as f32).sqrt() > SOUND_RMS {
                loud += 1;
            }
            sum = 0.0;
            n = 0;
        }
    }
    (loud * FRAME) as f64 / rate
}

/// The kind of meeting, from what was seen and how much came through the
/// speakers.
pub fn decide(signals: &Signals, system_sound_secs: f64, duration_secs: f64) -> MeetingMode {
    if signals.strong()
        || (system_sound_secs >= MIN_SYSTEM_SOUND_SECS
            && system_sound_secs >= MIN_SYSTEM_SOUND_SHARE * duration_secs)
    {
        MeetingMode::Call
    } else {
        MeetingMode::InPerson
    }
}

/// Decide a stopped recording's kind, and forget what was seen.
pub fn finish(id: &str, dir: &Path, duration_secs: f64) -> MeetingMode {
    let signals = SEEN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(id)
        .unwrap_or_default();
    let system = if signals.strong() {
        0.0
    } else {
        sound_secs(&dir.join(super::transcript::Source::System.file()))
    };
    let mode = decide(&signals, system, duration_secs);
    log::info!(
        "Meeting {id} is {mode:?}: {signals:?}, {system:.0} s of sound from the speakers in {duration_secs:.0} s"
    );
    mode
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str) -> CallApp {
        *CALL_APPS.iter().find(|a| a.name == name).unwrap()
    }

    #[test]
    fn call_windows_are_recognised() {
        assert!(is_call_title(&app("Zoom"), "Zoom Meeting"));
        assert!(!is_call_title(&app("Zoom"), "Zoom Workplace"));
        assert!(is_call_title(&app("Slack"), "Huddle: #design"));
        assert!(!is_call_title(&app("Slack"), "Slack | general | Acme"));
        assert!(is_call_title(&app("Chrome"), "Meet – abc-defg-hij"));
        assert!(is_call_title(
            &app("Chrome"),
            "Meet - Weekly sync - Google Chrome"
        ));
        assert!(!is_call_title(
            &app("Chrome"),
            "Meeting notes - Google Docs"
        ));
        assert!(!is_call_title(&app("Safari"), "Microsoft Teams - Chat"));
    }

    #[test]
    fn a_strong_sign_or_real_talk_from_the_speakers_makes_a_call() {
        let none = Signals::default();
        let zoom = Signals {
            call_app: Some("Zoom"),
            call_window: None,
        };
        assert_eq!(decide(&zoom, 0.0, 600.0), MeetingMode::Call);
        assert_eq!(decide(&none, 0.0, 600.0), MeetingMode::InPerson);
        assert_eq!(decide(&none, 200.0, 600.0), MeetingMode::Call);
        // A short clip in a long meeting in the room.
        assert_eq!(decide(&none, 30.0, 7200.0), MeetingMode::InPerson);
        // A few sounds from the Mac.
        assert_eq!(decide(&none, 5.0, 60.0), MeetingMode::InPerson);
    }
}
