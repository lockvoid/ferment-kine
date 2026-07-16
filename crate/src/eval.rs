//! (document, signals) → resolved property values. This module owns VALUES:
//! input resolution, curve evaluation, animator composition, and the flat
//! resolved scene handed to render.rs. It sees schema types (above) and hands
//! down only its own resolved types. Geometry primitives (kurbo) are used via
//! the vello_cpu re-export so type identity with the renderer is guaranteed.

use std::collections::HashMap;
use std::sync::Arc;

use color::{AlphaColor, HueDirection, Oklab, Oklch, Srgb};
use vello_cpu::kurbo::{Affine, BezPath, Point, Rect, Shape as KurboShape};
use vello_cpu::Pixmap;

use crate::assets::DecodedAssets;
use crate::schema::SchemaError;
use crate::schema::{
    self, Align, Bindable, ColorExpr, ColorRef, ColorValue, Composite, Document, Ease, Fit,
    Geometry, Hue, Input, NamedEase, Node, Paint, Slant, StaggerFrom, VAlign, ValueLit, Weight,
};
use crate::validate::{parse_color, parse_target, prop_type, PropType, UnitLevel};

pub type Color = AlphaColor<Srgb>;

// --- resolved scene -----------------------------------------------------------

pub struct Scene {
    pub width: f64,
    pub height: f64,
    pub root: RNode,
}

pub enum RNode {
    Group(RGroup),
    Shape(RShape),
    Image(RImage),
    Text(Box<RText>),
}

pub struct RImage {
    pub transform: Affine,
    pub opacity: f64,
    pub frame: Rect,
    pub fit: Fit,
    pub corner_radius: f64,
    /// The premultiplied frame sampled for this render (§4 time rule).
    pub image: Arc<Pixmap>,
}

pub struct RGroup {
    pub transform: Affine,
    pub opacity: f64,
    pub children: Vec<RNode>,
}

pub struct RShape {
    pub transform: Affine,
    pub opacity: f64,
    pub path: BezPath,
    pub fill: Option<RPaint>,
    pub stroke: Option<RStroke>,
}

pub enum RPaint {
    Solid(Color),
    Linear {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
        stops: Vec<(f32, Color)>,
    },
    Radial {
        cx: f64,
        cy: f64,
        r: f64,
        stops: Vec<(f32, Color)>,
    },
}

pub struct RStroke {
    pub color: Color,
    pub width: f64,
    pub cap: schema::LineCap,
    pub join: schema::LineJoin,
}

pub struct RText {
    pub transform: Affine,
    pub opacity: f64,
    pub frame: Rect,
    pub content: String,
    pub style: RTextStyle,
    pub unit_animators: Vec<RUnitAnimator>,
}

pub struct RTextStyle {
    pub family: String,
    pub weight: f32,
    pub italic: bool,
    pub size: f32,
    pub letter_spacing: f32,
    pub line_height: f32,
    pub align: Align,
    pub valign: VAlign,
    pub fill: Color,
    pub active_fill: Option<Color>,
    pub stroke: Option<(Color, f64)>,
    pub shadow: Option<RShadow>,
    pub pill: Option<RPill>,
}

pub struct RShadow {
    pub color: Color,
    pub dx: f64,
    pub dy: f64,
    pub blur: f64,
}

pub struct RPill {
    pub color: Color,
    pub radius: f64,
    pub padding_x: f64,
    pub padding_y: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitProperty {
    Opacity,
    TranslateX,
    TranslateY,
    Scale,
    Rotate,
    Color,
    PillColor,
    PillOpacity,
}

pub struct RUnitAnimator {
    pub level: UnitLevel,
    pub property: UnitProperty,
    pub weight: RWeight,
    pub map: ValueMap,
    pub composite: Composite,
    pub amount: f64,
    pub space: InterpSpace,
}

pub enum RWeight {
    Activations(Vec<f64>),
    Stagger(RStagger),
}

pub struct RStagger {
    /// The driver's resolved progress at this render.
    pub driver_u: f64,
    pub total: Option<f64>,
    pub each: Option<f64>,
    pub from: StaggerFrom,
    pub ease: Option<Curve>,
    pub seed: u64,
    pub indices: Vec<u32>,
}

#[derive(Debug, Clone, Copy)]
pub struct InterpSpace {
    pub space: schema::ColorSpace,
    pub hue: Hue,
}

impl Default for InterpSpace {
    fn default() -> Self {
        InterpSpace {
            space: schema::ColorSpace::Oklab,
            hue: Hue::Shorter,
        }
    }
}

// --- curves ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Curve {
    Linear,
    Hold,
    Bezier { x1: f64, y1: f64, x2: f64, y2: f64 },
    Points(Vec<[f64; 2]>),
}

impl Curve {
    pub fn from_ease(ease: &Ease) -> Curve {
        match ease {
            Ease::Named(NamedEase::Linear) => Curve::Linear,
            Ease::Named(NamedEase::Hold) => Curve::Hold,
            Ease::Named(NamedEase::EaseIn) => Curve::Bezier {
                x1: 0.42,
                y1: 0.0,
                x2: 1.0,
                y2: 1.0,
            },
            Ease::Named(NamedEase::EaseOut) => Curve::Bezier {
                x1: 0.0,
                y1: 0.0,
                x2: 0.58,
                y2: 1.0,
            },
            Ease::Named(NamedEase::EaseInOut) => Curve::Bezier {
                x1: 0.42,
                y1: 0.0,
                x2: 0.58,
                y2: 1.0,
            },
            Ease::Bezier(b) => Curve::Bezier {
                x1: b[0],
                y1: b[1],
                x2: b[2],
                y2: b[3],
            },
            Ease::Points(points) => Curve::Points(points.clone()),
            // Springs were baked to points at validation.
            Ease::Spring { .. } => unreachable!("springs are baked at validation"),
        }
    }

    pub fn eval(&self, u: f64) -> f64 {
        let u = u.clamp(0.0, 1.0);
        match self {
            Curve::Linear => u,
            Curve::Hold => {
                if u >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            }
            Curve::Bezier { x1, y1, x2, y2 } => {
                let t = solve_bezier_t(*x1, *x2, u);
                cubic(*y1, *y2, t)
            }
            Curve::Points(points) => {
                if u <= points[0][0] {
                    return points[0][1];
                }
                for window in points.windows(2) {
                    let [x0, y0] = window[0];
                    let [x1, y1] = window[1];
                    if u <= x1 {
                        if x1 - x0 <= f64::EPSILON {
                            return y1;
                        }
                        let f = (u - x0) / (x1 - x0);
                        return y0 + (y1 - y0) * f;
                    }
                }
                points[points.len() - 1][1]
            }
        }
    }
}

/// Cubic bezier coordinate for control values (c1, c2) at parameter t
/// (endpoints pinned at 0 and 1).
fn cubic(c1: f64, c2: f64, t: f64) -> f64 {
    let mt = 1.0 - t;
    3.0 * mt * mt * t * c1 + 3.0 * mt * t * t * c2 + t * t * t
}

/// Solve x(t) = x for t via Newton with bisection fallback (CSS cubic-bezier).
fn solve_bezier_t(x1: f64, x2: f64, x: f64) -> f64 {
    let mut t = x;
    for _ in 0..8 {
        let error = cubic(x1, x2, t) - x;
        if error.abs() < 1e-7 {
            return t;
        }
        let mt = 1.0 - t;
        let derivative = 3.0 * mt * mt * x1 + 6.0 * mt * t * (x2 - x1) + 3.0 * t * t * (1.0 - x2);
        if derivative.abs() < 1e-7 {
            break;
        }
        t -= error / derivative;
    }
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    t = x;
    for _ in 0..32 {
        let error = cubic(x1, x2, t) - x;
        if error.abs() < 1e-7 {
            break;
        }
        if error > 0.0 {
            hi = t;
        } else {
            lo = t;
        }
        t = (lo + hi) / 2.0;
    }
    t
}

// --- value mapping ----------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub enum MapValue {
    Num(f64),
    Color(Color),
}

pub struct MapKey {
    pub at: f64,
    pub value: MapValue,
    /// Ease of the segment ARRIVING at this keyframe.
    pub ease: Option<Curve>,
}

pub struct ValueMap {
    pub keys: Vec<MapKey>,
}

impl ValueMap {
    pub fn eval(&self, u: f64, space: InterpSpace) -> MapValue {
        let u = u.clamp(0.0, 1.0);
        let keys = &self.keys;
        if u <= keys[0].at {
            return keys[0].value;
        }
        for i in 1..keys.len() {
            if u <= keys[i].at {
                let span = keys[i].at - keys[i - 1].at;
                let local = if span <= f64::EPSILON {
                    1.0
                } else {
                    (u - keys[i - 1].at) / span
                };
                let f = match &keys[i].ease {
                    Some(curve) => curve.eval(local),
                    None => local,
                };
                return lerp_values(keys[i - 1].value, keys[i].value, f, space);
            }
        }
        keys[keys.len() - 1].value
    }
}

fn lerp_values(a: MapValue, b: MapValue, f: f64, space: InterpSpace) -> MapValue {
    match (a, b) {
        (MapValue::Num(x), MapValue::Num(y)) => MapValue::Num(x + (y - x) * f),
        (MapValue::Color(x), MapValue::Color(y)) => MapValue::Color(lerp_color(x, y, f, space)),
        _ => a,
    }
}

/// Interpolate premultiplied in the given space (AlphaColor::lerp premultiplies,
/// lerps, un-premultiplies). Output converts back to sRGB; components are only
/// gamut-clipped at paint time, never mid-interpolation.
pub fn lerp_color(a: Color, b: Color, f: f64, space: InterpSpace) -> Color {
    let t = f as f32;
    let direction = match space.hue {
        Hue::Shorter => HueDirection::Shorter,
        Hue::Longer => HueDirection::Longer,
        Hue::Increasing => HueDirection::Increasing,
        Hue::Decreasing => HueDirection::Decreasing,
    };
    match space.space {
        schema::ColorSpace::Srgb => a.lerp(b, t, direction),
        schema::ColorSpace::Oklab => a
            .convert::<Oklab>()
            .lerp(b.convert::<Oklab>(), t, direction)
            .convert::<Srgb>(),
        schema::ColorSpace::Oklch => a
            .convert::<Oklch>()
            .lerp(b.convert::<Oklch>(), t, direction)
            .convert::<Srgb>(),
    }
}

// --- composition ---------------------------------------------------------------------

pub fn composite_num(previous: f64, value: f64, composite: Composite, amount: f64) -> f64 {
    let composed = match composite {
        Composite::Replace => value,
        Composite::Add => previous + value,
        Composite::Multiply => previous * value,
    };
    previous + (composed - previous) * amount
}

pub fn composite_color(previous: Color, value: Color, amount: f64, space: InterpSpace) -> Color {
    // Colors composite replace-only in v1 (validated); amount is the lerp.
    lerp_color(previous, value, amount, space)
}

// --- stagger --------------------------------------------------------------------------

/// Per-unit weights for a stagger at the resolved driver progress. Offsets
/// consume `total` (or `each`·(n−1)) of the driver span; each unit's window is
/// the remainder. Deterministic: seeded shuffle, no ambient randomness.
pub fn stagger_weights(stagger: &RStagger, n: usize) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let ranks = stagger_ranks(stagger, n);
    let tau = match (stagger.total, stagger.each) {
        (Some(total), _) => total,
        (None, Some(each)) => each * (n.saturating_sub(1)) as f64,
        (None, None) => 0.0,
    }
    .clamp(0.0, 0.999);
    let window = 1.0 - tau;

    ranks
        .into_iter()
        .map(|rank| {
            let distributed = match &stagger.ease {
                Some(curve) => curve.eval(rank),
                None => rank,
            };
            let offset = tau * distributed;
            ((stagger.driver_u - offset) / window).clamp(0.0, 1.0)
        })
        .collect()
}

fn stagger_ranks(stagger: &RStagger, n: usize) -> Vec<f64> {
    if n == 1 {
        return vec![0.0];
    }
    let last = (n - 1) as f64;
    match stagger.from {
        StaggerFrom::Start => (0..n).map(|i| i as f64 / last).collect(),
        StaggerFrom::End => (0..n).map(|i| (last - i as f64) / last).collect(),
        StaggerFrom::Center => {
            let center = last / 2.0;
            (0..n)
                .map(|i| (i as f64 - center).abs() / center.max(f64::EPSILON))
                .collect()
        }
        StaggerFrom::Edges => {
            let center = last / 2.0;
            (0..n)
                .map(|i| 1.0 - (i as f64 - center).abs() / center.max(f64::EPSILON))
                .collect()
        }
        StaggerFrom::Random => {
            let mut order: Vec<usize> = (0..n).collect();
            let mut state = stagger.seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
            for i in (1..n).rev() {
                // splitmix64 step.
                let mut z = state;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                z ^= z >> 31;
                state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let j = (z % (i as u64 + 1)) as usize;
                order.swap(i, j);
            }
            let mut ranks = vec![0.0; n];
            for (position, unit) in order.into_iter().enumerate() {
                ranks[unit] = position as f64 / last;
            }
            ranks
        }
        StaggerFrom::Index => {
            let max = stagger
                .indices
                .iter()
                .take(n)
                .copied()
                .max()
                .unwrap_or(0)
                .max(1) as f64;
            (0..n)
                .map(|i| stagger.indices.get(i).copied().unwrap_or(i as u32) as f64 / max)
                .collect()
        }
    }
}

// --- input resolution --------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum InputValue {
    Unit(f64),
    UnitArray(Vec<f64>),
    Time(f64),
    Number(f64),
    Str(String),
    Color(Color),
    Enum(String),
    Font(String),
}

impl InputValue {
    fn as_number(&self) -> Option<f64> {
        match self {
            InputValue::Unit(v) | InputValue::Time(v) | InputValue::Number(v) => Some(*v),
            _ => None,
        }
    }
}

/// Resolve declared inputs against supplied signals. Supply is LENIENT: missing
/// or wrongly-typed signals fall back to defaults, unknown keys are ignored.
/// The C-ABI `t` is sugar for a declared `time` input; an explicit signal wins.
pub fn resolve_inputs(
    doc: &Document,
    signals: &serde_json::Value,
    t: f64,
) -> Result<HashMap<String, InputValue>, SchemaError> {
    let supplied = match signals {
        serde_json::Value::Object(map) => Some(map),
        serde_json::Value::Null => None,
        _ => return Err(SchemaError::new("signals", "must be a JSON object")),
    };

    let mut values = HashMap::new();
    for input in &doc.inputs {
        let key = input.key();
        let signal = supplied.and_then(|map| map.get(key));
        let value = match input {
            Input::Unit(i) => InputValue::Unit(
                signal
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(i.default)
                    .clamp(0.0, 1.0),
            ),
            Input::UnitArray(i) => {
                let array = signal.and_then(serde_json::Value::as_array).map(|items| {
                    items
                        .iter()
                        .map(|v| v.as_f64().unwrap_or(0.0).clamp(0.0, 1.0))
                        .collect::<Vec<_>>()
                });
                InputValue::UnitArray(array.unwrap_or_else(|| i.default.clone()))
            }
            Input::Time(i) => {
                let fallback = if key == "time" { t.max(0.0) } else { i.default };
                InputValue::Time(
                    signal
                        .and_then(serde_json::Value::as_f64)
                        .unwrap_or(fallback)
                        .max(0.0),
                )
            }
            Input::Number(i) => {
                let mut v = signal
                    .and_then(serde_json::Value::as_f64)
                    .unwrap_or(i.default);
                if let Some(min) = i.min {
                    v = v.max(min);
                }
                if let Some(max) = i.max {
                    v = v.min(max);
                }
                InputValue::Number(v)
            }
            Input::String(i) => InputValue::Str(
                signal
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| i.default.clone()),
            ),
            Input::Color(i) => InputValue::Color(
                signal
                    .and_then(serde_json::Value::as_str)
                    .and_then(parse_color)
                    .unwrap_or_else(|| parse_color(&i.default).expect("validated default")),
            ),
            Input::Enum(i) => {
                let candidate = signal
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                InputValue::Enum(match candidate {
                    Some(v) if i.values.contains(&v) => v,
                    _ => i.default.clone(),
                })
            }
            Input::FontFamily(i) => InputValue::Font(
                signal
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| i.default.clone()),
            ),
        };
        values.insert(key.to_string(), value);
    }
    Ok(values)
}

// --- document evaluation --------------------------------------------------------------------

struct Ctx<'a> {
    doc: &'a Document,
    inputs: &'a HashMap<String, InputValue>,
    colors: &'a HashMap<String, Color>,
    assets: &'a DecodedAssets,
}

pub fn evaluate(
    doc: &Document,
    assets: &DecodedAssets,
    signals: &serde_json::Value,
    t: f64,
) -> Result<Scene, SchemaError> {
    let inputs = resolve_inputs(doc, signals, t)?;
    let colors = resolve_colors(doc, &inputs, signals);
    let ctx = Ctx {
        doc,
        inputs: &inputs,
        colors: &colors,
        assets,
    };
    let (root, _bounds) = resolve_node(&ctx, &doc.root)?;
    Ok(Scene {
        width: doc.size.width,
        height: doc.size.height,
        root,
    })
}

impl Ctx<'_> {
    fn num(&self, binding: &Bindable<f64>) -> f64 {
        match binding {
            Bindable::Literal(v) => *v,
            Bindable::Input(key) => self.inputs[key.as_str()].as_number().unwrap_or(0.0),
        }
    }

    fn opt_num(&self, binding: &Option<Bindable<f64>>, default: f64) -> f64 {
        binding.as_ref().map_or(default, |b| self.num(b))
    }

    fn string(&self, binding: &Bindable<String>) -> String {
        match binding {
            Bindable::Literal(v) => v.clone(),
            Bindable::Input(key) => match &self.inputs[key.as_str()] {
                InputValue::Str(s) | InputValue::Enum(s) | InputValue::Font(s) => s.clone(),
                _ => String::new(),
            },
        }
    }

    fn color(&self, value: &ColorValue) -> Color {
        match value {
            ColorValue::Literal(v) => parse_color(v).expect("validated color literal"),
            ColorValue::Input(key) => input_color(self.inputs, key),
            ColorValue::Color(key) => *self.colors.get(key).expect("validated color reference"),
        }
    }

    fn driver_u(&self, driver: &str, period: Option<f64>) -> f64 {
        match &self.inputs[driver] {
            InputValue::Unit(v) => *v,
            InputValue::Time(t) => {
                let period = period.expect("validated period");
                (t.rem_euclid(period)) / period
            }
            _ => 0.0,
        }
    }

    fn map_value(&self, value: &Bindable<ValueLit>, property_type: PropType) -> MapValue {
        match value {
            Bindable::Literal(ValueLit::Number(v)) => MapValue::Num(*v),
            Bindable::Literal(ValueLit::Color(s)) => {
                MapValue::Color(parse_color(s).expect("validated color literal"))
            }
            Bindable::Literal(ValueLit::ColorRef(key)) => {
                MapValue::Color(*self.colors.get(key).expect("validated color reference"))
            }
            Bindable::Input(key) => match (&self.inputs[key.as_str()], property_type) {
                (InputValue::Color(c), _) => MapValue::Color(*c),
                (v, _) => MapValue::Num(v.as_number().unwrap_or(0.0)),
            },
        }
    }
}

fn input_color(inputs: &HashMap<String, InputValue>, key: &str) -> Color {
    match inputs.get(key) {
        Some(InputValue::Color(c)) => *c,
        _ => AlphaColor::from_rgba8(0, 0, 0, 255),
    }
}

fn bind_num(binding: &Bindable<f64>, inputs: &HashMap<String, InputValue>) -> f64 {
    match binding {
        Bindable::Literal(v) => *v,
        Bindable::Input(key) => inputs
            .get(key)
            .and_then(InputValue::as_number)
            .unwrap_or(0.0),
    }
}

// --- color table (§3) --------------------------------------------------------

/// Resolve the document's color table in declaration order. An entry's
/// `override` input wins when that signal was actually supplied; otherwise the
/// computed value is used. Earlier entries are available to later ones.
fn resolve_colors(
    doc: &Document,
    inputs: &HashMap<String, InputValue>,
    signals: &serde_json::Value,
) -> HashMap<String, Color> {
    let supplied = signals.as_object();
    let mut table: HashMap<String, Color> = HashMap::new();
    for entry in &doc.colors {
        let overridden = entry.override_input.as_ref().and_then(|over| {
            let was_supplied = supplied.is_some_and(|map| map.contains_key(&over.input));
            match (was_supplied, inputs.get(over.input.as_str())) {
                (true, Some(InputValue::Color(c))) => Some(*c),
                _ => None,
            }
        });
        let color = overridden.unwrap_or_else(|| resolve_color_expr(&entry.value, inputs, &table));
        table.insert(entry.key.clone(), color);
    }
    table
}

fn resolve_color_expr(
    expr: &ColorExpr,
    inputs: &HashMap<String, InputValue>,
    table: &HashMap<String, Color>,
) -> Color {
    let space = InterpSpace::default();
    match expr {
        ColorExpr::Literal(s) => parse_color(s).expect("validated color literal"),
        ColorExpr::Input(key) => input_color(inputs, key),
        ColorExpr::Alpha { of, amount } => with_alpha(
            resolve_color_ref(of, inputs, table),
            bind_num(amount, inputs),
        ),
        ColorExpr::Contrast { of, candidates } => {
            let base = resolve_color_ref(of, inputs, table);
            let poles: Vec<Color> = match candidates {
                Some(list) => list
                    .iter()
                    .map(|r| resolve_color_ref(r, inputs, table))
                    .collect(),
                None => vec![
                    AlphaColor::from_rgba8(0, 0, 0, 255),
                    AlphaColor::from_rgba8(255, 255, 255, 255),
                ],
            };
            contrast_pick(base, &poles)
        }
        ColorExpr::Mix { a, b, t } => lerp_color(
            resolve_color_ref(a, inputs, table),
            resolve_color_ref(b, inputs, table),
            bind_num(t, inputs).clamp(0.0, 1.0),
            space,
        ),
    }
}

fn resolve_color_ref(
    reference: &ColorRef,
    inputs: &HashMap<String, InputValue>,
    table: &HashMap<String, Color>,
) -> Color {
    match reference {
        ColorRef::Literal(s) => parse_color(s).expect("validated color literal"),
        ColorRef::Input(key) => input_color(inputs, key),
        ColorRef::Entry(key) => *table.get(key).expect("validated earlier entry"),
    }
}

/// Absolute alpha replacement — rgb unchanged, alpha set to `amount` (§3).
pub(crate) fn with_alpha(color: Color, amount: f64) -> Color {
    let mut components = color.components;
    components[3] = amount.clamp(0.0, 1.0) as f32;
    AlphaColor::new(components)
}

/// The candidate whose Oklab lightness is farthest from `base`; ties keep the
/// earlier candidate (§3 contrast).
pub(crate) fn contrast_pick(base: Color, candidates: &[Color]) -> Color {
    let base_l = base.convert::<Oklab>().components[0];
    let mut best = candidates[0];
    let mut best_distance = -1.0f32;
    for candidate in candidates {
        let distance = (candidate.convert::<Oklab>().components[0] - base_l).abs();
        if distance > best_distance {
            best_distance = distance;
            best = *candidate;
        }
    }
    best
}

/// Node-level scalar property state, composed over by animators.
struct NodeState {
    translate_x: f64,
    translate_y: f64,
    scale: f64,
    scale_x: f64,
    scale_y: f64,
    rotate: f64,
    opacity: f64,
    color: Option<Color>,
    stroke_color: Option<Color>,
    shadow_color: Option<Color>,
}

fn resolve_node(ctx: &Ctx, node: &Node) -> Result<(RNode, Rect), SchemaError> {
    match node {
        Node::Group(group) => {
            let mut children = Vec::with_capacity(group.children.len());
            let mut bounds: Option<Rect> = None;
            for child in &group.children {
                let (resolved, child_bounds) = resolve_node(ctx, child)?;
                bounds = Some(match bounds {
                    Some(b) => b.union(child_bounds),
                    None => child_bounds,
                });
                children.push(resolved);
            }
            let bounds = bounds.unwrap_or(Rect::ZERO);

            let transform = group.transform.as_ref();
            let mut state = NodeState {
                translate_x: transform.map_or(0.0, |t| ctx.opt_num(&t.translate_x, 0.0)),
                translate_y: transform.map_or(0.0, |t| ctx.opt_num(&t.translate_y, 0.0)),
                scale: transform.map_or(1.0, |t| ctx.opt_num(&t.scale, 1.0)),
                scale_x: 1.0,
                scale_y: 1.0,
                rotate: transform.map_or(0.0, |t| ctx.opt_num(&t.rotate, 0.0)),
                opacity: group.opacity.as_ref().map_or(1.0, |b| ctx.num(b)),
                color: None,
                stroke_color: None,
                shadow_color: None,
            };
            apply_node_animators(ctx, &group.key, &mut state)?;

            let anchor = Point::new(
                bounds.x0
                    + transform.map_or(0.5, |t| ctx.opt_num(&t.anchor_x, 0.5)) * bounds.width(),
                bounds.y0
                    + transform.map_or(0.5, |t| ctx.opt_num(&t.anchor_y, 0.5)) * bounds.height(),
            );
            let affine = build_affine(&state, anchor);
            let reported = affine.transform_rect_bbox(bounds);
            Ok((
                RNode::Group(RGroup {
                    transform: affine,
                    opacity: state.opacity.clamp(0.0, 1.0),
                    children,
                }),
                reported,
            ))
        }
        Node::Shape(shape) => {
            let path = resolve_geometry(ctx, &shape.geometry);
            let bounds = path.bounding_box();

            let mut fill = shape.fill.as_ref().map(|paint| resolve_paint(ctx, paint));
            let mut stroke = shape.stroke.as_ref().map(|s| RStroke {
                color: ctx.color(&s.color),
                width: ctx.num(&s.width),
                cap: s.cap.unwrap_or(schema::LineCap::Butt),
                join: s.join.unwrap_or(schema::LineJoin::Miter),
            });

            let mut state = identity_state();
            apply_node_animators(ctx, &shape.key, &mut state)?;
            if let (Some(color), Some(RPaint::Solid(solid))) = (state.color, fill.as_mut()) {
                *solid = color;
            }
            if let (Some(color), Some(s)) = (state.stroke_color, stroke.as_mut()) {
                s.color = color;
            }

            let affine = build_affine(&state, bounds.center());
            Ok((
                RNode::Shape(RShape {
                    transform: affine,
                    opacity: state.opacity.clamp(0.0, 1.0),
                    path,
                    fill,
                    stroke,
                }),
                affine.transform_rect_bbox(bounds),
            ))
        }
        Node::Image(image) => {
            let x = ctx.num(&image.frame.x);
            let y = ctx.num(&image.frame.y);
            let frame = Rect::new(
                x,
                y,
                x + ctx.num(&image.frame.width),
                y + ctx.num(&image.frame.height),
            );
            let decoded = ctx
                .assets
                .get(&image.asset)
                .expect("validated asset reference");
            // §4: animated assets show the frame at `time mod loopDuration`; the
            // sampling clock is the standard `time` input (0 when unsupplied).
            let time = match ctx.inputs.get("time") {
                Some(InputValue::Time(t)) => *t,
                _ => 0.0,
            };
            let sampled = decoded.sample(time).clone();

            let mut state = identity_state();
            state.opacity = image.opacity.as_ref().map_or(1.0, |b| ctx.num(b));
            apply_node_animators(ctx, &image.key, &mut state)?;
            let affine = build_affine(&state, frame.center());
            Ok((
                RNode::Image(RImage {
                    transform: affine,
                    opacity: state.opacity.clamp(0.0, 1.0),
                    frame,
                    fit: image.fit.unwrap_or(Fit::Cover),
                    corner_radius: image.corner_radius.as_ref().map_or(0.0, |b| ctx.num(b)),
                    image: sampled,
                }),
                affine.transform_rect_bbox(frame),
            ))
        }
        Node::Text(text) => {
            let frame = Rect::new(
                ctx.num(&text.frame.x),
                ctx.num(&text.frame.y),
                ctx.num(&text.frame.x) + ctx.num(&text.frame.width),
                ctx.num(&text.frame.y) + ctx.num(&text.frame.height),
            );

            let style = &text.style;
            let mut resolved_style = RTextStyle {
                family: ctx.string(&style.font_family),
                weight: ctx.opt_num(&style.weight, 400.0) as f32,
                italic: style.slant == Some(Slant::Italic),
                size: ctx.num(&style.size) as f32,
                letter_spacing: ctx.opt_num(&style.letter_spacing, 0.0) as f32,
                line_height: ctx.opt_num(&style.line_height, 1.2) as f32,
                align: style.align.unwrap_or(Align::Left),
                valign: style.valign.unwrap_or(VAlign::Top),
                fill: ctx.color(&style.fill),
                active_fill: style.active_fill.as_ref().map(|c| ctx.color(c)),
                stroke: style
                    .stroke
                    .as_ref()
                    .map(|s| (ctx.color(&s.color), ctx.num(&s.width))),
                shadow: style.shadow.as_ref().map(|s| RShadow {
                    color: ctx.color(&s.color),
                    dx: ctx.opt_num(&s.offset_x, 0.0),
                    dy: ctx.opt_num(&s.offset_y, 0.0),
                    blur: ctx.opt_num(&s.blur, 0.0),
                }),
                pill: style.pill.as_ref().map(|p| RPill {
                    color: ctx.color(&p.color),
                    radius: ctx.opt_num(&p.radius, 0.0),
                    padding_x: ctx.opt_num(&p.padding_x, 0.0),
                    padding_y: ctx.opt_num(&p.padding_y, 0.0),
                }),
            };

            let mut state = identity_state();
            apply_node_animators(ctx, &text.key, &mut state)?;
            if let Some(color) = state.color {
                resolved_style.fill = color;
            }
            if let Some(color) = state.stroke_color {
                if let Some(stroke) = resolved_style.stroke.as_mut() {
                    stroke.0 = color;
                }
            }
            if let Some(color) = state.shadow_color {
                if let Some(shadow) = resolved_style.shadow.as_mut() {
                    shadow.color = color;
                }
            }

            let unit_animators = resolve_unit_animators(ctx, &text.key, &resolved_style)?;
            let affine = build_affine(&state, frame.center());
            Ok((
                RNode::Text(Box::new(RText {
                    transform: affine,
                    opacity: state.opacity.clamp(0.0, 1.0),
                    frame,
                    content: ctx.string(&text.content),
                    style: resolved_style,
                    unit_animators,
                })),
                affine.transform_rect_bbox(frame),
            ))
        }
    }
}

fn identity_state() -> NodeState {
    NodeState {
        translate_x: 0.0,
        translate_y: 0.0,
        scale: 1.0,
        scale_x: 1.0,
        scale_y: 1.0,
        rotate: 0.0,
        opacity: 1.0,
        color: None,
        stroke_color: None,
        shadow_color: None,
    }
}

fn build_affine(state: &NodeState, anchor: Point) -> Affine {
    Affine::translate((state.translate_x, state.translate_y))
        * Affine::translate((anchor.x, anchor.y))
        * Affine::rotate(state.rotate.to_radians())
        * Affine::scale_non_uniform(state.scale * state.scale_x, state.scale * state.scale_y)
        * Affine::translate((-anchor.x, -anchor.y))
}

fn apply_node_animators(ctx: &Ctx, key: &str, state: &mut NodeState) -> Result<(), SchemaError> {
    for animator in &ctx.doc.animators {
        let (target, level) = parse_target(&animator.target);
        if target != key || level.is_some() {
            continue;
        }
        let driver = animator
            .driver
            .as_deref()
            .expect("validated node-level driver");
        let u = ctx.driver_u(driver, animator.period);
        let property_type = prop_type(&animator.property);
        let map = build_value_map(ctx, animator, property_type, None)?;
        let space = interp_space(animator);
        let amount = animator
            .amount
            .as_ref()
            .map_or(1.0, |a| ctx.num(a))
            .clamp(0.0, 1.0);
        let composite = animator.composite.unwrap_or(Composite::Replace);
        let value = map.eval(u, space);

        match (animator.property.as_str(), value) {
            ("opacity", MapValue::Num(v)) => {
                state.opacity = composite_num(state.opacity, v, composite, amount);
            }
            ("translateX", MapValue::Num(v)) => {
                state.translate_x = composite_num(state.translate_x, v, composite, amount);
            }
            ("translateY", MapValue::Num(v)) => {
                state.translate_y = composite_num(state.translate_y, v, composite, amount);
            }
            ("scale", MapValue::Num(v)) => {
                state.scale = composite_num(state.scale, v, composite, amount);
            }
            ("scaleX", MapValue::Num(v)) => {
                state.scale_x = composite_num(state.scale_x, v, composite, amount);
            }
            ("scaleY", MapValue::Num(v)) => {
                state.scale_y = composite_num(state.scale_y, v, composite, amount);
            }
            ("rotate", MapValue::Num(v)) => {
                state.rotate = composite_num(state.rotate, v, composite, amount);
            }
            ("color", MapValue::Color(c)) => {
                let previous = state.color.unwrap_or(c);
                state.color = Some(composite_color(previous, c, amount, space));
            }
            ("strokeColor", MapValue::Color(c)) => {
                let previous = state.stroke_color.unwrap_or(c);
                state.stroke_color = Some(composite_color(previous, c, amount, space));
            }
            ("shadowColor", MapValue::Color(c)) => {
                let previous = state.shadow_color.unwrap_or(c);
                state.shadow_color = Some(composite_color(previous, c, amount, space));
            }
            _ => {}
        }
    }
    Ok(())
}

fn interp_space(animator: &schema::Animator) -> InterpSpace {
    InterpSpace {
        space: animator.color_space.unwrap_or(schema::ColorSpace::Oklab),
        hue: animator.hue.unwrap_or(Hue::Shorter),
    }
}

/// Build the value map: keyframes verbatim, or from/to as two keyframes with the
/// ease arriving at the end. Implicit sub-unit karaoke fills fill → activeFill.
fn build_value_map(
    ctx: &Ctx,
    animator: &schema::Animator,
    property_type: PropType,
    karaoke: Option<(&RTextStyle, UnitProperty)>,
) -> Result<ValueMap, SchemaError> {
    if let Some(keyframes) = &animator.keyframes {
        let keys = keyframes
            .iter()
            .map(|k| MapKey {
                at: k.at,
                value: ctx.map_value(&k.value, property_type),
                ease: k.ease.as_ref().map(Curve::from_ease),
            })
            .collect();
        return Ok(ValueMap { keys });
    }

    let ease = animator.ease.as_ref().map(Curve::from_ease);
    let (from, to) = match (&animator.from, &animator.to) {
        (Some(from), Some(to)) => (
            ctx.map_value(from, property_type),
            ctx.map_value(to, property_type),
        ),
        _ => match karaoke {
            Some((style, UnitProperty::Color)) => (
                MapValue::Color(style.fill),
                MapValue::Color(style.active_fill.unwrap_or(style.fill)),
            ),
            Some((style, UnitProperty::PillColor)) => {
                let base = style.pill.as_ref().map(|p| p.color).unwrap_or(style.fill);
                (MapValue::Color(base), MapValue::Color(base))
            }
            _ => (MapValue::Num(0.0), MapValue::Num(1.0)),
        },
    };
    Ok(ValueMap {
        keys: vec![
            MapKey {
                at: 0.0,
                value: from,
                ease: None,
            },
            MapKey {
                at: 1.0,
                value: to,
                ease,
            },
        ],
    })
}

fn resolve_unit_animators(
    ctx: &Ctx,
    text_key: &str,
    style: &RTextStyle,
) -> Result<Vec<RUnitAnimator>, SchemaError> {
    let mut resolved = Vec::new();
    for animator in &ctx.doc.animators {
        let (target, Some(level)) = parse_target(&animator.target) else {
            continue;
        };
        if target != text_key {
            continue;
        }
        let property = match animator.property.as_str() {
            "opacity" => UnitProperty::Opacity,
            "translateX" => UnitProperty::TranslateX,
            "translateY" => UnitProperty::TranslateY,
            "scale" => UnitProperty::Scale,
            "rotate" => UnitProperty::Rotate,
            "color" => UnitProperty::Color,
            "pillColor" => UnitProperty::PillColor,
            "pillOpacity" => UnitProperty::PillOpacity,
            other => {
                return Err(SchemaError::new(
                    "animators",
                    format!("unexpected unit property \"{other}\""),
                ));
            }
        };
        let property_type = prop_type(&animator.property);
        let weight = match animator.weight.as_ref().expect("validated unit weight") {
            Weight::Input(key) => match &ctx.inputs[key.as_str()] {
                InputValue::UnitArray(values) => RWeight::Activations(values.clone()),
                _ => RWeight::Activations(Vec::new()),
            },
            Weight::Stagger(stagger) => RWeight::Stagger(RStagger {
                driver_u: ctx.driver_u(&stagger.driver, stagger.period),
                total: stagger.total,
                each: stagger.each,
                from: stagger.from,
                ease: stagger.ease.as_ref().map(Curve::from_ease),
                seed: stagger.seed.unwrap_or(0),
                indices: stagger.indices.clone().unwrap_or_default(),
            }),
        };
        resolved.push(RUnitAnimator {
            level,
            property,
            weight,
            map: build_value_map(ctx, animator, property_type, Some((style, property)))?,
            composite: animator.composite.unwrap_or(Composite::Replace),
            amount: animator
                .amount
                .as_ref()
                .map_or(1.0, |a| ctx.num(a))
                .clamp(0.0, 1.0),
            space: interp_space(animator),
        });
    }
    Ok(resolved)
}

fn resolve_geometry(ctx: &Ctx, geometry: &Geometry) -> BezPath {
    use vello_cpu::kurbo::{Ellipse, RoundedRect};
    match geometry {
        Geometry::Rect(g) => {
            let (x, y) = (ctx.num(&g.x), ctx.num(&g.y));
            Rect::new(x, y, x + ctx.num(&g.width), y + ctx.num(&g.height)).to_path(0.1)
        }
        Geometry::RoundedRect(g) => {
            let (x, y) = (ctx.num(&g.x), ctx.num(&g.y));
            RoundedRect::new(
                x,
                y,
                x + ctx.num(&g.width),
                y + ctx.num(&g.height),
                ctx.num(&g.radius),
            )
            .to_path(0.1)
        }
        Geometry::Ellipse(g) => Ellipse::new(
            (ctx.num(&g.cx), ctx.num(&g.cy)),
            (ctx.num(&g.rx), ctx.num(&g.ry)),
            0.0,
        )
        .to_path(0.1),
        Geometry::Path(g) => BezPath::from_svg(&g.d).expect("validated SVG path"),
    }
}

fn resolve_paint(ctx: &Ctx, paint: &Paint) -> RPaint {
    match paint {
        Paint::Solid(p) => RPaint::Solid(ctx.color(&p.color)),
        Paint::LinearGradient(p) => RPaint::Linear {
            x1: ctx.num(&p.x1),
            y1: ctx.num(&p.y1),
            x2: ctx.num(&p.x2),
            y2: ctx.num(&p.y2),
            stops: p
                .stops
                .iter()
                .map(|s| (s.at as f32, ctx.color(&s.color)))
                .collect(),
        },
        Paint::RadialGradient(p) => RPaint::Radial {
            cx: ctx.num(&p.cx),
            cy: ctx.num(&p.cy),
            r: ctx.num(&p.r),
            stops: p
                .stops
                .iter()
                .map(|s| (s.at as f32, ctx.color(&s.color)))
                .collect(),
        },
    }
}
