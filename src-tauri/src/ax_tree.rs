//! Reading and pressing another app's controls through the accessibility API,
//! for skills that drive an app without screenshots (Felix's "start a Claude
//! session"). Everything goes through Handy's own Accessibility grant.

use serde::Serialize;

/// One control in an app's accessibility tree.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Node {
    pub depth: usize,
    pub role: String,
    /// Title, description or value — whatever names the control.
    pub label: String,
}

impl Node {
    pub fn matches(&self, role: &str, label: &str) -> bool {
        self.role == role && self.label.to_lowercase().contains(&label.to_lowercase())
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::Node;
    use core_foundation::array::{CFArray, CFArrayGetTypeID};
    use core_foundation::base::{CFGetTypeID, CFRelease, CFTypeRef, TCFType};
    use core_foundation::string::{CFString, CFStringGetTypeID, CFStringRef};
    use std::ffi::c_void;

    type AXUIElementRef = *const c_void;
    type AXError = i32;
    const AX_SUCCESS: AXError = 0;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AXError;
        fn AXUIElementSetMessagingTimeout(element: AXUIElementRef, seconds: f32) -> AXError;
        fn AXUIElementPerformAction(element: AXUIElementRef, action: CFStringRef) -> AXError;
        fn AXIsProcessTrusted() -> bool;
        fn AXUIElementSetAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: CFTypeRef,
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
        .filter(|s| !s.trim().is_empty())
    }

    fn label(element: AXUIElementRef) -> String {
        ["AXTitle", "AXDescription", "AXValue", "AXHelp"]
            .iter()
            .find_map(|a| string_attribute(element, a))
            .unwrap_or_default()
    }

    /// Walk the tree depth-first; `visit` returns true to stop.
    fn walk(
        element: AXUIElementRef,
        depth: usize,
        max_depth: usize,
        visit: &mut dyn FnMut(AXUIElementRef, Node) -> bool,
    ) -> bool {
        unsafe { AXUIElementSetMessagingTimeout(element, 0.5) };
        let node = Node {
            depth,
            role: string_attribute(element, "AXRole").unwrap_or_default(),
            label: label(element),
        };
        if visit(element, node) {
            return true;
        }
        if depth >= max_depth {
            return false;
        }
        let Some(children) = copy_attribute(element, "AXChildren") else {
            return false;
        };
        // SAFETY: type-checked; the array is retained by `children` while we read it.
        if unsafe { CFGetTypeID(children.0) } != unsafe { CFArrayGetTypeID() } {
            return false;
        }
        let array: CFArray<*const c_void> =
            unsafe { CFArray::wrap_under_get_rule(children.0 as _) };
        array
            .iter()
            .any(|child| walk(*child, depth + 1, max_depth, visit))
    }

    pub fn is_trusted() -> bool {
        unsafe { AXIsProcessTrusted() }
    }

    /// Electron apps build their accessibility tree only once asked to.
    pub fn expose_electron_tree(pid: i32) {
        use core_foundation::boolean::CFBoolean;
        let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
        unsafe { AXUIElementSetMessagingTimeout(app.0, 0.5) };
        let attribute = CFString::new("AXManualAccessibility");
        // SAFETY: app and the boolean are live CF objects.
        unsafe {
            AXUIElementSetAttributeValue(
                app.0,
                attribute.as_concrete_TypeRef(),
                CFBoolean::true_value().as_CFTypeRef(),
            )
        };
    }

    pub fn window_titles(pid: i32) -> Vec<String> {
        let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
        unsafe { AXUIElementSetMessagingTimeout(app.0, 0.3) };
        let Some(windows) = copy_attribute(app.0, "AXWindows") else {
            return Vec::new();
        };
        // SAFETY: type-checked; the array is retained by `windows` while we read it.
        if unsafe { CFGetTypeID(windows.0) } != unsafe { CFArrayGetTypeID() } {
            return Vec::new();
        }
        let array: CFArray<*const c_void> = unsafe { CFArray::wrap_under_get_rule(windows.0 as _) };
        array
            .iter()
            .filter_map(|w| string_attribute(*w, "AXTitle"))
            .collect()
    }

    pub fn dump(pid: i32, max_depth: usize) -> Vec<Node> {
        let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
        let mut nodes = Vec::new();
        walk(app.0, 0, max_depth, &mut |_, node| {
            nodes.push(node);
            false
        });
        nodes
    }

    pub fn texts(
        pid: i32,
        max_depth: usize,
        max_nodes: usize,
        budget: std::time::Duration,
    ) -> Vec<Vec<String>> {
        let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
        let started = std::time::Instant::now();
        let mut out = Vec::new();
        walk(app.0, 0, max_depth, &mut |element, _| {
            let texts: Vec<String> = [
                "AXTitle",
                "AXDescription",
                "AXValue",
                "AXHelp",
                "AXIdentifier",
                "AXDOMIdentifier",
            ]
            .iter()
            .filter_map(|a| string_attribute(element, a))
            .collect();
            if !texts.is_empty() {
                out.push(texts);
            }
            out.len() >= max_nodes || started.elapsed() > budget
        });
        out
    }

    pub fn press(pid: i32, role: &str, label: &str, max_depth: usize) -> Result<(), String> {
        let app = Owned(unsafe { AXUIElementCreateApplication(pid) });
        let mut result = Err(format!("No {role} \"{label}\""));
        walk(app.0, 0, max_depth, &mut |element, node| {
            if !node.matches(role, label) {
                return false;
            }
            let action = CFString::new("AXPress");
            // SAFETY: element is live for the duration of the walk callback.
            let err = unsafe { AXUIElementPerformAction(element, action.as_concrete_TypeRef()) };
            result = if err == AX_SUCCESS {
                Ok(())
            } else {
                Err(format!("Pressing {role} \"{label}\" failed ({err})"))
            };
            true
        });
        result
    }
}

/// Deep enough for Electron apps, whose controls sit ~20 levels down.
pub const DEFAULT_DEPTH: usize = 40;

/// Whether Handy may read and press other apps' controls.
pub fn is_trusted() -> bool {
    #[cfg(target_os = "macos")]
    return mac::is_trusted();
    #[cfg(not(target_os = "macos"))]
    false
}

/// Ask an Electron app (Claude, Slack…) to build its accessibility tree.
pub fn expose_electron_tree(pid: i32) {
    #[cfg(target_os = "macos")]
    mac::expose_electron_tree(pid);
    #[cfg(not(target_os = "macos"))]
    let _ = pid;
}

/// The titles of an app's windows (a browser's name its front tab).
pub fn window_titles(pid: i32) -> Vec<String> {
    #[cfg(target_os = "macos")]
    return mac::window_titles(pid);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        Vec::new()
    }
}

/// Every control in the app with this pid.
pub fn dump(pid: i32, max_depth: usize) -> Vec<Node> {
    #[cfg(target_os = "macos")]
    return mac::dump(pid, max_depth);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, max_depth);
        Vec::new()
    }
}

/// Every text on each control (title, description, value, help and ids),
/// walking at most `max_nodes` controls or for `budget`, whichever ends first.
pub fn texts(
    pid: i32,
    max_depth: usize,
    max_nodes: usize,
    budget: std::time::Duration,
) -> Vec<Vec<String>> {
    #[cfg(target_os = "macos")]
    return mac::texts(pid, max_depth, max_nodes, budget);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, max_depth, max_nodes, budget);
        Vec::new()
    }
}

/// Press the first control with this role whose label contains `label`.
pub fn press(pid: i32, role: &str, label: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    return mac::press(pid, role, label, DEFAULT_DEPTH);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (pid, role, label);
        Err("Not supported on this platform".into())
    }
}

/// The pid of a running app, by bundle id.
pub fn pid_of(bundle_id: &str) -> Option<i32> {
    #[cfg(target_os = "macos")]
    {
        use objc2_app_kit::NSRunningApplication;
        use objc2_foundation::NSString;
        let apps = NSRunningApplication::runningApplicationsWithBundleIdentifier(
            &NSString::from_str(bundle_id),
        );
        apps.iter().next().map(|app| app.processIdentifier())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bundle_id;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_role_and_label_case_insensitively() {
        let node = Node {
            depth: 3,
            role: "AXButton".into(),
            label: "Trust Workspace".into(),
        };
        assert!(node.matches("AXButton", "trust"));
        assert!(!node.matches("AXLink", "trust"));
        assert!(!node.matches("AXButton", "send"));
    }
}
