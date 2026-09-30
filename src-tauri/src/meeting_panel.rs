//! A small panel on the side of the screen while a meeting records: the
//! timer, Stop, and the notes scratchpad, so there's something on screen
//! that says Felix is recording and somewhere to jot things down without
//! opening the settings window. It opens when a recording starts and goes
//! away when the recording stops. In between it can fold into a slim
//! vertical tab on the screen's right edge (with live sound waves), which
//! can be dragged up and down but not closed, so it's always there to open
//! again.
//!
//! It can take the keyboard (to type notes) without making Felix the active
//! app, so the call app keeps its place. Dropped near a side of the screen,
//! it snaps to it. The window changes size in one step; the page animates
//! the panel growing out of the tab and back.

use serde::Serialize;
use specta::Type;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

const LABEL: &str = "meeting_panel";
const EXPANDED: (f64, f64) = (340.0, 500.0);
/// The tab, flush with the screen's right edge.
const COLLAPSED: (f64, f64) = (48.0, 176.0);
/// Space from the screen's right edge.
const EDGE: f64 = 16.0;
/// Down from the top of the screen's usable area.
const TOP: f64 = 72.0;
/// Dropped this close to a side of the screen, the panel snaps to it.
const SNAP: f64 = 48.0;
/// How long the panel has to be still before it snaps.
const SNAP_AFTER: Duration = Duration::from_millis(250);

#[cfg(target_os = "macos")]
tauri_nspanel::tauri_panel! {
    panel!(MeetingPanel {
        config: {
            can_become_key_window: true,
            is_floating_panel: true
        }
    })
}

/// Placed on a screen at least once this run; after that it stays where
/// the user left it.
static PLACED: AtomicBool = AtomicBool::new(false);
static EXPANDED_NOW: AtomicBool = AtomicBool::new(true);
/// Counts moves, so only the last one of a drag snaps.
static MOVES: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Serialize, Type)]
pub struct MeetingPanelState {
    pub expanded: bool,
}

pub fn create(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        use tauri_nspanel::{CollectionBehavior, PanelBuilder, PanelLevel, StyleMask};
        match PanelBuilder::<_, MeetingPanel>::new(app, LABEL)
            .url(tauri::WebviewUrl::App(
                "src/meeting-panel/index.html".into(),
            ))
            .title("Meeting")
            .level(PanelLevel::Floating)
            .size(tauri::Size::Logical(tauri::LogicalSize {
                width: EXPANDED.0,
                height: EXPANDED.1,
            }))
            .has_shadow(false)
            .transparent(true)
            .no_activate(true)
            .corner_radius(0.0)
            .style_mask(StyleMask::empty().borderless().nonactivating_panel())
            .with_window(|w| w.decorations(false).transparent(true).focusable(true))
            .collection_behavior(
                CollectionBehavior::new()
                    .can_join_all_spaces()
                    .full_screen_auxiliary(),
            )
            .build()
        {
            Ok(panel) => {
                panel.hide();
                if let Some(window) = app.get_webview_window(LABEL) {
                    let w = window.clone();
                    window.on_window_event(move |event| {
                        if let tauri::WindowEvent::Moved(_) = event {
                            keep_tab_on_edge(&w);
                            snap_when_dropped(&w);
                        }
                    });
                }
                if crate::settings::get_settings(app).hide_from_screen_share {
                    if let Some(window) = app.get_webview_window(LABEL) {
                        let _ = window.set_content_protected(true);
                    }
                }
            }
            Err(e) => log::error!("Couldn't create the meeting panel: {e}"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// Where the panel goes the first time: the top right of the screen the
/// mouse is on.
fn first_place(app: &AppHandle, width: f64) -> Option<(f64, f64)> {
    let monitor = crate::overlay::get_monitor_with_cursor(app)?;
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let x = (area.position.x as f64 + area.size.width as f64) / scale - width - EDGE;
    let y = area.position.y as f64 / scale + TOP;
    Some((x, y))
}

/// The usable area (logical x, y, w, h) of the screen the window is on.
fn screen_of(window: &tauri::WebviewWindow) -> Option<(f64, f64, f64, f64)> {
    let monitor = window.current_monitor().ok().flatten()?;
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    Some((
        area.position.x as f64 / scale,
        area.position.y as f64 / scale,
        area.size.width as f64 / scale,
        area.size.height as f64 / scale,
    ))
}

/// Size the window for the panel or the tab, against the right edge of its
/// screen, keeping its height on screen where it was.
fn fit(window: &tauri::WebviewWindow, expanded: bool) {
    let (width, height) = if expanded { EXPANDED } else { COLLAPSED };
    let scale = window.scale_factor().unwrap_or(1.0);
    if let (Some((sx, sy, sw, sh)), Ok(pos)) = (screen_of(window), window.outer_position()) {
        let x = if expanded {
            sx + sw - width - EDGE
        } else {
            sx + sw - width
        };
        let top = pos.y as f64 / scale;
        let y = top.clamp(sy, (sy + sh - height).max(sy));
        set_frame(window, (x, y, width, height), false);
    } else {
        let _ = window.set_size(tauri::LogicalSize::new(width, height));
    }
}

/// Move and size the window in one step (logical, from the top left), so
/// it doesn't jump twice; `animate` slides it there.
fn set_frame(window: &tauri::WebviewWindow, (x, y, w, h): (f64, f64, f64, f64), animate: bool) {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::{NSScreen, NSWindow};
        use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
        if let (Some(mtm), Ok(ns)) = (MainThreadMarker::new(), window.ns_window()) {
            // Cocoa counts up from the bottom of the main screen.
            if let Some(main) = NSScreen::screens(mtm).firstObject() {
                let bottom = main.frame().size.height - y - h;
                let rect = NSRect::new(NSPoint::new(x, bottom), NSSize::new(w, h));
                // SAFETY: Tauri's NSWindow for this webview, on the main thread.
                let ns: &NSWindow = unsafe { &*(ns as *const NSWindow) };
                ns.setFrame_display_animate(rect, true, animate);
                return;
            }
        }
    }
    let _ = animate;
    let _ = window.set_position(tauri::LogicalPosition::new(x, y));
    let _ = window.set_size(tauri::LogicalSize::new(w, h));
}

/// Where a panel at `x` (logical, `width` wide) snaps to on a screen from
/// `sx`, `sw` wide: flush with a side it's near, else nowhere.
fn snapped_x(x: f64, width: f64, sx: f64, sw: f64) -> Option<f64> {
    let left = sx + EDGE;
    let right = sx + sw - width - EDGE;
    [left, right]
        .into_iter()
        .find(|to| (x - to).abs() <= SNAP && (x - to).abs() > 0.5)
}

/// Once a drag of the open panel ends near a side, slide it flush.
fn snap_when_dropped(window: &tauri::WebviewWindow) {
    if !EXPANDED_NOW.load(Ordering::SeqCst) {
        return;
    }
    let this = MOVES.fetch_add(1, Ordering::SeqCst) + 1;
    let window = window.clone();
    std::thread::spawn(move || {
        std::thread::sleep(SNAP_AFTER);
        if MOVES.load(Ordering::SeqCst) != this {
            return;
        }
        let w = window.clone();
        let _ = window.run_on_main_thread(move || {
            if !EXPANDED_NOW.load(Ordering::SeqCst) || mouse_down() {
                // Still being dragged: the next move tries again.
                return;
            }
            let scale = w.scale_factor().unwrap_or(1.0);
            let (Some((sx, _, sw, _)), Ok(pos), Ok(size)) =
                (screen_of(&w), w.outer_position(), w.outer_size())
            else {
                return;
            };
            let (x, y) = (pos.x as f64 / scale, pos.y as f64 / scale);
            let (width, height) = (size.width as f64 / scale, size.height as f64 / scale);
            if let Some(to) = snapped_x(x, width, sx, sw) {
                set_frame(&w, (to, y, width, height), true);
            }
        });
    });
}

/// Whether a mouse button is held (a drag still going).
fn mouse_down() -> bool {
    #[cfg(target_os = "macos")]
    {
        objc2_app_kit::NSEvent::pressedMouseButtons() != 0
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// Dragging the tab only moves it up and down the edge.
fn keep_tab_on_edge(window: &tauri::WebviewWindow) {
    if EXPANDED_NOW.load(Ordering::SeqCst) {
        return;
    }
    let scale = window.scale_factor().unwrap_or(1.0);
    let (Some((sx, _, sw, _)), Ok(pos)) = (screen_of(window), window.outer_position()) else {
        return;
    };
    let x = sx + sw - COLLAPSED.0;
    if (pos.x as f64 / scale - x).abs() > 0.5 {
        let _ = window.set_position(tauri::LogicalPosition::new(x, pos.y as f64 / scale));
    }
}

fn emit_state(app: &AppHandle) {
    let _ = app.emit_to(
        LABEL,
        "meeting-panel",
        MeetingPanelState {
            expanded: EXPANDED_NOW.load(Ordering::SeqCst),
        },
    );
}

/// A recording started or resumed: open the panel, expanded.
pub fn show(app: &AppHandle) {
    if !crate::settings::get_settings(app).meeting_panel {
        return;
    }
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window(LABEL) else {
            return;
        };
        if !PLACED.swap(true, Ordering::SeqCst) {
            EXPANDED_NOW.store(true, Ordering::SeqCst);
            if let Some((x, y)) = first_place(&handle, EXPANDED.0) {
                let _ = window.set_position(tauri::LogicalPosition::new(x, y));
            }
            let _ = window.set_size(tauri::LogicalSize::new(EXPANDED.0, EXPANDED.1));
        } else if !EXPANDED_NOW.swap(true, Ordering::SeqCst) {
            fit(&window, true);
        }
        emit_state(&handle);
        // In front, but without taking the keyboard from the app in use
        // (a plain `show` would make it the key window).
        #[cfg(target_os = "macos")]
        {
            use tauri_nspanel::ManagerExt;
            match handle.get_webview_panel(LABEL) {
                Ok(panel) => panel.show(),
                Err(_) => {
                    let _ = window.show();
                }
            }
        }
        #[cfg(not(target_os = "macos"))]
        let _ = window.show();
    });
}

/// The recording stopped.
pub fn hide(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(window) = handle.get_webview_window(LABEL) {
            let _ = window.hide();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snaps_to_a_near_side_only() {
        // A 1440-wide screen, a 340-wide panel: flush right is x = 1084.
        assert_eq!(snapped_x(1060.0, 340.0, 0.0, 1440.0), Some(1084.0));
        assert_eq!(snapped_x(40.0, 340.0, 0.0, 1440.0), Some(EDGE));
        assert_eq!(snapped_x(600.0, 340.0, 0.0, 1440.0), None);
        // Already there.
        assert_eq!(snapped_x(1084.0, 340.0, 0.0, 1440.0), None);
        // A second screen to the right.
        assert_eq!(snapped_x(1470.0, 340.0, 1440.0, 1920.0), Some(1456.0));
    }
}

#[tauri::command]
#[specta::specta]
pub fn get_meeting_panel_state() -> MeetingPanelState {
    MeetingPanelState {
        expanded: EXPANDED_NOW.load(Ordering::SeqCst),
    }
}

/// Fold the panel into the tab on the edge, or open it again.
#[tauri::command]
#[specta::specta]
pub fn set_meeting_panel_expanded(app: AppHandle, expanded: bool) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window(LABEL) else {
            return;
        };
        EXPANDED_NOW.store(expanded, Ordering::SeqCst);
        // Resized first, so the page animates at its new size.
        fit(&window, expanded);
        emit_state(&handle);
    });
}

/// How loud the meeting is right now, 0 to 1, for the tab's waves.
#[tauri::command]
#[specta::specta]
pub fn meeting_level(app: AppHandle) -> f32 {
    app.try_state::<std::sync::Arc<crate::meetings::manager::MeetingManager>>()
        .and_then(|m| m.level())
        .unwrap_or(0.0)
}

/// Open the Meetings page in the settings window.
#[tauri::command]
#[specta::specta]
pub fn open_meetings_page(app: AppHandle) {
    crate::notices::open_settings(&app, "meetings");
}
