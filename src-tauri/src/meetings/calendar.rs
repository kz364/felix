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
    /// Attendees known only by a one-word e-mail handle ("Peterlai"), which
    /// may be first and last name run together.
    #[serde(default)]
    pub handles: Vec<String>,
    /// [`VERSION`] when it was read from the calendar; older ones are read
    /// again.
    #[serde(default)]
    pub version: u32,
    /// The other attendees' e-mail addresses, lowercased. Fewer than
    /// `attendees` when someone is listed without one.
    #[serde(default)]
    pub emails: Vec<String>,
    /// The user's own address on the invite, lowercased.
    #[serde(default)]
    pub my_email: Option<String>,
}

/// Bumped when [`Invite`] gains something worth reading the calendar again
/// for (1: handles, 2: e-mail addresses, for telling whether a meeting was
/// only with people from the user's own organisation).
pub const VERSION: u32 = 2;

/// The address in an attendee's `mailto:` URL, lowercased.
pub fn email_of(url: Option<&str>) -> Option<String> {
    let url = url?.trim();
    let rest = url.get(..7).filter(|p| p.eq_ignore_ascii_case("mailto:"))?;
    let address = url[rest.len()..].split('?').next()?.trim().to_lowercase();
    let (local, domain) = address.split_once('@')?;
    (!local.is_empty() && domain.contains('.')).then_some(address)
}

/// Everything after the last @, lowercased.
pub fn domain_of(email: &str) -> Option<&str> {
    email
        .rsplit_once('@')
        .map(|(_, d)| d)
        .filter(|d| !d.is_empty())
}

/// Whether everyone on the invite is from the user's own e-mail domain:
/// someone else is invited, the user's address is known, every attendee
/// has an address, and all of them share the user's domain. Only then is
/// sharing the notes with the invitees not sharing them outside.
pub fn can_share(invite: Option<&Invite>) -> bool {
    let Some(invite) = invite else {
        return false;
    };
    let Some(mine) = invite.my_email.as_deref().and_then(domain_of) else {
        return false;
    };
    !invite.attendees.is_empty()
        && invite.emails.len() >= invite.attendees.len()
        && invite
            .emails
            .iter()
            .all(|e| domain_of(e).is_some_and(|d| d.eq_ignore_ascii_case(mine)))
}

pub fn load(dir: &Path) -> Option<Invite> {
    super::summary::load_json(dir, FILE)
}

/// Keep the invite of the calendar event `event_id` with the meeting when
/// it starts, so the link survives the event being moved or others
/// overlapping it.
pub fn save_for_event(dir: &Path, event_id: &str) {
    if let Some(invite) = imp::invite_for_event(event_id) {
        let _ = super::summary::save_json(dir, FILE, &invite);
    }
}

/// The event's attendees for a meeting from `start_ms` to `end_ms` (Unix
/// ms), saved with the meeting the first time. None without Calendar
/// access or a matching event with attendees.
/// A meeting linked to its calendar event (see
/// [`super::manager::MeetingInfo::event_id`]) gets that event's invite, not
/// whichever event overlaps most.
pub fn for_meeting(dir: &Path, start_ms: i64, end_ms: i64) -> Option<Invite> {
    let saved = load(dir);
    if let Some(i) = saved.as_ref().filter(|i| i.version >= VERSION) {
        return Some(i.clone());
    }
    let linked = super::manager::read_info(dir)
        .and_then(|i| i.event_id)
        .and_then(|id| imp::invite_for_event(&id));
    let Some(invite) = linked.or_else(|| imp::invite_between(start_ms, end_ms)) else {
        return saved;
    };
    let _ = super::summary::save_json(dir, FILE, &invite);
    Some(invite)
}

/// The words of the invited people's names, to spell right in this
/// meeting's transcript only. Not e-mail handles ("Peterlai"): nobody says
/// those. Read from the saved invite, or else the calendar without saving
/// (while recording, the end is still a guess).
pub fn name_words(dir: &Path, start_ms: i64, end_ms: i64) -> Vec<String> {
    let Some(invite) = load(dir).or_else(|| imp::invite_between(start_ms, end_ms)) else {
        return Vec::new();
    };
    let mut words: Vec<String> = Vec::new();
    for name in invite
        .attendees
        .iter()
        .filter(|a| !invite.handles.contains(a))
    {
        for w in crate::rules::name_words(name) {
            if !words.contains(&w) {
                words.push(w);
            }
        }
    }
    words
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

/// A meeting coming up on the calendar, for the Meetings tab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, specta::Type)]
pub struct Upcoming {
    pub id: String,
    pub title: String,
    /// Unix milliseconds.
    pub start_ms: i64,
    pub end_ms: i64,
    /// Everyone invited but the user.
    pub attendees: Vec<String>,
    /// The video call link in the event, if it has one.
    pub link: Option<String>,
}

/// How far ahead the Meetings tab looks.
const UPCOMING_HOURS: i64 = 24;
/// At most this many are shown.
const UPCOMING_MAX: usize = 8;

/// The meetings from now (including ones under way) to a day ahead: events
/// with someone else invited or a call link, not all-day, not declined or
/// cancelled. Empty without Calendar access.
#[tauri::command]
#[specta::specta]
pub async fn upcoming_meetings() -> Vec<Upcoming> {
    tauri::async_runtime::spawn_blocking(|| {
        let now = chrono::Utc::now().timestamp_millis();
        let mut events = imp::upcoming(now, now + UPCOMING_HOURS * 60 * 60 * 1000);
        events.sort_by_key(|e| e.start_ms);
        events.truncate(UPCOMING_MAX);
        events
    })
    .await
    .unwrap_or_default()
}

/// A meeting starting this much later still counts as the one being
/// recorded: started during the meeting, or up to two minutes before it.
/// Any earlier is more likely something else than joining early.
const EARLY_MS: i64 = 2 * 60 * 1000;

/// Of the calendar's meetings, the one a recording started at `now_ms` is
/// of: under way or about to start, the one starting nearest to now.
pub fn meeting_at(events: Vec<Upcoming>, now_ms: i64) -> Option<Upcoming> {
    events
        .into_iter()
        .filter(|e| e.start_ms <= now_ms + EARLY_MS && e.end_ms > now_ms)
        .min_by_key(|e| (e.start_ms - now_ms).abs())
}

/// The calendar meeting being recorded now, if there is one.
pub fn meeting_now() -> Option<Upcoming> {
    let now = chrono::Utc::now().timestamp_millis();
    meeting_at(imp::upcoming(now, now + EARLY_MS), now)
}

/// The first video call link (Meet, Zoom, Teams, Webex) in an event's
/// fields, as written there.
pub fn call_link(fields: &[&str]) -> Option<String> {
    const HOSTS: &[&str] = &[
        "meet.google.com/",
        "zoom.us/j/",
        "zoom.us/my/",
        "teams.microsoft.com/l/meetup-join",
        "teams.live.com/meet",
        "webex.com/",
    ];
    fields.iter().find_map(|f| {
        f.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '(' | ')'))
            .map(|w| w.trim_end_matches(['.', ',', ';']))
            .find(|w| w.starts_with("https://") && HOSTS.iter().any(|h| w.contains(h)))
            .map(str::to_string)
    })
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
    use super::{display_name, email_of, Invite};
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_event_kit::{EKAuthorizationStatus, EKEntityType, EKEvent, EKEventStore};
    use objc2_foundation::{NSDate, NSError, NSString};

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
            let Some(invite) = read_invite(&event) else {
                continue;
            };
            let e_start = (unsafe { event.startDate() }.timeIntervalSince1970() * 1000.0) as i64;
            let e_end = (unsafe { event.endDate() }.timeIntervalSince1970() * 1000.0) as i64;
            let overlap = e_end.min(end_ms) - e_start.max(start_ms);
            if overlap > 0 && best.as_ref().is_none_or(|(o, _)| overlap > *o) {
                best = Some((overlap, invite));
            }
        }
        best.map(|(_, i)| i)
    }

    /// The invite of one event, by the id [`super::Upcoming::id`] has.
    pub fn invite_for_event(id: &str) -> Option<Invite> {
        if access() != "full" {
            return None;
        }
        let store = unsafe { EKEventStore::new() };
        let event = unsafe { store.eventWithIdentifier(&NSString::from_str(id)) }?;
        read_invite(&event)
    }

    /// The attendees of a timed event that has some.
    fn read_invite(event: &EKEvent) -> Option<Invite> {
        if unsafe { event.isAllDay() } {
            return None;
        }
        let attendees = unsafe { event.attendees() }.filter(|a| a.count() > 0)?;
        let mut invite = Invite {
            title: unsafe { event.title() }.to_string(),
            version: super::VERSION,
            ..Default::default()
        };
        for a in attendees.iter() {
            let name = unsafe { a.name() }.map(|n| n.to_string());
            let url = unsafe { a.URL() }.absoluteString().map(|u| u.to_string());
            let email = email_of(url.as_deref());
            let Some(n) = display_name(name.as_deref(), url.as_deref()) else {
                // Not a name anyone would say, but still someone invited.
                if let Some(e) = email.filter(|_| !unsafe { a.isCurrentUser() }) {
                    if !invite.emails.contains(&e) {
                        invite.emails.push(e);
                    }
                }
                continue;
            };
            if unsafe { a.isCurrentUser() } {
                invite.me = Some(n);
                invite.my_email = email;
                continue;
            }
            // Someone with no address is always counted, so there are
            // fewer addresses than people.
            match email {
                Some(e) if invite.attendees.contains(&n) => {
                    if !invite.emails.contains(&e) {
                        invite.emails.push(e);
                    }
                }
                Some(e) => {
                    invite.attendees.push(n.clone());
                    invite.emails.push(e);
                }
                None => invite.attendees.push(n.clone()),
            }
            let named = name.as_deref().is_some_and(|n| !n.contains('@'));
            if !named && !n.contains(' ') && !invite.handles.contains(&n) {
                invite.handles.push(n);
            }
        }
        Some(invite)
    }

    pub fn upcoming(from_ms: i64, to_ms: i64) -> Vec<super::Upcoming> {
        use objc2_event_kit::{EKEventStatus, EKParticipantStatus};
        if access() != "full" {
            return Vec::new();
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let secs = |ms: i64| (ms - now_ms) as f64 / 1000.0;
        let store = unsafe { EKEventStore::new() };
        let from = NSDate::dateWithTimeIntervalSinceNow(secs(from_ms));
        let to = NSDate::dateWithTimeIntervalSinceNow(secs(to_ms));
        let events = unsafe {
            let predicate =
                store.predicateForEventsWithStartDate_endDate_calendars(&from, &to, None);
            store.eventsMatchingPredicate(&predicate)
        };
        let mut out = Vec::new();
        for event in events.iter() {
            if unsafe { event.isAllDay() } || unsafe { event.status() } == EKEventStatus::Canceled {
                continue;
            }
            let start_ms = (unsafe { event.startDate() }.timeIntervalSince1970() * 1000.0) as i64;
            let end_ms = (unsafe { event.endDate() }.timeIntervalSince1970() * 1000.0) as i64;
            if end_ms <= from_ms {
                continue;
            }
            let mut attendees = Vec::new();
            let mut declined = false;
            if let Some(list) = unsafe { event.attendees() } {
                for a in list.iter() {
                    if unsafe { a.isCurrentUser() } {
                        declined =
                            unsafe { a.participantStatus() } == EKParticipantStatus::Declined;
                        continue;
                    }
                    let name = unsafe { a.name() }.map(|n| n.to_string());
                    let url = unsafe { a.URL() }.absoluteString().map(|u| u.to_string());
                    if let Some(n) = display_name(name.as_deref(), url.as_deref()) {
                        if !attendees.contains(&n) {
                            attendees.push(n);
                        }
                    }
                }
            }
            let url = unsafe { event.URL() }
                .and_then(|u| u.absoluteString())
                .map(|u| u.to_string())
                .unwrap_or_default();
            let location = unsafe { event.location() }
                .map(|l| l.to_string())
                .unwrap_or_default();
            let notes = unsafe { event.notes() }
                .map(|n| n.to_string())
                .unwrap_or_default();
            let link = super::call_link(&[&url, &location, &notes]);
            if declined || (attendees.is_empty() && link.is_none()) {
                continue;
            }
            out.push(super::Upcoming {
                id: unsafe { event.eventIdentifier() }
                    .map(|i| i.to_string())
                    .unwrap_or_else(|| format!("{start_ms}")),
                title: unsafe { event.title() }.to_string(),
                start_ms,
                end_ms,
                attendees,
                link,
            });
        }
        out
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
    pub fn invite_for_event(_: &str) -> Option<Invite> {
        None
    }
    pub fn upcoming(_: i64, _: i64) -> Vec<super::Upcoming> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_call_link_is_found_in_any_field() {
        assert_eq!(
            call_link(&[
                "",
                "Room 4",
                "Join: <https://meet.google.com/abc-defg-hij>."
            ]),
            Some("https://meet.google.com/abc-defg-hij".into())
        );
        assert_eq!(
            call_link(&["https://us02web.zoom.us/j/123?pwd=x", "", ""]),
            Some("https://us02web.zoom.us/j/123?pwd=x".into())
        );
        assert_eq!(
            call_link(&["https://example.com/agenda", "Cafe", "lunch"]),
            None
        );
    }

    #[test]
    fn the_invites_names_are_this_meetings_words_but_not_handles() {
        let dir = tempfile::tempdir().unwrap();
        let invite = Invite {
            title: "Sync".into(),
            attendees: vec!["Sam Rivera".into(), "Priya".into(), "Peterlai".into()],
            me: Some("Kaspar".into()),
            handles: vec!["Peterlai".into()],
            version: VERSION,
            ..Default::default()
        };
        super::super::summary::save_json(dir.path(), FILE, &invite).unwrap();
        assert_eq!(name_words(dir.path(), 0, 0), vec!["Sam", "Rivera", "Priya"]);
    }

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

    #[test]
    fn a_recording_is_of_the_meeting_under_way_or_about_to_start() {
        let min = 60 * 1000;
        let event = |title: &str, start: i64, end: i64| Upcoming {
            id: title.into(),
            title: title.into(),
            start_ms: start * min,
            end_ms: end * min,
            attendees: vec![],
            link: None,
        };
        let now = 100 * min;
        let pick = |events| meeting_at(events, now).map(|e| e.title);
        // Joined a minute early, while a long block is still on.
        let events = vec![event("Focus block", 60, 180), event("HPE call", 101, 135)];
        assert_eq!(pick(events).as_deref(), Some("HPE call"));
        // Two minutes early still counts; five does not.
        assert_eq!(pick(vec![event("Sync", 102, 130)]).as_deref(), Some("Sync"));
        assert_eq!(pick(vec![event("Sync", 105, 130)]), None);
        // Late to one that started ten minutes ago.
        assert_eq!(
            pick(vec![event("Standup", 90, 120)]).as_deref(),
            Some("Standup")
        );
        // Nothing for one half an hour away or one that's over.
        assert_eq!(
            pick(vec![event("Later", 130, 160), event("Done", 40, 95)]),
            None
        );
    }

    fn invite(me: Option<&str>, emails: &[&str], attendees: &[&str]) -> Invite {
        Invite {
            attendees: attendees.iter().map(|a| a.to_string()).collect(),
            emails: emails.iter().map(|a| a.to_string()).collect(),
            my_email: me.map(str::to_string),
            version: VERSION,
            ..Default::default()
        }
    }

    #[test]
    fn a_meeting_can_be_shared_only_when_everyone_is_from_the_users_domain() {
        let same = invite(
            Some("me@acme.com"),
            &["sam@acme.com", "priya@acme.com"],
            &["Sam", "Priya"],
        );
        assert!(can_share(Some(&same)));
        let outside = invite(
            Some("me@acme.com"),
            &["sam@acme.com", "pat@gmail.com"],
            &["Sam", "Pat"],
        );
        assert!(!can_share(Some(&outside)));
        assert!(!can_share(None));
        // Someone listed by name only, with no address to check.
        let nameless = invite(Some("me@acme.com"), &["sam@acme.com"], &["Sam", "Priya"]);
        assert!(!can_share(Some(&nameless)));
        // Nobody else, or no way to tell the user's own domain.
        assert!(!can_share(Some(&invite(Some("me@acme.com"), &[], &[]))));
        let unknown = invite(None, &["sam@acme.com"], &["Sam"]);
        assert!(!can_share(Some(&unknown)));
    }

    #[test]
    fn addresses_come_from_mailto_links_in_lowercase() {
        assert_eq!(
            email_of(Some("mailto:Sam.Rivera@Acme.com")).as_deref(),
            Some("sam.rivera@acme.com")
        );
        assert_eq!(email_of(Some("https://example.com")), None);
        assert_eq!(email_of(Some("mailto:nonsense")), None);
        assert_eq!(email_of(None), None);
    }
}
