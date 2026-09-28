//! The live draft as a bubble Felix draws itself: a see-through, click-through
//! window by the text cursor, or just above the recording pill when the app
//! doesn't say where the cursor is. It follows the cursor if the user clicks
//! elsewhere mid-dictation. Nothing is typed into the field and no input
//! source changes, so it can't leave anything behind.

use crate::text_field::{ScreenRect, TypingSpot};
use serde::Serialize;
use specta::Type;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

const LABEL: &str = "draft_bubble";
/// The widest the bubble gets (the page caps it), for keeping it on screen.
const MAX_BUBBLE_WIDTH: f64 = 420.0;
/// Space kept from the screen's edges.
const MARGIN: f64 = 8.0;
/// Space between the bubble and the cursor line or the pill.
const GAP: f64 = 6.0;
/// How far left of the cursor the bubble starts, so its text lines up.
const LEFT_INSET: f64 = 14.0;
/// The pill's card is this much shorter than its window, with the gap on
/// the side away from the screen edge.
const PILL_SLACK: f64 = 10.0;
/// How often the cursor is looked up.
const FOLLOW_EVERY: Duration = Duration::from_millis(120);
/// Moves smaller than this aren't worth doing (cursor jitter).
const MIN_MOVE: f64 = 3.0;

#[cfg(target_os = "macos")]
tauri_nspanel::tauri_panel! {
    panel!(DraftBubblePanel {
        config: {
            can_become_key_window: false,
            is_floating_panel: true
        }
    })
}

/// Which side of the window the bubble hugs, for the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum BubbleAnchor {
    /// Bubble at the bottom of the window, above the cursor or pill.
    Above,
    /// Bubble at the top of the window, below them.
    Below,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum BubbleAlign {
    /// Starts at the cursor.
    Start,
    /// Centred over the pill.
    Center,
}

/// Where the bubble goes, in points from the top-left of its screen (the
/// bubble window covers the screen). `x`/`y` is the bubble's corner or edge
/// middle nearest the cursor, as `anchor` and `align` say; the page glides
/// it there.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Type)]
pub struct BubblePlace {
    pub x: f64,
    pub y: f64,
    pub anchor: BubbleAnchor,
    pub align: BubbleAlign,
    /// Jump rather than glide (first showing, or another screen).
    pub instant: bool,
}

/// Where the bubble goes relative to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Target {
    Spot(TypingSpot),
    /// The recording pill's window; `at_top` when the pill sits at the top.
    Pill {
        rect: ScreenRect,
        at_top: bool,
    },
}

/// Where the bubble goes on `screen` (global points) for this target, in
/// points relative to the screen.
pub fn place(target: Target, screen: ScreenRect) -> BubblePlace {
    // (x, y of the edge above, y of the edge below, align, prefer above)
    let (x, above, below, align, prefer_above) = match target {
        Target::Spot(TypingSpot::Caret(r)) => (
            r.x - LEFT_INSET,
            r.y - GAP,
            r.y + r.h + GAP,
            BubbleAlign::Start,
            true,
        ),
        // Only the field is known: above its top edge, where the bubble
        // doesn't cover what's being typed.
        Target::Spot(TypingSpot::Field(r)) => {
            (r.x, r.y - GAP, r.y + r.h + GAP, BubbleAlign::Start, true)
        }
        Target::Pill { rect, at_top } => (
            rect.x + rect.w / 2.0,
            // The card hugs the screen edge, leaving slack on the far side.
            rect.y + if at_top { 0.0 } else { PILL_SLACK } - GAP,
            rect.y + rect.h - if at_top { PILL_SLACK } else { 0.0 } + GAP,
            BubbleAlign::Center,
            !at_top,
        ),
    };
    // Room for a two-line bubble on either side.
    let room = 64.0;
    let fits_above = above - room >= screen.y;
    let fits_below = below + room <= screen.y + screen.h;
    let is_above = if prefer_above {
        fits_above || !fits_below
    } else {
        !fits_below && fits_above
    };
    let y = if is_above { above } else { below };
    let (lo, hi) = match align {
        BubbleAlign::Start => (
            screen.x + MARGIN,
            screen.x + screen.w - MAX_BUBBLE_WIDTH - MARGIN,
        ),
        BubbleAlign::Center => (
            screen.x + MARGIN + MAX_BUBBLE_WIDTH / 2.0,
            screen.x + screen.w - MARGIN - MAX_BUBBLE_WIDTH / 2.0,
        ),
    };
    let x = x.clamp(lo, hi.max(lo));
    BubblePlace {
        x: x - screen.x,
        y: y.clamp(screen.y, screen.y + screen.h) - screen.y,
        anchor: if is_above {
            BubbleAnchor::Above
        } else {
            BubbleAnchor::Below
        },
        align,
        instant: false,
    }
}

/// Create the (hidden) bubble window at startup.
pub fn create(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    {
        use tauri_nspanel::{CollectionBehavior, PanelBuilder, PanelLevel, StyleMask};
        match PanelBuilder::<_, DraftBubblePanel>::new(app, LABEL)
            .url(tauri::WebviewUrl::App("src/draft/index.html".into()))
            .title("Live draft")
            .level(PanelLevel::Status)
            .size(tauri::Size::Logical(tauri::LogicalSize {
                width: 800.0,
                height: 600.0,
            }))
            .has_shadow(false)
            .transparent(true)
            .no_activate(true)
            .corner_radius(0.0)
            .style_mask(StyleMask::empty().borderless().nonactivating_panel())
            .with_window(|w| w.decorations(false).transparent(true).focusable(false))
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
                    // Clicks and scrolling go to whatever is underneath.
                    let _ = window.set_ignore_cursor_events(true);
                    if crate::settings::get_settings(app).hide_from_screen_share {
                        let _ = window.set_content_protected(true);
                    }
                }
            }
            Err(e) => log::error!("Couldn't create the live draft bubble: {e}"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// Bumped by every `show`/`hide`, so a follower from an earlier dictation
/// stops, and a late text update can't bring the bubble back.
static GENERATION: AtomicU64 = AtomicU64::new(0);
static VISIBLE: AtomicBool = AtomicBool::new(false);

/// Start a bubble for the dictation that's starting. It appears with the
/// first text.
pub fn begin(app: &AppHandle) {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    VISIBLE.store(false, Ordering::SeqCst);
    let _ = app.emit_to(LABEL, "draft-text", "");
    spawn_follower(app, generation);
}

/// Show this text in the bubble.
pub fn set_text(app: &AppHandle, text: &str) {
    let generation = GENERATION.load(Ordering::SeqCst);
    let _ = app.emit_to(LABEL, "draft-text", text);
    if !text.is_empty() && !VISIBLE.swap(true, Ordering::SeqCst) {
        let target = target_now(app);
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || {
            if GENERATION.load(Ordering::SeqCst) != generation {
                return;
            }
            if let Some(window) = app2.get_webview_window(LABEL) {
                if let Some(target) = target {
                    LAST.lock().unwrap().take();
                    move_to(&app2, &window, target);
                }
                let _ = window.set_ignore_cursor_events(true);
                let _ = window.show();
            }
        });
    }
}

/// Take the bubble away (the dictation stopped).
pub fn end(app: &AppHandle) {
    GENERATION.fetch_add(1, Ordering::SeqCst);
    if !VISIBLE.swap(false, Ordering::SeqCst) {
        return;
    }
    let _ = app.emit_to(LABEL, "draft-text", "");
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.hide();
    }
}

fn pill_target(app: &AppHandle) -> Option<Target> {
    let window = app.get_webview_window("recording_overlay")?;
    let scale = window.scale_factor().ok()?;
    let pos = window.outer_position().ok()?;
    let size = window.outer_size().ok()?;
    Some(Target::Pill {
        rect: ScreenRect {
            x: pos.x as f64 / scale,
            y: pos.y as f64 / scale,
            w: size.width as f64 / scale,
            h: size.height as f64 / scale,
        },
        at_top: crate::settings::get_settings(app).overlay_position
            == crate::settings::OverlayPosition::Top,
    })
}

/// The screen (in points) holding this point, or the first one.
fn screen_at(app: &AppHandle, x: f64, y: f64) -> Option<ScreenRect> {
    let monitors = app.available_monitors().ok()?;
    let rects: Vec<ScreenRect> = monitors
        .iter()
        .map(|m| {
            let s = m.scale_factor();
            ScreenRect {
                x: m.position().x as f64 / s,
                y: m.position().y as f64 / s,
                w: m.size().width as f64 / s,
                h: m.size().height as f64 / s,
            }
        })
        .collect();
    rects
        .iter()
        .find(|r| x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h)
        .or(rects.first())
        .copied()
}

fn target_now(app: &AppHandle) -> Option<Target> {
    crate::text_field::typing_spot()
        .map(Target::Spot)
        .or_else(|| pill_target(app))
}

/// The screen the bubble is on and where it was last sent.
static LAST: std::sync::Mutex<Option<(ScreenRect, BubblePlace)>> = std::sync::Mutex::new(None);

/// Glide the bubble to `target`: cover the target's screen with the window
/// (moving it only when the screen changes) and tell the page where to go.
fn move_to(app: &AppHandle, window: &tauri::WebviewWindow, target: Target) {
    let (px, py) = match target {
        Target::Spot(TypingSpot::Caret(r) | TypingSpot::Field(r)) => (r.x, r.y),
        Target::Pill { rect, .. } => (rect.x + rect.w / 2.0, rect.y),
    };
    let Some(screen) = screen_at(app, px, py) else {
        return;
    };
    let mut place = place(target, screen);
    let mut last = LAST.lock().unwrap();
    match *last {
        Some((s, p))
            if s == screen
                && (p.x - place.x).abs() < MIN_MOVE
                && (p.y - place.y).abs() < MIN_MOVE
                && p.anchor == place.anchor
                && p.align == place.align =>
        {
            return;
        }
        Some((s, _)) if s == screen => {}
        _ => {
            // First showing, or another screen: cover it and jump.
            let _ = window.set_position(tauri::Position::Logical(tauri::LogicalPosition {
                x: screen.x,
                y: screen.y,
            }));
            let _ = window.set_size(tauri::Size::Logical(tauri::LogicalSize {
                width: screen.w,
                height: screen.h,
            }));
            place.instant = true;
        }
    }
    let _ = app.emit_to(LABEL, "draft-place", place);
    *last = Some((screen, place));
}

/// Keep the bubble by the cursor while this dictation lasts.
fn spawn_follower(app: &AppHandle, generation: u64) {
    let app = app.clone();
    std::thread::spawn(move || {
        while GENERATION.load(Ordering::SeqCst) == generation {
            std::thread::sleep(FOLLOW_EVERY);
            if !VISIBLE.load(Ordering::SeqCst) {
                continue;
            }
            // Looking up the cursor asks the app over Accessibility: done
            // here, off the main thread; only the window work runs there.
            let Some(target) = target_now(&app) else {
                continue;
            };
            let app2 = app.clone();
            let _ = app.run_on_main_thread(move || {
                if GENERATION.load(Ordering::SeqCst) != generation {
                    return;
                }
                if let Some(window) = app2.get_webview_window(LABEL) {
                    move_to(&app2, &window, target);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: ScreenRect = ScreenRect {
        x: 0.0,
        y: 0.0,
        w: 1512.0,
        h: 982.0,
    };

    #[test]
    fn the_bubble_sits_just_above_the_cursor() {
        let caret = ScreenRect {
            x: 300.0,
            y: 500.0,
            w: 1.0,
            h: 18.0,
        };
        let p = place(Target::Spot(TypingSpot::Caret(caret)), SCREEN);
        assert_eq!(p.x, 300.0 - LEFT_INSET);
        assert_eq!(p.y, 500.0 - GAP);
        assert_eq!(p.anchor, BubbleAnchor::Above);
        assert_eq!(p.align, BubbleAlign::Start);
    }

    #[test]
    fn near_the_top_of_the_screen_it_goes_below_and_stays_on_screen() {
        let caret = ScreenRect {
            x: 1500.0,
            y: 40.0,
            w: 1.0,
            h: 18.0,
        };
        let p = place(Target::Spot(TypingSpot::Caret(caret)), SCREEN);
        assert_eq!(p.anchor, BubbleAnchor::Below);
        assert_eq!(p.y, 40.0 + 18.0 + GAP);
        assert_eq!(p.x, SCREEN.w - MAX_BUBBLE_WIDTH - MARGIN);
    }

    #[test]
    fn without_a_cursor_it_sits_on_the_pill() {
        let pill = ScreenRect {
            x: 628.0,
            y: 917.0,
            w: 256.0,
            h: 50.0,
        };
        let p = place(
            Target::Pill {
                rect: pill,
                at_top: false,
            },
            SCREEN,
        );
        assert_eq!(p.x, 628.0 + 128.0);
        assert_eq!(p.y, 917.0 + PILL_SLACK - GAP);
        assert_eq!(p.anchor, BubbleAnchor::Above);
        assert_eq!(p.align, BubbleAlign::Center);
        // A pill at the top gets the bubble just below it.
        let top = ScreenRect { y: 46.0, ..pill };
        let p = place(
            Target::Pill {
                rect: top,
                at_top: true,
            },
            SCREEN,
        );
        assert_eq!(p.anchor, BubbleAnchor::Below);
        assert_eq!(p.y, 46.0 + 50.0 - PILL_SLACK + GAP);
    }

    #[test]
    fn places_are_relative_to_their_screen() {
        let second = ScreenRect {
            x: 1512.0,
            y: -200.0,
            w: 1920.0,
            h: 1080.0,
        };
        let caret = ScreenRect {
            x: 2000.0,
            y: 300.0,
            w: 1.0,
            h: 18.0,
        };
        let p = place(Target::Spot(TypingSpot::Caret(caret)), second);
        assert_eq!(p.x, 2000.0 - LEFT_INSET - 1512.0);
        assert_eq!(p.y, 300.0 - GAP + 200.0);
    }
}
