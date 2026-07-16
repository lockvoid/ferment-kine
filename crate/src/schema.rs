//! Serde types for the v1 motion document (docs/SCHEMA.md). This module owns
//! SHAPE only: strict field/enum parsing with path-carrying errors. Legality
//! (cross-references, domains, §6) lives in validate.rs; values in eval.rs.
//!
//! Parsing is strict: unknown fields, unknown enum values and duplicate keys
//! are hard errors. `parse` couples serde_path_to_error (paths for type/field
//! errors) with a duplicate-key pre-pass (serde_json is last-wins by default).

use std::collections::HashSet;
use std::fmt;

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;

/// A parse/validation failure with the JSON-pointer-ish path it occurred at.
#[derive(Debug, Clone)]
pub struct SchemaError {
    pub path: String,
    pub message: String,
}

impl SchemaError {
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        SchemaError {
            path: path.into(),
            message: message.into(),
        }
    }
}

impl fmt::Display for SchemaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.path.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{}: {}", self.path, self.message)
        }
    }
}

pub fn parse(json: &str) -> Result<Document, SchemaError> {
    reject_duplicate_keys(json)?;
    let deserializer = &mut serde_json::Deserializer::from_str(json);
    serde_path_to_error::deserialize(deserializer).map_err(|e| {
        let path = e.path().to_string();
        let path = if path == "." { String::new() } else { path };
        SchemaError::new(path, e.inner().to_string())
    })
}

// --- document ----------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Document {
    pub version: u32,
    pub size: Size,
    #[serde(default)]
    pub inputs: Vec<Input>,
    #[serde(default)]
    pub colors: Vec<ColorEntry>,
    #[serde(default)]
    pub assets: Vec<Asset>,
    pub root: Node,
    #[serde(default)]
    pub animators: Vec<Animator>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

// --- inputs ------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Input {
    Unit(UnitInput),
    UnitArray(UnitArrayInput),
    Time(TimeInput),
    Number(NumberInput),
    String(StringInput),
    Color(ColorInput),
    Enum(EnumInput),
    FontFamily(FontFamilyInput),
}

impl Input {
    pub fn key(&self) -> &str {
        match self {
            Input::Unit(i) => &i.key,
            Input::UnitArray(i) => &i.key,
            Input::Time(i) => &i.key,
            Input::Number(i) => &i.key,
            Input::String(i) => &i.key,
            Input::Color(i) => &i.key,
            Input::Enum(i) => &i.key,
            Input::FontFamily(i) => &i.key,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            Input::Unit(_) => "unit",
            Input::UnitArray(_) => "unitArray",
            Input::Time(_) => "time",
            Input::Number(_) => "number",
            Input::String(_) => "string",
            Input::Color(_) => "color",
            Input::Enum(_) => "enum",
            Input::FontFamily(_) => "fontFamily",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UnitInput {
    pub key: String,
    pub default: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct UnitArrayInput {
    pub key: String,
    pub default: Vec<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TimeInput {
    pub key: String,
    pub default: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NumberInput {
    pub key: String,
    pub default: f64,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct StringInput {
    pub key: String,
    pub default: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ColorInput {
    pub key: String,
    pub default: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EnumInput {
    pub key: String,
    pub values: Vec<String>,
    pub default: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FontFamilyInput {
    pub key: String,
    pub default: String,
}

// --- colors (§3) -------------------------------------------------------------

/// One entry in the document's private color table.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ColorEntry {
    pub key: String,
    pub value: ColorExpr,
    /// Binding to a declared color input; when supplied it replaces the value.
    #[serde(default, rename = "override")]
    pub override_input: Option<InputRef>,
}

/// A `{ "input": "key" }` binding object (color override / fn input arg).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputRef {
    pub input: String,
}

/// A color-table entry's value: literal, input binding, or a derivation fn.
#[derive(Debug, Clone)]
pub enum ColorExpr {
    Literal(String),
    Input(String),
    Alpha {
        of: ColorRef,
        amount: Bindable<f64>,
    },
    Contrast {
        of: ColorRef,
        candidates: Option<Vec<ColorRef>>,
    },
    Mix {
        a: ColorRef,
        b: ColorRef,
        t: Bindable<f64>,
    },
}

impl<'de> Deserialize<'de> for ColorExpr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct AlphaArgs {
            of: ColorRef,
            amount: Bindable<f64>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ContrastArgs {
            of: ColorRef,
            #[serde(default)]
            candidates: Option<Vec<ColorRef>>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct MixArgs {
            a: ColorRef,
            b: ColorRef,
            t: Bindable<f64>,
        }

        let value = serde_json::Value::deserialize(deserializer)?;
        if let serde_json::Value::String(s) = &value {
            return Ok(ColorExpr::Literal(s.clone()));
        }
        let map = value.as_object().ok_or_else(|| {
            D::Error::custom("expected a color literal, { input }, or a { fn } object")
        })?;

        if let Some(function) = map.get("fn") {
            let name = function
                .as_str()
                .ok_or_else(|| D::Error::custom("color function `fn` must be a string"))?;
            // Strip the tag so the argument structs stay strict (deny_unknown_fields).
            let mut args = map.clone();
            args.remove("fn");
            let args = serde_json::Value::Object(args);
            return match name {
                "alpha" => {
                    let a: AlphaArgs = serde_json::from_value(args).map_err(D::Error::custom)?;
                    Ok(ColorExpr::Alpha {
                        of: a.of,
                        amount: a.amount,
                    })
                }
                "contrast" => {
                    let a: ContrastArgs = serde_json::from_value(args).map_err(D::Error::custom)?;
                    Ok(ColorExpr::Contrast {
                        of: a.of,
                        candidates: a.candidates,
                    })
                }
                "mix" => {
                    let a: MixArgs = serde_json::from_value(args).map_err(D::Error::custom)?;
                    Ok(ColorExpr::Mix {
                        a: a.a,
                        b: a.b,
                        t: a.t,
                    })
                }
                other => Err(D::Error::custom(format!(
                    "unknown color function \"{other}\""
                ))),
            };
        }
        if map.contains_key("input") {
            let r: InputRef = serde_json::from_value(value).map_err(D::Error::custom)?;
            return Ok(ColorExpr::Input(r.input));
        }
        Err(D::Error::custom(
            "expected a color literal, { input }, or a { fn } object",
        ))
    }
}

/// A color-function argument: literal `#…`, input binding, or the string key of
/// an EARLIER table entry.
#[derive(Debug, Clone)]
pub enum ColorRef {
    Literal(String),
    Input(String),
    Entry(String),
}

impl<'de> Deserialize<'de> for ColorRef {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Str(String),
            Input(InputRef),
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Str(s) if s.starts_with('#') => ColorRef::Literal(s),
            Raw::Str(s) => ColorRef::Entry(s),
            Raw::Input(r) => ColorRef::Input(r.input),
        })
    }
}

/// A color-typed leaf: literal hex, input binding, or a `{ "color": key }`
/// reference into the table (§3).
#[derive(Debug, Clone)]
pub enum ColorValue {
    Literal(String),
    Input(String),
    Color(String),
}

impl<'de> Deserialize<'de> for ColorValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Str(String),
            Obj(Obj),
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        enum Obj {
            #[serde(rename = "input")]
            Input(String),
            #[serde(rename = "color")]
            Color(String),
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Str(s) => ColorValue::Literal(s),
            Raw::Obj(Obj::Input(k)) => ColorValue::Input(k),
            Raw::Obj(Obj::Color(k)) => ColorValue::Color(k),
        })
    }
}

// --- assets (§4) -------------------------------------------------------------

/// An embedded raster asset. `data` is base64 of the encoded file; decoding,
/// budget enforcement, and container-signature checks happen at load (assets.rs).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Asset {
    pub key: String,
    pub kind: AssetKind,
    pub mime: ImageMime,
    pub data: String,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetKind {
    Image,
}

impl AssetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AssetKind::Image => "image",
        }
    }
}

/// The declared container format. Animated forms (gif, apng, webp) are
/// first-class; png/jpeg are always single-frame.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub enum ImageMime {
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/jpeg")]
    Jpeg,
    #[serde(rename = "image/webp")]
    Webp,
    #[serde(rename = "image/gif")]
    Gif,
    #[serde(rename = "image/apng")]
    Apng,
}

impl ImageMime {
    pub fn as_str(self) -> &'static str {
        match self {
            ImageMime::Png => "image/png",
            ImageMime::Jpeg => "image/jpeg",
            ImageMime::Webp => "image/webp",
            ImageMime::Gif => "image/gif",
            ImageMime::Apng => "image/apng",
        }
    }
}

// --- bindings ----------------------------------------------------------------

/// A leaf value: literal, or `{ "input": "key" }` binding of matching type.
#[derive(Debug, Clone)]
pub enum Bindable<T> {
    Literal(T),
    Input(String),
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Bindable<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Hand-rolled (not untagged) so binding objects keep strict fields and
        // error messages stay concrete.
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw<T> {
            Binding(BindingRef),
            Literal(T),
        }
        match Raw::<T>::deserialize(deserializer)? {
            Raw::Binding(b) => Ok(Bindable::Input(b.input)),
            Raw::Literal(v) => Ok(Bindable::Literal(v)),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingRef {
    input: String,
}

// --- nodes -------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Node {
    Group(GroupNode),
    Shape(ShapeNode),
    Image(ImageNode),
    Text(Box<TextNode>),
}

impl Node {
    pub fn key(&self) -> &str {
        match self {
            Node::Group(n) => &n.key,
            Node::Shape(n) => &n.key,
            Node::Image(n) => &n.key,
            Node::Text(n) => &n.key,
        }
    }

    pub fn role(&self) -> Option<&str> {
        match self {
            Node::Group(n) => n.role.as_deref(),
            Node::Shape(n) => n.role.as_deref(),
            Node::Image(n) => n.role.as_deref(),
            Node::Text(n) => n.role.as_deref(),
        }
    }
}

/// An `image` node: an assets-table entry drawn into `frame` per `fit`, with an
/// optional rounded clip. Transform/opacity follow the standard animator
/// vocabulary; animated assets sample by the §4 rule (no per-node fields).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ImageNode {
    pub key: String,
    #[serde(default)]
    pub role: Option<String>,
    pub asset: String,
    pub frame: Frame,
    #[serde(default)]
    pub fit: Option<Fit>,
    #[serde(default)]
    pub corner_radius: Option<Bindable<f64>>,
    #[serde(default)]
    pub opacity: Option<Bindable<f64>>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Fit {
    Cover,
    Contain,
    Fill,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GroupNode {
    pub key: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub transform: Option<Transform>,
    #[serde(default)]
    pub opacity: Option<Bindable<f64>>,
    #[serde(default)]
    pub children: Vec<Node>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Transform {
    #[serde(default)]
    pub translate_x: Option<Bindable<f64>>,
    #[serde(default)]
    pub translate_y: Option<Bindable<f64>>,
    #[serde(default)]
    pub scale: Option<Bindable<f64>>,
    #[serde(default)]
    pub rotate: Option<Bindable<f64>>,
    #[serde(default)]
    pub anchor_x: Option<Bindable<f64>>,
    #[serde(default)]
    pub anchor_y: Option<Bindable<f64>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ShapeNode {
    pub key: String,
    #[serde(default)]
    pub role: Option<String>,
    pub geometry: Geometry,
    #[serde(default)]
    pub fill: Option<Paint>,
    #[serde(default)]
    pub stroke: Option<ShapeStroke>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Geometry {
    Rect(RectGeometry),
    RoundedRect(RoundedRectGeometry),
    Ellipse(EllipseGeometry),
    Path(PathGeometry),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RectGeometry {
    pub x: Bindable<f64>,
    pub y: Bindable<f64>,
    pub width: Bindable<f64>,
    pub height: Bindable<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RoundedRectGeometry {
    pub x: Bindable<f64>,
    pub y: Bindable<f64>,
    pub width: Bindable<f64>,
    pub height: Bindable<f64>,
    pub radius: Bindable<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct EllipseGeometry {
    pub cx: Bindable<f64>,
    pub cy: Bindable<f64>,
    pub rx: Bindable<f64>,
    pub ry: Bindable<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PathGeometry {
    pub d: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Paint {
    Solid(SolidPaint),
    LinearGradient(LinearGradientPaint),
    RadialGradient(RadialGradientPaint),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct SolidPaint {
    pub color: ColorValue,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct LinearGradientPaint {
    pub x1: Bindable<f64>,
    pub y1: Bindable<f64>,
    pub x2: Bindable<f64>,
    pub y2: Bindable<f64>,
    pub stops: Vec<GradientStop>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RadialGradientPaint {
    pub cx: Bindable<f64>,
    pub cy: Bindable<f64>,
    pub r: Bindable<f64>,
    pub stops: Vec<GradientStop>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct GradientStop {
    pub at: f64,
    pub color: ColorValue,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ShapeStroke {
    pub color: ColorValue,
    pub width: Bindable<f64>,
    #[serde(default)]
    pub cap: Option<LineCap>,
    #[serde(default)]
    pub join: Option<LineJoin>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LineCap {
    Butt,
    Round,
    Square,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum LineJoin {
    Miter,
    Round,
    Bevel,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TextNode {
    pub key: String,
    #[serde(default)]
    pub role: Option<String>,
    pub content: Bindable<String>,
    pub frame: Frame,
    pub style: TextStyle,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Frame {
    pub x: Bindable<f64>,
    pub y: Bindable<f64>,
    pub width: Bindable<f64>,
    pub height: Bindable<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TextStyle {
    pub font_family: Bindable<String>,
    #[serde(default)]
    pub weight: Option<Bindable<f64>>,
    #[serde(default)]
    pub slant: Option<Slant>,
    pub size: Bindable<f64>,
    #[serde(default)]
    pub letter_spacing: Option<Bindable<f64>>,
    #[serde(default)]
    pub line_height: Option<Bindable<f64>>,
    #[serde(default)]
    pub align: Option<Align>,
    #[serde(default)]
    pub valign: Option<VAlign>,
    pub fill: ColorValue,
    #[serde(default)]
    pub active_fill: Option<ColorValue>,
    #[serde(default)]
    pub stroke: Option<TextStroke>,
    #[serde(default)]
    pub shadow: Option<TextShadow>,
    #[serde(default)]
    pub pill: Option<TextPill>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Slant {
    Upright,
    Italic,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Align {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum VAlign {
    Top,
    Center,
    Bottom,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TextStroke {
    pub color: ColorValue,
    pub width: Bindable<f64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TextShadow {
    pub color: ColorValue,
    #[serde(default)]
    pub offset_x: Option<Bindable<f64>>,
    #[serde(default)]
    pub offset_y: Option<Bindable<f64>>,
    #[serde(default)]
    pub blur: Option<Bindable<f64>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TextPill {
    pub color: ColorValue,
    #[serde(default)]
    pub radius: Option<Bindable<f64>>,
    #[serde(default)]
    pub padding_x: Option<Bindable<f64>>,
    #[serde(default)]
    pub padding_y: Option<Bindable<f64>>,
}

// --- animators ---------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Animator {
    pub target: String,
    pub property: String,
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(default)]
    pub period: Option<f64>,
    #[serde(default)]
    pub weight: Option<Weight>,
    #[serde(default)]
    pub from: Option<Bindable<ValueLit>>,
    #[serde(default)]
    pub to: Option<Bindable<ValueLit>>,
    #[serde(default)]
    pub ease: Option<Ease>,
    #[serde(default)]
    pub keyframes: Option<Vec<Keyframe>>,
    #[serde(default)]
    pub composite: Option<Composite>,
    #[serde(default)]
    pub amount: Option<Bindable<f64>>,
    #[serde(default)]
    pub color_space: Option<ColorSpace>,
    #[serde(default)]
    pub hue: Option<Hue>,
}

/// A number, a literal color, or a `{ "color": key }` table reference; typed
/// against the property at validation.
#[derive(Debug, Clone)]
pub enum ValueLit {
    Number(f64),
    Color(String),
    ColorRef(String),
}

impl<'de> Deserialize<'de> for ValueLit {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Number(f64),
            Color(String),
            Ref(ColorRefObj),
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ColorRefObj {
            color: String,
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Number(n) => ValueLit::Number(n),
            Raw::Color(s) => ValueLit::Color(s),
            Raw::Ref(r) => ValueLit::ColorRef(r.color),
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Keyframe {
    pub at: f64,
    pub value: Bindable<ValueLit>,
    #[serde(default)]
    pub ease: Option<Ease>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub enum Weight {
    #[serde(rename = "input")]
    Input(String),
    #[serde(rename = "stagger")]
    Stagger(Stagger),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Stagger {
    pub driver: String,
    #[serde(default)]
    pub period: Option<f64>,
    #[serde(default)]
    pub total: Option<f64>,
    #[serde(default)]
    pub each: Option<f64>,
    pub from: StaggerFrom,
    #[serde(default)]
    pub ease: Option<Ease>,
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub indices: Option<Vec<u32>>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StaggerFrom {
    Start,
    Center,
    Edges,
    End,
    Random,
    Index,
}

#[derive(Debug, Clone)]
pub enum Ease {
    Named(NamedEase),
    Bezier([f64; 4]),
    Points(Vec<[f64; 2]>),
    Spring { bounce: f64, duration: f64 },
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum NamedEase {
    Linear,
    Hold,
    EaseIn,
    EaseOut,
    EaseInOut,
}

impl<'de> Deserialize<'de> for Ease {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        enum Structured {
            Bezier([f64; 4]),
            Points(Vec<[f64; 2]>),
            Spring(SpringSpec),
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        struct SpringSpec {
            bounce: f64,
            duration: f64,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Named(NamedEase),
            Structured(Structured),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Named(named) => Ok(Ease::Named(named)),
            Raw::Structured(Structured::Bezier(b)) => Ok(Ease::Bezier(b)),
            Raw::Structured(Structured::Points(p)) => Ok(Ease::Points(p)),
            Raw::Structured(Structured::Spring(s)) => Ok(Ease::Spring {
                bounce: s.bounce,
                duration: s.duration,
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Composite {
    Replace,
    Add,
    Multiply,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ColorSpace {
    Oklab,
    Srgb,
    Oklch,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Hue {
    Shorter,
    Longer,
    Increasing,
    Decreasing,
}

// --- duplicate-key rejection ---------------------------------------------------

/// serde_json is silently last-wins on duplicate object keys; the schema says
/// they are hard errors. This transcoding pass visits every object and errors
/// on the first repeat, tracking the path for the message.
fn reject_duplicate_keys(json: &str) -> Result<(), SchemaError> {
    struct Check;

    impl<'de> Visitor<'de> for Check {
        type Value = ();

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("any JSON value")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
            let mut seen: HashSet<String> = HashSet::new();
            while let Some(key) = map.next_key::<String>()? {
                if !seen.insert(key.clone()) {
                    return Err(de::Error::custom(format!("duplicate key \"{key}\"")));
                }
                map.next_value_seed(CheckSeed)?;
            }
            Ok(())
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<(), A::Error> {
            while seq.next_element_seed(CheckSeed)?.is_some() {}
            Ok(())
        }

        fn visit_bool<E>(self, _: bool) -> Result<(), E> {
            Ok(())
        }
        fn visit_i64<E>(self, _: i64) -> Result<(), E> {
            Ok(())
        }
        fn visit_u64<E>(self, _: u64) -> Result<(), E> {
            Ok(())
        }
        fn visit_f64<E>(self, _: f64) -> Result<(), E> {
            Ok(())
        }
        fn visit_str<E>(self, _: &str) -> Result<(), E> {
            Ok(())
        }
        fn visit_unit<E>(self) -> Result<(), E> {
            Ok(())
        }
    }

    struct CheckSeed;

    impl<'de> de::DeserializeSeed<'de> for CheckSeed {
        type Value = ();

        fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
            deserializer.deserialize_any(Check)
        }
    }

    let deserializer = &mut serde_json::Deserializer::from_str(json);
    serde_path_to_error::deserialize::<_, DupDoc>(deserializer)
        .map(|_| ())
        .map_err(|e| {
            let path = e.path().to_string();
            let path = if path == "." { String::new() } else { path };
            SchemaError::new(path, e.inner().to_string())
        })?;
    return Ok(());

    // Wrapper so serde_path_to_error tracks the path during the check pass.
    struct DupDoc;

    impl<'de> Deserialize<'de> for DupDoc {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            deserializer.deserialize_any(Check)?;
            Ok(DupDoc)
        }
    }
}
