//! Thread-local last-error string exposed across the FFI boundary. Every extern
//! fn clears it on entry and sets it on failure; `kine_last_error()` returns a
//! pointer valid until the next call on the same thread.

use std::cell::RefCell;
use std::ffi::{c_char, CString};

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

pub fn clear() {
    LAST_ERROR.with(|slot| *slot.borrow_mut() = None);
}

pub fn set(message: impl Into<String>) {
    let text = message.into();
    // A NUL in the message would truncate it; replace so the caller still gets
    // something legible rather than a silently empty string.
    let cstring = CString::new(text)
        .unwrap_or_else(|_| CString::new("kine error (message contained NUL)").unwrap());
    LAST_ERROR.with(|slot| *slot.borrow_mut() = Some(cstring));
}

pub fn ptr() -> *const c_char {
    LAST_ERROR.with(|slot| match &*slot.borrow() {
        Some(cstring) => cstring.as_ptr(),
        None => std::ptr::null(),
    })
}
