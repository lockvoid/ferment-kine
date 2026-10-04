//! The GPU raster flavor: `vello_gpu` over wgpu/Metal. Behind the `gpu`
//! feature, which the ruby gem and the rails server never enable — they build
//! the CPU flavor and must not pull the wgpu tree.
//!
//! This module owns NO drawing. The scene walk lives once in [`crate::render`]
//! and is generic over [`Canvas`]; all that is here is the wgpu venue, an image
//! atlas, and the [`Canvas`] impl that routes `vello_gpu`.
//!
//! **The engine renders on the HOST's device and queue.** `from_metal` builds
//! wgpu directly on the `MTLDevice`/`MTLCommandQueue` the app hands over, so
//! the texture kine writes and the texture the compositor samples are the same
//! device's by construction, and both sit on one queue — no cross-queue fence,
//! and no blocking wait, is needed to order them.
//!
//! The `gpu-gles` flavor is the same venue on Android: `from_current_gles`
//! builds wgpu's GLES backend on the EGL context CURRENT on the calling thread,
//! and renders into a GL texture the host names. One context executes kine's
//! commands and the host's in issue order, so again nothing waits.

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

/// Total atlas area, in texels, that the engine's `Resources` can hand out —
/// the image atlas config [`render_settings`] gives the renderer, which makes
/// the `Resources` it hands back.
fn atlas_capacity() -> u64 {
    let config = render_settings().memory_settings.image_atlas_config;
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

/// Process-wide GPU raster engine: wgpu device/queue + one `vello_gpu`
/// renderer + the image atlas. Scenes stay per-document, per-render.
pub(crate) struct GpuEngine {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Built on first render, at that render's size, and reused after — the
    /// renderer re-configures itself when `RenderSize` changes, so one instance
    /// serves every document size.
    renderer: Option<vello_gpu::Renderer>,
    /// Made with the renderer: vello hands the two out as a pair.
    resources: Option<vello_gpu::Resources>,
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
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    adapter: String,
    /// The host context around a render — set by [`Self::from_current_gles`]
    /// only.
    #[cfg(feature = "gpu-gles")]
    gl: Option<gl_state::HostGl>,
}

impl GpuEngine {
    /// Build on wgpu's own device — the parity harness and any host that has no
    /// device to lend. NOT the shipping path on iOS; see [`Self::from_metal`].
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub(crate) fn new() -> Result<Self, String> {
        let instance = instance();
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
            ..Default::default()
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
            ..Default::default()
        }))
        .map_err(|e| format!("no Metal adapter: {e}"))?;
        let info = adapter.get_info();
        let name = raw_device.name().to_string();

        // Timestamps are never queried, so the period is inert; 1.0 is the
        // value wgpu-hal itself uses for Apple silicon.
        let descriptor = device_descriptor();
        let hal_device = unsafe {
            wgpu::hal::metal::Device::device_from_raw(
                raw_device,
                wgpu::Features::empty(),
                &descriptor.required_limits,
            )
        };
        let hal_queue = unsafe { wgpu::hal::metal::Queue::queue_from_raw(raw_queue, 1.0) };
        let (device, queue) = unsafe {
            adapter.create_device_from_hal::<wgpu::hal::api::Metal>(
                wgpu::hal::OpenDevice {
                    device: hal_device,
                    queue: hal_queue,
                },
                &descriptor,
            )
        }
        .map_err(|e| format!("GPU device from host Metal handles failed: {e}"))?;

        Ok(Self::assemble(
            device,
            queue,
            format!("{} (host device+queue, {})", name, describe(&info)),
        ))
    }

    /// Build on the GLES context CURRENT on the calling thread (the host's
    /// EGL context). Every later call into the engine — renders and the final
    /// drop — must come from a thread where that same context is current.
    ///
    /// # Safety
    /// An EGL context must be current on the calling thread.
    #[cfg(feature = "gpu-gles")]
    pub(crate) unsafe fn from_current_gles() -> Result<Self, String> {
        let pending = unsafe { gl_state::take_errors() }?;
        if !pending.is_empty() {
            crate::log::emit(
                crate::log::WARN,
                &format!("GL {} pending before kine's engine — the host's, cleared", gl_state::describe(&pending)),
            );
        }
        let exposed = unsafe {
            wgpu::hal::gles::Adapter::new_external(gl_loader::proc_address, wgpu::GlBackendOptions::default())
        }
        .ok_or_else(|| "no GLES adapter on the current context — is an EGL context current?".to_string())?;
        let instance = instance();
        let adapter = unsafe { instance.create_adapter_from_hal(exposed) };
        let info = adapter.get_info();
        let limits = wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits());
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("kine"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            ..Default::default()
        }))
        .map_err(|e| format!("GPU device on the host GLES context failed: {e}"))?;
        // wgpu's capability probe asks for parameters a driver may not know —
        // GL_INVALID_ENUM, nothing to act on. Anything else is a setup that failed.
        let probed = unsafe { gl_state::take_errors() }?;
        if probed.iter().any(|&error| error != gl_state::INVALID_ENUM) {
            return Err(format!("GL {} creating the GPU device", gl_state::describe(&probed)));
        }
        if !probed.is_empty() {
            crate::log::emit(
                crate::log::INFO,
                &format!("GL {} from wgpu's capability probe — cleared", gl_state::describe(&probed)),
            );
        }
        let gl = unsafe { gl_state::HostGl::adopt(&device.limits()) }?;
        let mut engine = Self::assemble(
            device,
            queue,
            format!("host GLES context, {}", describe(&info)),
        );
        engine.gl = Some(gl);
        Ok(engine)
    }

    fn assemble(device: wgpu::Device, queue: wgpu::Queue, adapter: String) -> Self {
        crate::log::emit(crate::log::INFO, &format!("gpu engine on {adapter}"));
        Self {
            device,
            queue,
            renderer: None,
            resources: None,
            images: Vec::new(),
            atlas_used: 0,
            atlas_warned: false,
            #[cfg(test)]
            atlas_warnings: 0,
            adapter,
            #[cfg(feature = "gpu-gles")]
            gl: None,
        }
    }

    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
    pub(crate) fn device(&self) -> &wgpu::Device {
        &self.device
    }

    /// What the engine is actually running on — reported so a wrong-device or
    /// software-fallback situation is never silent.
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
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
        let (Some(renderer), Some(resources)) = (self.renderer.as_mut(), self.resources.as_mut())
        else {
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
            renderer.destroy_image(resources, &mut encoder, id);
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
            let (renderer, resources) = vello_gpu::Renderer::new_with(
                &self.device,
                &vello_gpu::RenderTargetConfig {
                    format: TEXTURE_FORMAT,
                    width: width as u16,
                    height: height as u16,
                },
                render_settings(),
            );
            self.renderer = Some(renderer);
            self.resources = Some(resources);
        }
        // Split the borrows: the canvas holds renderer/resources/images while it
        // builds, then drops before the render pass reclaims them.
        let renderer = self.renderer.as_mut().expect("renderer built above");
        let resources = self.resources.as_mut().expect("built with the renderer");

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("kine gpu"),
            });
        let mut hybrid = vello_gpu::Scene::new(width as u16, height as u16);
        {
            let mut canvas = GpuCanvas {
                scene: &mut hybrid,
                resources: &mut *resources,
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
                resources,
                &self.device,
                &self.queue,
                &mut encoder,
                &vello_gpu::RenderSize {
                    width: width as u16,
                    height: height as u16,
                },
                view,
                None,
                &vello_gpu::TextureBindings::new(),
                vello_gpu::TargetInit::Clear(vello_gpu::ClearSettings::default()),
            )
            .map_err(|e| format!("gpu render failed: {e}"))?;
        self.queue.submit([encoder.finish()]);
        // GLES executes the submission on this thread as it is issued; a
        // non-blocking poll lets wgpu retire the staging buffers it used.
        #[cfg(feature = "gpu-gles")]
        let _ = self.device.poll(wgpu::PollType::Poll);
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
                None,
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
                // The render clears the target, so its contents on the way in
                // are discardable.
                wgpu::TextureUses::UNINITIALIZED,
            )
        };
        let view = wrapped.create_view(&wgpu::TextureViewDescriptor::default());
        self.render_to_view(scene, width, height, viewport, &view)
    }

    /// Rasterize into a GL texture the HOST created and owns, named by `texture`.
    /// kine borrows it for the render and never deletes it.
    ///
    /// The texture must be a complete `GL_TEXTURE_2D` of `GL_RGBA8`, exactly
    /// `width`×`height`, one level. GL cannot answer those questions without a
    /// stall, so — unlike the Metal path — they are the caller's contract.
    ///
    /// On return — success or failure — the context is back at GLES defaults
    /// for everything the render bound or enabled ([`gl_state::HostGl::leave`]),
    /// and the GL errors the render raised are read and cleared: any one fails
    /// it, since a target a pass could not draw into holds garbage.
    ///
    /// # Safety
    /// The context the engine was built on must be current, and `texture` must
    /// name a texture of that context (or its share group) matching the above.
    #[cfg(feature = "gpu-gles")]
    pub(crate) unsafe fn render_to_gl_texture(
        &mut self,
        scene: &Scene,
        width: u32,
        height: u32,
        viewport: (f64, f64),
        texture: u32,
    ) -> Result<(), String> {
        validate_request(width, height, viewport)?;
        let name = std::num::NonZeroU32::new(texture)
            .ok_or_else(|| "gl texture name is 0".to_string())?;
        let gl = self
            .gl
            .ok_or_else(|| "the engine was not built on a GLES context".to_string())?;
        let pending = unsafe { gl.take_errors() };
        if !pending.is_empty() {
            crate::log::emit(
                crate::log::WARN,
                &format!("GL {} pending before a kine render — the host's, cleared", gl_state::describe(&pending)),
            );
        }
        unsafe { gl.enter() };
        let rendered = unsafe { self.render_into_gl(scene, width, height, viewport, name) };
        unsafe { gl.leave() };
        let raised = unsafe { gl.take_errors() };
        match rendered {
            Ok(()) if !raised.is_empty() => Err(format!(
                "GL {} during the render — the frame is not trusted",
                gl_state::describe(&raised)
            )),
            rendered => rendered,
        }
    }

    /// [`Self::render_to_gl_texture`] inside the host context's bracket.
    #[cfg(feature = "gpu-gles")]
    unsafe fn render_into_gl(
        &mut self,
        scene: &Scene,
        width: u32,
        height: u32,
        viewport: (f64, f64),
        name: std::num::NonZeroU32,
    ) -> Result<(), String> {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        let hal_texture = {
            let hal_device = unsafe { self.device.as_hal::<wgpu::hal::api::Gles>() }
                .ok_or_else(|| "the engine's device is not a GLES device".to_string())?;
            let descriptor = wgpu::hal::TextureDescriptor {
                label: Some("kine gpu host target"),
                size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: TEXTURE_FORMAT,
                usage: wgpu::TextureUses::COLOR_TARGET | wgpu::TextureUses::RESOURCE,
                memory_flags: wgpu::hal::MemoryFlags::empty(),
                view_formats: Vec::new(),
            };
            // A drop callback keeps ownership with the host: wgpu only deletes
            // textures it was handed without one.
            unsafe { hal_device.texture_from_raw(name, &descriptor, Some(Box::new(|| {}))) }
        };
        let wrapped = unsafe {
            self.device.create_texture_from_hal::<wgpu::hal::api::Gles>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("kine gpu host target"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: TEXTURE_FORMAT,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                },
                // The render clears the target, so its contents on the way in
                // are discardable.
                wgpu::TextureUses::UNINITIALIZED,
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
    #[cfg_attr(not(feature = "gpu"), allow(dead_code))]
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
        let mapped = buffer
            .slice(..)
            .get_mapped_range()
            .map_err(|e| format!("gpu readback range failed: {e}"))?;
        for row in mapped.chunks_exact(padded as usize) {
            out.extend_from_slice(&row[..width as usize * 4]);
        }
        drop(mapped);
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

/// vello's defaults, except that a GLES image atlas page is 2048², not
/// 4096²: a page is made on the first image and costs 16 MB instead of 64 on a
/// phone.
fn render_settings() -> vello_gpu::RenderSettings {
    #[cfg_attr(not(feature = "gpu-gles"), allow(unused_mut))]
    let mut settings = vello_gpu::RenderSettings::default();
    #[cfg(feature = "gpu-gles")]
    {
        settings.memory_settings.image_atlas_config.atlas_size = (2048, 2048);
    }
    settings
}

fn instance() -> wgpu::Instance {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = if cfg!(feature = "gpu-gles") {
        wgpu::Backends::GL
    } else {
        wgpu::Backends::METAL
    };
    wgpu::Instance::new(descriptor)
}

/// GL entry points for wgpu's GLES backend on the host's context: the core
/// functions straight from `libGLESv3.so`, the rest through `eglGetProcAddress`
/// (which, before EGL 1.5, is not obliged to answer core names).
#[cfg(feature = "gpu-gles")]
mod gl_loader {
    use std::ffi::{c_char, c_int, c_void, CString};
    use std::sync::OnceLock;

    extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    #[link(name = "EGL")]
    extern "C" {
        fn eglGetProcAddress(procname: *const c_char) -> *const c_void;
    }

    const RTLD_NOW: c_int = 2;

    fn gles() -> *mut c_void {
        static LIBRARY: OnceLock<usize> = OnceLock::new();
        *LIBRARY.get_or_init(|| unsafe { dlopen(c"libGLESv3.so".as_ptr(), RTLD_NOW) as usize }) as *mut c_void
    }

    pub(super) fn proc_address(name: &str) -> *const c_void {
        let Ok(symbol) = CString::new(name) else {
            return std::ptr::null();
        };
        unsafe {
            let library = gles();
            if !library.is_null() {
                let direct = dlsym(library, symbol.as_ptr());
                if !direct.is_null() {
                    return direct;
                }
            }
            eglGetProcAddress(symbol.as_ptr())
        }
    }
}

/// The host's context around a GLES render. wgpu's GLES backend assumes that
/// the vertex array its device made stays bound and that the pixel store packs
/// rows tightly; on the way out it leaves its own bindings, sampler objects,
/// scissor and blend state, and row lengths behind. A host that never heard of
/// them would draw scissored, sample through kine's samplers, or upload with
/// kine's stride. [`HostGl::enter`] restores wgpu's assumptions, and
/// [`HostGl::leave`] returns everything a render can touch to GLES defaults.
#[cfg(feature = "gpu-gles")]
mod gl_state {
    use std::ffi::c_void;

    use super::gl_loader::proc_address;

    const ZERO: u32 = 0;
    const ONE: u32 = 1;
    const TEXTURE_2D: u32 = 0x0DE1;
    const SCISSOR_TEST: u32 = 0x0C11;
    const BLEND: u32 = 0x0BE2;
    const UNPACK_ROW_LENGTH: u32 = 0x0CF2;
    const UNPACK_ALIGNMENT: u32 = 0x0CF5;
    const PACK_ROW_LENGTH: u32 = 0x0D02;
    const PACK_ALIGNMENT: u32 = 0x0D05;
    const FUNC_ADD: u32 = 0x8006;
    const UNPACK_IMAGE_HEIGHT: u32 = 0x806E;
    const TEXTURE0: u32 = 0x84C0;
    const VERTEX_ARRAY_BINDING: u32 = 0x85B5;
    const ARRAY_BUFFER: u32 = 0x8892;
    const PIXEL_PACK_BUFFER: u32 = 0x88EB;
    const PIXEL_UNPACK_BUFFER: u32 = 0x88EC;
    const UNIFORM_BUFFER: u32 = 0x8A11;
    const TEXTURE_2D_ARRAY: u32 = 0x8C1A;
    const FRAMEBUFFER: u32 = 0x8D40;
    const COPY_READ_BUFFER: u32 = 0x8F36;
    const COPY_WRITE_BUFFER: u32 = 0x8F37;

    /// Buffer targets a render can leave bound. The element array buffer is
    /// vertex-array state and goes with the vertex array.
    const BUFFER_TARGETS: [u32; 6] = [
        ARRAY_BUFFER,
        COPY_READ_BUFFER,
        COPY_WRITE_BUFFER,
        PIXEL_PACK_BUFFER,
        PIXEL_UNPACK_BUFFER,
        UNIFORM_BUFFER,
    ];

    #[derive(Clone, Copy)]
    pub(super) struct HostGl {
        use_program: unsafe extern "C" fn(u32),
        bind_framebuffer: unsafe extern "C" fn(u32, u32),
        bind_vertex_array: unsafe extern "C" fn(u32),
        bind_buffer: unsafe extern "C" fn(u32, u32),
        bind_buffer_base: unsafe extern "C" fn(u32, u32, u32),
        active_texture: unsafe extern "C" fn(u32),
        bind_texture: unsafe extern "C" fn(u32, u32),
        bind_sampler: unsafe extern "C" fn(u32, u32),
        disable: unsafe extern "C" fn(u32),
        blend_equation: unsafe extern "C" fn(u32),
        blend_func: unsafe extern "C" fn(u32, u32),
        pixel_store: unsafe extern "C" fn(u32, i32),
        get_error: unsafe extern "C" fn() -> u32,
        /// wgpu's own vertex array, made and bound when its device opened.
        vertex_array: u32,
        /// Texture units and uniform-buffer slots a render can bind. wgpu
        /// numbers both from 0 across a pipeline layout, which the device's
        /// limits cap per stage — two stages. Within GLES 3.0's minimums (32
        /// units, 24 slots) for the WebGL2 limits the engine requests.
        texture_units: u32,
        uniform_slots: u32,
    }

    impl HostGl {
        /// Load the entry points, note the vertex array wgpu's device just
        /// bound, and return the context to defaults.
        ///
        /// # Safety
        /// The context the device was opened on must be current.
        pub(super) unsafe fn adopt(limits: &wgpu::Limits) -> Result<Self, String> {
            let get_integer: unsafe extern "C" fn(u32, *mut i32) = entry("glGetIntegerv")?;
            let mut vertex_array = 0;
            unsafe { get_integer(VERTEX_ARRAY_BINDING, &mut vertex_array) };
            let raised = unsafe { take_errors() }?;
            if !raised.is_empty() {
                return Err(format!("GL {} reading the vertex array binding", describe(&raised)));
            }
            let gl = Self {
                use_program: entry("glUseProgram")?,
                bind_framebuffer: entry("glBindFramebuffer")?,
                bind_vertex_array: entry("glBindVertexArray")?,
                bind_buffer: entry("glBindBuffer")?,
                bind_buffer_base: entry("glBindBufferBase")?,
                active_texture: entry("glActiveTexture")?,
                bind_texture: entry("glBindTexture")?,
                bind_sampler: entry("glBindSampler")?,
                disable: entry("glDisable")?,
                blend_equation: entry("glBlendEquation")?,
                blend_func: entry("glBlendFunc")?,
                pixel_store: entry("glPixelStorei")?,
                get_error: entry("glGetError")?,
                vertex_array: vertex_array as u32,
                texture_units: 2 * limits.max_sampled_textures_per_shader_stage,
                uniform_slots: 2 * limits.max_uniform_buffers_per_shader_stage,
            };
            unsafe { gl.leave() };
            let raised = unsafe { take_errors() }?;
            if !raised.is_empty() {
                return Err(format!("GL {} returning the context to defaults", describe(&raised)));
            }
            Ok(gl)
        }

        /// [`take_errors`] through the loaded entry point.
        ///
        /// # Safety
        /// The engine's context must be current.
        pub(super) unsafe fn take_errors(&self) -> Vec<u32> {
            unsafe { drain(self.get_error) }
        }

        /// # Safety
        /// The engine's context must be current.
        pub(super) unsafe fn enter(&self) {
            unsafe {
                (self.bind_vertex_array)(self.vertex_array);
                (self.pixel_store)(UNPACK_ALIGNMENT, 1);
                (self.pixel_store)(PACK_ALIGNMENT, 1);
            }
        }

        /// Program, framebuffer, vertex array and buffers unbound; textures and
        /// samplers unbound on every unit a render can use, unit 0 active;
        /// scissor test and blending off, blending back to `ADD`, `ONE, ZERO`;
        /// pixel store at alignment 4 and row lengths 0. The viewport and the
        /// scissor box stay at the last target's size.
        ///
        /// # Safety
        /// The engine's context must be current.
        pub(super) unsafe fn leave(&self) {
            unsafe {
                (self.use_program)(0);
                (self.bind_framebuffer)(FRAMEBUFFER, 0);
                (self.bind_vertex_array)(0);
                for target in BUFFER_TARGETS {
                    (self.bind_buffer)(target, 0);
                }
                for slot in 0..self.uniform_slots {
                    (self.bind_buffer_base)(UNIFORM_BUFFER, slot, 0);
                }
                for unit in (0..self.texture_units).rev() {
                    (self.active_texture)(TEXTURE0 + unit);
                    (self.bind_texture)(TEXTURE_2D, 0);
                    (self.bind_texture)(TEXTURE_2D_ARRAY, 0);
                    (self.bind_sampler)(unit, 0);
                }
                (self.disable)(SCISSOR_TEST);
                (self.disable)(BLEND);
                (self.blend_equation)(FUNC_ADD);
                (self.blend_func)(ONE, ZERO);
                (self.pixel_store)(UNPACK_ALIGNMENT, 4);
                (self.pixel_store)(PACK_ALIGNMENT, 4);
                (self.pixel_store)(UNPACK_ROW_LENGTH, 0);
                (self.pixel_store)(UNPACK_IMAGE_HEIGHT, 0);
                (self.pixel_store)(PACK_ROW_LENGTH, 0);
            }
        }
    }

    pub(super) const INVALID_ENUM: u32 = 0x0500;

    /// Read and clear the pending GL error flags — at most one per kind, so a
    /// context that keeps answering is cut off.
    ///
    /// # Safety
    /// A GL context must be current.
    pub(super) unsafe fn take_errors() -> Result<Vec<u32>, String> {
        Ok(unsafe { drain(entry("glGetError")?) })
    }

    unsafe fn drain(get_error: unsafe extern "C" fn() -> u32) -> Vec<u32> {
        let mut errors = Vec::new();
        while errors.len() < 8 {
            match unsafe { get_error() } {
                0 => break,
                error => errors.push(error),
            }
        }
        errors
    }

    /// `error 0x501` / `errors 0x500, 0x501`.
    pub(super) fn describe(errors: &[u32]) -> String {
        let codes: Vec<String> = errors.iter().map(|error| format!("0x{error:x}")).collect();
        let noun = if errors.len() == 1 { "error" } else { "errors" };
        format!("{noun} {}", codes.join(", "))
    }

    /// A GL entry point as the `extern "C"` signature `F` the caller names.
    fn entry<F: Copy>(name: &str) -> Result<F, String> {
        let address = proc_address(name);
        if address.is_null() {
            return Err(format!("GL entry point {name} is missing"));
        }
        assert_eq!(size_of::<F>(), size_of::<*const c_void>());
        Ok(unsafe { std::mem::transmute_copy(&address) })
    }
}

#[cfg_attr(not(feature = "gpu"), allow(dead_code))]
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

/// The `vello_gpu` flavor. Holds the wgpu handles because image paints must
/// reach the atlas mid-walk — see [`Canvas::set_paint_image`].
struct GpuCanvas<'a> {
    scene: &'a mut vello_gpu::Scene,
    resources: &'a mut vello_gpu::Resources,
    renderer: &'a mut vello_gpu::Renderer,
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
    /// (`vello_gpu/src/render/wgpu/mod.rs`), so an exhausted atlas is a panic
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
        // vello_gpu 0.3.0 pads an atlas image by IMAGE_PADDING — 0 texels.
        let area = u64::from(pixmap.width()) * u64::from(pixmap.height());
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
        let drawn = if stroke {
            builder.stroke_glyphs(glyphs)
        } else {
            builder.fill_glyphs(glyphs)
        };
        render::report_blank_glyphs(drawn);
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
