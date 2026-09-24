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

const TEXT_ROLES: &[&str] = &["AXTextField", "AXTextArea", "AXComboBox", "AXSearchField"];

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

    use super::{FocusProbe, TEXT_ROLES};

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

/// Whether the frontmost app's focused element can take pasted text.
pub fn paste_target() -> PasteTarget {
    #[cfg(target_os = "macos")]
    {
        let (bundle_id, lazy_ax) = crate::app_context::frontmost_bundle();
        let probe = ax::probe_focus();
        let target = classify_target(bundle_id.as_deref().unwrap_or(""), lazy_ax, &probe);
        log::debug!(
            "Paste target in {:?}: {:?} -> {:?}",
            bundle_id,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn u16s(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
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
        let vocab = vec!["Kaspar".to_string()];
        assert_eq!(
            adapt_to_context("Kaspar agrees.", "and", "", &vocab),
            " Kaspar agrees."
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
}
