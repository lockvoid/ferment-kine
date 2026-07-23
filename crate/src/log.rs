//! Host-registered log sink. Kine never prints on its own — diagnostics flow
//! through the callback the host registers (`kine_set_log_callback`), so a
//! failing render or a font-family miss is loud in the host's own log stream
//! (os_log on iOS) instead of dying silently in a thread-local error slot.
//! No callback registered = silent, exactly the pre-sink behavior.

use std::ffi::{c_char, CString};
use std::sync::atomic::{AtomicUsize, Ordering};

pub const INFO: i32 = 0;
pub const WARN: i32 = 1;
pub const ERROR: i32 = 2;

pub type Callback = extern "C" fn(level: i32, message: *const c_char);

/// The registered callback as a raw fn address; 0 = none. An atomic (not a
/// lock) because `emit` fires from render paths on arbitrary threads.
static CALLBACK: AtomicUsize = AtomicUsize::new(0);

pub fn set_callback(callback: Option<Callback>) {
    CALLBACK.store(callback.map_or(0, |f| f as usize), Ordering::Release);
}

pub fn emit(level: i32, message: &str) {
    let raw = CALLBACK.load(Ordering::Acquire);
    if raw == 0 {
        return;
    }
    let callback: Callback = unsafe { std::mem::transmute(raw) };
    let Ok(text) = CString::new(message) else {
        return;
    };
    callback(level, text.as_ptr());
}
