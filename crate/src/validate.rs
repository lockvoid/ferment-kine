//! Legality checks for parsed documents (SCHEMA.md §6) plus spring baking.
//! Everything here is load-time: cross-references, domains, vocabulary. Values
//! are eval.rs's business; shapes are schema.rs's.

use std::collections::{HashMap, HashSet};

use color::{AlphaColor, Srgb};

use crate::assets::{decode_all, DecodedAssets};
use crate::schema::*;

/// A validated document: springs baked to points curves, roles collected, and
/// every asset decoded to premultiplied frames (owned for the handle's life).
pub struct Compiled {
    pub doc: Document,
    pub roles: Vec<String>,
    pub assets: DecodedAssets,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Group,
    Shape,
    Image,
    Text,
}

/// The sub-unit level a per-unit animator targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitLevel {
    Glyphs,
    Words,
    Lines,
}

/// What kind of value a property carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropType {
    Number,
    Color,
}

pub fn validate(mut doc: Document) -> Result<Compiled, SchemaError> {
    if doc.version != 1 {
        return Err(SchemaError::new(
            "version",
            format!("must be 1, got {}", doc.version),
        ));
    }
    let size_ok = |v: f64| v.is_finite() && v > 0.0;
    if !size_ok(doc.size.width) || !size_ok(doc.size.height) {
        return Err(SchemaError::new(
            "size",
            "width and height must be positive",
        ));
    }

    let inputs = check_inputs(&doc.inputs)?;
    let colors = check_colors(&doc.colors, &inputs)?;
    // Validate + decode assets up front: image nodes resolve against these keys,
    // and decoding is part of document load (§7 step 2).
    let assets = decode_all(&doc.assets)?;
    let asset_keys: HashSet<String> = assets.keys().cloned().collect();

    let mut nodes: HashMap<String, NodeKind> = HashMap::new();
    let mut node_meta: HashMap<String, NodeInfo> = HashMap::new();
    let mut roles: Vec<String> = Vec::new();
    let mut seen_roles: HashSet<String> = HashSet::new();
    check_node(
        &doc.root,
        "root",
        &inputs,
        &colors,
        &asset_keys,
        &mut nodes,
        &mut node_meta,
        &mut roles,
        &mut seen_roles,
    )?;

    for (index, animator) in doc.animators.iter().enumerate() {
        let path = format!("animators[{index}]");
        check_animator(animator, &path, &inputs, &colors, &nodes, &node_meta)?;
    }

    bake_document_springs(&mut doc)?;

    Ok(Compiled { doc, roles, assets })
}

/// Facts about a node that animator checks need beyond its kind.
struct NodeInfo {
    has_solid_fill: bool,
    has_stroke: bool,
    has_shadow: bool,
    has_pill: bool,
    has_active_fill: bool,
}

// --- inputs -------------------------------------------------------------------

/// The host's envelopes (§2) and the value each rests at: the host drives them
/// only while an entrance or an exit plays.
pub const HOST_ENVELOPES: [(&str, f64); 2] = [("inProgress", 1.0), ("outProgress", 0.0)];

fn envelope_rule(key: &str, settled: f64) -> String {
    let plays = if key == "inProgress" { "an entrance" } else { "an exit" };
    format!(
        "{key} defaults to {settled} — a host envelope rests settled: the host drives it only while \
         {plays} plays, and motion the document drives itself rides \"time\""
    )
}

fn check_inputs(inputs: &[Input]) -> Result<HashMap<String, &'static str>, SchemaError> {
    let mut map = HashMap::new();
    for (index, input) in inputs.iter().enumerate() {
        let path = format!("inputs[{index}]");
        let key = input.key();
        if !valid_input_key(key) {
            return Err(SchemaError::new(
                format!("{path}.key"),
                format!("\"{key}\" must match [a-z][a-zA-Z0-9]*"),
            ));
        }
        if map.insert(key.to_string(), input.type_name()).is_some() {
            return Err(SchemaError::new(
                format!("{path}.key"),
                format!("duplicate input key \"{key}\""),
            ));
        }
        match input {
            Input::Unit(i) => {
                if !(0.0..=1.0).contains(&i.default) {
                    return Err(SchemaError::new(
                        format!("{path}.default"),
                        "unit default outside [0, 1]",
                    ));
                }
            }
            Input::UnitArray(i) => {
                for (j, v) in i.default.iter().enumerate() {
                    if !(0.0..=1.0).contains(v) {
                        return Err(SchemaError::new(
                            format!("{path}.default[{j}]"),
                            "unit default outside [0, 1]",
                        ));
                    }
                }
            }
            Input::Time(i) => {
                if i.default < 0.0 {
                    return Err(SchemaError::new(
                        format!("{path}.default"),
                        "time default must be >= 0",
                    ));
                }
            }
            Input::Number(i) => {
                if let (Some(min), Some(max)) = (i.min, i.max) {
                    if min > max {
                        return Err(SchemaError::new(
                            format!("{path}.min"),
                            "min greater than max",
                        ));
                    }
                }
            }
            Input::Color(i) => {
                parse_color(&i.default).ok_or_else(|| {
                    SchemaError::new(format!("{path}.default"), malformed_color(&i.default))
                })?;
            }
            Input::Enum(i) => {
                if i.values.is_empty() {
                    return Err(SchemaError::new(
                        format!("{path}.values"),
                        "enum values must be non-empty",
                    ));
                }
                if !i.values.contains(&i.default) {
                    return Err(SchemaError::new(
                        format!("{path}.default"),
                        format!("\"{}\" is not one of the declared values", i.default),
                    ));
                }
            }
            Input::String(_) | Input::FontFamily(_) => {}
        }
        if let Some(&(_, settled)) = HOST_ENVELOPES.iter().find(|(envelope, _)| *envelope == key) {
            match input {
                Input::Unit(i) if i.default == settled => {}
                Input::Unit(_) => {
                    return Err(SchemaError::new(
                        format!("{path}.default"),
                        envelope_rule(key, settled),
                    ));
                }
                _ => {
                    return Err(SchemaError::new(
                        format!("{path}.type"),
                        format!("{key} is a host envelope — declare it as unit"),
                    ));
                }
            }
        }
    }
    Ok(map)
}

fn valid_input_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

fn valid_node_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

// --- nodes --------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn check_node(
    node: &Node,
    path: &str,
    inputs: &HashMap<String, &'static str>,
    colors: &HashSet<String>,
    assets: &HashSet<String>,
    nodes: &mut HashMap<String, NodeKind>,
    node_meta: &mut HashMap<String, NodeInfo>,
    roles: &mut Vec<String>,
    seen_roles: &mut HashSet<String>,
) -> Result<(), SchemaError> {
    let key = node.key();
    if !valid_node_key(key) {
        return Err(SchemaError::new(
            format!("{path}.key"),
            format!("\"{key}\" must match [a-zA-Z][a-zA-Z0-9_-]*"),
        ));
    }
    let kind = match node {
        Node::Group(_) => NodeKind::Group,
        Node::Shape(_) => NodeKind::Shape,
        Node::Image(_) => NodeKind::Image,
        Node::Text(_) => NodeKind::Text,
    };
    if nodes.insert(key.to_string(), kind).is_some() {
        return Err(SchemaError::new(
            format!("{path}.key"),
            format!("duplicate node key \"{key}\""),
        ));
    }
    if let Some(role) = node.role() {
        if !seen_roles.insert(role.to_string()) {
            return Err(SchemaError::new(
                format!("{path}.role"),
                format!("duplicate role \"{role}\""),
            ));
        }
        roles.push(role.to_string());
    }

    let mut info = NodeInfo {
        has_solid_fill: false,
        has_stroke: false,
        has_shadow: false,
        has_pill: false,
        has_active_fill: false,
    };

    match node {
        Node::Group(group) => {
            if let Some(transform) = &group.transform {
                check_transform(transform, &format!("{path}.transform"), inputs)?;
            }
            check_opt_binding_number(&group.opacity, &format!("{path}.opacity"), inputs)?;
            for (index, child) in group.children.iter().enumerate() {
                check_node(
                    child,
                    &format!("{path}.children[{index}]"),
                    inputs,
                    colors,
                    assets,
                    nodes,
                    node_meta,
                    roles,
                    seen_roles,
                )?;
            }
        }
        Node::Shape(shape) => {
            if shape.fill.is_none() && shape.stroke.is_none() {
                return Err(SchemaError::new(path, "shape requires fill or stroke"));
            }
            check_geometry(&shape.geometry, &format!("{path}.geometry"), inputs)?;
            if let Some(fill) = &shape.fill {
                info.has_solid_fill = matches!(fill, Paint::Solid(_));
                check_paint(fill, &format!("{path}.fill"), inputs, colors)?;
            }
            if let Some(stroke) = &shape.stroke {
                info.has_stroke = true;
                check_color_value(
                    &stroke.color,
                    &format!("{path}.stroke.color"),
                    inputs,
                    colors,
                )?;
                check_binding_number(&stroke.width, &format!("{path}.stroke.width"), inputs)?;
            }
        }
        Node::Image(image) => {
            if !assets.contains(&image.asset) {
                return Err(SchemaError::new(
                    format!("{path}.asset"),
                    format!("references unknown asset \"{}\"", image.asset),
                ));
            }
            check_frame(&image.frame, &format!("{path}.frame"), inputs)?;
            check_opt_binding_number(
                &image.corner_radius,
                &format!("{path}.cornerRadius"),
                inputs,
            )?;
            check_opt_binding_number(&image.opacity, &format!("{path}.opacity"), inputs)?;
        }
        Node::Text(text) => {
            check_binding_string(&text.content, &format!("{path}.content"), inputs)?;
            check_frame(&text.frame, &format!("{path}.frame"), inputs)?;
            let style = &text.style;
            let style_path = format!("{path}.style");
            check_binding_font(
                &style.font_family,
                &format!("{style_path}.fontFamily"),
                inputs,
            )?;
            check_opt_binding_number(&style.weight, &format!("{style_path}.weight"), inputs)?;
            check_binding_number(&style.size, &format!("{style_path}.size"), inputs)?;
            check_opt_binding_number(
                &style.letter_spacing,
                &format!("{style_path}.letterSpacing"),
                inputs,
            )?;
            check_opt_binding_number(
                &style.line_height,
                &format!("{style_path}.lineHeight"),
                inputs,
            )?;
            check_color_value(&style.fill, &format!("{style_path}.fill"), inputs, colors)?;
            if let Some(active) = &style.active_fill {
                info.has_active_fill = true;
                check_color_value(active, &format!("{style_path}.activeFill"), inputs, colors)?;
            }
            if let Some(stroke) = &style.stroke {
                info.has_stroke = true;
                check_color_value(
                    &stroke.color,
                    &format!("{style_path}.stroke.color"),
                    inputs,
                    colors,
                )?;
                check_binding_number(&stroke.width, &format!("{style_path}.stroke.width"), inputs)?;
            }
            if let Some(shadow) = &style.shadow {
                info.has_shadow = true;
                check_color_value(
                    &shadow.color,
                    &format!("{style_path}.shadow.color"),
                    inputs,
                    colors,
                )?;
                check_opt_binding_number(
                    &shadow.offset_x,
                    &format!("{style_path}.shadow.offsetX"),
                    inputs,
                )?;
                check_opt_binding_number(
                    &shadow.offset_y,
                    &format!("{style_path}.shadow.offsetY"),
                    inputs,
                )?;
                check_opt_binding_number(
                    &shadow.blur,
                    &format!("{style_path}.shadow.blur"),
                    inputs,
                )?;
            }
            if let Some(pill) = &style.pill {
                info.has_pill = true;
                check_color_value(
                    &pill.color,
                    &format!("{style_path}.pill.color"),
                    inputs,
                    colors,
                )?;
                check_opt_binding_number(
                    &pill.radius,
                    &format!("{style_path}.pill.radius"),
                    inputs,
                )?;
                check_opt_binding_number(
                    &pill.padding_x,
                    &format!("{style_path}.pill.paddingX"),
                    inputs,
                )?;
                check_opt_binding_number(
                    &pill.padding_y,
                    &format!("{style_path}.pill.paddingY"),
                    inputs,
                )?;
                check_opt_binding_number(
                    &pill.opacity,
                    &format!("{style_path}.pill.opacity"),
                    inputs,
                )?;
            }
            if let Some(backdrop) = &style.backdrop {
                check_color_value(
                    &backdrop.color,
                    &format!("{style_path}.backdrop.color"),
                    inputs,
                    colors,
                )?;
                check_opt_binding_number(
                    &backdrop.radius,
                    &format!("{style_path}.backdrop.radius"),
                    inputs,
                )?;
                check_opt_binding_number(
                    &backdrop.padding_x,
                    &format!("{style_path}.backdrop.paddingX"),
                    inputs,
                )?;
                check_opt_binding_number(
                    &backdrop.padding_y,
                    &format!("{style_path}.backdrop.paddingY"),
                    inputs,
                )?;
            }
        }
    }

    node_meta.insert(key.to_string(), info);
    Ok(())
}

fn check_transform(
    transform: &Transform,
    path: &str,
    inputs: &HashMap<String, &'static str>,
) -> Result<(), SchemaError> {
    check_opt_binding_number(
        &transform.translate_x,
        &format!("{path}.translateX"),
        inputs,
    )?;
    check_opt_binding_number(
        &transform.translate_y,
        &format!("{path}.translateY"),
        inputs,
    )?;
    check_opt_binding_number(&transform.scale, &format!("{path}.scale"), inputs)?;
    check_opt_binding_number(&transform.rotate, &format!("{path}.rotate"), inputs)?;
    check_anchor(&transform.anchor_x, &format!("{path}.anchorX"), inputs)?;
    check_anchor(&transform.anchor_y, &format!("{path}.anchorY"), inputs)?;
    Ok(())
}

/// An anchor is a FRACTION of the group's bounds (§5): a literal inside
/// [0, 1] or a unit input.
fn check_anchor(
    anchor: &Option<Bindable<f64>>,
    path: &str,
    inputs: &HashMap<String, &'static str>,
) -> Result<(), SchemaError> {
    match anchor {
        Some(Bindable::Literal(value)) if !(0.0..=1.0).contains(value) => Err(SchemaError::new(
            path,
            format!(
                "{value} is outside 0..1 — an anchor is a FRACTION of the group's bounds \
                 (0.5 = center, the default), never pixels"
            ),
        )),
        Some(binding) => binding_check(binding, path, inputs, &["unit"], || Ok(())),
        None => Ok(()),
    }
}

fn check_frame(
    frame: &Frame,
    path: &str,
    inputs: &HashMap<String, &'static str>,
) -> Result<(), SchemaError> {
    check_binding_number(&frame.x, &format!("{path}.x"), inputs)?;
    check_binding_number(&frame.y, &format!("{path}.y"), inputs)?;
    check_binding_number(&frame.width, &format!("{path}.width"), inputs)?;
    check_binding_number(&frame.height, &format!("{path}.height"), inputs)?;
    Ok(())
}

fn check_geometry(
    geometry: &Geometry,
    path: &str,
    inputs: &HashMap<String, &'static str>,
) -> Result<(), SchemaError> {
    match geometry {
        Geometry::Rect(g) => {
            check_binding_number(&g.x, &format!("{path}.x"), inputs)?;
            check_binding_number(&g.y, &format!("{path}.y"), inputs)?;
            check_binding_number(&g.width, &format!("{path}.width"), inputs)?;
            check_binding_number(&g.height, &format!("{path}.height"), inputs)?;
        }
        Geometry::RoundedRect(g) => {
            check_binding_number(&g.x, &format!("{path}.x"), inputs)?;
            check_binding_number(&g.y, &format!("{path}.y"), inputs)?;
            check_binding_number(&g.width, &format!("{path}.width"), inputs)?;
            check_binding_number(&g.height, &format!("{path}.height"), inputs)?;
            check_binding_number(&g.radius, &format!("{path}.radius"), inputs)?;
        }
        Geometry::Ellipse(g) => {
            check_binding_number(&g.cx, &format!("{path}.cx"), inputs)?;
            check_binding_number(&g.cy, &format!("{path}.cy"), inputs)?;
            check_binding_number(&g.rx, &format!("{path}.rx"), inputs)?;
            check_binding_number(&g.ry, &format!("{path}.ry"), inputs)?;
        }
        Geometry::Path(g) => {
            vello_cpu::kurbo::BezPath::from_svg(&g.d).map_err(|e| {
                SchemaError::new(format!("{path}.d"), format!("invalid SVG path data: {e}"))
            })?;
        }
    }
    Ok(())
}

fn check_paint(
    paint: &Paint,
    path: &str,
    inputs: &HashMap<String, &'static str>,
    colors: &HashSet<String>,
) -> Result<(), SchemaError> {
    match paint {
        Paint::Solid(p) => check_color_value(&p.color, &format!("{path}.color"), inputs, colors),
        Paint::LinearGradient(p) => {
            check_binding_number(&p.x1, &format!("{path}.x1"), inputs)?;
            check_binding_number(&p.y1, &format!("{path}.y1"), inputs)?;
            check_binding_number(&p.x2, &format!("{path}.x2"), inputs)?;
            check_binding_number(&p.y2, &format!("{path}.y2"), inputs)?;
            check_stops(&p.stops, path, inputs, colors)
        }
        Paint::RadialGradient(p) => {
            check_binding_number(&p.cx, &format!("{path}.cx"), inputs)?;
            check_binding_number(&p.cy, &format!("{path}.cy"), inputs)?;
            check_binding_number(&p.r, &format!("{path}.r"), inputs)?;
            check_stops(&p.stops, path, inputs, colors)
        }
    }
}

fn check_stops(
    stops: &[GradientStop],
    path: &str,
    inputs: &HashMap<String, &'static str>,
    colors: &HashSet<String>,
) -> Result<(), SchemaError> {
    if stops.len() < 2 {
        return Err(SchemaError::new(
            format!("{path}.stops"),
            "gradient needs at least 2 stops",
        ));
    }
    let mut previous = -1.0f64;
    for (index, stop) in stops.iter().enumerate() {
        let stop_path = format!("{path}.stops[{index}]");
        if !(0.0..=1.0).contains(&stop.at) || stop.at <= previous {
            return Err(SchemaError::new(
                format!("{stop_path}.at"),
                "stops must be monotone increasing within [0, 1]",
            ));
        }
        previous = stop.at;
        check_color_value(&stop.color, &format!("{stop_path}.color"), inputs, colors)?;
    }
    Ok(())
}

// --- binding type checks --------------------------------------------------------

fn binding_check(
    binding: &Bindable<impl Sized>,
    path: &str,
    inputs: &HashMap<String, &'static str>,
    accepted: &[&str],
    literal_ok: impl FnOnce() -> Result<(), SchemaError>,
) -> Result<(), SchemaError> {
    match binding {
        Bindable::Input(key) => match inputs.get(key.as_str()) {
            None => Err(SchemaError::new(
                path,
                format!("binding references undeclared input \"{key}\""),
            )),
            Some(t) if accepted.contains(t) => Ok(()),
            Some(t) => Err(SchemaError::new(
                path,
                format!(
                    "binding type mismatch: input \"{key}\" is {t}, expected {}",
                    accepted.join("|")
                ),
            )),
        },
        Bindable::Literal(_) => literal_ok(),
    }
}

fn check_binding_number(
    binding: &Bindable<f64>,
    path: &str,
    inputs: &HashMap<String, &'static str>,
) -> Result<(), SchemaError> {
    binding_check(
        binding,
        path,
        inputs,
        &["number", "unit", "time"],
        || Ok(()),
    )
}

fn check_opt_binding_number(
    binding: &Option<Bindable<f64>>,
    path: &str,
    inputs: &HashMap<String, &'static str>,
) -> Result<(), SchemaError> {
    match binding {
        Some(b) => check_binding_number(b, path, inputs),
        None => Ok(()),
    }
}

/// A color-typed leaf: literal, color-input binding, or `{color}` table ref.
fn check_color_value(
    value: &ColorValue,
    path: &str,
    inputs: &HashMap<String, &'static str>,
    colors: &HashSet<String>,
) -> Result<(), SchemaError> {
    match value {
        ColorValue::Literal(s) => parse_color(s)
            .map(|_| ())
            .ok_or_else(|| SchemaError::new(path, malformed_color(s))),
        ColorValue::Input(key) => match inputs.get(key.as_str()) {
            Some(&"color") => Ok(()),
            Some(t) => Err(SchemaError::new(
                path,
                format!("binding type mismatch: input \"{key}\" is {t}, expected color"),
            )),
            None => Err(SchemaError::new(
                path,
                format!("binding references undeclared input \"{key}\""),
            )),
        },
        ColorValue::Color(key) => {
            if colors.contains(key) {
                Ok(())
            } else {
                Err(SchemaError::new(
                    path,
                    format!("reference to unknown color entry \"{key}\""),
                ))
            }
        }
    }
}

// --- colors table (§3 / §7) --------------------------------------------------

/// Validate the color table and return the set of declared entry keys (for
/// `{color}` references elsewhere). Entries may only reference EARLIER entries,
/// which makes the table acyclic by construction.
fn check_colors(
    entries: &[ColorEntry],
    inputs: &HashMap<String, &'static str>,
) -> Result<HashSet<String>, SchemaError> {
    let mut earlier: HashSet<String> = HashSet::new();
    for (index, entry) in entries.iter().enumerate() {
        let path = format!("colors[{index}]");
        if !valid_input_key(&entry.key) {
            return Err(SchemaError::new(
                format!("{path}.key"),
                format!("\"{}\" must match [a-z][a-zA-Z0-9]*", entry.key),
            ));
        }
        if earlier.contains(&entry.key) {
            return Err(SchemaError::new(
                format!("{path}.key"),
                format!("duplicate color entry key \"{}\"", entry.key),
            ));
        }
        check_color_expr(&entry.value, &format!("{path}.value"), inputs, &earlier)?;
        if let Some(over) = &entry.override_input {
            match inputs.get(over.input.as_str()) {
                Some(&"color") => {}
                Some(t) => {
                    return Err(SchemaError::new(
                        format!("{path}.override"),
                        format!(
                            "override binds \"{}\" which is {t}, expected a color input",
                            over.input
                        ),
                    ));
                }
                None => {
                    return Err(SchemaError::new(
                        format!("{path}.override"),
                        format!("override references undeclared input \"{}\"", over.input),
                    ));
                }
            }
        }
        earlier.insert(entry.key.clone());
    }
    Ok(earlier)
}

fn check_color_expr(
    expr: &ColorExpr,
    path: &str,
    inputs: &HashMap<String, &'static str>,
    earlier: &HashSet<String>,
) -> Result<(), SchemaError> {
    match expr {
        ColorExpr::Literal(s) => parse_color(s)
            .map(|_| ())
            .ok_or_else(|| SchemaError::new(path, malformed_color(s))),
        ColorExpr::Input(key) => check_color_input(key, path, inputs),
        ColorExpr::Alpha { of, amount } => {
            check_color_ref(of, &format!("{path}.of"), inputs, earlier)?;
            check_unit_arg(amount, &format!("{path}.amount"), inputs)
        }
        ColorExpr::Contrast { of, candidates } => {
            check_color_ref(of, &format!("{path}.of"), inputs, earlier)?;
            if let Some(list) = candidates {
                if list.len() < 2 {
                    return Err(SchemaError::new(
                        format!("{path}.candidates"),
                        "contrast needs at least 2 candidates",
                    ));
                }
                for (i, reference) in list.iter().enumerate() {
                    check_color_ref(
                        reference,
                        &format!("{path}.candidates[{i}]"),
                        inputs,
                        earlier,
                    )?;
                }
            }
            Ok(())
        }
        ColorExpr::Mix { a, b, t } => {
            check_color_ref(a, &format!("{path}.a"), inputs, earlier)?;
            check_color_ref(b, &format!("{path}.b"), inputs, earlier)?;
            check_unit_arg(t, &format!("{path}.t"), inputs)
        }
    }
}

fn check_color_ref(
    reference: &ColorRef,
    path: &str,
    inputs: &HashMap<String, &'static str>,
    earlier: &HashSet<String>,
) -> Result<(), SchemaError> {
    match reference {
        ColorRef::Literal(s) => parse_color(s)
            .map(|_| ())
            .ok_or_else(|| SchemaError::new(path, malformed_color(s))),
        ColorRef::Input(key) => check_color_input(key, path, inputs),
        ColorRef::Entry(key) => {
            if earlier.contains(key) {
                Ok(())
            } else {
                Err(SchemaError::new(
                    path,
                    format!("reference to unknown or later color entry \"{key}\""),
                ))
            }
        }
    }
}

fn check_color_input(
    key: &str,
    path: &str,
    inputs: &HashMap<String, &'static str>,
) -> Result<(), SchemaError> {
    match inputs.get(key) {
        Some(&"color") => Ok(()),
        Some(t) => Err(SchemaError::new(
            path,
            format!("input \"{key}\" is {t}, expected color"),
        )),
        None => Err(SchemaError::new(
            path,
            format!("references undeclared input \"{key}\""),
        )),
    }
}

/// A color-fn `amount`/`t`: literal ∈ [0,1] or a unit/number input binding.
fn check_unit_arg(
    binding: &Bindable<f64>,
    path: &str,
    inputs: &HashMap<String, &'static str>,
) -> Result<(), SchemaError> {
    match binding {
        Bindable::Literal(v) => {
            if (0.0..=1.0).contains(v) {
                Ok(())
            } else {
                Err(SchemaError::new(path, "must be within [0, 1]"))
            }
        }
        Bindable::Input(key) => match inputs.get(key.as_str()) {
            Some(&"unit") | Some(&"number") => Ok(()),
            Some(t) => Err(SchemaError::new(
                path,
                format!("input \"{key}\" is {t}, expected unit or number"),
            )),
            None => Err(SchemaError::new(
                path,
                format!("references undeclared input \"{key}\""),
            )),
        },
    }
}

fn check_binding_string(
    binding: &Bindable<String>,
    path: &str,
    inputs: &HashMap<String, &'static str>,
) -> Result<(), SchemaError> {
    binding_check(binding, path, inputs, &["string"], || Ok(()))
}

fn check_binding_font(
    binding: &Bindable<String>,
    path: &str,
    inputs: &HashMap<String, &'static str>,
) -> Result<(), SchemaError> {
    binding_check(binding, path, inputs, &["fontFamily"], || Ok(()))
}

// --- animators -------------------------------------------------------------------

const NODE_TRANSFORM_PROPS: &[&str] = &[
    "opacity",
    "translateX",
    "translateY",
    "scale",
    "scaleX",
    "scaleY",
    "rotate",
];
const UNIT_PROPS: &[&str] = &[
    "opacity",
    "translateX",
    "translateY",
    "scale",
    "rotate",
    "color",
    "pillColor",
    "pillOpacity",
];

pub fn parse_target(target: &str) -> (&str, Option<UnitLevel>) {
    if let Some(key) = target.strip_suffix(".glyphs") {
        (key, Some(UnitLevel::Glyphs))
    } else if let Some(key) = target.strip_suffix(".words") {
        (key, Some(UnitLevel::Words))
    } else if let Some(key) = target.strip_suffix(".lines") {
        (key, Some(UnitLevel::Lines))
    } else {
        (target, None)
    }
}

/// The value type a property carries (color vs number).
pub fn prop_type(property: &str) -> PropType {
    match property {
        "color" | "strokeColor" | "shadowColor" | "pillColor" => PropType::Color,
        _ => PropType::Number,
    }
}

fn check_animator(
    animator: &Animator,
    path: &str,
    inputs: &HashMap<String, &'static str>,
    colors: &HashSet<String>,
    nodes: &HashMap<String, NodeKind>,
    node_meta: &HashMap<String, NodeInfo>,
) -> Result<(), SchemaError> {
    let (node_key, level) = parse_target(&animator.target);
    let kind = *nodes.get(node_key).ok_or_else(|| {
        SchemaError::new(
            format!("{path}.target"),
            format!("unknown node key \"{node_key}\""),
        )
    })?;
    let info = &node_meta[node_key];

    let property = animator.property.as_str();
    match level {
        Some(_) if kind != NodeKind::Text => {
            return Err(SchemaError::new(
                format!("{path}.target"),
                "sub-unit targets (.glyphs/.words/.lines) require a text node",
            ));
        }
        Some(level) => {
            if !UNIT_PROPS.contains(&property) {
                return Err(SchemaError::new(
                    format!("{path}.property"),
                    format!("\"{property}\" is not animatable on text sub-units"),
                ));
            }
            if (property == "pillColor" || property == "pillOpacity") && level != UnitLevel::Words {
                return Err(SchemaError::new(
                    format!("{path}.target"),
                    "pill properties animate per word (.words)",
                ));
            }
            if (property == "pillColor" || property == "pillOpacity") && !info.has_pill {
                return Err(SchemaError::new(
                    format!("{path}.property"),
                    "pill properties require a pill declared in the text style",
                ));
            }
            if animator.weight.is_none() {
                return Err(SchemaError::new(path, "per-unit animators require weight"));
            }
            if animator.driver.is_some() {
                return Err(SchemaError::new(
                    format!("{path}.driver"),
                    "per-unit animators use weight, not driver",
                ));
            }
        }
        None => {
            let allowed = match (kind, property) {
                (_, p) if NODE_TRANSFORM_PROPS.contains(&p) => true,
                (NodeKind::Shape, "color") => info.has_solid_fill,
                (NodeKind::Shape, "strokeColor") => info.has_stroke,
                (NodeKind::Text, "color") => true,
                (NodeKind::Text, "strokeColor") => info.has_stroke,
                (NodeKind::Text, "shadowColor") => info.has_shadow,
                _ => false,
            };
            if !allowed {
                return Err(SchemaError::new(
                    format!("{path}.property"),
                    format!("\"{property}\" is not animatable on this target"),
                ));
            }
            if animator.weight.is_some() {
                return Err(SchemaError::new(
                    format!("{path}.weight"),
                    "weight is for per-unit targets; node-level animators use driver",
                ));
            }
            if animator.driver.is_none() {
                return Err(SchemaError::new(
                    path,
                    "node-level animators require driver",
                ));
            }
        }
    }

    if let Some(driver) = &animator.driver {
        check_driver(
            driver,
            animator.period,
            &format!("{path}.driver"),
            path,
            inputs,
        )?;
    }
    if let Some(weight) = &animator.weight {
        match weight {
            Weight::Input(key) => match inputs.get(key.as_str()) {
                Some(&"unitArray") => {}
                Some(t) => {
                    return Err(SchemaError::new(
                        format!("{path}.weight.input"),
                        format!("input \"{key}\" is {t}, expected unitArray"),
                    ));
                }
                None => {
                    return Err(SchemaError::new(
                        format!("{path}.weight.input"),
                        format!("references undeclared input \"{key}\""),
                    ));
                }
            },
            Weight::Stagger(stagger) => {
                let stagger_path = format!("{path}.weight.stagger");
                check_driver(
                    &stagger.driver,
                    stagger.period,
                    &format!("{stagger_path}.driver"),
                    &stagger_path,
                    inputs,
                )?;
                match (stagger.total, stagger.each) {
                    (Some(_), Some(_)) | (None, None) => {
                        return Err(SchemaError::new(
                            stagger_path,
                            "exactly one of total/each is required",
                        ));
                    }
                    (Some(total), None) if !(0.0..1.0).contains(&total) => {
                        return Err(SchemaError::new(
                            format!("{stagger_path}.total"),
                            "must be within [0, 1)",
                        ));
                    }
                    (None, Some(each)) if each < 0.0 => {
                        return Err(SchemaError::new(
                            format!("{stagger_path}.each"),
                            "must be >= 0",
                        ));
                    }
                    _ => {}
                }
                if stagger.from == StaggerFrom::Random && stagger.seed.is_none() {
                    return Err(SchemaError::new(
                        format!("{stagger_path}.seed"),
                        "from: random requires seed",
                    ));
                }
                if stagger.from == StaggerFrom::Index && stagger.indices.is_none() {
                    return Err(SchemaError::new(
                        format!("{stagger_path}.indices"),
                        "from: index requires indices",
                    ));
                }
            }
        }
    }

    check_value_mapping(animator, path, inputs, colors, level, info)?;

    if let Some(amount) = &animator.amount {
        binding_check(amount, &format!("{path}.amount"), inputs, &["unit"], || {
            Ok(())
        })?;
        if let Bindable::Literal(v) = amount {
            if !(0.0..=1.0).contains(v) {
                return Err(SchemaError::new(
                    format!("{path}.amount"),
                    "must be within [0, 1]",
                ));
            }
        }
    }

    if prop_type(property) == PropType::Color {
        if matches!(
            animator.composite,
            Some(Composite::Add) | Some(Composite::Multiply)
        ) {
            return Err(SchemaError::new(
                format!("{path}.composite"),
                "add/multiply composites are not supported for color properties in v1",
            ));
        }
    } else if animator.color_space.is_some() || animator.hue.is_some() {
        return Err(SchemaError::new(
            format!("{path}.colorSpace"),
            "colorSpace/hue apply to color properties only",
        ));
    }

    Ok(())
}

fn check_driver(
    driver: &str,
    period: Option<f64>,
    driver_path: &str,
    owner_path: &str,
    inputs: &HashMap<String, &'static str>,
) -> Result<(), SchemaError> {
    match inputs.get(driver) {
        Some(&"unit") => {
            if period.is_some() {
                return Err(SchemaError::new(
                    format!("{owner_path}.period"),
                    "period applies to time drivers only",
                ));
            }
            Ok(())
        }
        Some(&"time") => match period {
            Some(p) if p > 0.0 => Ok(()),
            Some(_) => Err(SchemaError::new(
                format!("{owner_path}.period"),
                "must be > 0",
            )),
            None => Err(SchemaError::new(
                format!("{owner_path}.period"),
                "time driver requires period",
            )),
        },
        Some(t) => Err(SchemaError::new(
            driver_path.to_string(),
            format!("input \"{driver}\" is {t}, expected unit or time"),
        )),
        None => Err(SchemaError::new(
            driver_path.to_string(),
            format!("references undeclared input \"{driver}\""),
        )),
    }
}

fn check_value_mapping(
    animator: &Animator,
    path: &str,
    inputs: &HashMap<String, &'static str>,
    colors: &HashSet<String>,
    level: Option<UnitLevel>,
    info: &NodeInfo,
) -> Result<(), SchemaError> {
    let property_type = prop_type(&animator.property);

    let check_value = |value: &Bindable<ValueLit>, value_path: &str| -> Result<(), SchemaError> {
        match value {
            Bindable::Input(key) => {
                let accepted: &[&str] = match property_type {
                    PropType::Number => &["number", "unit", "time"],
                    PropType::Color => &["color"],
                };
                match inputs.get(key.as_str()) {
                    Some(t) if accepted.contains(t) => Ok(()),
                    Some(t) => Err(SchemaError::new(
                        value_path,
                        format!("binding type mismatch: input \"{key}\" is {t}"),
                    )),
                    None => Err(SchemaError::new(
                        value_path,
                        format!("binding references undeclared input \"{key}\""),
                    )),
                }
            }
            Bindable::Literal(ValueLit::ColorRef(key)) if property_type == PropType::Color => {
                if colors.contains(key) {
                    Ok(())
                } else {
                    Err(SchemaError::new(
                        value_path,
                        format!("reference to unknown color entry \"{key}\""),
                    ))
                }
            }
            Bindable::Literal(ValueLit::ColorRef(_)) => Err(SchemaError::new(
                value_path,
                "expected a number value for a number property",
            )),
            Bindable::Literal(ValueLit::Number(_)) if property_type == PropType::Number => Ok(()),
            Bindable::Literal(ValueLit::Color(s)) if property_type == PropType::Color => {
                parse_color(s)
                    .map(|_| ())
                    .ok_or_else(|| SchemaError::new(value_path, malformed_color(s)))
            }
            Bindable::Literal(ValueLit::Number(_)) => Err(SchemaError::new(
                value_path,
                "expected a color value for a color property",
            )),
            Bindable::Literal(ValueLit::Color(_)) => Err(SchemaError::new(
                value_path,
                "expected a number value for a number property",
            )),
        }
    };

    match &animator.keyframes {
        Some(keyframes) => {
            if animator.from.is_some() || animator.to.is_some() || animator.ease.is_some() {
                return Err(SchemaError::new(
                    format!("{path}.keyframes"),
                    "keyframes and from/to/ease are mutually exclusive",
                ));
            }
            if keyframes.len() < 2 {
                return Err(SchemaError::new(
                    format!("{path}.keyframes"),
                    "needs at least 2 keyframes",
                ));
            }
            let first = &keyframes[0];
            let last = &keyframes[keyframes.len() - 1];
            if first.ease.is_some() {
                return Err(SchemaError::new(
                    format!("{path}.keyframes[0].ease"),
                    "ease describes the arriving segment; the first keyframe has none",
                ));
            }
            if first.at != 0.0 || last.at != 1.0 {
                return Err(SchemaError::new(
                    format!("{path}.keyframes"),
                    "explicit at: 0 first and at: 1 last keyframes are required",
                ));
            }
            let mut previous = -1.0f64;
            for (index, keyframe) in keyframes.iter().enumerate() {
                if keyframe.at <= previous {
                    return Err(SchemaError::new(
                        format!("{path}.keyframes[{index}].at"),
                        "keyframe positions must be strictly increasing",
                    ));
                }
                previous = keyframe.at;
                check_value(&keyframe.value, &format!("{path}.keyframes[{index}].value"))?;
            }
        }
        None => {
            // Sub-unit color animators may omit from/to: fill → activeFill.
            let implicit_karaoke = property_type == PropType::Color && level.is_some();
            match (&animator.from, &animator.to) {
                (Some(from), Some(to)) => {
                    check_value(from, &format!("{path}.from"))?;
                    check_value(to, &format!("{path}.to"))?;
                }
                (None, None) if implicit_karaoke => {
                    if !info.has_active_fill && animator.property == "color" {
                        return Err(SchemaError::new(
                            format!("{path}.to"),
                            "implicit fill → activeFill requires activeFill in the text style",
                        ));
                    }
                }
                _ => {
                    return Err(SchemaError::new(
                        path,
                        "value mapping requires from/to (or keyframes)",
                    ));
                }
            }
        }
    }
    Ok(())
}

// --- colors ---------------------------------------------------------------------

/// Any CSS color: a name (`white`, `darkslategray`), `#rgb`, `#rgba`,
/// `#rrggbb`, `#rrggbbaa`, `rgb()/rgba()`, `hsl()`, `oklch()` — whatever the
/// `color` crate's own parser accepts.
///
/// It used to be a hand-rolled hex slicer taking exactly `#rrggbb` and
/// `#rrggbbaa`. That is the one format with a trap in it: eight hex digits do
/// not say where the alpha byte lives, and an author who writes `#ff3e2723`
/// meaning opaque dark brown gets a near-transparent orange instead — no
/// error, just the wrong picture. Names and six-digit hex have no such
/// ambiguity, so the cure is to accept the whole CSS surface rather than to
/// document the trap.
pub fn parse_color(text: &str) -> Option<AlphaColor<Srgb>> {
    color::parse_color(text.trim())
        .ok()
        .map(|color| color.to_alpha_color::<Srgb>())
}

fn malformed_color(text: &str) -> String {
    format!(
        "malformed color \"{text}\" (expected a CSS color: a name like \"white\", \
         #rgb / #rrggbb / #rrggbbaa with alpha LAST, or rgb()/hsl()/oklch())"
    )
}

// --- spring baking ----------------------------------------------------------------

fn bake_document_springs(doc: &mut Document) -> Result<(), SchemaError> {
    for (index, animator) in doc.animators.iter_mut().enumerate() {
        let path = format!("animators[{index}]");
        if let Some(ease) = &mut animator.ease {
            bake_ease(ease, &format!("{path}.ease"))?;
        }
        if let Some(keyframes) = &mut animator.keyframes {
            for (k, keyframe) in keyframes.iter_mut().enumerate() {
                if let Some(ease) = &mut keyframe.ease {
                    bake_ease(ease, &format!("{path}.keyframes[{k}].ease"))?;
                }
            }
        }
        if let Some(Weight::Stagger(stagger)) = &mut animator.weight {
            if let Some(ease) = &mut stagger.ease {
                bake_ease(ease, &format!("{path}.weight.stagger.ease"))?;
            }
        }
    }
    Ok(())
}

fn bake_ease(ease: &mut Ease, path: &str) -> Result<(), SchemaError> {
    match ease {
        Ease::Spring { bounce, duration } => {
            if !(-1.0..=1.0).contains(bounce) {
                return Err(SchemaError::new(
                    format!("{path}.spring.bounce"),
                    "must be within [-1, 1]",
                ));
            }
            if !(*duration > 0.0 && *duration <= 1.0) {
                return Err(SchemaError::new(
                    format!("{path}.spring.duration"),
                    "must be within (0, 1]",
                ));
            }
            *ease = Ease::Points(bake_spring(*bounce, *duration));
            Ok(())
        }
        Ease::Bezier(b) => {
            if !(0.0..=1.0).contains(&b[0]) || !(0.0..=1.0).contains(&b[2]) {
                return Err(SchemaError::new(
                    path,
                    "bezier x coordinates must be within [0, 1]",
                ));
            }
            Ok(())
        }
        Ease::Points(points) => {
            if points.len() < 2 {
                return Err(SchemaError::new(
                    path,
                    "points ease needs at least 2 points",
                ));
            }
            let mut previous = -1.0f64;
            for point in points.iter() {
                if !(0.0..=1.0).contains(&point[0]) || point[0] < previous {
                    return Err(SchemaError::new(
                        path,
                        "points `in` coordinates must be monotone within [0, 1]",
                    ));
                }
                previous = point[0];
            }
            Ok(())
        }
        Ease::Named(_) => Ok(()),
    }
}

/// Deterministically bake an Apple-parameterized spring into a piecewise-linear
/// curve. `bounce` maps to damping (ζ = 1 − bounce); `duration` is perceptual
/// settling as a fraction of the driver span — the response reaches 1 there and
/// holds. 64 samples across the spring window.
fn bake_spring(bounce: f64, duration: f64) -> Vec<[f64; 2]> {
    const SAMPLES: usize = 64;
    // Envelope decays to this residue exactly at the settling time.
    const RESIDUE: f64 = 1e-3;

    let zeta = (1.0 - bounce).max(0.025);
    let mut points = Vec::with_capacity(SAMPLES + 2);

    for i in 0..=SAMPLES {
        let s = i as f64 / SAMPLES as f64;
        let y = if zeta < 1.0 {
            // Underdamped: settle chosen so e^(−ζω·1) = RESIDUE.
            let zw = -RESIDUE.ln();
            let wd = zw / zeta * (1.0 - zeta * zeta).sqrt();
            1.0 - (-zw * s).exp() * ((wd * s).cos() + zw / wd * (wd * s).sin())
        } else {
            // Critically damped (covers bounce <= 0): (1 + λs)e^(−λs) = RESIDUE at s=1.
            let mut lambda = 5.0f64;
            for _ in 0..32 {
                let f = (1.0 + lambda) * (-lambda).exp() - RESIDUE;
                let df = -lambda * (-lambda).exp();
                lambda -= f / df;
            }
            1.0 - (1.0 + lambda * s) * (-lambda * s).exp()
        };
        points.push([s * duration, y]);
    }
    if duration < 1.0 {
        points.push([1.0, 1.0]);
    } else if let Some(last) = points.last_mut() {
        last[1] = 1.0;
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spring_bake_is_deterministic_and_settles() {
        let a = bake_spring(0.3, 0.6);
        let b = bake_spring(0.3, 0.6);
        assert_eq!(a, b);
        assert_eq!(a.last().unwrap(), &[1.0, 1.0]);
        assert!(a.iter().any(|p| p[1] > 1.0), "bounce 0.3 should overshoot");
    }

    #[test]
    fn spring_bake_critical_no_overshoot() {
        let curve = bake_spring(0.0, 1.0);
        assert!(
            curve.iter().all(|p| p[1] <= 1.0 + 1e-9),
            "bounce 0 must not overshoot"
        );
        assert!((curve.last().unwrap()[1] - 1.0).abs() < 1e-9);
    }
}
