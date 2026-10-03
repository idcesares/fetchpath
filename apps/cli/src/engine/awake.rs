//! Keeping the computer awake while an always-on engine downloads (FP-101,
//! design §10). Only while downloads are running: an idle always-on engine
//! leaves sleep to the person's power settings.

use windows_sys::Win32::System::Power::{
    ES_CONTINUOUS, ES_SYSTEM_REQUIRED, SetThreadExecutionState,
};

/// The request Windows holds for the calling thread. Owned by one thread,
/// which must be the one that calls [`Awake::set`], and released when
/// dropped.
#[derive(Default)]
pub struct Awake {
    held: bool,
}

impl Awake {
    /// Asks Windows to stay awake, or stops asking. Calls Windows only on a
    /// change.
    pub fn set(&mut self, wanted: bool) {
        if wanted == self.held {
            return;
        }
        let flags = if wanted {
            ES_CONTINUOUS | ES_SYSTEM_REQUIRED
        } else {
            ES_CONTINUOUS
        };
        // SAFETY: no pointers; the call only changes this thread's request.
        // A zero return means the request was not changed, so it is kept
        // as not held and tried again on the next change.
        if unsafe { SetThreadExecutionState(flags) } != 0 {
            self.held = wanted;
        }
    }
}

impl Drop for Awake {
    fn drop(&mut self) {
        self.set(false);
    }
}
