//! The GPU raster flavor: `vello_hybrid` over wgpu/Metal. Behind the `gpu`
//! feature, which the ruby gem and the rails server never enable — they build
//! the CPU flavor and must not pull the wgpu tree.
//!
//! This module owns NO drawing. The scene walk lives once in [`crate::render`]
//! and is generic over [`Canvas`]; all that is here is the wgpu venue, an image
//! atlas, and the [`Canvas`] impl that routes `vello_hybrid`.
//!
//! **The engine renders on the HOST's device and queue.** `from_metal` builds
//! wgpu directly on the `MTLDevice`/`MTLCommandQueue` the app hands over, so
//! the texture kine writes and the texture the compositor samples are the same
//! device's by construction, and both sit on one queue — no cross-queue fence,
//! and no blocking wait, is needed to order them.

use std::sync::{Arc, Weak};

use vello_common::paint::{Image, ImageId, ImageSource, PaintType};
use vello_common::pixmap::Pixmap;
use vello_cpu::kurbo::{Affine, BezPath, Rect, Stroke};
use vello_cpu::peniko::ImageSampler;
use vello_cpu::Glyph;
use vello_common::filter_effects::Filter;

use crate::eval::Scene;
use crate::render::{self, Canvas, RunStyle};

/// The one texture format kine renders: premultiplied RGBA8, sRGB — byte-shaped
/// exactly like the CPU flavor's `Pixmap`, so hosts need no format branch.
pub(crate) const TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Total atlas area, in texels, that `vello_hybrid::Resources::new()` can hand
/// out. Derived from `AtlasConfig::default()` because `Resources` gives no way
/// to read (or set) its own config in 0.0.9 — if that constructor ever takes a
/// config, this must follow it or the occupancy report goes silently wrong.
fn atlas_capacity() -> u64 {
    let config = vello_hybrid::AtlasConfig::default();
    let (width, height) = config.atlas_size;
    u64::from(width) * u64::from(height) * config.max_atlases as u64
}

/// Report occupancy from here on. Ruled at ~75%: high enough not to cry wolf,
/// early enough that a host sees pressure before documents start falling back
/// to the CPU flavor.
const ATLAS_WARN_FRACTION: f64 = 0.75;

/// Re-arm the warning only once occupancy has fallen well clear of the
/// threshold. Without this band, a workload sitting near 75% — one asset
/// evicted, one uploaded, repeatedly — would warn on every single frame.
const ATLAS_REARM_FRACTION: f64 = 0.60;

/// One atlas-resident decoded frame.
struct CachedImage {
    /// Weak, not raw: a live `Weak` pins the allocation, so the address behind
    /// `key` can never be recycled by a different pixmap while this entry
    /// lives. A dead weak means the asset is gone — evict, don't serve stale
    /// pixels for a document that no longer exists.
    pixmap: Weak<Pixmap>,
    key: usize,
    id: ImageId,
    /// Padded texels this entry holds — subtracted again on eviction.
    area: u64,
}

/// Process-wide GPU raster engine: wgpu device/queue + one `vello_hybrid`
/// renderer + the image atlas. Scenes stay per-document, per-render.
pub(crate) struct GpuEngine {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Built on first render, at that render's size, and reused after — the
    /// renderer re-configures itself when `RenderSize` changes, so one instance
    /// serves every document size.
    renderer: Option<vello_hybrid::Renderer>,
    resources: vello_hybrid::Resources,
    images: Vec<CachedImage>,
    /// Padded atlas area held by live entries, and whether the occupancy warning
    /// has already fired for the current climb.
    atlas_used: u64,
    atlas_warned: bool,
    /// Warnings this engine has emitted. Test-only seam: the log sink is
    /// process-global and shared with concurrently-running tests, so "warned
    /// exactly once" is only assertable per-engine.
    #[cfg(test)]
    atlas_warnings: usize,
    /// Description of the adapter actually in use — reported through the log
    /// sink at init so a wrong-device or software-fallback situation is never
    /// silent.
    adapter: String,
}

impl GpuEngine {
    /// Build on wgpu's own device — the parity harness and any host that has no
    /// device to lend. NOT the shipping path on iOS; see [`Self::from_metal`].
    pub(crate) fn new() -> Result<Self, String> {
        let instance = instance();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .map_err(|e| format!("no GPU adapter: {e}"))?;
        let info = adapter.get_info();
        let (device, queue) = pollster::block_on(adapter.request_device(&device_descriptor()))
            .map_err(|e| format!("GPU device request failed: {e}"))?;
        Ok(Self::assemble(device, queue, describe(&info)))
    }

    /// Build on the HOST's `MTLDevice` and `MTLCommandQueue` (raw, non-owning
    /// pointers — both are retained for the engine's lifetime).
    ///
    /// # Safety
    /// `device` must be a live `id<MTLDevice>` and `queue` a live
    /// `id<MTLCommandQueue>` created from it.
    #[cfg(target_vendor = "apple")]
    pub(crate) unsafe fn from_metal(
        device: *mut std::ffi::c_void,
        queue: *mut std::ffi::c_void,
    ) -> Result<Self, String> {
        use objc2::rc::Retained;
        use objc2::runtime::ProtocolObject;
        use objc2_metal::{MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice};

        if device.is_null() || queue.is_null() {
            return Err("metal device/queue pointer is null".to_string());
        }
        // Retain: the host owns these, we only borrow for the engine's life.
        let raw_device: Retained<ProtocolObject<dyn MTLDevice>> =
            unsafe { Retained::retain(device.cast()) }
                .ok_or_else(|| "metal device pointer is not retainable".to_string())?;
        let raw_queue: Retained<ProtocolObject<dyn MTLCommandQueue>> =
            unsafe { Retained::retain(queue.cast()) }
                .ok_or_else(|| "metal queue pointer is not retainable".to_string())?;

        // Never silently cross devices (dispatch hard constraint). wgpu validates
        // against an ENUMERATED adapter while rendering on the device handed in
        // here, so the two must describe the same GPU. On iOS there is exactly
        // one; a mismatch means the host handed us a device from elsewhere.
        let system = MTLCreateSystemDefaultDevice()
            .ok_or_else(|| "no system default MTLDevice".to_string())?;
        if !std::ptr::eq(
            Retained::as_ptr(&raw_device).cast::<u8>(),
            Retained::as_ptr(&system).cast::<u8>(),
        ) {
            return Err(format!(
                "host MTLDevice \"{}\" is not the system default \"{}\" — refusing to cross devices",
                raw_device.name(),
                system.name(),
            ));
        }

        let instance = instance();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .map_err(|e| format!("no Metal adapter: {e}"))?;
        let info = adapter.get_info();
        let name = raw_device.name().to_string();

        // Timestamps are never queried, so the period is inert; 1.0 is the
        // value wgpu-hal itself uses for Apple silicon.
        let hal_device =
            unsafe { wgpu::hal::metal::Device::device_from_raw(raw_device, wgpu::Features::empty()) };
        let hal_queue = unsafe { wgpu::hal::metal::Queue::queue_from_raw(raw_queue, 1.0) };
        let (device, queue) = unsafe {
            adapter.create_device_from_hal::<wgpu::hal::api::Metal>(
                wgpu::hal::OpenDevice {
                    device: hal_device,
                    queue: hal_queue,
                },
                &device_descriptor(),
            )
        }
        .map_err(|e| format!("GPU device from host Metal handles failed: {e}"))?;

        Ok(Self::assemble(
            device,
            queue,
            format!("{} (host device+queue, {})", name, describe(&info)),
        ))
    }

    fn assemble(device: wgpu::Device, queue: wgpu::Queue, adapter: String) -> Self {
        crate::log::emit(crate::log::INFO, &format!("gpu engine on {adapter}"));
        Self {
            device,
            queue,
            renderer: None,
            resources: vello_hybrid::Resources::new(),
            images: Vec::new(),
            atlas_used: 0,
            atlas_warned: false,
            #[cfg(test)]
            atlas_warnings: 0,
            adapter,
        }
    }

    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// What the engine is actually running on — reported so a wrong-device or
    /// software-fallback situation is never silent.
    pub(crate) fn adapter(&self) -> &str {
        &self.adapter
    }

    /// Live atlas entries — the seam the eviction test reads.
    #[cfg(test)]
    pub(crate) fn cached_image_count(&self) -> usize {
        self.images.len()
    }

    /// Release atlas slots whose decoded frame is gone. Two reasons this can
    /// never be folded into the draw walk:
    ///  - the key is the pixmap's ADDRESS, and a freed allocation can be reused
    ///    by the next document — a surviving entry would then serve the previous
    ///    document's picture;
    ///  - `destroy_image` clears the atlas region through the COMMAND ENCODER
    ///    while uploads are staged through `queue.write_texture`, and staged
    ///    writes land before the submitted buffer runs. Clearing in the same
    ///    encoder as a frame's uploads therefore wipes what that frame just
    ///    uploaded. Its own encoder, submitted first, orders it unambiguously.
    fn evict_dead_images(&mut self) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let mut dead = Vec::new();
        let mut reclaimed = 0u64;
        self.images.retain(|cached| {
            let alive = cached.pixmap.strong_count() > 0;
            if !alive {
                dead.push(cached.id);
                reclaimed += cached.area;
            }
            alive
        });
        self.atlas_used = self.atlas_used.saturating_sub(reclaimed);
        if dead.is_empty() {
            return;
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kine gpu atlas evict"),
            });
        for id in dead {
            renderer.destroy_image(
                &mut self.resources,
                &self.device,
                &self.queue,
                &mut encoder,
                id,
            );
        }
        self.queue.submit([encoder.finish()]);
    }

    /// Rasterize `scene` into `view` (must be a [`TEXTURE_FORMAT`] target of
    /// exactly `width`×`height`). Submits and RETURNS — it does not wait for the
    /// GPU. Ordering against the host's own draws is the shared queue's job.
    pub(crate) fn render_to_view(
        &mut self,
        scene: &Scene,
        width: u32,
        height: u32,
        viewport: (f64, f64),
        view: &wgpu::TextureView,
    ) -> Result<(), String> {
        validate_request(width, height, viewport)?;
        self.evict_dead_images();

        if self.renderer.is_none() {
            self.renderer = Some(vello_hybrid::Renderer::new(
                &self.device,
                &vello_hybrid::RenderTargetConfig {
                    format: TEXTURE_FORMAT,
                    width,
                    height,
                },
            ));
        }
        // Split the borrows: the canvas holds renderer/resources/images while it
        // builds, then drops before the render pass reclaims them.
        let renderer = self.renderer.as_mut().expect("renderer built above");

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kine gpu"),
            });
        let mut hybrid = vello_hybrid::Scene::new(width as u16, height as u16);
        {
            let mut canvas = GpuCanvas {
                scene: &mut hybrid,
                resources: &mut self.resources,
                renderer,
                device: &self.device,
                queue: &self.queue,
                encoder: &mut encoder,
                images: &mut self.images,
                error: None,
            };
            render::draw_node(
                &mut canvas,
                &scene.root,
                render::root_transform(scene, width, height, viewport),
            )?;
            if let Some(message) = canvas.error {
                return Err(message);
            }
        }

        renderer
            .render(
                &hybrid,
                &mut self.resources,
                &self.device,
                &self.queue,
                &mut encoder,
                &vello_hybrid::RenderSize { width, height },
                view,
                &vello_hybrid::TextureBindings::new(),
            )
            .map_err(|e| format!("gpu render failed: {e}"))?;
        self.queue.submit([encoder.finish()]);
        self.report_occupancy();
        Ok(())
    }

    /// Announce atlas pressure through the log sink once per climb past the
    /// ruled threshold. Deliberately NOT an eviction policy: entries are held by
    /// the documents that own them, so the host — which knows which projects are
    /// live — is the one that can act. Silence here would mean the first sign of
    /// pressure is documents quietly falling back to the CPU flavor.
    fn report_occupancy(&mut self) {
        // Live entries are the truth; the canvas records each entry's area as it
        // uploads, so this is a sum over at most a few dozen rows.
        self.atlas_used = self.images.iter().map(|cached| cached.area).sum();
        let capacity = atlas_capacity();
        if capacity == 0 {
            return;
        }
        let fraction = self.atlas_used as f64 / capacity as f64;
        if fraction < ATLAS_REARM_FRACTION {
            self.atlas_warned = false;
        }
        if self.atlas_warned || fraction < ATLAS_WARN_FRACTION {
            return;
        }
        self.atlas_warned = true;
        #[cfg(test)]
        {
            self.atlas_warnings += 1;
        }
        crate::log::emit(
            crate::log::WARN,
            &format!(
                "gpu image atlas {:.0}% full ({} of {} texels across {} live assets) — further image documents will fall back to the CPU flavor",
                fraction * 100.0,
                self.atlas_used,
                capacity,
                self.images.len(),
            ),
        );
    }

    /// Atlas occupancy as a fraction of capacity — the seam the warning test
    /// reads.
    #[cfg(test)]
    pub(crate) fn atlas_occupancy(&self) -> f64 {
        self.atlas_used as f64 / atlas_capacity() as f64
    }

    #[cfg(test)]
    pub(crate) fn atlas_warning_count(&self) -> usize {
        self.atlas_warnings
    }

    /// Rasterize into a texture the HOST created and owns. kine borrows it for
    /// the render and never retains it past the call; the host's `MTLTexture`
    /// refcount is untouched.
    ///
    /// The texture must be `MTLPixelFormatRGBA8Unorm`, exactly `width`×`height`,
    /// and carry the render-target usage — all three are checked, because
    /// handing wgpu a mismatched texture is undefined behavior, not an error.
    ///
    /// # Safety
    /// `texture` must be a live `id<MTLTexture>` created by the same device the
    /// engine was built on.
    #[cfg(target_vendor = "apple")]
    pub(crate) unsafe fn render_to_metal_texture(
        &mut self,
        scene: &Scene,
        width: u32,
        height: u32,
        viewport: (f64, f64),
        texture: *mut std::ffi::c_void,
    ) -> Result<(), String> {
        use objc2::rc::Retained;
        use objc2::runtime::ProtocolObject;
        use objc2_metal::{MTLPixelFormat, MTLTexture, MTLTextureUsage};

        validate_request(width, height, viewport)?;
        if texture.is_null() {
            return Err("metal texture pointer is null".to_string());
        }
        let raw: Retained<ProtocolObject<dyn MTLTexture>> =
            unsafe { Retained::retain(texture.cast()) }
                .ok_or_else(|| "metal texture pointer is not retainable".to_string())?;

        if raw.pixelFormat() != MTLPixelFormat::RGBA8Unorm {
            return Err(format!(
                "target texture is {:?}, kine renders MTLPixelFormatRGBA8Unorm",
                raw.pixelFormat()
            ));
        }
        if raw.width() as u32 != width || raw.height() as u32 != height {
            return Err(format!(
                "target texture is {}x{}, render was asked for {width}x{height}",
                raw.width(),
                raw.height()
            ));
        }
        if !raw.usage().contains(MTLTextureUsage::RenderTarget) {
            return Err(
                "target texture lacks MTLTextureUsageRenderTarget — kine renders into it"
                    .to_string(),
            );
        }

        let hal_texture = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                raw,
                TEXTURE_FORMAT,
                objc2_metal::MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent {
                    width,
                    height,
                    depth: 1,
                },
            )
        };
        let wrapped = unsafe {
            self.device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("kine gpu host target"),
                    size: wgpu::Extent3d {
                        width,
                        height,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: TEXTURE_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
            )
        };
        let view = wrapped.create_view(&wgpu::TextureViewDescriptor::default());
        self.render_to_view(scene, width, height, viewport, &view)
    }

    /// Rasterize and read the pixels back — premultiplied RGBA8, row-major,
    /// stride `width * 4`, byte-shaped exactly like [`render::render_rgba`].
    ///
    /// This BLOCKS on GPU completion (S0 measured ~1.5 ms of round trip for that
    /// alone), so it is the parity harness's entry point, not a frame path.
    pub(crate) fn render_rgba(
        &mut self,
        scene: &Scene,
        width: u32,
        height: u32,
        viewport: (f64, f64),
    ) -> Result<Vec<u8>, String> {
        // Before ANY wgpu resource exists: an out-of-range size is kine's error
        // to report, not a wgpu validation panic thrown from inside a texture
        // descriptor.
        validate_request(width, height, viewport)?;

        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("kine gpu readback target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.render_to_view(scene, width, height, viewport, &view)?;

        // Buffer rows are 256-aligned per wgpu; the tight rows are re-cut below.
        let padded = (width * 4).next_multiple_of(256);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("kine gpu readback"),
            size: padded as u64 * height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kine gpu readback"),
            });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);

        let (sender, receiver) = std::sync::mpsc::channel();
        buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| format!("gpu readback poll failed: {e}"))?;
        receiver
            .recv()
            .map_err(|_| "gpu readback callback never fired".to_string())?
            .map_err(|e| format!("gpu readback map failed: {e}"))?;

        let mut out = Vec::with_capacity(width as usize * height as usize * 4);
        for row in buffer
            .slice(..)
            .get_mapped_range()
            .chunks_exact(padded as usize)
        {
            out.extend_from_slice(&row[..width as usize * 4]);
        }
        buffer.unmap();
        Ok(out)
    }
}

/// The same admission the CPU flavor applies, plus the viewport check — run at
/// every GPU entry point BEFORE a wgpu resource is created, so a bad request is
/// kine's error rather than a wgpu validation panic.
fn validate_request(width: u32, height: u32, viewport: (f64, f64)) -> Result<(), String> {
    render::validate_size(width, height)?;
    let (view_y, view_h) = viewport;
    if view_h <= 0.0 || !view_y.is_finite() || !view_h.is_finite() {
        return Err(format!("invalid viewport y={view_y} h={view_h}"));
    }
    Ok(())
}

fn instance() -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::METAL;
    wgpu::Instance::new(descriptor)
}

fn device_descriptor() -> wgpu::DeviceDescriptor<'static> {
    wgpu::DeviceDescriptor {
        label: Some("kine"),
        required_features: wgpu::Features::empty(),
        ..Default::default()
    }
}

fn describe(info: &wgpu::AdapterInfo) -> String {
    format!("{:?}/{}", info.backend, info.name)
}

/// The `vello_hybrid` flavor. Holds the wgpu handles because image paints must
/// reach the atlas mid-walk — see [`Canvas::set_paint_image`].
struct GpuCanvas<'a> {
    scene: &'a mut vello_hybrid::Scene,
    resources: &'a mut vello_hybrid::Resources,
    renderer: &'a mut vello_hybrid::Renderer,
    device: &'a wgpu::Device,
    queue: &'a wgpu::Queue,
    encoder: &'a mut wgpu::CommandEncoder,
    images: &'a mut Vec<CachedImage>,
    /// Parked failure from `set_paint_image`, which the trait gives no way to
    /// return. Read once the walk finishes.
    error: Option<String>,
}

impl GpuCanvas<'_> {
    /// Atlas id for a decoded frame, uploading on first sight. Cheap on repeat:
    /// a scrub re-renders the same asset every frame and must not re-upload it.
    /// Atlas id for a decoded frame, uploading on first sight.
    ///
    /// `Renderer::upload_image` UNWRAPS the allocation
    /// (`vello_hybrid/src/render/wgpu.rs:596`), so an exhausted atlas is a panic
    /// inside the renderer, not an error — measured: 32 live 2048×2048 assets,
    /// then `called Result::unwrap() on an Err value: AtlasLimitReached`. kine
    /// does not let that cross its boundary: the upload is caught and reported,
    /// the render fails cleanly, and the host falls back to the CPU flavor.
    ///
    /// Catching is sound here because `allocate` fails BEFORE it mutates the
    /// cache (it returns `Err` from the atlas manager and the slot vector is
    /// never touched), so the engine is unchanged and stays usable — asserted by
    /// `atlas_exhaustion_is_an_error_not_a_panic`.
    fn atlas_id(&mut self, pixmap: &Arc<Pixmap>) -> Result<ImageId, String> {
        let key = Arc::as_ptr(pixmap) as usize;
        if let Some(cached) = self.images.iter().find(|cached| cached.key == key) {
            return Ok(cached.id);
        }
        let renderer = &mut *self.renderer;
        let resources = &mut *self.resources;
        let encoder = &mut *self.encoder;
        let device = self.device;
        let queue = self.queue;
        let id = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            renderer.upload_image(resources, device, queue, encoder, pixmap)
        }))
        .map_err(|_| {
            // The caught panic still reaches stderr through the default hook
            // (replacing that hook is process-global and racy with concurrent
            // renders), so report it through kine's own sink too — the host
            // learns why it just dropped to the CPU flavor.
            let message = format!(
                "gpu image atlas is full — cannot upload a {}x{} frame",
                pixmap.width(),
                pixmap.height()
            );
            crate::log::emit(crate::log::ERROR, &message);
            message
        })?;
        // GLYPH_PADDING-style transparent padding rides every atlas allocation;
        // vello_hybrid's IMAGE_PADDING is 1 texel per side.
        let area = (u64::from(pixmap.width()) + 2) * (u64::from(pixmap.height()) + 2);
        self.images.push(CachedImage {
            pixmap: Arc::downgrade(pixmap),
            key,
            id,
            area,
        });
        Ok(id)
    }
}

impl Canvas for GpuCanvas<'_> {
    fn set_transform(&mut self, affine: Affine) {
        self.scene.set_transform(affine);
    }

    fn set_paint(&mut self, paint: impl Into<PaintType>) {
        self.scene.set_paint(paint);
    }

    fn set_paint_image(&mut self, pixmap: &Arc<Pixmap>, sampler: ImageSampler) {
        // The trait returns nothing (the CPU flavor cannot fail here), so a
        // failed upload is parked and read by `render_to_view` after the walk —
        // the render fails as a whole rather than silently dropping an image.
        match self.atlas_id(pixmap) {
            Ok(id) => self.scene.set_paint(Image {
                image: ImageSource::opaque_id(id),
                sampler,
            }),
            // First failure wins; later ones are almost certainly the same cause.
            Err(message) => {
                self.error.get_or_insert(message);
            }
        }
    }

    fn set_paint_transform(&mut self, affine: Affine) {
        self.scene.set_paint_transform(affine);
    }

    fn set_stroke(&mut self, stroke: Stroke) {
        self.scene.set_stroke(stroke);
    }

    fn fill_path(&mut self, path: &BezPath) {
        self.scene.fill_path(path);
    }

    fn stroke_path(&mut self, path: &BezPath) {
        self.scene.stroke_path(path);
    }

    fn fill_rect(&mut self, rect: &Rect) {
        self.scene.fill_rect(rect);
    }

    fn push_opacity_layer(&mut self, opacity: f32) {
        self.scene.push_opacity_layer(opacity);
    }

    fn push_clip_layer(&mut self, path: &BezPath) {
        self.scene.push_clip_layer(path);
    }

    fn push_filter_layer(&mut self, filter: Option<Filter>) {
        self.scene.push_layer(None, None, None, None, filter);
    }

    fn pop_layer(&mut self) {
        self.scene.pop_layer();
    }

    fn draw_glyph(&mut self, run: &RunStyle, glyph: Glyph, linear: Affine, stroke: bool) {
        let builder = self
            .scene
            .glyph_run(self.resources, &run.font)
            .font_size(run.size)
            .normalized_coords(&run.coords)
            .glyph_transform(linear)
            .hint(false);
        let glyphs = std::iter::once(glyph);
        if stroke {
            builder.stroke_glyphs(glyphs)
        } else {
            builder.fill_glyphs(glyphs)
        }
    }
}

// --- engine registry --------------------------------------------------------
//
// Mirrors `handle.rs`: an opaque i64 names a process-global engine, ids are
// monotonic and never reused, and a freed id is simply absent (use-after-free
// reports an error, never a crash). Unlike documents an engine is MUTABLE — the
// atlas and the renderer are per-engine state — so renders serialize on its
// mutex. One engine, one Metal queue, one raster at a time by design.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};

static ENGINES: OnceLock<RwLock<HashMap<i64, Arc<Mutex<GpuEngine>>>>> = OnceLock::new();
// Starts at 1 so 0 is always the error/invalid sentinel.
static NEXT_ENGINE_ID: AtomicI64 = AtomicI64::new(1);

fn engines() -> &'static RwLock<HashMap<i64, Arc<Mutex<GpuEngine>>>> {
    ENGINES.get_or_init(|| RwLock::new(HashMap::new()))
}

pub(crate) fn insert_engine(engine: GpuEngine) -> i64 {
    let id = NEXT_ENGINE_ID.fetch_add(1, Ordering::Relaxed);
    engines()
        .write()
        .expect("engine registry poisoned")
        .insert(id, Arc::new(Mutex::new(engine)));
    id
}

/// The `Arc` keeps the engine alive for a whole render even if another thread
/// destroys the handle concurrently.
pub(crate) fn get_engine(id: i64) -> Option<Arc<Mutex<GpuEngine>>> {
    engines().read().ok()?.get(&id).cloned()
}

pub(crate) fn remove_engine(id: i64) {
    if let Ok(mut map) = engines().write() {
        map.remove(&id);
    }
}

#[cfg(test)]
pub(crate) fn engine_exists(id: i64) -> bool {
    engines()
        .read()
        .map(|map| map.contains_key(&id))
        .unwrap_or(false)
}
