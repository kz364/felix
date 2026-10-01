//! Who was invited: the calendar event a meeting was recorded in, read
//! with EventKit (the Mac's Calendar, including accounts under Internet
//! Accounts). The attendees are candidates for the voices nobody named
//! (see [`super::speakers`]). Felix only reads, and only once the user has
//! allowed Calendar access from Settings; it never asks on its own.

use serde::{Deserialize, Serialize};
use std::path::Path;

pub const FILE: &str = "calendar.json";

/// The event a meeting overlapped most.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Invite {
    pub title: String,
    /// Everyone invited but the user.
    pub attendees: Vec<String>,
    /// The user's own name on the invite, if it has one.
    pub me: Option<String>,
}

pub fn load(dir: &Path) -> Option<Invite> {
    super::summary::load_json(dir, FILE)
}

/// The event's attendees for a meeting from `start_ms` to `end_ms` (Unix
/// ms), saved with the meeting the first time. None without Calendar
/// access or a matching event with attendees.
pub fn for_meeting(dir: &Path, start_ms: i64, end_ms: i64) -> Option<Invite> {
    if let Some(i) = load(dir) {
        return Some(i);
    }
    let invite = imp::invite_between(start_ms, end_ms)?;
    let _ = super::summary::save_json(dir, FILE, &invite);
    Some(invite)
}

/// "full", "denied", "not_determined" (or "restricted", "write_only").
pub fn access() -> &'static str {
    imp::access()
}

/// Ask macOS for Calendar access (shows its prompt the first time) and say
/// what the user chose.
#[tauri::command]
#[specta::specta]
pub async fn request_calendar_access() -> String {
    imp::request().await;
    access().to_string()
}

#[tauri::command]
#[specta::specta]
pub fn calendar_access() -> String {
    access().to_string()
}

/// Names from attendee display names; e-mail-only attendees become the part
/// before the @, tidied ("sam.rivera" → "Sam Rivera").
pub fn display_name(name: Option<&str>, url: Option<&str>) -> Option<String> {
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    if let Some(n) = name {
        if !n.contains('@') {
            return Some(n.to_string());
        }
    }
    let email = name
        .filter(|n| n.contains('@'))
        .or_else(|| url.map(|u| u.trim_start_matches("mailto:")))?;
    let local = email.split('@').next()?;
    let words: Vec<String> = local
        .split(['.', '_', '-'])
        .filter(|w| !w.is_empty() && w.chars().all(char::is_alphabetic))
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
                .unwrap_or_default()
        })
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{display_name, Invite};
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_event_kit::{EKAuthorizationStatus, EKEntityType, EKEventStore};
    use objc2_foundation::{NSDate, NSError};

    pub fn access() -> &'static str {
        let status = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
        match status {
            EKAuthorizationStatus::FullAccess => "full",
            EKAuthorizationStatus::Denied => "denied",
            EKAuthorizationStatus::Restricted => "restricted",
            EKAuthorizationStatus::WriteOnly => "write_only",
            _ => "not_determined",
        }
    }

    pub async fn request() {
        // The store and the block live on a thread of their own until macOS
        // answers (they aren't Send).
        let _ = tauri::async_runtime::spawn_blocking(|| {
            let (tx, rx) = std::sync::mpsc::channel::<()>();
            let store = unsafe { EKEventStore::new() };
            let done = RcBlock::new(move |_granted: Bool, _error: *mut NSError| {
                let _ = tx.send(());
            });
            unsafe {
                store.requestFullAccessToEventsWithCompletion(RcBlock::as_ptr(&done));
            }
            let _ = rx.recv_timeout(std::time::Duration::from_secs(120));
        })
        .await;
    }

    pub fn invite_between(start_ms: i64, end_ms: i64) -> Option<Invite> {
        if access() != "full" {
            return None;
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let secs = |ms: i64| (ms - now_ms) as f64 / 1000.0;
        let store = unsafe { EKEventStore::new() };
        let from = NSDate::dateWithTimeIntervalSinceNow(secs(start_ms));
        let to = NSDate::dateWithTimeIntervalSinceNow(secs(end_ms.max(start_ms + 60_000)));
        let events = unsafe {
            let predicate =
                store.predicateForEventsWithStartDate_endDate_calendars(&from, &to, None);
            store.eventsMatchingPredicate(&predicate)
        };
        let mut best: Option<(i64, Invite)> = None;
        for event in events.iter() {
            let (Some(attendees), all_day) =
                (unsafe { event.attendees() }, unsafe { event.isAllDay() })
            else {
                continue;
            };
            if all_day || attendees.count() == 0 {
                continue;
            }
            let e_start = (unsafe { event.startDate() }.timeIntervalSince1970() * 1000.0) as i64;
            let e_end = (unsafe { event.endDate() }.timeIntervalSince1970() * 1000.0) as i64;
            let overlap = e_end.min(end_ms) - e_start.max(start_ms);
            if overlap <= 0 {
                continue;
            }
            let mut invite = Invite {
                title: unsafe { event.title() }.to_string(),
                ..Default::default()
            };
            for a in attendees.iter() {
                let name = unsafe { a.name() }.map(|n| n.to_string());
                let url = unsafe { a.URL() }.absoluteString().map(|u| u.to_string());
                let Some(n) = display_name(name.as_deref(), url.as_deref()) else {
                    continue;
                };
                if unsafe { a.isCurrentUser() } {
                    invite.me = Some(n);
                } else if !invite.attendees.contains(&n) {
                    invite.attendees.push(n);
                }
            }
            if best.as_ref().is_none_or(|(o, _)| overlap > *o) {
                best = Some((overlap, invite));
            }
        }
        best.map(|(_, i)| i)
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::Invite;
    pub fn access() -> &'static str {
        "restricted"
    }
    pub async fn request() {}
    pub fn invite_between(_: i64, _: i64) -> Option<Invite> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attendee_names_come_from_names_or_addresses() {
        assert_eq!(
            display_name(Some("Sam Rivera"), None).as_deref(),
            Some("Sam Rivera")
        );
        assert_eq!(
            display_name(Some("sam.rivera@example.com"), None).as_deref(),
            Some("Sam Rivera")
        );
        assert_eq!(
            display_name(None, Some("mailto:priya_k@example.com")).as_deref(),
            Some("Priya K")
        );
        assert_eq!(display_name(None, Some("mailto:12345@example.com")), None);
    }
}
