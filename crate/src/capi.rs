//! The extern-C boundary — glue only, no logic. Mirrors include/kine.h, the
//! single source of truth every host binding links against. Every entry point
//! is panic-proof: a panic or error is caught, recorded in the thread-local
//! last-error, and reported as a sentinel value ({NULL,0} buffer / negative
//! int) — a panic must never unwind across FFI.

#![allow(non_camel_case_types)]

use std::ffi::{c_char, CStr};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use crate::validate::Compiled;
use crate::{error, fonts, handle, render};

enum Encoding {
    Png,
    Rgba,
}

/// Owned byte buffer handed to the caller. `{NULL, 0}` signals failure — call
/// `kine_last_error()`. Any non-null buffer must be released with
/// `kine_buf_free`.
#[repr(C)]
pub struct kine_buf {
    pub ptr: *mut u8,
    pub len: usize,
}

impl kine_buf {
    fn null() -> Self {
        kine_buf {
            ptr: std::ptr::null_mut(),
            len: 0,
        }
    }

    fn from_vec(bytes: Vec<u8>) -> Self {
        let mut boxed = bytes.into_boxed_slice();
        let ptr = boxed.as_mut_ptr();
        let len = boxed.len();
        std::mem::forget(boxed);
        kine_buf { ptr, len }
    }
}

/// Register a font from raw bytes (TTF/OTF). Returns 0 on success, -1 on error
/// (see `kine_last_error()`). Idempotent for identical bytes.
// The pointer contract (valid for len bytes) is the C ABI documented in
// include/kine.h; the boundary stays a safe fn like its siblings.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn kine_register_font(bytes: *const u8, len: usize) -> i32 {
    error::clear();
    let result = catch_unwind(AssertUnwindSafe(|| {
        if bytes.is_null() || len == 0 {
            return Err("font data is empty".to_string());
        }
        let data = unsafe { std::slice::from_raw_parts(bytes, len) };
        fonts::register(data).map(|_| ())
    }));
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(message)) => {
            error::set(message);
            -1
        }
        Err(_) => {
            error::set("panic in kine_register_font");
            -1
        }
    }
}

/// Render a v1 motion document at time `t` with the given signals into PNG
/// bytes (straight alpha). `t` is sugar for the document's `time` input; an
/// explicit `time` signal wins. Parsing/validation is strict; signal supply is
/// lenient. One-shot convenience — for repeated renders of one document, use a
/// handle (`kine_document_create`).
#[no_mangle]
pub extern "C" fn kine_render_document(
    doc_json: *const c_char,
    t: f64,
    signals_json: *const c_char,
    width: u32,
    height: u32,
) -> kine_buf {
    guard(move || {
        let compiled = compile(cstr(doc_json, "document")?)?;
        render_scene(&compiled, t, signals_json, width, height, Encoding::Png)
    })
}

/// Like `kine_render_document`, but returns raw RGBA8 (premultiplied, sRGB,
/// row-major, stride = width*4) for texture-upload clients.
#[no_mangle]
pub extern "C" fn kine_render_document_rgba(
    doc_json: *const c_char,
    t: f64,
    signals_json: *const c_char,
    width: u32,
    height: u32,
) -> kine_buf {
    guard(move || {
        let compiled = compile(cstr(doc_json, "document")?)?;
        render_scene(&compiled, t, signals_json, width, height, Encoding::Rgba)
    })
}

/// Parse + validate a document and describe its interface:
/// `{ "version", "size", "inputs": [...], "roles": [...] }`. The first
/// schema/validation error is reported through the error channel.
#[no_mangle]
pub extern "C" fn kine_probe(doc_json: *const c_char) -> kine_buf {
    guard(move || {
        let compiled = compile(cstr(doc_json, "document")?)?;
        Ok(interface_json(&compiled).to_string().into_bytes())
    })
}

// --- handles ----------------------------------------------------------------

/// Parse + validate a document and keep it as a reusable handle (parsing the
/// same document every frame is waste). Returns the handle (> 0), or 0 on a
/// parse/validation error (see `kine_last_error`). Free it with
/// `kine_document_free`.
#[no_mangle]
pub extern "C" fn kine_document_create(doc_json: *const c_char) -> i64 {
    error::clear();
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<i64, String> {
        let compiled = compile(cstr(doc_json, "document")?)?;
        Ok(handle::insert(compiled))
    }));
    match result {
        Ok(Ok(id)) => id,
        Ok(Err(message)) => {
            error::set(message);
            0
        }
        Err(_) => {
            error::set("panic in kine_document_create");
            0
        }
    }
}

/// Describe a handle's interface (same payload as `kine_probe`).
#[no_mangle]
pub extern "C" fn kine_document_probe(handle: i64) -> kine_buf {
    guard(move || {
        let compiled = document(handle)?;
        Ok(interface_json(&compiled).to_string().into_bytes())
    })
}

/// Render a handle at time `t` with the given signals into raw RGBA8
/// (premultiplied, sRGB, row-major, stride = width*4). Safe to call
/// concurrently from any thread on the same or different handles.
#[no_mangle]
pub extern "C" fn kine_document_render_rgba(
    handle: i64,
    t: f64,
    signals_json: *const c_char,
    width: u32,
    height: u32,
) -> kine_buf {
    guard(move || {
        let compiled = document(handle)?;
        render_scene(&compiled, t, signals_json, width, height, Encoding::Rgba)
    })
}

/// Free a document handle. Idempotent; a use-after-free reports an error on the
/// next call, never crashes.
#[no_mangle]
pub extern "C" fn kine_document_free(handle: i64) {
    let _ = catch_unwind(AssertUnwindSafe(|| handle::remove(handle)));
}

/// Crate + schema version, e.g. "kine 0.1.0 (schema v1)". Static; never freed.
#[no_mangle]
pub extern "C" fn kine_version() -> *const c_char {
    // NUL-terminated in the literal so it is a valid C string with no allocation.
    const VERSION: &str = concat!("kine ", env!("CARGO_PKG_VERSION"), " (schema v1)\0");
    VERSION.as_ptr() as *const c_char
}

/// The probe payload: declared inputs echoed with their type-specific fields.
fn interface_json(compiled: &crate::validate::Compiled) -> serde_json::Value {
    use crate::schema::Input;
    use serde_json::json;

    let inputs: Vec<serde_json::Value> = compiled
        .doc
        .inputs
        .iter()
        .map(|input| match input {
            Input::Unit(i) => json!({"key": i.key, "type": "unit", "default": i.default}),
            Input::UnitArray(i) => json!({"key": i.key, "type": "unitArray", "default": i.default}),
            Input::Time(i) => json!({"key": i.key, "type": "time", "default": i.default}),
            Input::Number(i) => {
                let mut value = json!({"key": i.key, "type": "number", "default": i.default});
                if let Some(min) = i.min {
                    value["min"] = json!(min);
                }
                if let Some(max) = i.max {
                    value["max"] = json!(max);
                }
                value
            }
            Input::String(i) => json!({"key": i.key, "type": "string", "default": i.default}),
            Input::Color(i) => json!({"key": i.key, "type": "color", "default": i.default}),
            Input::Enum(i) => {
                json!({"key": i.key, "type": "enum", "values": i.values, "default": i.default})
            }
            Input::FontFamily(i) => {
                json!({"key": i.key, "type": "fontFamily", "default": i.default})
            }
        })
        .collect();

    // Assets manifest in document order (§4): key, kind, mime, and whether the
    // decoded asset has more than one frame.
    let assets: Vec<serde_json::Value> = compiled
        .doc
        .assets
        .iter()
        .map(|asset| {
            // Every doc asset has a decoded entry (decode_all inserts one per
            // element and rejects duplicate keys), so index rather than guard.
            let animated = compiled.assets[&asset.key].animated();
            json!({
                "key": asset.key,
                "kind": asset.kind.as_str(),
                "mime": asset.mime.as_str(),
                "animated": animated,
            })
        })
        .collect();

    json!({
        "version": compiled.doc.version,
        "size": { "width": compiled.doc.size.width, "height": compiled.doc.size.height },
        "inputs": inputs,
        "roles": compiled.roles,
        "assets": assets,
    })
}

/// Free a buffer returned by `kine_render_document` / `kine_probe`.
#[no_mangle]
pub extern "C" fn kine_buf_free(buf: kine_buf) {
    if buf.ptr.is_null() || buf.len == 0 {
        return;
    }
    unsafe {
        let slice = std::slice::from_raw_parts_mut(buf.ptr, buf.len);
        drop(Box::from_raw(slice as *mut [u8]));
    }
}

/// The current thread's last error message, or NULL. Valid until the next
/// kine_* call on this thread.
#[no_mangle]
pub extern "C" fn kine_last_error() -> *const c_char {
    error::ptr()
}

// --- internal helpers -------------------------------------------------------

/// Run `body` under a panic guard, converting the result into a buffer and
/// recording failures in the last-error slot.
fn guard(body: impl FnOnce() -> Result<Vec<u8>, String>) -> kine_buf {
    error::clear();
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(Ok(bytes)) => kine_buf::from_vec(bytes),
        Ok(Err(message)) => {
            error::set(message);
            kine_buf::null()
        }
        Err(_) => {
            error::set("panic in kine");
            kine_buf::null()
        }
    }
}

/// Parse + validate a document (shared by the one-shot and handle paths).
fn compile(doc_text: &str) -> Result<Compiled, String> {
    let doc = crate::schema::parse(doc_text).map_err(|e| e.to_string())?;
    crate::validate::validate(doc).map_err(|e| e.to_string())
}

fn document(handle: i64) -> Result<Arc<Compiled>, String> {
    handle::get(handle).ok_or_else(|| "invalid or freed document handle".to_string())
}

/// Evaluate + render a compiled document — the single render implementation
/// behind every one-shot and handle entry point.
fn render_scene(
    compiled: &Compiled,
    t: f64,
    signals_json: *const c_char,
    width: u32,
    height: u32,
    encoding: Encoding,
) -> Result<Vec<u8>, String> {
    let signals = if signals_json.is_null() {
        serde_json::Value::Null
    } else {
        parse_json(cstr(signals_json, "signals")?, "signals")?
    };
    let scene = crate::eval::evaluate(&compiled.doc, &compiled.assets, &signals, t)
        .map_err(|e| e.to_string())?;
    match encoding {
        Encoding::Png => render::render_png(&scene, width, height),
        Encoding::Rgba => render::render_rgba(&scene, width, height),
    }
}

fn cstr<'a>(ptr: *const c_char, what: &str) -> Result<&'a str, String> {
    if ptr.is_null() {
        return Err(format!("{what} pointer is null"));
    }
    unsafe { CStr::from_ptr(ptr) }
        .to_str()
        .map_err(|_| format!("{what} is not valid UTF-8"))
}

fn parse_json(text: &str, what: &str) -> Result<serde_json::Value, String> {
    serde_json::from_str(text).map_err(|e| format!("invalid {what} JSON: {e}"))
}
