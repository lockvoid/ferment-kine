//! Asset decoding + budget enforcement (SCHEMA.md §4). base64 → bytes →
//! container-signature and size guards → decode ALL frames (blend/dispose
//! composited to full-canvas RGBA by the `image` facade) → premultiplied
//! Pixmaps + per-frame durations. Done once at document load; the handle owns
//! the decoded memory for its lifetime, which the decoded-size budget bounds.
//!
//! Pure Rust throughout (`image` over image-webp/gif/png), so this compiles to
//! iOS staticlib / Linux cdylib / wasm unchanged. Animated WebP goes through the
//! patched image-webp 0.2.5 (see Cargo.toml) — released 0.2.4 mis-composites.

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;

use base64::Engine;
use image::{AnimationDecoder, ImageDecoder};
use vello_common::pixmap::PixelMetadata;
use vello_cpu::peniko::ImageAlphaType;
use vello_cpu::Pixmap;

use crate::schema::{Asset, ImageMime, SchemaError};

// Budgets (§4). Bytes are 1024-based ("2 MB" = 2 MiB).
const MAX_ASSET_BYTES: usize = 2 * 1024 * 1024;
const MAX_DOC_BYTES: usize = 4 * 1024 * 1024;
const MAX_DIM: u32 = 2048;
const MAX_FRAMES: usize = 120;
const MAX_DECODED_BYTES: u64 = 32 * 1024 * 1024;
// De-facto browser rule: a 0ms frame is shown for 100ms (§4).
const ZERO_DURATION_MS: f64 = 100.0;

/// A decoded asset: premultiplied full-canvas frames and their durations. A
/// still is one frame with `loop_ms == 0`. Each frame's pixel dimensions live on
/// the Pixmap itself (`frames[0].width()`).
pub struct DecodedImage {
    pub frames: Vec<Arc<Pixmap>>,
    pub durations_ms: Vec<f64>,
    pub loop_ms: f64,
}

impl DecodedImage {
    pub fn animated(&self) -> bool {
        self.frames.len() > 1
    }

    /// The frame visible at `time` seconds under the §4 rule: `time mod
    /// loopDuration`, frame `i` shown across `[Σ<i, Σ≤i)`. Stills and
    /// zero-length loops show the first frame.
    pub fn sample(&self, time_seconds: f64) -> &Arc<Pixmap> {
        if self.frames.len() <= 1 || self.loop_ms <= 0.0 {
            return &self.frames[0];
        }
        let ms = (time_seconds * 1000.0).rem_euclid(self.loop_ms);
        let mut acc = 0.0;
        for (i, d) in self.durations_ms.iter().enumerate() {
            acc += d;
            if ms < acc {
                return &self.frames[i];
            }
        }
        self.frames.last().expect("non-empty frames")
    }
}

pub type DecodedAssets = HashMap<String, DecodedImage>;

/// Validate + decode every asset (§4). Key/dup/budget/signature failures are
/// hard errors; the returned map's keys are also the set `image` nodes resolve
/// against.
pub fn decode_all(assets: &[Asset]) -> Result<DecodedAssets, SchemaError> {
    let mut out: DecodedAssets = HashMap::new();
    let mut total_encoded = 0usize;

    for (index, asset) in assets.iter().enumerate() {
        let path = format!("assets[{index}]");
        if !valid_key(&asset.key) {
            return Err(SchemaError::new(
                format!("{path}.key"),
                format!("\"{}\" must match [a-z][a-zA-Z0-9]*", asset.key),
            ));
        }
        if out.contains_key(&asset.key) {
            return Err(SchemaError::new(
                format!("{path}.key"),
                format!("duplicate asset key \"{}\"", asset.key),
            ));
        }

        let bytes = base64::engine::general_purpose::STANDARD
            .decode(asset.data.as_bytes())
            .map_err(|e| {
                SchemaError::new(format!("{path}.data"), format!("malformed base64: {e}"))
            })?;

        if bytes.len() > MAX_ASSET_BYTES {
            return Err(SchemaError::new(
                format!("{path}.data"),
                format!(
                    "encoded asset is {} bytes, exceeds the {MAX_ASSET_BYTES}-byte per-asset limit",
                    bytes.len()
                ),
            ));
        }
        total_encoded += bytes.len();
        if total_encoded > MAX_DOC_BYTES {
            return Err(SchemaError::new(
                path,
                format!("encoded assets total {total_encoded} bytes, exceeds the {MAX_DOC_BYTES}-byte per-document limit"),
            ));
        }

        if sniff(&bytes) != Some(expected_container(asset.mime)) {
            return Err(SchemaError::new(
                format!("{path}.data"),
                format!(
                    "container signature does not match declared mime \"{}\"",
                    asset.mime.as_str()
                ),
            ));
        }

        let decoded = decode_image(asset.mime, &bytes, &path)?;
        out.insert(asset.key.clone(), decoded);
    }

    Ok(out)
}

fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

// --- container sniffing -------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Container {
    Png,
    Jpeg,
    Gif,
    Webp,
}

/// The container implied by the declared mime (png and apng share the PNG
/// container; they differ only by the presence of an `acTL` chunk).
fn expected_container(mime: ImageMime) -> Container {
    match mime {
        ImageMime::Png | ImageMime::Apng => Container::Png,
        ImageMime::Jpeg => Container::Jpeg,
        ImageMime::Gif => Container::Gif,
        ImageMime::Webp => Container::Webp,
    }
}

/// The container a byte string actually is, by magic number.
fn sniff(bytes: &[u8]) -> Option<Container> {
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.starts_with(PNG) {
        Some(Container::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Container::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(Container::Gif)
    } else if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(Container::Webp)
    } else {
        None
    }
}

// --- decode -------------------------------------------------------------------

fn decode_image(mime: ImageMime, bytes: &[u8], path: &str) -> Result<DecodedImage, SchemaError> {
    use image::codecs::{gif::GifDecoder, jpeg::JpegDecoder, png::PngDecoder, webp::WebPDecoder};

    match mime {
        ImageMime::Png => {
            let decoder =
                PngDecoder::new(Cursor::new(bytes)).map_err(|e| decode_err(path, mime, e))?;
            decode_still(decoder, mime, path)
        }
        ImageMime::Jpeg => {
            let decoder =
                JpegDecoder::new(Cursor::new(bytes)).map_err(|e| decode_err(path, mime, e))?;
            decode_still(decoder, mime, path)
        }
        ImageMime::Gif => {
            let decoder =
                GifDecoder::new(Cursor::new(bytes)).map_err(|e| decode_err(path, mime, e))?;
            decode_animated(decoder, mime, path)
        }
        ImageMime::Webp => {
            let mut decoder =
                WebPDecoder::new(Cursor::new(bytes)).map_err(|e| decode_err(path, mime, e))?;
            if decoder.has_animation() {
                // Dispose to a transparent background — kine always composites
                // onto a transparent canvas. Without this, image-webp skips the
                // dispose-to-background clear for non-alpha frames (its default
                // when no background is set), diverging from libwebp; setting it
                // also routes non-alpha disposal through the #178-fixed clear.
                decoder
                    .set_background_color(image::Rgba([0, 0, 0, 0]))
                    .map_err(|e| decode_err(path, mime, e))?;
                decode_animated(decoder, mime, path)
            } else {
                decode_still(decoder, mime, path)
            }
        }
        ImageMime::Apng => {
            let decoder =
                PngDecoder::new(Cursor::new(bytes)).map_err(|e| decode_err(path, mime, e))?;
            if decoder.is_apng().map_err(|e| decode_err(path, mime, e))? {
                let apng = decoder.apng().map_err(|e| decode_err(path, mime, e))?;
                decode_animated(apng, mime, path)
            } else {
                decode_still(decoder, mime, path)
            }
        }
    }
}

fn decode_still(
    decoder: impl ImageDecoder,
    mime: ImageMime,
    path: &str,
) -> Result<DecodedImage, SchemaError> {
    let (width, height) = decoder.dimensions();
    check_dims(width, height, path)?;
    let rgba = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| decode_err(path, mime, e))?
        .to_rgba8();
    let pixmap = premultiply(&rgba);
    Ok(DecodedImage {
        frames: vec![Arc::new(pixmap)],
        durations_ms: vec![0.0],
        loop_ms: 0.0,
    })
}

fn decode_animated<'a>(
    decoder: impl AnimationDecoder<'a>,
    mime: ImageMime,
    path: &str,
) -> Result<DecodedImage, SchemaError> {
    let mut frames: Vec<Arc<Pixmap>> = Vec::new();
    let mut durations_ms: Vec<f64> = Vec::new();
    let mut decoded_bytes = 0u64;

    // The `image` facade exposes no animated frame count from the header, so the
    // frame and decoded-size caps are enforced during iteration. Each frame is
    // decoded then checked before the next is pulled, so peak memory is bounded
    // to the budget plus one in-flight frame — not a decode bomb.
    for frame_result in decoder.into_frames() {
        let frame = frame_result.map_err(|e| decode_err(path, mime, e))?;
        if frames.len() >= MAX_FRAMES {
            return Err(SchemaError::new(
                path,
                format!("animated asset exceeds the {MAX_FRAMES}-frame limit"),
            ));
        }
        let delay = frame.delay();
        let (numer, denom) = delay.numer_denom_ms();
        let ms = if denom == 0 {
            0.0
        } else {
            numer as f64 / denom as f64
        };
        let buffer = frame.into_buffer();
        let (fw, fh) = buffer.dimensions();
        if frames.is_empty() {
            check_dims(fw, fh, path)?;
        }
        decoded_bytes += fw as u64 * fh as u64 * 4;
        if decoded_bytes > MAX_DECODED_BYTES {
            return Err(SchemaError::new(
                path,
                format!(
                    "decoded animation exceeds the {MAX_DECODED_BYTES}-byte (frames x w x h x 4) limit"
                ),
            ));
        }
        frames.push(Arc::new(premultiply(&buffer)));
        durations_ms.push(if ms <= 0.0 { ZERO_DURATION_MS } else { ms });
    }

    if frames.is_empty() {
        return Err(SchemaError::new(
            path,
            "animated asset decoded to zero frames",
        ));
    }
    let loop_ms = durations_ms.iter().sum();
    Ok(DecodedImage {
        frames,
        durations_ms,
        loop_ms,
    })
}

fn check_dims(width: u32, height: u32, path: &str) -> Result<(), SchemaError> {
    if width == 0 || height == 0 {
        return Err(SchemaError::new(path, "image has a zero dimension"));
    }
    if width > MAX_DIM || height > MAX_DIM {
        return Err(SchemaError::new(
            path,
            format!("image is {width}x{height}, exceeds the {MAX_DIM}x{MAX_DIM} limit"),
        ));
    }
    Ok(())
}

/// Straight-alpha RGBA8 → premultiplied Pixmap (vello wants premultiplied; every
/// pure-Rust decoder hands back straight alpha, so this pass is mandatory).
fn premultiply(rgba: &image::RgbaImage) -> Pixmap {
    let (width, height) = rgba.dimensions();
    let pixels: Vec<u8> = rgba
        .pixels()
        .flat_map(|pixel| {
            let [r, g, b, a] = pixel.0;
            let alpha = a as u16;
            let mul = |c: u8| ((alpha * c as u16) / 255) as u8;
            [mul(r), mul(g), mul(b), a]
        })
        .collect();
    Pixmap::from_parts(
        pixels,
        width as u16,
        height as u16,
        PixelMetadata::new(ImageAlphaType::AlphaPremultiplied, true),
    )
}

fn decode_err(path: &str, mime: ImageMime, error: impl std::fmt::Display) -> SchemaError {
    SchemaError::new(
        format!("{path}.data"),
        format!("failed to decode {} asset: {error}", mime.as_str()),
    )
}
