//! Resolved scene → pixels → PNG. This module owns PIXELS: node walking with
//! transform stacks, parley layout + unit segmentation for text, per-unit deltas
//! at glyph_run time, pills, shadows, strokes. It consumes only eval's resolved
//! types; values were decided upstream.
//!
//! The scene walk is generic over [`Canvas`], the one seam between kine and a
//! rasterizer. `vello_cpu` backs it here (server, posters, fallback); `gpu.rs`
//! backs it with `vello_hybrid` on device. There is exactly ONE `draw_*`
//! implementation, so a document cannot drift between the two flavors.

use std::sync::Arc;

use vello_common::filter_effects::{Filter, FilterFunction};
use vello_common::paint::{Image, ImageSource, PaintType};
use vello_cpu::kurbo::{Affine, BezPath, Point, Rect, RoundedRect, Shape as KurboShape, Stroke, Vec2};
use vello_cpu::peniko::color::DynamicColor;
use vello_cpu::peniko::{ColorStop, Gradient, ImageSampler};
use vello_cpu::{Glyph, Pixmap, RenderContext, RenderSettings, Resources};

use parley::{
    Alignment, AlignmentOptions, FontFamily, FontStyle, FontWeight, Layout, LayoutContext,
    LineHeight, PositionedLayoutItem, StyleProperty,
};

use crate::eval::{
    self, stagger_weights, Color, MapValue, RImage, RNode, RPaint, RShape, RText, RUnitAnimator,
    RWeight, Scene, UnitProperty,
};
use crate::bubble;
use crate::fonts;
use crate::schema::{Align, Fit, LineCap, LineJoin, VAlign};
use crate::validate::UnitLevel;

// --- the rasterizer seam ----------------------------------------------------------

/// The raster surface the scene walk paints onto. Both flavors resolve the same
/// `vello_common` geometry — only the back-end differs — so implementing this is
/// the whole cost of adding a rasterizer.
pub(crate) trait Canvas {
    fn set_transform(&mut self, affine: Affine);
    fn set_paint(&mut self, paint: impl Into<PaintType>);
    /// Image paints are the ONE place the flavors diverge. vello_cpu samples a
    /// `Pixmap` in place; vello_hybrid takes atlas-resident images only
    /// (`ImageSource::Pixmap` is an `unimplemented!()` panic there), so the
    /// CANVAS resolves the source, never the caller.
    fn set_paint_image(&mut self, pixmap: &Arc<Pixmap>, sampler: ImageSampler);
    fn set_paint_transform(&mut self, affine: Affine);
    fn set_stroke(&mut self, stroke: Stroke);
    fn fill_path(&mut self, path: &BezPath);
    fn stroke_path(&mut self, path: &BezPath);
    fn fill_rect(&mut self, rect: &Rect);
    fn push_opacity_layer(&mut self, opacity: f32);
    fn push_clip_layer(&mut self, path: &BezPath);
    fn push_filter_layer(&mut self, filter: Option<Filter>);
    fn pop_layer(&mut self);
    /// Paint is set by the caller; this draws one glyph of `run` under the
    /// already-composed unit transform.
    fn draw_glyph(&mut self, run: &RunStyle, glyph: Glyph, linear: Affine, stroke: bool);
}

/// The `vello_cpu` flavor — the server's renderer, and the device's fallback.
pub(crate) struct CpuCanvas<'a> {
    ctx: &'a mut RenderContext,
    resources: &'a mut Resources,
}

impl Canvas for CpuCanvas<'_> {
    fn set_transform(&mut self, affine: Affine) {
        self.ctx.set_transform(affine);
    }

    fn set_paint(&mut self, paint: impl Into<PaintType>) {
        self.ctx.set_paint(paint);
    }

    fn set_paint_image(&mut self, pixmap: &Arc<Pixmap>, sampler: ImageSampler) {
        self.ctx.set_paint(Image {
            image: ImageSource::Pixmap(pixmap.clone()),
            sampler,
        });
    }

    fn set_paint_transform(&mut self, affine: Affine) {
        self.ctx.set_paint_transform(affine);
    }

    fn set_stroke(&mut self, stroke: Stroke) {
        self.ctx.set_stroke(stroke);
    }

    fn fill_path(&mut self, path: &BezPath) {
        self.ctx.fill_path(path);
    }

    fn stroke_path(&mut self, path: &BezPath) {
        self.ctx.stroke_path(path);
    }

    fn fill_rect(&mut self, rect: &Rect) {
        self.ctx.fill_rect(rect);
    }

    fn push_opacity_layer(&mut self, opacity: f32) {
        self.ctx.push_opacity_layer(opacity);
    }

    fn push_clip_layer(&mut self, path: &BezPath) {
        self.ctx.push_clip_layer(path);
    }

    fn push_filter_layer(&mut self, filter: Option<Filter>) {
        self.ctx.push_layer(None, None, None, None, filter);
    }

    fn pop_layer(&mut self) {
        self.ctx.pop_layer();
    }

    fn draw_glyph(&mut self, run: &RunStyle, glyph: Glyph, linear: Affine, stroke: bool) {
        let builder = self
            .ctx
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

/// Render to a PNG blob (RGBA8, straight/un-premultiplied alpha per PNG).
pub fn render_png(scene: &Scene, width: u32, height: u32) -> Result<Vec<u8>, String> {
    render_pixmap(scene, width, height, (0.0, scene.height))?
        .into_png()
        .map_err(|e| format!("PNG encoding failed: {e}"))
}

/// Render a vertical design-space VIEWPORT `(view_y, view_h)` of the scene
/// into the target — the grown-canvas companion to `grown_extents`: pass
/// its `(top, bottom - top)` and a target sized to the grown aspect, and
/// text that overflows the doc canvas renders instead of cropping.
/// `(0, scene.height)` is exactly the plain render.
pub fn render_rgba_viewport(
    scene: &Scene,
    width: u32,
    height: u32,
    view_y: f64,
    view_h: f64,
) -> Result<Vec<u8>, String> {
    if view_h <= 0.0 || !view_y.is_finite() || !view_h.is_finite() {
        return Err(format!("invalid viewport y={view_y} h={view_h}"));
    }
    Ok(render_pixmap(scene, width, height, (view_y, view_h))?
        .data_as_u8_slice()
        .to_vec())
}

/// Render to raw RGBA8 bytes: premultiplied alpha, sRGB, row-major, stride =
/// width*4, len = width*height*4. This is what texture-upload clients want (no
/// PNG round-trip). Note: premultiplied, unlike `render_png` (straight alpha).
pub fn render_rgba(scene: &Scene, width: u32, height: u32) -> Result<Vec<u8>, String> {
    Ok(render_pixmap(scene, width, height, (0.0, scene.height))?
        .data_as_u8_slice()
        .to_vec())
}

/// Reject rasters no flavor should attempt. Shared, so the GPU path can never
/// accept a size the CPU path refuses.
pub(crate) fn validate_size(width: u32, height: u32) -> Result<(), String> {
    // 8192x8192 = 256 MB of RGBA — generous for real exports (covers 8K), and a
    // hard ceiling so a valid-per-dimension but enormous request (e.g. 65535x65535
    // ≈ 17 GB) is rejected rather than OOM-killing the process.
    const MAX_PIXELS: u64 = 8192 * 8192;

    if width == 0 || height == 0 {
        return Err("render size must be non-zero".to_string());
    }
    if width > u16::MAX as u32 || height > u16::MAX as u32 {
        return Err(format!("render size exceeds {}", u16::MAX));
    }
    if width as u64 * height as u64 > MAX_PIXELS {
        return Err(format!(
            "render area {width}x{height} exceeds {MAX_PIXELS} pixels"
        ));
    }
    Ok(())
}

/// Design space → target pixels for a vertical viewport `(view_y, view_h)`.
/// Shared so both flavors place the scene identically.
pub(crate) fn root_transform(
    scene: &Scene,
    width: u32,
    height: u32,
    viewport: (f64, f64),
) -> Affine {
    let (view_y, view_h) = viewport;
    Affine::scale_non_uniform(width as f64 / scene.width, height as f64 / view_h)
        * Affine::translate((0.0, -view_y))
}

fn render_pixmap(
    scene: &Scene,
    width: u32,
    height: u32,
    viewport: (f64, f64),
) -> Result<Pixmap, String> {
    validate_size(width, height)?;

    // Single-threaded: the multi-threaded dispatcher doesn't support filter
    // effects (text shadows) yet, and one thread keeps renders deterministic
    // independent of host core count.
    let settings = RenderSettings {
        num_threads: 0,
        ..RenderSettings::default()
    };
    let mut ctx = RenderContext::new_with(width as u16, height as u16, settings);
    let mut resources = Resources::new();

    let root = root_transform(scene, width, height, viewport);
    {
        let mut canvas = CpuCanvas {
            ctx: &mut ctx,
            resources: &mut resources,
        };
        draw_node(&mut canvas, &scene.root, root)?;
    }

    let mut pixmap = Pixmap::new(width as u16, height as u16);
    ctx.flush();
    ctx.render_to_pixmap(&mut resources, &mut pixmap);
    Ok(pixmap)
}

pub(crate) fn draw_node<C: Canvas>(
    canvas: &mut C,
    node: &RNode,
    parent: Affine,
) -> Result<(), String> {
    match node {
        RNode::Group(group) => {
            let layered = push_opacity(canvas, group.opacity);
            let affine = parent * group.transform;
            for child in &group.children {
                draw_node(canvas, child, affine)?;
            }
            pop_opacity(canvas, layered);
        }
        RNode::Shape(shape) => {
            let layered = push_opacity(canvas, shape.opacity);
            draw_shape(canvas, shape, parent);
            pop_opacity(canvas, layered);
        }
        RNode::Image(image) => {
            let layered = push_opacity(canvas, image.opacity);
            draw_image(canvas, image, parent);
            pop_opacity(canvas, layered);
        }
        RNode::Text(text) => {
            let layered = push_opacity(canvas, text.opacity);
            draw_text(canvas, text, parent)?;
            pop_opacity(canvas, layered);
        }
    }
    Ok(())
}

fn push_opacity<C: Canvas>(canvas: &mut C, opacity: f64) -> bool {
    if opacity < 1.0 {
        canvas.push_opacity_layer(opacity as f32);
        true
    } else {
        false
    }
}

fn pop_opacity<C: Canvas>(canvas: &mut C, layered: bool) {
    if layered {
        canvas.pop_layer();
    }
}

// --- shapes -----------------------------------------------------------------------

fn draw_shape<C: Canvas>(canvas: &mut C, shape: &RShape, parent: Affine) {
    canvas.set_transform(parent * shape.transform);
    if let Some(fill) = &shape.fill {
        set_paint(canvas, fill);
        canvas.fill_path(&shape.path);
    }
    if let Some(stroke) = &shape.stroke {
        canvas.set_paint(stroke.color);
        canvas.set_stroke(build_stroke(stroke.width, stroke.cap, stroke.join));
        canvas.stroke_path(&shape.path);
    }
}

fn build_stroke(width: f64, cap: LineCap, join: LineJoin) -> Stroke {
    use vello_cpu::kurbo::{Cap, Join};
    let stroke = Stroke::new(width);
    let stroke = stroke.with_caps(match cap {
        LineCap::Butt => Cap::Butt,
        LineCap::Round => Cap::Round,
        LineCap::Square => Cap::Square,
    });
    stroke.with_join(match join {
        LineJoin::Miter => Join::Miter,
        LineJoin::Round => Join::Round,
        LineJoin::Bevel => Join::Bevel,
    })
}

fn set_paint<C: Canvas>(canvas: &mut C, paint: &RPaint) {
    match paint {
        RPaint::Solid(color) => canvas.set_paint(*color),
        RPaint::Linear {
            x1,
            y1,
            x2,
            y2,
            stops,
        } => {
            let gradient = Gradient::new_linear(Point::new(*x1, *y1), Point::new(*x2, *y2))
                .with_stops(color_stops(stops).as_slice());
            canvas.set_paint(gradient);
        }
        RPaint::Radial { cx, cy, r, stops } => {
            let gradient = Gradient::new_radial(Point::new(*cx, *cy), *r as f32)
                .with_stops(color_stops(stops).as_slice());
            canvas.set_paint(gradient);
        }
    }
}

fn color_stops(stops: &[(f32, Color)]) -> Vec<ColorStop> {
    stops
        .iter()
        .map(|(at, color)| ColorStop::from((*at, DynamicColor::from_alpha_color(*color))))
        .collect()
}

// --- images ---------------------------------------------------------------------

/// Draw a (sampled) image frame into `frame` per `fit`, clipped to the rounded
/// frame. `cover`/`fill` cover the frame (overflow cropped by the clip);
/// `contain` letterboxes (the empty margin stays transparent — only the placed
/// rect is filled). The paint transform maps texel space onto the placed rect in
/// document coordinates; the standard node transform/opacity apply around it.
fn draw_image<C: Canvas>(canvas: &mut C, image: &RImage, parent: Affine) {
    use vello_cpu::peniko::{Extend, ImageQuality};

    let iw = image.image.width() as f64;
    let ih = image.image.height() as f64;
    let frame = image.frame;
    let (fw, fh) = (frame.width(), frame.height());
    if iw <= 0.0 || ih <= 0.0 || fw <= 0.0 || fh <= 0.0 {
        return;
    }

    let (sx, sy) = match image.fit {
        Fit::Fill => (fw / iw, fh / ih),
        Fit::Cover => {
            let s = (fw / iw).max(fh / ih);
            (s, s)
        }
        Fit::Contain => {
            let s = (fw / iw).min(fh / ih);
            (s, s)
        }
    };
    let placed_w = iw * sx;
    let placed_h = ih * sy;
    let px = frame.x0 + (fw - placed_w) * 0.5;
    let py = frame.y0 + (fh - placed_h) * 0.5;
    let fit_affine = Affine::translate((px, py)) * Affine::scale_non_uniform(sx, sy);
    let placed = Rect::new(px, py, px + placed_w, py + placed_h);

    let radius = image.corner_radius.max(0.0);
    let clip = RoundedRect::from_rect(frame, radius).to_path(0.1);

    canvas.set_transform(parent * image.transform);
    canvas.push_clip_layer(&clip);
    canvas.set_paint_image(
        &image.image,
        ImageSampler {
            x_extend: Extend::Pad,
            y_extend: Extend::Pad,
            quality: ImageQuality::Medium,
            alpha: 1.0,
        },
    );
    canvas.set_paint_transform(fit_affine);
    canvas.fill_rect(&placed);
    canvas.set_paint_transform(Affine::IDENTITY);
    canvas.pop_layer();
}

// --- text ------------------------------------------------------------------------

/// One drawable glyph with its unit memberships resolved.
struct GlyphRecord {
    run: usize,
    id: u32,
    x: f32,
    y: f32,
    advance: f32,
    line: usize,
    /// None for whitespace glyphs (excluded from word/glyph units).
    word: Option<usize>,
    glyph_unit: Option<usize>,
}

/// Per-run draw parameters (font handle, size, variation coords).
pub(crate) struct RunStyle {
    pub(crate) font: vello_cpu::peniko::FontData,
    pub(crate) size: f32,
    pub(crate) coords: Vec<i16>,
}

/// Scalar per-unit transform state composed by animators.
#[derive(Clone, Copy)]
struct UnitTransform {
    tx: f64,
    ty: f64,
    scale: f64,
    rotate: f64,
}

impl UnitTransform {
    const IDENTITY: UnitTransform = UnitTransform {
        tx: 0.0,
        ty: 0.0,
        scale: 1.0,
        rotate: 0.0,
    };

    fn is_identity(&self) -> bool {
        self.tx == 0.0 && self.ty == 0.0 && self.scale == 1.0 && self.rotate == 0.0
    }

    fn affine_about(&self, center: Point) -> Affine {
        if self.is_identity() {
            return Affine::IDENTITY;
        }
        Affine::translate((self.tx, self.ty))
            * Affine::translate((center.x, center.y))
            * Affine::rotate(self.rotate.to_radians())
            * Affine::scale(self.scale)
            * Affine::translate((-center.x, -center.y))
    }
}

/// The parley layout `draw_text` draws with — wrap at the frame width,
/// explicit newlines honored, aligned per style. Shared with the
/// layout-size measure (`text_layout_height`) so the two can never
/// drift. Errors mirror the render-time font contract.
fn build_text_layout(text: &RText) -> Result<Layout<()>, String> {
    let style = &text.style;
    let Some(mut font_ctx) = fonts::context() else {
        return Err(format!(
            "no fonts registered — register \"{}\" before rendering",
            style.family
        ));
    };
    if font_ctx.collection.family_id(&style.family).is_none() {
        return Err(format!("font family not registered: \"{}\"", style.family));
    }
    let mut layout_ctx: LayoutContext<()> = LayoutContext::new();
    let mut builder = layout_ctx.ranged_builder(&mut font_ctx, &text.content, 1.0, true);
    builder.push_default(StyleProperty::FontSize(style.size));
    builder.push_default(FontFamily::named(&style.family));
    builder.push_default(StyleProperty::FontWeight(FontWeight::new(style.weight)));
    builder.push_default(StyleProperty::FontStyle(if style.italic {
        FontStyle::Italic
    } else {
        FontStyle::Normal
    }));
    builder.push_default(StyleProperty::LetterSpacing(style.letter_spacing));
    builder.push_default(StyleProperty::LineHeight(LineHeight::FontSizeRelative(
        style.line_height,
    )));
    let mut layout: Layout<()> = builder.build(&text.content);
    layout.break_all_lines(Some(text.frame.width() as f32));
    layout.align(
        match style.align {
            Align::Left => Alignment::Left,
            Align::Center => Alignment::Center,
            Align::Right => Alignment::Right,
        },
        AlignmentOptions::default(),
    );
    Ok(layout)
}

/// Laid-out block height of a text node in design units — the measure
/// behind the grown-canvas query. `None` for empty content or an
/// unregistered font (the measure is total: no growth, not an error).
pub fn text_layout_height(text: &RText) -> Option<f64> {
    if text.content.is_empty() {
        return None;
    }
    build_text_layout(text).ok().map(|l| l.height() as f64)
}

/// Vertical extent `(top, bottom)` the scene's TEXT content needs, in
/// design units — `(0, scene.height)` when everything fits. A text
/// block taller than its frame grows the extent per the node's
/// `valign` (top → down, center → both ways, bottom → up), matching
/// where `draw_text` places overflowing lines. Frame-space only:
/// node/animator transforms are NOT applied — this is the resting
/// layout, not the choreography.
pub fn grown_extents(scene: &Scene) -> (f64, f64) {
    fn walk(node: &RNode, top: &mut f64, bottom: &mut f64) {
        match node {
            RNode::Group(group) => {
                for child in &group.children {
                    walk(child, top, bottom);
                }
            }
            RNode::Text(text) => {
                let Some(height) = text_layout_height(text) else {
                    return;
                };
                let frame = text.frame;
                if height <= frame.height() {
                    return;
                }
                let (node_top, node_bottom) = match text.style.valign {
                    VAlign::Top => (frame.y0, frame.y0 + height),
                    VAlign::Center => {
                        let center = (frame.y0 + frame.y1) * 0.5;
                        (center - height * 0.5, center + height * 0.5)
                    }
                    VAlign::Bottom => (frame.y1 - height, frame.y1),
                };
                *top = top.min(node_top);
                *bottom = bottom.max(node_bottom);
            }
            RNode::Shape(_) | RNode::Image(_) => {}
        }
    }
    let mut top = 0.0;
    let mut bottom = scene.height;
    walk(&scene.root, &mut top, &mut bottom);
    (top, bottom)
}

fn draw_text<C: Canvas>(canvas: &mut C, text: &RText, parent: Affine) -> Result<(), String> {
    if text.content.is_empty() {
        return Ok(());
    }
    let style = &text.style;
    // Layout once per input set; everything after is per-glyph draw math.
    let layout = build_text_layout(text)?;

    let valign_dy = match style.valign {
        VAlign::Top => 0.0,
        VAlign::Center => (text.frame.height() - layout.height() as f64) * 0.5,
        VAlign::Bottom => text.frame.height() - layout.height() as f64,
    };
    let origin = Vec2::new(text.frame.x0, text.frame.y0 + valign_dy);
    let full = parent * text.transform * Affine::translate(origin);

    // --- segmentation: glyphs with line/word/glyph-unit memberships ------------

    let mut runs: Vec<RunStyle> = Vec::new();
    let mut glyphs: Vec<GlyphRecord> = Vec::new();
    let mut line_boxes: Vec<Rect> = Vec::new();
    let mut word_count = 0usize;
    let mut glyph_unit_count = 0usize;
    let mut previous_was_space = true;

    for (line_index, line) in layout.lines().enumerate() {
        let metrics = line.metrics();
        let line_top = (metrics.baseline - metrics.ascent) as f64;
        let line_bottom = (metrics.baseline + metrics.descent) as f64;
        let mut line_box: Option<Rect> = None;
        // A line boundary always ends a word — an explicit "\n" carries no
        // space cluster, and a word bleeding across lines would drag hulls,
        // pills and word-animators out to the block extents.
        previous_was_space = true;

        for item in line.items() {
            let PositionedLayoutItem::GlyphRun(glyph_run) = item else {
                continue;
            };
            let run = glyph_run.run();
            let run_index = runs.len();
            runs.push(RunStyle {
                font: run.font().clone(),
                size: run.font_size(),
                coords: run.normalized_coords().to_vec(),
            });

            // Mirror of GlyphRun::positioned_glyphs (offset/baseline accumulation
            // over visual clusters), walked per cluster so each glyph knows its
            // cluster's whitespace/word context. Uniform style per node keeps the
            // glyph run covering its whole run.
            let baseline = glyph_run.baseline();
            let mut offset = glyph_run.offset();
            for cluster in run.visual_clusters() {
                let is_space = cluster.is_space_or_nbsp();
                let word = if is_space {
                    previous_was_space = true;
                    None
                } else {
                    if previous_was_space {
                        word_count += 1;
                    }
                    previous_was_space = false;
                    Some(word_count - 1)
                };
                for glyph in cluster.glyphs() {
                    let x = offset + glyph.x;
                    let y = baseline + glyph.y;
                    offset += glyph.advance;
                    let glyph_unit = if is_space {
                        None
                    } else {
                        glyph_unit_count += 1;
                        Some(glyph_unit_count - 1)
                    };
                    let record = GlyphRecord {
                        run: run_index,
                        id: glyph.id,
                        x,
                        y,
                        advance: glyph.advance,
                        line: line_index,
                        word,
                        glyph_unit,
                    };
                    if !is_space {
                        let glyph_box =
                            Rect::new(x as f64, line_top, (x + glyph.advance) as f64, line_bottom);
                        line_box = Some(line_box.map_or(glyph_box, |b| b.union(glyph_box)));
                    }
                    glyphs.push(record);
                }
            }
        }
        line_boxes.push(line_box.unwrap_or(Rect::new(0.0, line_top, 0.0, line_bottom)));
    }

    let line_count = line_boxes.len();
    let mut word_boxes: Vec<Rect> = vec![Rect::ZERO; word_count];
    let mut word_seen: Vec<bool> = vec![false; word_count];
    for record in &glyphs {
        let Some(word) = record.word else { continue };
        let line = &line_boxes[record.line];
        let glyph_box = Rect::new(
            record.x as f64,
            line.y0,
            (record.x + record.advance) as f64,
            line.y1,
        );
        word_boxes[word] = if word_seen[word] {
            word_boxes[word].union(glyph_box)
        } else {
            glyph_box
        };
        word_seen[word] = true;
    }

    // --- per-unit animator states ------------------------------------------------

    let unit_count = |level: UnitLevel| match level {
        UnitLevel::Glyphs => glyph_unit_count,
        UnitLevel::Words => word_count,
        UnitLevel::Lines => line_count,
    };

    let mut transforms: [Vec<UnitTransform>; 3] = [
        vec![UnitTransform::IDENTITY; glyph_unit_count],
        vec![UnitTransform::IDENTITY; word_count],
        vec![UnitTransform::IDENTITY; line_count],
    ];
    let level_index = |level: UnitLevel| match level {
        UnitLevel::Glyphs => 0usize,
        UnitLevel::Words => 1,
        UnitLevel::Lines => 2,
    };

    // Color/opacity compose per glyph in document order across levels; pill
    // properties per word.
    let mut glyph_colors: Vec<Color> = vec![style.fill; glyphs.len()];
    let mut glyph_opacity: Vec<f64> = vec![1.0; glyphs.len()];
    let mut pill_colors: Vec<Color> =
        vec![style.pill.as_ref().map_or(style.fill, |p| p.color); word_count];
    let mut pill_opacity: Vec<f64> =
        vec![style.pill.as_ref().map_or(1.0, |p| p.opacity); word_count];

    for animator in &text.unit_animators {
        let n = unit_count(animator.level);
        let weights = animator_weights(animator, n);

        match animator.property {
            UnitProperty::TranslateX
            | UnitProperty::TranslateY
            | UnitProperty::Scale
            | UnitProperty::Rotate => {
                let states = &mut transforms[level_index(animator.level)];
                for (i, weight) in weights.iter().enumerate() {
                    let MapValue::Num(value) = animator.map.eval(*weight, animator.space) else {
                        continue;
                    };
                    let state = &mut states[i];
                    let slot = match animator.property {
                        UnitProperty::TranslateX => &mut state.tx,
                        UnitProperty::TranslateY => &mut state.ty,
                        UnitProperty::Scale => &mut state.scale,
                        _ => &mut state.rotate,
                    };
                    *slot = eval::composite_num(*slot, value, animator.composite, animator.amount);
                }
            }
            UnitProperty::Opacity | UnitProperty::Color => {
                for (g, record) in glyphs.iter().enumerate() {
                    let Some(unit) = record_unit(record, animator.level) else {
                        continue;
                    };
                    let weight = weights[unit];
                    match animator.map.eval(weight, animator.space) {
                        MapValue::Num(value) if animator.property == UnitProperty::Opacity => {
                            glyph_opacity[g] = eval::composite_num(
                                glyph_opacity[g],
                                value,
                                animator.composite,
                                animator.amount,
                            );
                        }
                        MapValue::Color(value) if animator.property == UnitProperty::Color => {
                            glyph_colors[g] = eval::composite_color(
                                glyph_colors[g],
                                value,
                                animator.amount,
                                animator.space,
                            );
                        }
                        _ => {}
                    }
                }
            }
            UnitProperty::PillColor | UnitProperty::PillOpacity => {
                for (word, weight) in weights.iter().enumerate() {
                    match animator.map.eval(*weight, animator.space) {
                        MapValue::Color(value) if animator.property == UnitProperty::PillColor => {
                            pill_colors[word] = eval::composite_color(
                                pill_colors[word],
                                value,
                                animator.amount,
                                animator.space,
                            );
                        }
                        MapValue::Num(value) if animator.property == UnitProperty::PillOpacity => {
                            pill_opacity[word] = eval::composite_num(
                                pill_opacity[word],
                                value,
                                animator.composite,
                                animator.amount,
                            );
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    // Composite per-glyph affines: line ∘ word ∘ glyph, each about its own center.
    let glyph_affine = |record: &GlyphRecord| -> Affine {
        let mut affine = transforms[2][record.line].affine_about(line_boxes[record.line].center());
        if let Some(word) = record.word {
            affine *= transforms[1][word].affine_about(word_boxes[word].center());
        }
        if let Some(unit) = record.glyph_unit {
            let line = &line_boxes[record.line];
            let center = Point::new(
                record.x as f64 + record.advance as f64 * 0.5,
                (line.y0 + line.y1) * 0.5,
            );
            affine *= transforms[0][unit].affine_about(center);
        }
        affine
    };

    // --- draw: pills → shadow → fills → strokes ---------------------------------

    canvas.set_transform(full);

    if let Some(backdrop) = &style.backdrop {
        // One hull per line (a line's words are horizontally contiguous),
        // then every hull joins ONE path filled ONCE — a translucent
        // backdrop can't double-blend anywhere (line junctions included)
        // by construction. The strip follows its LINE's transform only —
        // per-word motion plays above a steady backdrop.
        let mut hulls: Vec<Option<Rect>> = vec![None; line_boxes.len()];
        for record in glyphs.iter() {
            let Some(word) = record.word else { continue };
            if !word_seen[word] {
                continue;
            }
            let hull = &mut hulls[record.line];
            *hull = Some(match hull {
                Some(rect) => rect.union(word_boxes[word]),
                None => word_boxes[word],
            });
        }
        // Axis-preserving line transforms (translate/scale — every caption
        // template) bake into the hull rects so the smooth bubble follows
        // line motion; a ROTATED line degrades to its own rounded-rect
        // subpath in the same single-fill path — the silhouette simplifies
        // for that frame, the alpha stays single either way.
        let axis_aligned = transforms[2].iter().all(|t| t.rotate == 0.0);
        let path = if axis_aligned {
            let rects: Vec<Rect> = hulls
                .iter()
                .enumerate()
                .filter_map(|(line, hull)| {
                    hull.map(|rect| {
                        let affine =
                            transforms[2][line].affine_about(line_boxes[line].center());
                        affine
                            .transform_rect_bbox(rect.inflate(backdrop.padding_x, backdrop.padding_y))
                    })
                })
                .collect();
            bubble::backdrop_path(&rects, backdrop.radius)
        } else {
            let mut path = BezPath::new();
            for (line, hull) in hulls.iter().enumerate() {
                let Some(rect) = hull else { continue };
                let padded = rect.inflate(backdrop.padding_x, backdrop.padding_y);
                let mut sub = RoundedRect::from_rect(padded, backdrop.radius).to_path(0.1);
                sub.apply_affine(transforms[2][line].affine_about(line_boxes[line].center()));
                path.extend(sub);
            }
            path
        };
        canvas.set_transform(full);
        canvas.set_paint(backdrop.color);
        canvas.fill_path(&path);
    }

    if let Some(pill) = &style.pill {
        for (word, word_box) in word_boxes.iter().enumerate() {
            if !word_seen[word] {
                continue;
            }
            let opacity = pill_opacity[word].clamp(0.0, 1.0);
            if opacity <= 0.0 {
                continue;
            }
            let padded = word_box.inflate(pill.padding_x, pill.padding_y);
            let path = RoundedRect::from_rect(padded, pill.radius).to_path(0.1);
            // A word's pill follows the word's own transform (and its line's).
            let line = glyphs
                .iter()
                .find(|g| g.word == Some(word))
                .map(|g| g.line)
                .unwrap_or(0);
            let affine = transforms[2][line].affine_about(line_boxes[line].center())
                * transforms[1][word].affine_about(word_boxes[word].center());
            canvas.set_transform(full * affine);
            canvas.set_paint(with_opacity(pill_colors[word], opacity));
            canvas.fill_path(&path);
        }
        canvas.set_transform(full);
    }

    if let Some(shadow) = &style.shadow {
        let sigma = (shadow.blur * 0.5).max(0.0) as f32;
        let filter =
            (sigma > 0.0).then(|| Filter::from_function(FilterFunction::Blur { radius: sigma }));
        canvas.push_filter_layer(filter);
        for (g, record) in glyphs.iter().enumerate() {
            let opacity = glyph_opacity[g].clamp(0.0, 1.0);
            if opacity <= 0.0 {
                continue;
            }
            draw_glyph(
                canvas,
                &runs[record.run],
                record,
                glyph_affine(record),
                Vec2::new(shadow.dx, shadow.dy),
                with_opacity(shadow.color, opacity),
                None,
            );
        }
        canvas.pop_layer();
    }

    for (g, record) in glyphs.iter().enumerate() {
        let opacity = glyph_opacity[g].clamp(0.0, 1.0);
        if opacity <= 0.0 {
            continue;
        }
        draw_glyph(
            canvas,
            &runs[record.run],
            record,
            glyph_affine(record),
            Vec2::ZERO,
            with_opacity(glyph_colors[g], opacity),
            None,
        );
    }

    if let Some((stroke_color, stroke_width)) = &style.stroke {
        canvas.set_stroke(Stroke::new(*stroke_width));
        for (g, record) in glyphs.iter().enumerate() {
            let opacity = glyph_opacity[g].clamp(0.0, 1.0);
            if opacity <= 0.0 {
                continue;
            }
            draw_glyph(
                canvas,
                &runs[record.run],
                record,
                glyph_affine(record),
                Vec2::ZERO,
                with_opacity(*stroke_color, opacity),
                Some(()),
            );
        }
    }

    Ok(())
}

fn record_unit(record: &GlyphRecord, level: UnitLevel) -> Option<usize> {
    match level {
        UnitLevel::Glyphs => record.glyph_unit,
        UnitLevel::Words => record.word,
        UnitLevel::Lines => Some(record.line),
    }
}

fn animator_weights(animator: &RUnitAnimator, n: usize) -> Vec<f64> {
    match &animator.weight {
        RWeight::Activations(values) => (0..n)
            .map(|i| values.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0))
            .collect(),
        RWeight::Stagger(stagger) => stagger_weights(stagger, n),
    }
}

fn with_opacity(color: Color, opacity: f64) -> Color {
    color.multiply_alpha(opacity as f32)
}

#[allow(clippy::too_many_arguments)]
fn draw_glyph<C: Canvas>(
    canvas: &mut C,
    run: &RunStyle,
    record: &GlyphRecord,
    affine: Affine,
    offset: Vec2,
    color: Color,
    stroke: Option<()>,
) {
    // Decompose: glyph position through the unit affine, outline through its
    // linear part (final point = A·p + L·q for outline point q at position p).
    let position = affine * Point::new(record.x as f64, record.y as f64) + offset;
    let linear = {
        let c = affine.as_coeffs();
        Affine::new([c[0], c[1], c[2], c[3], 0.0, 0.0])
    };

    canvas.set_paint(color);
    canvas.draw_glyph(
        run,
        Glyph {
            id: record.id,
            x: position.x as f32,
            y: position.y as f32,
        },
        linear,
        stroke.is_some(),
    );
}

/// Bounding box of pixels whose alpha exceeds `threshold`, in pixel coords
/// `(min_x, min_y, max_x, max_y)`. `None` for a fully-blank buffer. The ONE
/// definition of "ink" shared by every consumer (selection boxes, sticker
/// normalization, preview blank-frame trimming) — hosts must not grow their
/// own drifting scans.
pub fn ink_bounds_rgba(rgba: &[u8], width: u32, height: u32, threshold: u8) -> Option<(u32, u32, u32, u32)> {
    let (w, h) = (width as usize, height as usize);
    if rgba.len() < w * h * 4 {
        return None;
    }
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (usize::MAX, usize::MAX, 0usize, 0usize);
    let mut found = false;
    for y in 0..h {
        let row = y * w * 4;
        for x in 0..w {
            if rgba[row + x * 4 + 3] > threshold {
                found = true;
                if x < min_x { min_x = x; }
                if x > max_x { max_x = x; }
                if y < min_y { min_y = y; }
                if y > max_y { max_y = y; }
            }
        }
    }
    found.then(|| (min_x as u32, min_y as u32, max_x as u32, max_y as u32))
}
