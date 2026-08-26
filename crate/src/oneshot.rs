//! One-shot Rust-native render doors for tooling — the `kine` CLI and parity
//! checks: the same parse → validate → evaluate → raster pipeline as the C
//! surface, minus the FFI dance. CPU always; the GPU flavor rides the `gpu`
//! feature.

use crate::validate::Compiled;

fn compile(doc_json: &str) -> Result<Compiled, String> {
    let doc = crate::schema::parse(doc_json).map_err(|e| e.to_string())?;
    crate::validate::validate(doc).map_err(|e| e.to_string())
}

fn scene(
    compiled: &Compiled,
    signals_json: &str,
    t: f64,
) -> Result<crate::eval::Scene, String> {
    let signals: serde_json::Value = if signals_json.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_str(signals_json).map_err(|e| format!("signals: {e}"))?
    };
    crate::eval::evaluate(&compiled.doc, &compiled.assets, &signals, t).map_err(|e| e.to_string())
}

/// Register a font's raw bytes (TTF/OTF) into the process-global collection.
pub fn register_font(bytes: &[u8]) -> Result<usize, String> {
    crate::fonts::register(bytes)
}

/// Declare the render-fallback family (empty clears back to strict).
pub fn set_fallback_family(name: &str) -> Result<(), String> {
    crate::fonts::set_fallback(name)
}

/// CPU-flavor one-shot render to PNG bytes — the same path as
/// `kine_render_document`.
pub fn render_png_cpu(
    doc_json: &str,
    t: f64,
    signals_json: &str,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let compiled = compile(doc_json)?;
    let scene = scene(&compiled, signals_json, t)?;
    crate::render::render_png(&scene, width, height)
}

/// GPU-flavor one-shot render to PNG bytes (vello_hybrid over wgpu/Metal) —
/// the flavor the app's live preview runs.
#[cfg(feature = "gpu")]
pub fn render_png_gpu(
    doc_json: &str,
    t: f64,
    signals_json: &str,
    width: u32,
    height: u32,
) -> Result<Vec<u8>, String> {
    let compiled = compile(doc_json)?;
    let scene = scene(&compiled, signals_json, t)?;
    let mut engine = crate::gpu::GpuEngine::new()?;
    let mut pixels = engine.render_rgba(&scene, width, height, (0.0, scene.height))?;

    // PNG carries straight alpha; the raster is premultiplied.
    for px in pixels.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            for c in &mut px[..3] {
                *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
            }
        }
    }
    let image = image::RgbaImage::from_raw(width, height, pixels)
        .ok_or_else(|| "raster size mismatch".to_string())?;
    let mut out = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut out, image::ImageFormat::Png)
        .map_err(|e| format!("PNG encoding failed: {e}"))?;
    Ok(out.into_inner())
}
