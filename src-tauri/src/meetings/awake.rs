//! Keep the display (and so the Mac) awake while a meeting records: a Mac
//! that dozes off mid-call stops the recording.

/// Held for as long as the Mac should stay awake.
pub struct KeepAwake {
    #[cfg(target_os = "macos")]
    id: Option<u32>,
}

#[cfg(target_os = "macos")]
mod mac {
    use core_foundation::base::TCFType;
    use core_foundation::string::{CFString, CFStringRef};

    const LEVEL_ON: u32 = 255;

    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMAssertionCreateWithName(
            kind: CFStringRef,
            level: u32,
            name: CFStringRef,
            id: *mut u32,
        ) -> i32;
        fn IOPMAssertionRelease(id: u32) -> i32;
    }

    pub fn create(reason: &str) -> Option<u32> {
        let kind = CFString::new("PreventUserIdleDisplaySleep");
        let name = CFString::new(reason);
        let mut id = 0u32;
        // SAFETY: both strings are live CFStrings; id receives the assertion.
        let status = unsafe {
            IOPMAssertionCreateWithName(
                kind.as_concrete_TypeRef(),
                LEVEL_ON,
                name.as_concrete_TypeRef(),
                &mut id,
            )
        };
        (status == 0).then_some(id)
    }

    pub fn release(id: u32) {
        // SAFETY: id came from IOPMAssertionCreateWithName.
        unsafe { IOPMAssertionRelease(id) };
    }
}

impl KeepAwake {
    pub fn new(reason: &str) -> Self {
        #[cfg(target_os = "macos")]
        {
            let id = mac::create(reason);
            if id.is_none() {
                log::warn!("Couldn't keep the display awake while recording");
            }
            Self { id }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = reason;
            Self {}
        }
    }
}

impl Drop for KeepAwake {
    fn drop(&mut self) {
        #[cfg(target_os = "macos")]
        if let Some(id) = self.id.take() {
            mac::release(id);
        }
    }
}
