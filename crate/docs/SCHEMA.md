# Motion Document Schema — v1 (normative)

A motion document is a pure function `(inputs) → scene`. It has no intrinsic
duration, no state, and no logic. All variation over time enters through
declared inputs; all motion is expressed as animators mapping inputs to
property values. The document renders standalone using input defaults.

Wire format: JSON, camelCase, UTF-8. Parsing is STRICT: unknown fields,
unknown enum values, duplicate keys, and unresolvable references are hard
errors. The server is the only writer; strictness catches authoring bugs at
write time, never on device.

## 1. Root

```json
{
  "version": 1,
  "size": { "width": 1080, "height": 240 },
  "inputs": [],
  "assets": [],
  "root": {},
  "animators": []
}
```

- `version` — integer, must equal `1`.
- `size` — design box in abstract units (author px-like). The renderer maps
  the design box onto the target with scale `(W/width, H/height)`; callers
  are expected to keep the target aspect-correct.
- `assets` — reserved, must be `[]` in v1. Fonts are never assets — always
  referenced by family name and resolved against the registered collection.

## 2. Inputs

Every input declares `key`, `type`, `default`. Defaults are mandatory —
a document with no supplied signals renders its designed look.

```json
{ "key": "text",        "type": "string",    "default": "Hello world" },
{ "key": "activations", "type": "unitArray", "default": [] },
{ "key": "inProgress",  "type": "unit",      "default": 1 },
{ "key": "time",        "type": "time",      "default": 0 },
{ "key": "accent",      "type": "color",     "default": "#FFD400" },
{ "key": "intensity",   "type": "number",    "default": 6, "min": 0, "max": 24 },
{ "key": "variant",     "type": "enum",      "values": ["fill", "outline"], "default": "fill" },
{ "key": "font",        "type": "fontFamily", "default": "Inter" }
```

| type         | value                              | notes                            |
|--------------|------------------------------------|----------------------------------|
| `unit`       | number, clamped to [0, 1]          | progress-like drivers            |
| `unitArray`  | array of unit                      | per-word/glyph activations       |
| `time`       | number ≥ 0, seconds                | idle loops; see animator `period`|
| `number`     | number, optional `min`/`max` clamp |                                  |
| `string`     | string                             |                                  |
| `color`      | `#RRGGBB` or `#RRGGBBAA`           |                                  |
| `enum`       | one of declared `values`           |                                  |
| `fontFamily` | string                             | resolved against registered fonts|

Input keys: `[a-z][a-zA-Z0-9]*`, unique. Standard keys by convention (not
schema-special): `time`, `inProgress`, `outProgress`, `text`, `activations`,
`emphasis` — the engine and compiler wire these uniformly.

Signal supply at render is LENIENT: unsupplied inputs use defaults, unknown
signal keys are ignored (the engine broadcasts a standard set without knowing
each document's interface). Document-internal references are STRICT.

## 3. Nodes

Common: `kind`, `key` (doc-unique, `[a-zA-Z][a-zA-Z0-9_-]*`, immutable),
optional `role` (doc-unique string; compile-time composition address — in/out
animation fragments target roles, the compiler resolves them to keys).

Any leaf value below may be a literal or an input binding
`{ "input": "key" }` of matching type. `enum` inputs are declarable but not
bindable in v1.

### group

```json
{ "kind": "group", "key": "card", "role": "card",
  "transform": { "translateX": 0, "translateY": 0, "scale": 1, "rotate": 0,
                 "anchorX": 0.5, "anchorY": 0.5 },
  "opacity": 1,
  "children": [] }
```

`rotate` in degrees. `anchorX/Y` are fractions of the group's bounds.
`transform` and `opacity` are optional (identity / 1).

### shape

```json
{ "kind": "shape", "key": "plate",
  "geometry": { "kind": "roundedRect", "x": 0, "y": 0,
                "width": 1080, "height": 240, "radius": 24 },
  "fill":   { "kind": "solid", "color": "#000000CC" },
  "stroke": { "color": "#FFFFFF", "width": 3, "cap": "round", "join": "round" } }
```

Geometries: `rect` (x, y, width, height) · `roundedRect` (+ radius) ·
`ellipse` (cx, cy, rx, ry) · `path` (`d`: SVG path data string).

Paints: `solid` (color) · `linearGradient` (x1, y1, x2, y2, `stops:
[{at, color}]`) · `radialGradient` (cx, cy, r, stops). Gradient stops: `at`
∈ [0,1] monotone.

`fill` and `stroke` are each optional; at least one required.

### text

```json
{ "kind": "text", "key": "line", "role": "text",
  "content": { "input": "text" },
  "frame": { "x": 40, "y": 40, "width": 1000, "height": 160 },
  "style": {
    "fontFamily": "Inter", "weight": 700, "slant": "upright", "size": 64,
    "letterSpacing": 0, "lineHeight": 1.2,
    "align": "center", "valign": "center",
    "fill": "#FFFFFF",
    "activeFill": "#FFD400",
    "stroke": { "color": "#000000", "width": 2 },
    "shadow": { "color": "#00000080", "offsetX": 0, "offsetY": 4, "blur": 8 },
    "pill":   { "color": "#FF3B30", "radius": 10, "paddingX": 12, "paddingY": 6 } } }
```

- `content` — literal string or binding.
- Text wraps within `frame.width`; `valign`: top/center/bottom within frame.
- `weight` 1–1000, `slant` upright/italic; resolution against the registered
  collection follows CSS weight matching.
- `activeFill` — the karaoke secondary color: units at weight 1.0 render
  `activeFill`, weight 0.0 render `fill`, interpolated between (Oklab). It is
  a styling primitive; no animator required for a plain karaoke color swap
  when a `color`-property animator is present — see §5.
- `pill` — background box per WORD, behind glyphs, following per-word bounds
  (+padding, radius). Pill opacity/color animate per-unit like glyph props.
- `stroke`/`shadow`/`pill`/`activeFill` optional.
- Omitted style values default to: `align` left, `valign` top, `weight` 400,
  `slant` upright, `letterSpacing` 0, `lineHeight` 1.2.

Layout-affecting values (`frame`, `size`, `align`, `lineHeight`,
`letterSpacing`, `fontFamily`, `weight`, `content`) are NOT animatable —
paint and transform only (continuous animation never triggers re-layout).

## 4. Animators

Ordered list; evaluation order = document order; later animators compose on
top of earlier ones.

### Node-level

```json
{ "target": "card", "property": "opacity",
  "driver": "inProgress",
  "from": 0, "to": 1, "ease": "easeOut",
  "composite": "replace", "amount": 1 }
```

- `target` — node key. `property` — see vocabulary below.
- `driver` — input key of type `unit`, or `time` with mandatory
  `period` (seconds): `u = (time mod period) / period`.
- Value mapping: either `from`/`to` + `ease` (sugar for two keyframes), or
  `keyframes`:

```json
{ "keyframes": [
    { "at": 0.0, "value": 0 },
    { "at": 0.7, "value": 1.1, "ease": "easeOut" },
    { "at": 1.0, "value": 1.0, "ease": "easeIn" } ] }
```

`at` ∈ [0,1], strictly increasing, explicit first `at: 0` and last `at: 1`
required (no implicit endpoints). `ease` describes the segment ARRIVING at
that keyframe; `ease` on the first keyframe is invalid.

- `composite` — `replace` (default) | `add` | `multiply`. Color-typed
  properties support `replace` only in v1 (arithmetic composites on colors
  are undefined; rejected at validation).
- `amount` — optional [0,1] master weight (literal or unit-input binding):
  final = lerp(previous, composed, amount).

### Easing

`ease` is one of:

- named: `"linear" | "hold" | "easeIn" | "easeOut" | "easeInOut"`
- `{ "bezier": [x1, y1, x2, y2] }` — x clamped to [0,1], y unbounded
  (overshoot allowed)
- `{ "points": [[in, out], ...] }` — piecewise-linear; `in` ∈ [0,1]
  monotone, `out` unbounded (encodes bounces, elastic, baked springs)
- `{ "spring": { "bounce": 0.3, "duration": 0.6 } }` — Apple
  parameterization: `bounce` ∈ [-1,1] (dampingRatio = 1 − bounce),
  `duration` = perceptual settling as a FRACTION of the driver span.
  Deterministically baked to a `points` curve at load; evaluation stays a
  pure f(progress). Never store physics triples.

### Per-unit (text sub-units)

```json
{ "target": "line.words", "property": "scale",
  "weight": { "input": "activations" },
  "from": 1.0, "to": 1.12, "ease": { "spring": { "bounce": 0.4, "duration": 0.5 } },
  "composite": "multiply" }
```

```json
{ "target": "line.glyphs", "property": "translateY",
  "weight": { "stagger": { "driver": "inProgress", "total": 0.6,
                            "from": "start", "ease": "linear" } },
  "from": 40, "to": 0, "ease": "easeOut" }
```

- `target` — `<textNodeKey>.glyphs | .words | .lines`. Glyph units exclude
  whitespace (charsExcludingSpaces semantics).
- `weight` — the per-unit activation w ∈ [0,1] that drives the value
  mapping per unit (AE factorization: value(unit) = map(curve(w(unit)))).
  Two sources:
  - `{ "input": "<unitArray key>" }` — w(i) = array[i]; missing indices → 0,
    extra entries ignored (transcript pages vary).
  - `{ "stagger": { "driver", "total" | "each", "from", "ease" } }` —
    derived from a shared driver: unit i's window is offset within the
    driver span. `driver` follows the animator driver rules — a `time`
    driver carries its mandatory `period` inside the stagger object.
    `total` = fraction of the span consumed by offsets
    (`each` = per-unit fraction; exactly one of the two). `from`:
    `start | center | edges | end | random | index`; `random` requires
    `seed` (integer); `index` requires `indices` order array. `ease`
    distributes offsets non-linearly (GSAP distribution ease).
- Per-unit transforms apply around the unit's own center.

### Property vocabulary v1

| target kind        | properties                                             |
|--------------------|--------------------------------------------------------|
| group, shape, text | `opacity`, `translateX`, `translateY`, `scale`, `scaleX`, `scaleY`, `rotate` |
| shape              | `color` (solid fill), `strokeColor`                    |
| text (node-level)  | `color` (fill), `strokeColor`, `shadowColor`           |
| text sub-units     | `opacity`, `translateX`, `translateY`, `scale`, `rotate`, `color`, `pillColor`, `pillOpacity` |

`color` on sub-units interpolates `fill → activeFill` when `to` is omitted
and `activeFill` is declared; otherwise `from`/`to` colors are explicit.

### Color interpolation

Default space: **Oklab, premultiplied alpha**. Per-animator override:
`"colorSpace": "oklab" | "srgb" | "oklch"`; with `oklch`, optional
`"hue": "shorter" | "longer" | "increasing" | "decreasing"` (default
shorter). Gamut-map at output, never clamp mid-interpolation.

## 5. Evaluation semantics

1. Parse strictly; validate (§6).
2. Bake springs to points curves.
3. Resolve input values: supplied signal or default; clamp per type.
   The C-ABI `t` argument is sugar for the `time` input; an explicit
   `time` signal wins.
4. Resolve bindings in node properties.
5. Apply animators in document order. For each: compute driver value
   (or per-unit weights), map through keyframes/ease, compose onto the
   current property value (`replace`/`add`/`multiply`), apply `amount`.
6. Build the scene: groups nest transforms; text lays out once (layout is
   static per input set); per-unit deltas apply at draw time per glyph run.

Determinism: identical document + identical signals ⇒ identical pixels,
on every platform, forever. No randomness at render (stagger `random` is
seeded and baked at load), no clocks, no environment reads.

## 6. Validation (load-time hard errors)

- unknown field / unknown enum value anywhere
- `version` ≠ 1; `assets` ≠ []
- duplicate node `key` / duplicate `role` / duplicate input `key`
- animator `target` key that doesn't resolve; `.glyphs`-style target on a
  non-text node; `property` not in the vocabulary for the target
- `driver`/`weight`/binding referencing an undeclared input or one of the
  wrong type; `time` driver without `period`
- keyframes: `at` not strictly increasing, missing 0/1 endpoints, `ease`
  on the first keyframe
- value out of domain: unit outside [0,1] (defaults), malformed color,
  gradient stops non-monotone, enum default not in `values`
- text node without `content`; shape without fill and stroke
- binding type mismatch (e.g. `string` input bound to a color property)

`probe` returns `{ "version", "size", "inputs": [...], "roles": [...] }`
for a valid document, or the first validation error via the error channel.

## 7. Reserved for later versions (designed slots, no code)

`assets` (embedded images), group `clip` shapes, filter/effect nodes
(engine post-pass territory), converter chains between driver and curve,
`trigger`-type inputs, falloff-shape enums on stagger, grid staggers,
typographic-space per-unit `size`/`weight` animation, nested/component
documents, path morphing, dash patterns.
