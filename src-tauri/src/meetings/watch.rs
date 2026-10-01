//! Looking after recordings so the user doesn't have to:
//!
//! - Offer to record when a call app (Zoom, Teams, Meet in a browser…) takes
//!   the mic.
//! - Offer to stop, then stop after a countdown, when a call seems over: the
//!   call app let go of the mic, ten minutes of quiet, or a sign-off ("thanks
//!   everyone") followed by quiet. Never while the other side is talking.
//! - Stop cleanly when the Mac wakes from sleep, with Resume on offer.
//! - Warn before the length limit, then stop.
//! - After a very short recording, offer to throw it away.

use super::call_apps::{self, CallApp};
use super::manager::{MeetingInfo, MeetingManager};
use super::MeetingMode;
use crate::notices::{self, Action, Notice};
use crate::settings::get_settings;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Manager};

const TICK: Duration = Duration::from_secs(1);
/// A call app must hold the mic this many ticks in a row to count.
const HOLD_TICKS: u32 = 3;
/// While working out whether it's a call, look at the windows this often.
const WINDOWS_EVERY_TICKS: u32 = 5;
/// Don't offer to record the same app again sooner than this.
const OFFER_GAP: Duration = Duration::from_secs(10 * 60);
/// The call app let go of the mic this long ago: the call's over.
const RELEASE_GRACE: Duration = Duration::from_secs(20);
/// Both tracks quiet this long: the meeting's over.
const SILENCE: Duration = Duration::from_secs(10 * 60);
/// After a sign-off, this much quiet ends it.
const QUIET_AFTER_SIGN_OFF: Duration = Duration::from_secs(60);
/// The other side had sound this recently: never stop.
const STILL_TALKING: Duration = Duration::from_secs(10);
/// How long the "Stop recording?" card counts down.
const COUNTDOWN: Duration = Duration::from_secs(45);
/// Wall clock ran this much further than the monotonic clock: the Mac slept.
const SLEEP_GAP: Duration = Duration::from_secs(30);
/// The length-limit warning comes this long before.
const LIMIT_WARNING: Duration = Duration::from_secs(10 * 60);
/// "Keep going" adds this much.
const LIMIT_EXTENSION: Duration = Duration::from_secs(60 * 60);
/// Recordings shorter than this get "Keep this recording?".
const SHORT_RECORDING_SECS: f64 = 30.0;

/// Set by the "Keep recording" button.
static KEEP: AtomicBool = AtomicBool::new(false);
/// Added to the length limit by "Keep going", in ms.
static EXTENDED_MS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    CallEnded,
    Silence,
    SignedOff,
}

impl Ending {
    fn text(self, app: Option<&str>) -> String {
        match self {
            Ending::CallEnded => format!("{} stopped using the mic.", app.unwrap_or("The call")),
            Ending::Silence => "Nothing's been said for 10 minutes.".into(),
            Ending::SignedOff => "Sounds like everyone said goodbye.".into(),
        }
    }
}

/// What the watcher remembers about the recording in progress.
struct Watching {
    id: String,
    /// The call app seen holding the mic during this recording.
    call_app: Option<CallApp>,
    released_at: Option<Instant>,
    countdown: Option<(Ending, Instant)>,
    /// The user said "Keep recording" to this; asked again only once it
    /// stops being true and then is true again.
    kept: Option<Ending>,
    warned_limit: bool,
}

struct Watcher {
    app: AppHandle,
    wall: SystemTime,
    mono: Instant,
    holder: Option<CallApp>,
    held_ticks: u32,
    /// Ticks since this recording started, for the slower checks.
    ticks_recording: u32,
    offered: Vec<(&'static str, Instant)>,
    watching: Option<Watching>,
}

fn manager(app: &AppHandle) -> Option<Arc<MeetingManager>> {
    app.try_state::<Arc<MeetingManager>>()
        .map(|m| m.inner().clone())
}

fn stop(app: &AppHandle) {
    if let Some(m) = manager(app) {
        if let Err(e) = m.stop() {
            log::error!("Couldn't stop the meeting: {e}");
        }
    }
}

fn resume_action(id: String) -> Action {
    Action::new("Resume", move |app| {
        let app = app.clone();
        std::thread::spawn(move || {
            if let Some(m) = manager(&app) {
                if let Err(e) = m.resume(&id) {
                    log::error!("Couldn't resume the meeting: {e}");
                    let _ = tauri::Emitter::emit(&app, "meeting-error", e);
                }
            }
        });
    })
}

impl Watcher {
    fn tick(&mut self) {
        let wall = SystemTime::now();
        let mono = Instant::now();
        let slept = wall
            .duration_since(self.wall)
            .unwrap_or_default()
            .saturating_sub(mono.duration_since(self.mono));
        self.wall = wall;
        self.mono = mono;

        let settings = get_settings(&self.app);
        let Some(m) = manager(&self.app) else {
            return;
        };
        let working_out = m.with_recording(|_, info| info.mode_auto) == Some(true);
        let holder = if settings.meeting_detect_calls || settings.meeting_auto_stop || working_out {
            call_apps::current()
        } else {
            None
        };
        if holder.is_some() && holder == self.holder {
            self.held_ticks += 1;
        } else {
            self.held_ticks = u32::from(holder.is_some());
        }
        self.holder = holder;
        let held = holder.filter(|_| self.held_ticks >= HOLD_TICKS);

        let recording = m.with_recording(|r, info| {
            (
                info.id.clone(),
                info.mode,
                r.elapsed(),
                r.quiet_for("mic"),
                r.has_system_track().then(|| r.quiet_for("system")),
            )
        });
        let Some((id, mode, elapsed, mic_quiet, system_quiet)) = recording else {
            self.ticks_recording = 0;
            self.watching = None;
            if settings.meeting_detect_calls {
                if let Some(call) = held {
                    self.offer_to_record(call);
                }
            }
            return;
        };

        if slept > SLEEP_GAP {
            log::info!("The Mac slept for {slept:?} while recording; stopping");
            stop(&self.app);
            notices::show(
                &self.app,
                Notice::new(
                    "meeting_slept",
                    "Recording stopped while your Mac slept",
                    "Everything before is saved. Resume to carry on in the same meeting.",
                )
                .action(resume_action(id))
                .seconds(30),
            );
            return;
        }

        if working_out {
            self.ticks_recording += 1;
            let windows = self.ticks_recording.is_multiple_of(WINDOWS_EVERY_TICKS);
            super::detect::note(&id, held, windows);
        }

        if self.watching.as_ref().is_none_or(|w| w.id != id) {
            KEEP.store(false, Ordering::Relaxed);
            EXTENDED_MS.store(0, Ordering::Relaxed);
            self.watching = Some(Watching {
                id: id.clone(),
                call_app: None,
                released_at: None,
                countdown: None,
                kept: None,
                warned_limit: false,
            });
        }

        if self.check_limit(settings.meeting_max_hours, &id, elapsed) {
            return;
        }
        if !settings.meeting_auto_stop {
            return;
        }
        let watching = self.watching.as_mut().expect("set above");
        if mode == MeetingMode::Call {
            match holder {
                Some(call) => {
                    watching.call_app = Some(call);
                    watching.released_at = None;
                }
                None if watching.call_app.is_some() => {
                    watching.released_at.get_or_insert_with(Instant::now);
                }
                None => {}
            }
        }
        let quiet = system_quiet.map_or(mic_quiet, |s| s.min(mic_quiet));
        let signed_off = super::live::signed_off_at(&id).is_some_and(|at| {
            elapsed.saturating_sub(Duration::from_millis(at)) >= QUIET_AFTER_SIGN_OFF
                && quiet >= QUIET_AFTER_SIGN_OFF
        });
        let ending = if watching
            .released_at
            .is_some_and(|t| t.elapsed() >= RELEASE_GRACE)
        {
            Some(Ending::CallEnded)
        } else if quiet >= SILENCE {
            Some(Ending::Silence)
        } else if signed_off {
            Some(Ending::SignedOff)
        } else {
            None
        };
        let other_side_talking = system_quiet.is_some_and(|q| q < STILL_TALKING);

        if watching.kept.is_some() && watching.kept != ending {
            watching.kept = None;
        }
        if KEEP.swap(false, Ordering::Relaxed) {
            watching.kept = watching.countdown.map(|(e, _)| e);
            watching.countdown = None;
        }
        match (ending, watching.countdown) {
            (Some(_), _) if other_side_talking => watching.countdown = None,
            (None, Some(_)) => {
                // Talking again, or the call app took the mic back.
                watching.countdown = None;
                crate::overlay::dismiss_result_overlay(self.app.clone());
            }
            (Some(e), None) if watching.kept != Some(e) => {
                watching.countdown = Some((e, Instant::now() + COUNTDOWN));
                let app_name = watching.call_app.map(|a| a.name);
                notices::show(
                    &self.app,
                    Notice::new(
                        "meeting_auto_stop",
                        "Stop recording?",
                        format!(
                            "{} Stopping in {} seconds.",
                            e.text(app_name),
                            COUNTDOWN.as_secs()
                        ),
                    )
                    .action(Action::new("Keep recording", |_| {
                        KEEP.store(true, Ordering::Relaxed)
                    }))
                    .action(Action::new("Stop now", |app| {
                        let app = app.clone();
                        std::thread::spawn(move || stop(&app));
                    }))
                    .seconds(COUNTDOWN.as_secs() as u32),
                );
            }
            (Some(e), Some((_, deadline))) if Instant::now() >= deadline => {
                let app_name = watching.call_app.map(|a| a.name);
                let text = e.text(app_name);
                self.watching = None;
                log::info!("Stopping the meeting on its own: {e:?}");
                stop(&self.app);
                notices::show(
                    &self.app,
                    Notice::new("meeting_stopped", "Recording stopped", text)
                        .action(resume_action(id))
                        .seconds(15),
                );
            }
            _ => {}
        }
    }

    /// Warn before the length limit and stop at it. True if it stopped.
    fn check_limit(&mut self, max_hours: u32, id: &str, elapsed: Duration) -> bool {
        if max_hours == 0 {
            return false;
        }
        let limit = Duration::from_secs(max_hours as u64 * 3600)
            + Duration::from_millis(EXTENDED_MS.load(Ordering::Relaxed));
        let watching = self.watching.as_mut().expect("set by tick");
        if elapsed >= limit {
            self.watching = None;
            log::info!("Meeting {id} reached the length limit");
            stop(&self.app);
            notices::show(
                &self.app,
                Notice::new(
                    "meeting_limit",
                    "Recording stopped",
                    format!("It reached the {max_hours}-hour limit. Resume to carry on."),
                )
                .action(resume_action(id.to_string()))
                .seconds(30),
            );
            return true;
        }
        if elapsed + LIMIT_WARNING >= limit && !watching.warned_limit {
            watching.warned_limit = true;
            notices::show(
                &self.app,
                Notice::new(
                    "meeting_limit_warning",
                    "Recording stops in 10 minutes",
                    format!("It's close to the {max_hours}-hour limit."),
                )
                .action(Action::new("Keep going", |_| {
                    EXTENDED_MS.fetch_add(LIMIT_EXTENSION.as_millis() as u64, Ordering::Relaxed);
                }))
                .seconds(30),
            );
        } else if elapsed + LIMIT_WARNING < limit {
            watching.warned_limit = false;
        }
        false
    }

    fn offer_to_record(&mut self, call: CallApp) {
        self.offered.retain(|(_, at)| at.elapsed() < OFFER_GAP);
        if self.offered.iter().any(|(b, _)| *b == call.bundle_id) {
            return;
        }
        self.offered.push((call.bundle_id, Instant::now()));
        let text = if call.browser {
            format!(
                "{} is using the microphone. If it's a call, Felix can take notes.",
                call.name
            )
        } else {
            format!(
                "{} is using the microphone. Felix can take notes.",
                call.name
            )
        };
        let bundle = call.bundle_id;
        notices::show(
            &self.app,
            Notice::new("record_call", "Record this call?", text)
                .action(Action::new("Record", move |app| {
                    let app = app.clone();
                    std::thread::spawn(move || {
                        if let Some(m) = manager(&app) {
                            if let Err(e) = m.start_call(bundle) {
                                log::error!("Couldn't start recording the call: {e}");
                                let _ = tauri::Emitter::emit(&app, "meeting-error", e);
                            }
                        }
                    });
                }))
                // Not a muteable tip: turning it off is the setting, which
                // Settings → Meetings shows (a mute there was invisible).
                .action(Action::new("Stop asking", |app| {
                    let mut settings = get_settings(app);
                    settings.meeting_detect_calls = false;
                    crate::settings::write_settings(app, settings);
                    log::info!("Call detection turned off from the notice");
                }))
                .seconds(20),
        );
    }
}

/// Start watching for calls and looking after recordings.
pub fn spawn(app: &AppHandle) {
    let mut watcher = Watcher {
        app: app.clone(),
        wall: SystemTime::now(),
        mono: Instant::now(),
        holder: None,
        held_ticks: 0,
        ticks_recording: 0,
        offered: Vec::new(),
        watching: None,
    };
    let _ = std::thread::Builder::new()
        .name("meeting-watch".into())
        .spawn(move || {
            // Windows made after setup (the overlay) pick up the setting here.
            std::thread::sleep(Duration::from_secs(3));
            // "Record this call?" could be muted once, which turned call
            // detection off while Settings still showed it on.
            let mut settings = get_settings(&watcher.app);
            if settings.muted_notices.iter().any(|k| k == "record_call") {
                settings.muted_notices.retain(|k| k != "record_call");
                crate::settings::write_settings(&watcher.app, settings);
                log::info!("\"Record this call?\" is shown again");
            }
            if get_settings(&watcher.app).hide_from_screen_share {
                hide_from_screen_share(&watcher.app, true);
            }
            loop {
                watcher.tick();
                std::thread::sleep(TICK);
            }
        });
}

/// A recording started: suggest hiding Felix from screen sharing on calls.
pub fn started(app: &AppHandle, info: &MeetingInfo) {
    // A recording still working out what it is only counts once a call app
    // is involved.
    let call = info.mode == MeetingMode::Call && (!info.mode_auto || info.call_app.is_some());
    if !call || get_settings(app).hide_from_screen_share {
        return;
    }
    notices::show(
        app,
        Notice::new(
            "hide_from_screen_share",
            "Hide Felix when you share your screen?",
            "People on the call won't see Felix's pill or cards.",
        )
        .action(Action::new("Hide", |app| {
            let mut settings = get_settings(app);
            settings.hide_from_screen_share = true;
            crate::settings::write_settings(app, settings);
            hide_from_screen_share(app, true);
        }))
        .nudge(Duration::from_secs(30 * 24 * 3600))
        .seconds(12),
    );
}

/// A recording stopped: offer to throw away one that's only a few seconds.
pub fn stopped(app: &AppHandle, info: &MeetingInfo) {
    let seconds = info.tracks.iter().map(|t| t.seconds).fold(0.0, f64::max);
    if seconds >= SHORT_RECORDING_SECS || !info.resumed_at_ms.is_empty() || info.tracks.is_empty() {
        return;
    }
    let id = info.id.clone();
    notices::show(
        app,
        Notice::new(
            "short_meeting",
            "Keep this recording?",
            format!("It's only {} seconds long.", seconds.round() as u32),
        )
        .action(Action::new("Keep", |_| {}))
        .action(Action::new("Discard", move |app| {
            let app = app.clone();
            std::thread::spawn(move || {
                // It may be transcribing: wait for that to finish.
                for _ in 0..60 {
                    match super::manager::delete_meeting(app.clone(), id.clone()) {
                        Ok(()) => return,
                        Err(e) if e.contains("being processed") => {
                            std::thread::sleep(Duration::from_secs(1))
                        }
                        Err(e) => {
                            log::error!("Couldn't discard the meeting: {e}");
                            return;
                        }
                    }
                }
            });
        }))
        .seconds(15),
    );
}

/// Show or hide every Felix window in screen sharing and screenshots.
pub fn hide_from_screen_share(app: &AppHandle, hide: bool) {
    for (label, window) in app.webview_windows() {
        if let Err(e) = window.set_content_protected(hide) {
            log::warn!("Couldn't change screen-share visibility of {label}: {e}");
        }
    }
}
