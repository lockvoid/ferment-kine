//! Resolved scene → vello_cpu pixels → PNG. This module owns PIXELS: node
//! walking with transform stacks, parley layout + unit segmentation for text,
//! per-unit deltas at glyph_run time, pills, shadows, strokes. It consumes only
//! eval's resolved types; values were decided upstream.

use vello_common::filter_effects::{Filter, FilterFunction};
use vello_cpu::kurbo::{Affine, Point, Rect, RoundedRect, Shape as KurboShape, Stroke, Vec2};
use vello_cpu::peniko::color::DynamicColor;
use vello_cpu::peniko::{ColorStop, Gradient};
use vello_cpu::{Glyph, Pixmap, RenderContext, RenderSettings, Resources};

use parley::{
    Alignment, AlignmentOptions, FontFamily, FontStyle, FontWeight, Layout, LayoutContext,
    LineHeight, PositionedLayoutItem, StyleProperty,
};

use crate::eval::{
    self, stagger_weights, Color, MapValue, RNode, RPaint, RShape, RText, RUnitAnimator, RWeight,
    Scene, UnitProperty,
};
use crate::fonts;
use crate::schema::{Align, LineCap, LineJoin, VAlign};
use crate::validate::UnitLevel;

/// Render to a PNG blob (RGBA8, straight/un-premultiplied alpha per PNG).
pub fn render_png(scene: &Scene, width: u32, height: u32) -> Result<Vec<u8>, String> {
    render_pixmap(scene, width, height)?
        .into_png()
        .map_err(|e| format!("PNG encoding failed: {e}"))
}

/// Render to raw RGBA8 bytes: premultiplied alpha, sRGB, row-major, stride =
/// width*4, len = width*height*4. This is what texture-upload clients want (no
/// PNG round-trip). Note: premultiplied, unlike `render_png` (straight alpha).
pub fn render_rgba(scene: &Scene, width: u32, height: u32) -> Result<Vec<u8>, String> {
    Ok(render_pixmap(scene, width, height)?
        .data_as_u8_slice()
        .to_vec())
}

fn render_pixmap(scene: &Scene, width: u32, height: u32) -> Result<Pixmap, String> {
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

    // Single-threaded: the multi-threaded dispatcher doesn't support filter
    // effects (text shadows) yet, and one thread keeps renders deterministic
    // independent of host core count.
    let settings = RenderSettings {
        num_threads: 0,
        ..RenderSettings::default()
    };
    let mut ctx = RenderContext::new_with(width as u16, height as u16, settings);
    let mut resources = Resources::new();

    let root = Affine::scale_non_uniform(width as f64 / scene.width, height as f64 / scene.height);
    draw_node(&mut ctx, &mut resources, &scene.root, root)?;

    let mut pixmap = Pixmap::new(width as u16, height as u16);
    ctx.flush();
    ctx.render_to_pixmap(&mut resources, &mut pixmap);
    Ok(pixmap)
}

fn draw_node(
    ctx: &mut RenderContext,
    resources: &mut Resources,
    node: &RNode,
    parent: Affine,
) -> Result<(), String> {
    match node {
        RNode::Group(group) => {
            let layered = push_opacity(ctx, group.opacity);
            let affine = parent * group.transform;
            for child in &group.children {
                draw_node(ctx, resources, child, affine)?;
            }
            pop_opacity(ctx, layered);
        }
        RNode::Shape(shape) => {
            let layered = push_opacity(ctx, shape.opacity);
            draw_shape(ctx, shape, parent);
            pop_opacity(ctx, layered);
        }
        RNode::Text(text) => {
            let layered = push_opacity(ctx, text.opacity);
            draw_text(ctx, resources, text, parent)?;
            pop_opacity(ctx, layered);
        }
    }
    Ok(())
}

fn push_opacity(ctx: &mut RenderContext, opacity: f64) -> bool {
    if opacity < 1.0 {
        ctx.push_opacity_layer(opacity as f32);
        true
    } else {
        false
    }
}

fn pop_opacity(ctx: &mut RenderContext, layered: bool) {
    if layered {
        ctx.pop_layer();
    }
}

// --- shapes -----------------------------------------------------------------------

fn draw_shape(ctx: &mut RenderContext, shape: &RShape, parent: Affine) {
    ctx.set_transform(parent * shape.transform);
    if let Some(fill) = &shape.fill {
        set_paint(ctx, fill);
        ctx.fill_path(&shape.path);
    }
    if let Some(stroke) = &shape.stroke {
        ctx.set_paint(stroke.color);
        ctx.set_stroke(build_stroke(stroke.width, stroke.cap, stroke.join));
        ctx.stroke_path(&shape.path);
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

fn set_paint(ctx: &mut RenderContext, paint: &RPaint) {
    match paint {
        RPaint::Solid(color) => ctx.set_paint(*color),
        RPaint::Linear {
            x1,
            y1,
            x2,
            y2,
            stops,
        } => {
            let gradient = Gradient::new_linear(Point::new(*x1, *y1), Point::new(*x2, *y2))
                .with_stops(color_stops(stops).as_slice());
            ctx.set_paint(gradient);
        }
        RPaint::Radial { cx, cy, r, stops } => {
            let gradient = Gradient::new_radial(Point::new(*cx, *cy), *r as f32)
                .with_stops(color_stops(stops).as_slice());
            ctx.set_paint(gradient);
        }
    }
}

fn color_stops(stops: &[(f32, Color)]) -> Vec<ColorStop> {
    stops
        .iter()
        .map(|(at, color)| ColorStop::from((*at, DynamicColor::from_alpha_color(*color))))
        .collect()
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
struct RunStyle {
    font: vello_cpu::peniko::FontData,
    size: f32,
    coords: Vec<i16>,
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

fn draw_text(
    ctx: &mut RenderContext,
    resources: &mut Resources,
    text: &RText,
    parent: Affine,
) -> Result<(), String> {
    if text.content.is_empty() {
        return Ok(());
    }
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

    // Layout once per input set; everything after is per-glyph draw math.
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
    let mut pill_opacity: Vec<f64> = vec![1.0; word_count];

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

    ctx.set_transform(full);

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
            ctx.set_transform(full * affine);
            ctx.set_paint(with_opacity(pill_colors[word], opacity));
            ctx.fill_path(&path);
        }
        ctx.set_transform(full);
    }

    if let Some(shadow) = &style.shadow {
        let sigma = (shadow.blur * 0.5).max(0.0) as f32;
        let filter =
            (sigma > 0.0).then(|| Filter::from_function(FilterFunction::Blur { radius: sigma }));
        ctx.push_layer(None, None, None, None, filter);
        for (g, record) in glyphs.iter().enumerate() {
            let opacity = glyph_opacity[g].clamp(0.0, 1.0);
            if opacity <= 0.0 {
                continue;
            }
            draw_glyph(
                ctx,
                resources,
                &runs[record.run],
                record,
                glyph_affine(record),
                Vec2::new(shadow.dx, shadow.dy),
                with_opacity(shadow.color, opacity),
                None,
            );
        }
        ctx.pop_layer();
    }

    for (g, record) in glyphs.iter().enumerate() {
        let opacity = glyph_opacity[g].clamp(0.0, 1.0);
        if opacity <= 0.0 {
            continue;
        }
        draw_glyph(
            ctx,
            resources,
            &runs[record.run],
            record,
            glyph_affine(record),
            Vec2::ZERO,
            with_opacity(glyph_colors[g], opacity),
            None,
        );
    }

    if let Some((stroke_color, stroke_width)) = &style.stroke {
        ctx.set_stroke(Stroke::new(*stroke_width));
        for (g, record) in glyphs.iter().enumerate() {
            let opacity = glyph_opacity[g].clamp(0.0, 1.0);
            if opacity <= 0.0 {
                continue;
            }
            draw_glyph(
                ctx,
                resources,
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
fn draw_glyph(
    ctx: &mut RenderContext,
    resources: &mut Resources,
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

    ctx.set_paint(color);
    let builder = ctx
        .glyph_run(resources, &run.font)
        .font_size(run.size)
        .normalized_coords(&run.coords)
        .glyph_transform(linear)
        .hint(false);
    let glyph = std::iter::once(Glyph {
        id: record.id,
        x: position.x as f32,
        y: position.y as f32,
    });
    match stroke {
        Some(()) => builder.stroke_glyphs(glyph),
        None => builder.fill_glyphs(glyph),
    }
}
