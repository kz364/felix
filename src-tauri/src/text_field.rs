//! The focused text field, via the macOS Accessibility API: its text and
//! selection before a dictation is pasted and after. Used to
//!
//! * adapt the pasted text to what's around the cursor (spacing, casing), and
//! * record exactly which span a dictation inserted, as the basis for undo
//!   and voice editing of just that span.
//!
//! Works for native text fields and most web/Electron fields that expose
//! `AXValue`; anything else (terminals, canvas editors, secure fields) yields
//! no snapshot and dictation behaves as before. Offsets are UTF-16 code
//! units, as Accessibility reports them.

/// What the focused field looked like at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldSnapshot {
    pub pid: i32,
    pub role: String,
    /// Full text of the field, UTF-16 (Accessibility offsets index this).
    pub value: Vec<u16>,
    /// Selection as (location, length) in UTF-16 units; length 0 = caret.
    pub selection: Option<(usize, usize)>,
}

impl FieldSnapshot {
    /// Up to `max` characters before the selection start.
    pub fn text_before_cursor(&self, max: usize) -> String {
        let end = self
            .selection
            .map_or(self.value.len(), |(loc, _)| loc.min(self.value.len()));
        let text = String::from_utf16_lossy(&self.value[..end]);
        let chars: Vec<char> = text.chars().collect();
        chars[chars.len().saturating_sub(max)..].iter().collect()
    }

    /// Up to `max` characters after the selection end.
    pub fn text_after_cursor(&self, max: usize) -> String {
        let start = self.selection.map_or(self.value.len(), |(loc, len)| {
            (loc + len).min(self.value.len())
        });
        String::from_utf16_lossy(&self.value[start..])
            .chars()
            .take(max)
            .collect()
    }
}

/// Where a dictation landed, from the field's before/after text: the common
/// prefix and suffix are unchanged, what's between is the insertion. Returns
/// (start, length) in UTF-16 units of the after-text.
pub fn inserted_span(before: &[u16], after: &[u16]) -> Option<(usize, usize)> {
    let prefix = before.iter().zip(after).take_while(|(a, b)| a == b).count();
    let max_suffix = before.len().min(after.len()) - prefix;
    let suffix = before
        .iter()
        .rev()
        .zip(after.iter().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    let len = after.len().checked_sub(prefix + suffix)?;
    (len > 0).then_some((prefix, len))
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '\'' || c == '’'
}

/// Adjust dictated text to the characters around the cursor:
///
/// * add a leading space after a word or closing punctuation;
/// * add a trailing space when a word follows directly;
/// * mid-sentence (previous text doesn't end a sentence), lowercase the
///   first word unless it is "I", an acronym or a vocabulary term.
pub fn adapt_to_context(text: &str, before: &str, after: &str, vocabulary: &[String]) -> String {
    if text.is_empty() {
        return String::new();
    }
    let mut out = text.to_string();

    let prev = before.chars().last();
    let prev_non_space = before.trim_end().chars().last();
    let mid_sentence = matches!(prev_non_space, Some(c) if is_word_char(c) || c == ',' || c == ';' || c == ':' || c == '-' || c == '—');

    if mid_sentence {
        let first_word: String = out.chars().take_while(|c| is_word_char(*c)).collect();
        let keep = first_word == "I"
            || first_word.starts_with("I'")
            || first_word.starts_with("I’")
            || first_word.chars().skip(1).any(char::is_uppercase)
            || vocabulary
                .iter()
                .any(|v| v.split_whitespace().next() == Some(first_word.as_str()));
        if !keep {
            let mut chars = out.chars();
            if let Some(first) = chars.next() {
                out = first.to_lowercase().chain(chars).collect();
            }
        }
    }

    let starts_with_word = out.chars().next().is_some_and(is_word_char);
    if let Some(p) = prev {
        let wants_space = is_word_char(p)
            || matches!(
                p,
                '.' | ',' | '!' | '?' | ';' | ':' | ')' | ']' | '}' | '"' | '”'
            );
        if wants_space && starts_with_word {
            out.insert(0, ' ');
        }
    }

    let ends_with_word_or_punct = out.chars().last().is_some_and(|c| !c.is_whitespace());
    if after.chars().next().is_some_and(is_word_char) && ends_with_word_or_punct {
        out.push(' ');
    }
    out
}

/// The last dictation pasted, when casual style took its final period off.
struct LastPaste {
    pid: i32,
    text: String,
}

static LAST_PASTE: std::sync::Mutex<Option<LastPaste>> = std::sync::Mutex::new(None);

/// Remember what was just pasted, if its period was dropped; anything else
/// forgets the last paste.
pub fn remember_paste(pid: Option<i32>, pasted: &str, period_dropped: bool) {
    let last = match pid {
        Some(pid) if period_dropped && !pasted.trim().is_empty() => Some(LastPaste {
            pid,
            text: pasted.to_string(),
        }),
        _ => None,
    };
    *LAST_PASTE.lock().unwrap_or_else(|e| e.into_inner()) = last;
}

/// Whether `text` continues, as a new sentence, right after the last
/// dictation whose period was dropped, so the period should come back.
pub fn continues_last_paste(field: &FieldSnapshot, text: &str) -> bool {
    let last = LAST_PASTE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(last) = last.as_ref().filter(|l| l.pid == field.pid) else {
        return false;
    };
    let before = field.text_before_cursor(last.text.chars().count() + 2);
    new_sentence_after(&before, &last.text, text)
}

/// Words that carry on the previous sentence rather than start one.
const CONTINUATIONS: &[&str] = &[
    "and", "but", "or", "nor", "because", "which", "who", "whose", "where", "then", "plus", "than",
    "until", "unless", "while", "whereas", "though", "although", "if",
];

/// `before` (the text before the cursor) ends with the last pasted dictation
/// and `text` starts a new sentence: capitalised, not a continuing word.
fn new_sentence_after(before: &str, last: &str, text: &str) -> bool {
    let last = last.trim();
    if !before.trim_end().ends_with(last) || before.len() - before.trim_end().len() > 1 {
        return false;
    }
    if !last.chars().last().is_some_and(char::is_alphanumeric) {
        return false;
    }
    let first_word: String = text
        .trim_start()
        .chars()
        .take_while(|c| is_word_char(*c))
        .collect();
    first_word.chars().next().is_some_and(char::is_uppercase)
        && !CONTINUATIONS.contains(&first_word.to_lowercase().as_str())
}

/// Whether the focused element can take pasted text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteTarget {
    /// A text field (or something that says it is editable).
    Text,
    /// Confidently nothing to type into: no focused element in an app with
    /// reliable Accessibility, or a list, button, page body and the like.
    NoText,
    /// Can't tell; paste as usual.
    Unknown,
}

/// Apps that take typed text without exposing a text element: terminals
/// drawn on the GPU, remote desktops, VMs, editors with custom views and JVM
/// apps. Never classified as `NoText`.
const OPAQUE_APPS: &[&str] = &[
    "io.alacritty",
    "org.alacritty",
    "net.kovidgoyal.kitty",
    "com.github.wez.wezterm",
    "com.mitchellh.ghostty",
    "dev.warp.Warp-Stable",
    "co.zeit.hyper",
    "com.googlecode.iterm2",
    "dev.zed.Zed",
    "com.sublimetext.4",
    "com.sublimetext.3",
    "org.gnu.Emacs",
    "org.vim.MacVim",
    "com.neovide.neovide",
    "com.apple.ScreenSharing",
    "com.microsoft.rdc.macos",
    "com.parallels.desktop.console",
    "com.utmapp.UTM",
    "com.vmware.fusion",
];

const OPAQUE_PREFIXES: &[&str] = &["com.jetbrains.", "com.google.android.studio"];

/// Roles that are clearly not a place to type.
const NON_TEXT_ROLES: &[&str] = &[
    "AXList",
    "AXOutline",
    "AXTable",
    "AXBrowser",
    "AXRow",
    "AXCell",
    "AXButton",
    "AXCheckBox",
    "AXRadioButton",
    "AXPopUpButton",
    "AXMenuButton",
    "AXSlider",
    "AXImage",
    "AXLink",
    "AXStaticText",
    "AXToolbar",
    "AXTabGroup",
    "AXWebArea",
    "AXWindow",
];

fn is_opaque_app(bundle_id: &str) -> bool {
    OPAQUE_APPS.contains(&bundle_id) || OPAQUE_PREFIXES.iter().any(|p| bundle_id.starts_with(p))
}

/// What Accessibility said about the focused element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusProbe {
    /// The app reports no focused element.
    NoFocus,
    /// The query failed (permission, timeout, app not answering).
    Failed,
    Element {
        role: String,
        /// `AXEditable`, or `AXValue` being settable.
        editable: bool,
    },
}

/// Classify a paste target. Deliberately one-sided: anything doubtful is
/// `Unknown` so the paste goes ahead as before.
///
/// `lazy_ax` marks apps that build their Accessibility tree on demand
/// (Chromium, Electron, Firefox): there only a focused web page body is
/// trusted, since it shows the tree is live.
pub fn classify_target(bundle_id: &str, lazy_ax: bool, probe: &FocusProbe) -> PasteTarget {
    if let FocusProbe::Element { role, editable } = probe {
        if TEXT_ROLES.contains(&role.as_str()) || *editable {
            return PasteTarget::Text;
        }
    }
    if is_opaque_app(bundle_id) {
        return PasteTarget::Unknown;
    }
    if lazy_ax {
        return match probe {
            FocusProbe::Element { role, .. } if role == "AXWebArea" => PasteTarget::NoText,
            _ => PasteTarget::Unknown,
        };
    }
    match probe {
        FocusProbe::NoFocus => PasteTarget::NoText,
        FocusProbe::Failed => PasteTarget::Unknown,
        FocusProbe::Element { role, .. } => {
            if NON_TEXT_ROLES.contains(&role.as_str())
                || (bundle_id == "com.apple.finder" && !role.is_empty())
            {
                PasteTarget::NoText
            } else {
                PasteTarget::Unknown
            }
        }
    }
}

pub(crate) const TEXT_ROLES: &[&str] =
    &["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"];

/// A rectangle on screen in points, top-left origin (as Accessibility and
/// Tauri's logical positions both use on macOS).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// Where typing goes in the focused field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TypingSpot {
    /// The text cursor itself.
    Caret(ScreenRect),
    /// Only the field is known (apps that don't report the cursor).
    Field(ScreenRect),
}

/// Caret rectangles apps report that don't make sense: nothing, or the
/// placeholder (0, 0) some Electron apps give.
fn plausible_caret(r: ScreenRect) -> bool {
    r.h > 2.0 && r.h < 200.0 && r.w < 400.0 && !(r.x == 0.0 && r.y == 0.0)
}

/// A field frame worth pointing at: not the whole window of a web view.
fn plausible_field(r: ScreenRect) -> bool {
    r.w > 10.0 && r.h > 8.0 && r.h < 400.0 && !(r.x == 0.0 && r.y == 0.0)
}

#[cfg(target_os = "macos")]
mod ax {
    use super::FieldSnapshot;
    use core_foundation::base::{CFGetTypeID, CFRelease, CFTypeRef, TCFType};
    use core_foundation::string::{CFString, CFStringGetTypeID, CFStringRef};
    use std::ffi::c_void;

    type AXUIElementRef = *const c_void;
    type AXError = i32;
    const AX_SUCCESS: AXError = 0;
    const AX_VALUE_CF_RANGE: u32 = 4;
    const AX_VALUE_CG_POINT: u32 = 1;
    const AX_VALUE_CG_SIZE: u32 = 2;
    const AX_VALUE_CG_RECT: u32 = 3;

    #[repr(C)]
    #[derive(Default)]
    struct CGRect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct CGPair {
        a: f64,
        b: f64,
    }

    #[repr(C)]
    #[derive(Default)]
    struct CFRange {
        location: isize,
        length: isize,
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateSystemWide() -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AXError;
        fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, seconds: f32) -> AXError;
        fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> AXError;
        fn AXValueGetValue(value: CFTypeRef, kind: u32, out: *mut c_void) -> bool;
        fn AXUIElementCopyParameterizedAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            parameter: CFTypeRef,
            value: *mut CFTypeRef,
        ) -> AXError;
        fn AXUIElementSetAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: CFTypeRef,
        ) -> AXError;
        fn AXValueCreate(kind: u32, value: *const c_void) -> CFTypeRef;
        fn AXUIElementIsAttributeSettable(
            element: AXUIElementRef,
            attribute: CFStringRef,
            settable: *mut bool,
        ) -> AXError;
    }

    /// Owned CF reference, released on drop.
    struct Owned(CFTypeRef);
    impl Drop for Owned {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe { CFRelease(self.0) };
            }
        }
    }

    fn copy_attribute(element: AXUIElementRef, name: &str) -> Option<Owned> {
        let attribute = CFString::new(name);
        let mut value: CFTypeRef = std::ptr::null();
        // SAFETY: element is a live AXUIElement; value receives a +1 reference.
        let err = unsafe {
            AXUIElementCopyAttributeValue(element, attribute.as_concrete_TypeRef(), &mut value)
        };
        (err == AX_SUCCESS && !value.is_null()).then(|| Owned(value))
    }

    fn string_attribute(element: AXUIElementRef, name: &str) -> Option<String> {
        let value = copy_attribute(element, name)?;
        // SAFETY: type-checked before wrapping; wrap_under_get_rule retains.
        unsafe {
            (CFGetTypeID(value.0) == CFStringGetTypeID())
                .then(|| CFString::wrap_under_get_rule(value.0 as CFStringRef).to_string())
        }
    }

    use super::{plausible_caret, plausible_field, FocusProbe, ScreenRect, TypingSpot, TEXT_ROLES};

    /// Snapshot the system-wide focused element if it is an editable text
    /// field. `None` for secure fields, non-text focus, or apps that don't
    /// expose their text.
    pub fn focused_field() -> Option<FieldSnapshot> {
        // SAFETY: creates a +1 system-wide element, released by Owned.
        let system = Owned(unsafe { AXUIElementCreateSystemWide() });
        unsafe { AXUIElementSetMessagingTimeout(system.0, 0.25) };
        let focused = copy_attribute(system.0, "AXFocusedUIElement")?;
        unsafe { AXUIElementSetMessagingTimeout(focused.0, 0.25) };

        let role = string_attribute(focused.0, "AXRole").unwrap_or_default();
        let subrole = string_attribute(focused.0, "AXSubrole").unwrap_or_default();
        if subrole == "AXSecureTextField" || role == "AXSecureTextField" {
            return None;
        }
        if !TEXT_ROLES.contains(&role.as_str()) {
            return None;
        }
        let value = string_attribute(focused.0, "AXValue")?;

        let selection = copy_attribute(focused.0, "AXSelectedTextRange").and_then(|range| {
            let mut r = CFRange::default();
            // SAFETY: AXValueGetValue writes a CFRange for the CFRange type.
            let ok = unsafe {
                AXValueGetValue(
                    range.0,
                    AX_VALUE_CF_RANGE,
                    &mut r as *mut CFRange as *mut c_void,
                )
            };
            (ok && r.location >= 0 && r.length >= 0)
                .then(|| (r.location as usize, r.length as usize))
        });

        let mut pid = 0;
        unsafe { AXUIElementGetPid(focused.0, &mut pid) };
        Some(FieldSnapshot {
            pid,
            role,
            value: value.encode_utf16().collect(),
            selection,
        })
    }

    fn bounds_for(element: AXUIElementRef, location: isize, length: isize) -> Option<ScreenRect> {
        let range = CFRange { location, length };
        // SAFETY: AXValueCreate copies the CFRange; the +1 result is released by Owned.
        let param = Owned(unsafe {
            AXValueCreate(AX_VALUE_CF_RANGE, &range as *const CFRange as *const c_void)
        });
        if param.0.is_null() {
            return None;
        }
        let attribute = CFString::new("AXBoundsForRange");
        let mut value: CFTypeRef = std::ptr::null();
        // SAFETY: element and param are live; value receives a +1 reference.
        let err = unsafe {
            AXUIElementCopyParameterizedAttributeValue(
                element,
                attribute.as_concrete_TypeRef(),
                param.0,
                &mut value,
            )
        };
        if err != AX_SUCCESS || value.is_null() {
            return None;
        }
        let value = Owned(value);
        let mut r = CGRect::default();
        // SAFETY: AXValueGetValue writes a CGRect for the CGRect type.
        let ok = unsafe {
            AXValueGetValue(
                value.0,
                AX_VALUE_CG_RECT,
                &mut r as *mut CGRect as *mut c_void,
            )
        };
        ok.then_some(ScreenRect {
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        })
    }

    fn pair_attribute(element: AXUIElementRef, name: &str, kind: u32) -> Option<(f64, f64)> {
        let value = copy_attribute(element, name)?;
        let mut p = CGPair::default();
        // SAFETY: CGPoint and CGSize are both two f64s.
        let ok = unsafe { AXValueGetValue(value.0, kind, &mut p as *mut CGPair as *mut c_void) };
        ok.then_some((p.a, p.b))
    }

    /// Where the text cursor of the focused field is, or at least the field.
    pub fn typing_spot() -> Option<TypingSpot> {
        // SAFETY: creates a +1 system-wide element, released by Owned.
        let system = Owned(unsafe { AXUIElementCreateSystemWide() });
        unsafe { AXUIElementSetMessagingTimeout(system.0, 0.1) };
        let focused = copy_attribute(system.0, "AXFocusedUIElement")?;
        unsafe { AXUIElementSetMessagingTimeout(focused.0, 0.1) };
        let role = string_attribute(focused.0, "AXRole").unwrap_or_default();
        let subrole = string_attribute(focused.0, "AXSubrole").unwrap_or_default();
        if subrole == "AXSecureTextField" || role == "AXSecureTextField" {
            return None;
        }
        let editable = TEXT_ROLES.contains(&role.as_str())
            || bool_attribute(focused.0, "AXEditable").unwrap_or(false);
        if !editable {
            return None;
        }
        let selection = copy_attribute(focused.0, "AXSelectedTextRange").and_then(|range| {
            let mut r = CFRange::default();
            // SAFETY: AXValueGetValue writes a CFRange for the CFRange type.
            let ok = unsafe {
                AXValueGetValue(
                    range.0,
                    AX_VALUE_CF_RANGE,
                    &mut r as *mut CFRange as *mut c_void,
                )
            };
            (ok && r.location >= 0).then_some(r)
        });
        if let Some(sel) = selection {
            let at = sel.location + sel.length;
            // An empty range often has no bounds; the character before the
            // cursor does, and the cursor sits at its right edge.
            let caret = bounds_for(focused.0, at, 0)
                .filter(|r| plausible_caret(*r))
                .or_else(|| {
                    (at > 0)
                        .then(|| bounds_for(focused.0, at - 1, 1))
                        .flatten()
                        .filter(|r| plausible_caret(*r))
                        .map(|r| ScreenRect {
                            x: r.x + r.w,
                            w: 1.0,
                            ..r
                        })
                });
            if let Some(caret) = caret {
                return Some(TypingSpot::Caret(caret));
            }
        }
        let (x, y) = pair_attribute(focused.0, "AXPosition", AX_VALUE_CG_POINT)?;
        let (w, h) = pair_attribute(focused.0, "AXSize", AX_VALUE_CG_SIZE)?;
        let field = ScreenRect { x, y, w, h };
        plausible_field(field).then_some(TypingSpot::Field(field))
    }

    /// Select `(location, length)` (UTF-16 units) in the focused field.
    pub fn select_range(location: usize, length: usize) -> bool {
        // SAFETY: creates a +1 system-wide element, released by Owned.
        let system = Owned(unsafe { AXUIElementCreateSystemWide() });
        unsafe { AXUIElementSetMessagingTimeout(system.0, 0.25) };
        let Some(focused) = copy_attribute(system.0, "AXFocusedUIElement") else {
            return false;
        };
        let range = CFRange {
            location: location as isize,
            length: length as isize,
        };
        // SAFETY: AXValueCreate copies the CFRange; the +1 result is released by Owned.
        let value = Owned(unsafe {
            AXValueCreate(AX_VALUE_CF_RANGE, &range as *const CFRange as *const c_void)
        });
        if value.0.is_null() {
            return false;
        }
        let attribute = CFString::new("AXSelectedTextRange");
        // SAFETY: focused and value are live CF objects.
        let err = unsafe {
            AXUIElementSetAttributeValue(focused.0, attribute.as_concrete_TypeRef(), value.0)
        };
        err == AX_SUCCESS
    }

    const AX_ERROR_NO_VALUE: AXError = -25212;

    fn bool_attribute(element: AXUIElementRef, name: &str) -> Option<bool> {
        use core_foundation::boolean::{CFBoolean, CFBooleanGetTypeID};
        let value = copy_attribute(element, name)?;
        // SAFETY: type-checked before wrapping; wrap_under_get_rule retains.
        unsafe {
            (CFGetTypeID(value.0) == CFBooleanGetTypeID())
                .then(|| CFBoolean::wrap_under_get_rule(value.0 as _).into())
        }
    }

    fn is_settable(element: AXUIElementRef, name: &str) -> bool {
        let attribute = CFString::new(name);
        let mut settable = false;
        // SAFETY: element is live; settable is a valid out-pointer.
        let err = unsafe {
            AXUIElementIsAttributeSettable(element, attribute.as_concrete_TypeRef(), &mut settable)
        };
        err == AX_SUCCESS && settable
    }

    /// What the system-wide focused element is, for `classify_target`.
    pub fn probe_focus() -> FocusProbe {
        // SAFETY: creates a +1 system-wide element, released by Owned.
        let system = Owned(unsafe { AXUIElementCreateSystemWide() });
        unsafe { AXUIElementSetMessagingTimeout(system.0, 0.25) };
        let attribute = CFString::new("AXFocusedUIElement");
        let mut value: CFTypeRef = std::ptr::null();
        // SAFETY: value receives a +1 reference, released by Owned.
        let err = unsafe {
            AXUIElementCopyAttributeValue(system.0, attribute.as_concrete_TypeRef(), &mut value)
        };
        if err == AX_ERROR_NO_VALUE {
            return FocusProbe::NoFocus;
        }
        if err != AX_SUCCESS || value.is_null() {
            return FocusProbe::Failed;
        }
        let focused = Owned(value);
        unsafe { AXUIElementSetMessagingTimeout(focused.0, 0.25) };
        let role = string_attribute(focused.0, "AXRole").unwrap_or_default();
        let editable = bool_attribute(focused.0, "AXEditable").unwrap_or(false)
            || (!TEXT_ROLES.contains(&role.as_str())
                && role != "AXSlider"
                && role != "AXCheckBox"
                && is_settable(focused.0, "AXValue")
                && string_attribute(focused.0, "AXValue").is_some());
        FocusProbe::Element { role, editable }
    }
}

/// Snapshot of the focused text field, if it can be read.
pub fn focused_field() -> Option<FieldSnapshot> {
    #[cfg(target_os = "macos")]
    {
        ax::focused_field()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Where the user is typing, for drawing next to it.
pub fn typing_spot() -> Option<TypingSpot> {
    #[cfg(target_os = "macos")]
    {
        ax::typing_spot()
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

/// Put the cursor at the end of the focused field's text.
pub fn cursor_to_end() -> bool {
    #[cfg(target_os = "macos")]
    {
        ax::focused_field().is_some_and(|f| ax::select_range(f.value.len(), 0))
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Select all of the focused field's text, so the next paste replaces it.
/// Checks the field still holds `expected` (what was read before) and that
/// the selection took, so a changed or uncooperative field isn't clobbered.
pub fn select_whole_field(expected: &[u16]) -> bool {
    #[cfg(target_os = "macos")]
    {
        match ax::focused_field() {
            Some(field) if field.value == expected => {}
            _ => return false,
        }
        if !ax::select_range(0, expected.len()) {
            return false;
        }
        // Chromium and Electron apps (Slack) update the selection a moment
        // later, and may stop it before a trailing newline they keep in the
        // value. Accept a selection from the start over all the visible text.
        let visible = trimmed_len(expected);
        for _ in 0..10 {
            if let Some((0, len)) = ax::focused_field().and_then(|f| f.selection) {
                if len >= visible {
                    return true;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        false
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = expected;
        false
    }
}

/// Length of UTF-16 text without its trailing whitespace.
pub fn trimmed_len(text: &[u16]) -> usize {
    String::from_utf16_lossy(text)
        .trim_end()
        .encode_utf16()
        .count()
}

/// Chromium/Electron apps whose Accessibility tree Handy switched on, and
/// when. The tree takes a moment to build; after that the app reports its
/// focus like a native one.
static WOKEN_TREES: once_cell::sync::Lazy<
    std::sync::Mutex<std::collections::HashMap<i32, std::time::Instant>>,
> = once_cell::sync::Lazy::new(Default::default);

/// How long after switching the tree on its focus reports are trusted.
const TREE_BUILD_TIME: std::time::Duration = std::time::Duration::from_millis(700);

/// Ask a Chromium/Electron app to build its Accessibility tree (it stays on
/// while the app runs).
pub fn wake_tree(pid: i32) {
    {
        let mut woken = WOKEN_TREES.lock().unwrap_or_else(|e| e.into_inner());
        if woken.contains_key(&pid) {
            return;
        }
        woken.insert(pid, std::time::Instant::now());
    }
    crate::ax_tree::expose_electron_tree(pid);
    log::debug!("Switched on the Accessibility tree of pid {pid}");
}

/// If Handy has just switched on this app's tree, wait (at most the build
/// time) until it's built, so reading it finds the content.
pub fn wait_for_tree(pid: i32) {
    let woken = WOKEN_TREES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&pid)
        .copied();
    if let Some(at) = woken {
        if let Some(left) = TREE_BUILD_TIME.checked_sub(at.elapsed()) {
            std::thread::sleep(left);
        }
    }
}

/// Whether Handy switched on this app's tree long enough ago to trust it.
fn tree_is_live(pid: i32) -> bool {
    WOKEN_TREES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&pid)
        .is_some_and(|at| at.elapsed() >= TREE_BUILD_TIME)
}

/// Whether the frontmost app's focused element can take pasted text.
pub fn paste_target() -> PasteTarget {
    #[cfg(target_os = "macos")]
    {
        let (bundle_id, lazy_ax) = crate::app_context::frontmost_bundle();
        let live = lazy_ax && crate::app_context::frontmost_pid().is_some_and(tree_is_live);
        let probe = ax::probe_focus();
        let target = classify_target(bundle_id.as_deref().unwrap_or(""), lazy_ax && !live, &probe);
        log::debug!(
            "Paste target in {:?} (tree live: {}): {:?} -> {:?}",
            bundle_id,
            live,
            probe,
            target
        );
        target
    }
    #[cfg(not(target_os = "macos"))]
    {
        PasteTarget::Unknown
    }
}

/// Paste target when Felix itself is in front, else `None`. Must run off
/// the main thread: Felix's main thread answers the Accessibility query, so
/// asking from it times out. In Felix only a text box takes a paste; anything
/// else (a button, the page, nothing) shows the dictation instead.
pub fn own_app_paste_target() -> Option<PasteTarget> {
    #[cfg(target_os = "macos")]
    {
        if crate::app_context::frontmost_pid()? != std::process::id() as i32 {
            return None;
        }
        let probe = ax::probe_focus();
        let target = own_app_target(&probe);
        log::debug!("Paste target in Felix: {probe:?} -> {target:?}");
        Some(target)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn own_app_target(probe: &FocusProbe) -> PasteTarget {
    match probe {
        FocusProbe::Element { role, editable }
            if *editable || TEXT_ROLES.contains(&role.as_str()) =>
        {
            PasteTarget::Text
        }
        _ => PasteTarget::NoText,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u16s(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn in_felix_only_a_text_box_takes_a_paste() {
        let element = |role: &str, editable| FocusProbe::Element {
            role: role.into(),
            editable,
        };
        assert_eq!(
            own_app_target(&element("AXTextField", false)),
            PasteTarget::Text
        );
        assert_eq!(own_app_target(&element("AXGroup", true)), PasteTarget::Text);
        assert_eq!(
            own_app_target(&element("AXButton", false)),
            PasteTarget::NoText
        );
        assert_eq!(
            own_app_target(&element("AXWebArea", false)),
            PasteTarget::NoText
        );
        assert_eq!(own_app_target(&FocusProbe::NoFocus), PasteTarget::NoText);
        assert_eq!(own_app_target(&FocusProbe::Failed), PasteTarget::NoText);
    }

    #[test]
    fn inserted_span_finds_the_dictation() {
        assert_eq!(
            inserted_span(&u16s("Hello world"), &u16s("Hello brave world")),
            Some((6, 6))
        );
        assert_eq!(inserted_span(&u16s(""), &u16s("Hi there")), Some((0, 8)));
        assert_eq!(inserted_span(&u16s("abc"), &u16s("abc")), None);
        // Repeated characters at the seam don't confuse it.
        assert_eq!(inserted_span(&u16s("aa"), &u16s("aaaa")), Some((2, 2)));
        // Emoji are two UTF-16 units.
        assert_eq!(inserted_span(&u16s("x"), &u16s("x 👋")), Some((1, 3)));
    }

    #[test]
    fn cursor_context() {
        let snap = FieldSnapshot {
            pid: 1,
            role: "AXTextArea".into(),
            value: u16s("Dear Sam, thanks. Best"),
            selection: Some((18, 0)),
        };
        assert_eq!(snap.text_before_cursor(8), "thanks. ");
        assert_eq!(snap.text_after_cursor(4), "Best");
    }

    #[test]
    fn spacing_after_words_and_punctuation() {
        assert_eq!(
            adapt_to_context("Sounds good.", "Thanks!", "", &[]),
            " Sounds good."
        );
        assert_eq!(
            adapt_to_context("Sounds good.", "Thanks! ", "", &[]),
            "Sounds good."
        );
        assert_eq!(
            adapt_to_context("Sounds good.", "", "", &[]),
            "Sounds good."
        );
        assert_eq!(
            adapt_to_context("Sounds good.", "(", "", &[]),
            "Sounds good."
        );
    }

    #[test]
    fn mid_sentence_lowercases_first_word() {
        assert_eq!(
            adapt_to_context("Then we ship it.", "we test it,", "", &[]),
            " then we ship it."
        );
        assert_eq!(
            adapt_to_context("I think so.", "and", "", &[]),
            " I think so."
        );
        assert_eq!(adapt_to_context("PR is up.", "the", "", &[]), " PR is up.");
        let vocab = vec!["Sam".to_string()];
        assert_eq!(
            adapt_to_context("Sam agrees.", "and", "", &vocab),
            " Sam agrees."
        );
    }

    #[test]
    fn after_sentence_keeps_capital() {
        assert_eq!(
            adapt_to_context("Next point.", "Done. ", "", &[]),
            "Next point."
        );
    }

    #[test]
    fn trailing_space_before_following_word() {
        assert_eq!(adapt_to_context("really", "it's ", "good", &[]), "really ");
        assert_eq!(adapt_to_context("really", "it's ", " good", &[]), "really");
    }

    fn el(role: &str, editable: bool) -> FocusProbe {
        FocusProbe::Element {
            role: role.into(),
            editable,
        }
    }

    #[test]
    fn text_fields_are_targets_everywhere() {
        assert_eq!(
            classify_target("com.apple.Notes", false, &el("AXTextArea", false)),
            PasteTarget::Text
        );
        assert_eq!(
            classify_target("com.tinyspeck.slackmacgap", true, &el("AXGroup", true)),
            PasteTarget::Text
        );
    }

    #[test]
    fn confident_no_target() {
        assert_eq!(
            classify_target("com.apple.finder", false, &FocusProbe::NoFocus),
            PasteTarget::NoText
        );
        assert_eq!(
            classify_target("com.apple.finder", false, &el("AXScrollArea", false)),
            PasteTarget::NoText
        );
        assert_eq!(
            classify_target("com.apple.Safari", false, &el("AXWebArea", false)),
            PasteTarget::NoText
        );
        assert_eq!(
            classify_target("com.apple.Preview", false, &el("AXImage", false)),
            PasteTarget::NoText
        );
    }

    #[test]
    fn nothing_focused_in_electron_counts_once_its_tree_is_live() {
        let claude = "com.anthropic.claudefordesktop";
        // Tree not switched on (yet): can't tell, so paste.
        assert_eq!(
            classify_target(claude, true, &FocusProbe::NoFocus),
            PasteTarget::Unknown
        );
        // Switched on: judged like a native app.
        assert_eq!(
            classify_target(claude, false, &FocusProbe::NoFocus),
            PasteTarget::NoText
        );
        assert_eq!(
            classify_target(claude, false, &el("AXTextArea", false)),
            PasteTarget::Text
        );
        assert_eq!(
            classify_target(claude, false, &el("AXGroup", false)),
            PasteTarget::Unknown
        );
    }

    #[test]
    fn doubtful_cases_still_paste() {
        // Accessibility failed or the role is ambiguous.
        assert_eq!(
            classify_target("com.apple.Notes", false, &FocusProbe::Failed),
            PasteTarget::Unknown
        );
        assert_eq!(
            classify_target("com.apple.Notes", false, &el("AXGroup", false)),
            PasteTarget::Unknown
        );
        // Terminals, Electron and JVM apps type without a text element.
        assert_eq!(
            classify_target("net.kovidgoyal.kitty", false, &FocusProbe::NoFocus),
            PasteTarget::Unknown
        );
        assert_eq!(
            classify_target("com.hnc.Discord", true, &el("AXWindow", false)),
            PasteTarget::Unknown
        );
        assert_eq!(
            classify_target("com.google.Chrome", true, &FocusProbe::NoFocus),
            PasteTarget::Unknown
        );
        // ...but a focused page body shows the tree is live.
        assert_eq!(
            classify_target("com.google.Chrome", true, &el("AXWebArea", false)),
            PasteTarget::NoText
        );
        assert_eq!(
            classify_target("com.jetbrains.intellij", false, &el("AXWindow", false)),
            PasteTarget::Unknown
        );
    }

    #[test]
    fn a_dropped_period_comes_back_before_a_new_sentence() {
        let last = "I think the problem is the cache";
        let before = "Notes: I think the problem is the cache";
        assert!(new_sentence_after(
            before,
            last,
            "Every time we deploy it breaks"
        ));
        assert!(new_sentence_after(
            &format!("{before} "),
            last,
            "Every time"
        ));
        // A continuation, lowercase text, or text typed since: no period.
        assert!(!new_sentence_after(before, last, "And it breaks"));
        assert!(!new_sentence_after(before, last, "every time we deploy"));
        assert!(!new_sentence_after(
            "I think the problem is the cache, fixed",
            last,
            "Every"
        ));
        // The last dictation ended in something other than a word.
        assert!(!new_sentence_after(
            "run git reset --soft HEAD~1)",
            "git reset --soft HEAD~1)",
            "Then"
        ));
    }
}
