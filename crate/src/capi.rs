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
use crate::{error, fonts, handle, log, render};

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

/// Register a host log sink. Every FFI failure (`kine_last_error` content) and
/// font registration is reported through it — levels: 0 info, 1 warn, 2 error.
/// The callback may fire on ANY thread; the message pointer is valid only for
/// the duration of the call. NULL unregisters.
#[no_mangle]
pub extern "C" fn kine_set_log_callback(callback: Option<extern "C" fn(i32, *const c_char)>) {
    log::set_callback(callback);
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

/// Declare the render-fallback family (levels: strict vs degrade). With a
/// fallback set, a document naming an unregistered family still renders —
/// its text shaped by the fallback, WARNED once per family through the log
/// sink. Without one (the default), a missing family is a hard render error.
/// The write seams (probe `missingFonts`) stay strict either way. The name
/// must already resolve in the collection (the embedded "Inter" always
/// does); empty string clears back to strict. 0 on success, -1 on error.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
#[no_mangle]
pub extern "C" fn kine_set_fallback_family(name: *const c_char) -> i32 {
    error::clear();
    let result = catch_unwind(AssertUnwindSafe(|| {
        fonts::set_fallback(cstr(name, "family")?)
    }));
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(message)) => {
            error::set(message);
            -1
        }
        Err(_) => {
            error::set("panic in kine_set_fallback_family");
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
/// `{ "version", "size", "inputs": [...], "roles": [...], "assets": [...],
/// "fonts": [...], "missingFonts": [...] }`. The first
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

/// The GROWN canvas the document's text content needs with these signals,
/// as JSON `{"width","height","y"}` in DESIGN units: `width` is always the
/// doc width (lines wrap), `height ≥` the doc height, and `y ≤ 0` is the
/// grown canvas's top in doc coordinates (negative when a center/bottom
/// valigned block grows upward). `{doc width, doc height, 0}` = everything
/// fits. THE definition of text overflow — hosts size render targets and
/// selection boxes from this instead of guessing at font metrics, and pair
/// it with `kine_document_render_rgba_viewport` to render without cropping.
/// Resting layout only: animator transforms don't move the measure.
#[no_mangle]
pub extern "C" fn kine_document_layout_size(handle: i64, signals_json: *const c_char) -> kine_buf {
    guard(move || {
        let compiled = document(handle)?;
        let signals = if signals_json.is_null() {
            serde_json::Value::Null
        } else {
            parse_json(cstr(signals_json, "signals")?, "signals")?
        };
        let scene = crate::eval::evaluate(&compiled.doc, &compiled.assets, &signals, 0.0)
            .map_err(|e| e.to_string())?;
        let (top, bottom) = render::grown_extents(&scene);
        Ok(format!(
            r#"{{"width":{},"height":{},"y":{}}}"#,
            scene.width,
            bottom - top,
            top
        )
        .into_bytes())
    })
}

/// `kine_document_render_rgba` over a vertical design-space viewport
/// `(view_y, view_h)` — pass `kine_document_layout_size`'s `y`/`height`
/// (with a target raster of matching aspect) to render text that
/// overflows the doc canvas instead of cropping it. `(0, doc height)`
/// is exactly the plain render.
#[no_mangle]
pub extern "C" fn kine_document_render_rgba_viewport(
    handle: i64,
    t: f64,
    signals_json: *const c_char,
    width: u32,
    height: u32,
    view_y: f64,
    view_h: f64,
) -> kine_buf {
    guard(move || {
        let compiled = document(handle)?;
        let signals = if signals_json.is_null() {
            serde_json::Value::Null
        } else {
            parse_json(cstr(signals_json, "signals")?, "signals")?
        };
        let scene = crate::eval::evaluate(&compiled.doc, &compiled.assets, &signals, t)
            .map_err(|e| e.to_string())?;
        render::render_rgba_viewport(&scene, width, height, view_y, view_h)
    })
}

/// Union INK rect across `samples` frames evenly spaced over `[0, span]`
/// seconds, rendered at a small probe raster — returned as JSON
/// `{"x","y","width","height"}` in DESIGN-BOX FRACTIONS. `samples <= 1` (or
/// `span <= 0`) probes the resting frame only. An empty buffer (len 0) means
/// a fully-blank document. This is THE definition of visual bounds — hosts
/// (selection borders, sticker normalization, preview blank-trimming) consume
/// it instead of growing their own alpha scans.
#[no_mangle]
pub extern "C" fn kine_document_ink_union(
    handle: i64,
    signals_json: *const c_char,
    samples: u32,
    span: f64,
    width: u32,
    height: u32,
) -> kine_buf {
    guard(move || {
        let compiled = document(handle)?;
        ink_union_impl(&compiled, signals_json, samples, span, width, height)
    })
}

/// One-shot variant for hosts without the handle lifecycle (the Ruby gem's
/// registration-time probes) — compiles per call.
#[no_mangle]
pub extern "C" fn kine_ink_union(
    doc_json: *const c_char,
    signals_json: *const c_char,
    samples: u32,
    span: f64,
    width: u32,
    height: u32,
) -> kine_buf {
    guard(move || {
        let compiled = compile(cstr(doc_json, "document")?)?;
        ink_union_impl(&compiled, signals_json, samples, span, width, height)
    })
}

fn ink_union_impl(
    compiled: &Compiled,
    signals_json: *const c_char,
    samples: u32,
    span: f64,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    {
        let signals = if signals_json.is_null() {
            serde_json::Value::Null
        } else {
            parse_json(cstr(signals_json, "signals")?, "signals")?
        };
        if width == 0 || height == 0 {
            return Err("probe size must be positive".into());
        }
        // Text overflow: probe over the GROWN canvas (layout_size's answer),
        // so ink that grows past the design box is measured, not cropped.
        // Fractions are relative to the grown box — identical to the design
        // box whenever everything fits (stickers, fitting text) — and the
        // grown box itself rides the payload so consumers can map through
        // the same canvas the pixels render on. Growth is measured on the
        // resting layout (t = 0): text layout is signal-driven, not
        // time-driven, so every sample shares it.
        let rest = crate::eval::evaluate(&compiled.doc, &compiled.assets, &signals, 0.0)
            .map_err(|e| e.to_string())?;
        let (view_top, view_bottom) = render::grown_extents(&rest);
        let view_h = view_bottom - view_top;
        let count = if samples <= 1 || span <= 0.0 { 1 } else { samples };
        let mut union: Option<(u32, u32, u32, u32)> = None;
        for i in 0..count {
            let t = if count == 1 { 0.0 } else { span * (i as f64) / ((count - 1) as f64) };
            let scene = crate::eval::evaluate(&compiled.doc, &compiled.assets, &signals, t)
                .map_err(|e| e.to_string())?;
            let rgba = render::render_rgba_viewport(&scene, width, height, view_top, view_h)?;
            if let Some(b) = render::ink_bounds_rgba(&rgba, width, height, 8) {
                union = Some(match union {
                    None => b,
                    Some(u) => (u.0.min(b.0), u.1.min(b.1), u.2.max(b.2), u.3.max(b.3)),
                });
            }
        }
        let Some((min_x, min_y, max_x, max_y)) = union else {
            return Ok(Vec::new());
        };
        let (w, h) = (width as f64, height as f64);
        Ok(format!(
            "{{\"x\":{},\"y\":{},\"width\":{},\"height\":{},\"canvasWidth\":{},\"canvasHeight\":{},\"canvasY\":{}}}",
            min_x as f64 / w,
            min_y as f64 / h,
            (max_x - min_x + 1) as f64 / w,
            (max_y - min_y + 1) as f64 / h,
            rest.width,
            view_h,
            view_top
        )
        .into_bytes())
    }
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

    // Font manifest: which families the document references, and which of
    // them the process registry cannot serve RIGHT NOW. `fonts` is static
    // truth a host can validate against its catalog at the write seam;
    // `missingFonts` is registry state, so a host that registers lazily can
    // fetch exactly what a document needs before rendering it.
    let fonts = crate::schema::referenced_fonts(&compiled.doc);
    let missing = fonts::missing(&fonts);

    json!({
        "version": compiled.doc.version,
        "size": { "width": compiled.doc.size.width, "height": compiled.doc.size.height },
        "inputs": inputs,
        "roles": compiled.roles,
        "assets": assets,
        "fonts": fonts,
        "missingFonts": missing,
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

// --- GPU flavor (feature = "gpu") -------------------------------------------
//
// Same conventions as every entry point above: panic-proof, error-sentinel,
// message on the thread-local last-error channel. The engine renders on the
// HOST's device and queue and into the HOST's texture, so nothing here hands
// ownership of a Metal object across the boundary in either direction — kine
// borrows for the duration of the call and retains nothing but the device and
// queue the engine was created with.

/// Create the GPU engine on the host's `MTLDevice` and `MTLCommandQueue`.
/// Returns the engine handle (> 0), or 0 on failure (see `kine_last_error`).
///
/// Both pointers are required: kine renders on the host's device so its output
/// texture and the host's own draws share one device and one queue, which is
/// what orders them without a fence or a blocking wait. Passing the device of a
/// different GPU than the system default is refused rather than crossed.
///
/// ONE engine per process is the intended shape (it owns the image atlas and
/// the renderer). Free with `kine_gpu_engine_destroy`.
#[cfg(all(feature = "gpu", target_vendor = "apple"))]
#[no_mangle]
pub extern "C" fn kine_gpu_engine_create(
    mtl_device: *mut std::ffi::c_void,
    mtl_queue: *mut std::ffi::c_void,
) -> i64 {
    error::clear();
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<i64, String> {
        let engine = unsafe { crate::gpu::GpuEngine::from_metal(mtl_device, mtl_queue) }?;
        Ok(crate::gpu::insert_engine(engine))
    }));
    match result {
        Ok(Ok(id)) => id,
        Ok(Err(message)) => {
            error::set(message);
            0
        }
        Err(_) => {
            error::set("panic in kine_gpu_engine_create");
            0
        }
    }
}

/// Destroy a GPU engine. Idempotent; a use-after-free reports an error on the
/// next call, never a crash.
#[cfg(all(feature = "gpu", target_vendor = "apple"))]
#[no_mangle]
pub extern "C" fn kine_gpu_engine_destroy(engine: i64) {
    let _ = catch_unwind(AssertUnwindSafe(|| crate::gpu::remove_engine(engine)));
}

/// Rasterize a document handle at time `t` into a HOST-owned `MTLTexture`.
/// Returns 0 on success, -1 on failure (see `kine_last_error`).
///
/// The texture must be `MTLPixelFormatRGBA8Unorm`, exactly `width`x`height`, and
/// carry `MTLTextureUsageRenderTarget`; all three are checked. kine writes
/// premultiplied RGBA8, sRGB — the same bytes `kine_document_render_rgba`
/// produces on the CPU.
///
/// SUBMITS AND RETURNS: it does not wait for the GPU. Ordering against the
/// host's own command buffers is the shared queue's job, which is why the queue
/// is a creation parameter.
///
/// On ANY failure the target texture is left untouched and the host should fall
/// back to the CPU flavor for this frame.
#[cfg(all(feature = "gpu", target_vendor = "apple"))]
#[no_mangle]
pub extern "C" fn kine_gpu_render_document(
    engine: i64,
    document: i64,
    t: f64,
    signals_json: *const c_char,
    width: u32,
    height: u32,
    mtl_texture: *mut std::ffi::c_void,
) -> i32 {
    gpu_render(
        engine,
        document,
        t,
        signals_json,
        width,
        height,
        None,
        mtl_texture,
    )
}

/// `kine_gpu_render_document` over a vertical design-space viewport — the GPU
/// twin of `kine_document_render_rgba_viewport`, for text that overflows the
/// doc canvas.
#[cfg(all(feature = "gpu", target_vendor = "apple"))]
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub extern "C" fn kine_gpu_render_document_viewport(
    engine: i64,
    document: i64,
    t: f64,
    signals_json: *const c_char,
    width: u32,
    height: u32,
    view_y: f64,
    view_h: f64,
    mtl_texture: *mut std::ffi::c_void,
) -> i32 {
    gpu_render(
        engine,
        document,
        t,
        signals_json,
        width,
        height,
        Some((view_y, view_h)),
        mtl_texture,
    )
}

#[cfg(all(feature = "gpu", target_vendor = "apple"))]
#[allow(clippy::too_many_arguments)]
fn gpu_render(
    engine: i64,
    document: i64,
    t: f64,
    signals_json: *const c_char,
    width: u32,
    height: u32,
    viewport: Option<(f64, f64)>,
    mtl_texture: *mut std::ffi::c_void,
) -> i32 {
    error::clear();
    let result = catch_unwind(AssertUnwindSafe(|| -> Result<(), String> {
        let engine = crate::gpu::get_engine(engine)
            .ok_or_else(|| "invalid or destroyed gpu engine handle".to_string())?;
        let compiled = self::document(document)?;
        let signals = if signals_json.is_null() {
            serde_json::Value::Null
        } else {
            parse_json(cstr(signals_json, "signals")?, "signals")?
        };
        let scene = crate::eval::evaluate(&compiled.doc, &compiled.assets, &signals, t)
            .map_err(|e| e.to_string())?;
        let viewport = viewport.unwrap_or((0.0, scene.height));
        let mut engine = engine
            .lock()
            .map_err(|_| "gpu engine mutex poisoned".to_string())?;
        unsafe { engine.render_to_metal_texture(&scene, width, height, viewport, mtl_texture) }
    }));
    match result {
        Ok(Ok(())) => 0,
        Ok(Err(message)) => {
            error::set(message);
            -1
        }
        Err(_) => {
            error::set("panic in kine_gpu_render_document");
            -1
        }
    }
}

/// Whether this build carries the GPU flavor at all. Hosts branch on this
/// instead of probing for a symbol: the ruby gem and the rails server link a
/// CPU-only build where every `kine_gpu_*` entry point is absent.
#[no_mangle]
pub extern "C" fn kine_gpu_available() -> i32 {
    i32::from(cfg!(all(feature = "gpu", target_vendor = "apple")))
}
