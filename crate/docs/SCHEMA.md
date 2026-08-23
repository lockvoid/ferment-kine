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
  "colors": [],
  "assets": [],
  "root": {},
  "animators": []
}
```

- `version` — integer, must equal `1`.
- `size` — design box in abstract units (author px-like). The renderer maps
  the design box onto the target with scale `(W/width, H/height)`; callers
  are expected to keep the target aspect-correct.
- `assets` — embedded raster assets (§4). Fonts are never assets — always
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
| `color`      | any CSS color (see below)          | a name, hex, `rgb()`, `hsl()`, … |
| `enum`       | one of declared `values`           |                                  |
| `fontFamily` | string                             | resolved against registered fonts|

Input keys: `[a-z][a-zA-Z0-9]*`, unique. Standard keys by convention (not
schema-special): `time`, `inProgress`, `outProgress`, `focus`, `text`,
`activations`, `emphasis` — the engine and compiler wire these uniformly.
Standard SEED inputs (color-typed): `foreground`, `background`, `accent` —
the published color interface. Hosts map them from user pickers and brand
roles (`foreground ← brand.text`, `background ← brand.background`,
`accent ← brand.accent`); documents consume them through the colors table
(§3). A document declares only the seeds it uses; pickers render per probe.

Signal supply at render is LENIENT: unsupplied inputs use defaults, unknown
signal keys are ignored (the engine broadcasts a standard set without knowing
each document's interface). Document-internal references are STRICT.

## 3. Colors

The document's private color table: named colors derived from seeds and
each other, so the derivation (one accent → active/pill/emphasis, contrast
strokes, translucent plates) is document truth executed identically on
every platform — never host-side color math.

```json
"colors": [
  { "key": "textFill", "value": { "input": "foreground" } },
  { "key": "active",   "value": { "input": "accent" } },
  { "key": "pill",     "value": { "fn": "alpha", "of": "active", "amount": 0.3 } },
  { "key": "strokeC",  "value": { "fn": "contrast", "of": "textFill" },
    "override": { "input": "strokeColor" } },
  { "key": "plateC",   "value": { "fn": "mix", "a": "#000000", "b": "background", "t": 0.85 } }
]
```

- Entry `key`: `[a-z][a-zA-Z0-9]*`, unique across the table.
- `value` grammar — one of:
  - a literal CSS color: a name (`white`, `darkslategray`), `#RGB` / `#RGBA` /
    `#RRGGBB` / `#RRGGBBAA` — **alpha LAST**, so opaque dark brown is
    `#3E2723FF` and never `#FF3E2723` — or a function (`rgb()`, `rgba()`,
    `hsl()`, `oklch()`, …)
  - `{ "input": "<color input key>" }`
  - `{ "fn": "alpha", "of": ref, "amount": 0..1 }` — replaces alpha
  - `{ "fn": "contrast", "of": ref }` — `#000000` or `#FFFFFF`, whichever
    has the greater Oklab lightness distance from `of`
  - `{ "fn": "mix", "a": ref, "b": ref, "t": 0..1 }` — Oklab,
    premultiplied-alpha interpolation
- A `ref` argument is a literal (starts with `#`), a color-input binding
  object, or the string key of an EARLIER table entry — entries may only
  reference entries declared above them (acyclic by construction).
- `override` — optional binding to a declared color input; when that
  signal is supplied at render it replaces the computed value entirely.
  This is the explicit-surface channel: host style overrides win over
  derivation.
- The table is PRIVATE: probe does not emit it. The published interface
  stays the declared inputs (seeds + any override inputs).
- Usage: wherever a color value is accepted (node paints, text style,
  animator `from`/`to`), the form `{ "color": "<entry key>" }` references
  a table entry, alongside the existing literal and `{ "input": … }` forms.
- Variants (invert etc.) are NOT expressed in-document: a variant is a
  compiler-side overlay replacing table entries — structural selection,
  recompile, engine stays conditional-free.

### Function reference

All color math runs in Oklab with premultiplied alpha, evaluated once per
input set (§6 step 4). Functions are pure; results are ordinary colors.

`{ "fn": "alpha", "of": ref, "amount": a }`
- `a` ∈ [0,1]. Replaces the alpha channel: rgb unchanged, alpha = `a`.
  Absolute, not multiplicative — authors state final translucency; on
  nested alpha calls the outer one wins.

`{ "fn": "contrast", "of": ref, "candidates": [ref, …]? }`
- `candidates` = 2+ refs, default `["#000000", "#FFFFFF"]`. Returns the
  candidate with the greatest |ΔL| (Oklab lightness distance) from `of`;
  ties resolve to the earlier candidate. The auto-stroke / glyph-on-pill
  derivation; with custom candidates it clamps any input color onto a
  designed pole set.

`{ "fn": "mix", "a": ref, "b": ref, "t": t }`
- `t` ∈ [0,1]. Oklab premultiplied interpolation from `a` (t=0) to `b`
  (t=1) — the same lerp animators use (§6). lighten/darken are
  deliberately NOT functions: mix with white/black.

Reserved (§9): `nearest` (min-ΔE snap to candidates — the quantize cousin
of contrast), hue rotation, lighten/darken sugar.

## 4. Assets

Embedded raster assets — the document stays a single JSON artifact.

```json
"assets": [
  { "key": "avatar",  "kind": "image", "mime": "image/png",  "data": "<base64>" },
  { "key": "sticker", "kind": "image", "mime": "image/gif",  "data": "<base64>" }
]
```

- `key`: `[a-z][a-zA-Z0-9]*`, unique across the table.
- `kind`: `image` (the only kind in v1).
- `mime`: `image/png | image/jpeg | image/webp | image/gif | image/apng`.
  Animated formats (gif, apng, animated webp) are first-class.
- `data`: base64 of the encoded file. There is no other source form in
  v1 (`href` host-resolved references are reserved, §9).

Budgets — HARD validation errors, checked from container headers BEFORE
full decode (the decode-bomb guard, including its time axis):

| limit | value |
|---|---|
| encoded size per asset | ≤ 2 MB |
| encoded total per document | ≤ 4 MB |
| pixel dimensions | ≤ 2048 × 2048 |
| frames per animated asset | ≤ 120 |
| decoded size per asset (`frames × w × h × 4`) | ≤ 32 MB |

Decoding happens once at document load: all frames composited
(blend/dispose resolved) and premultiplied; the document handle owns that
memory for its lifetime — which is what the decoded budget bounds.
Embedded ICC color profiles are ignored in v1 — asset pixels are taken as
sRGB.

**Animated sampling rule**: an animated asset displays the frame at
`time mod loopDuration`, where `time` is the standard input and
`loopDuration` is the sum of the asset's frame durations. Looping is
implicit; there are no speed/loop/remap controls in v1 (reserved). With
no `time` signal supplied, the default (0) shows the first frame — so
standalone/preview renders are well-defined. Zero-duration frames are
treated as 100ms (the de-facto browser rule). This keeps animated assets
inside the pure-function contract: a pre-baked animation is just a
function of the `time` input.

Assets appear in documents through the `image` node (§5). Probe emits an
assets manifest: `[{ key, kind, mime, animated }]`.

## 5. Nodes

Common: `kind`, `key` (doc-unique, `[a-zA-Z][a-zA-Z0-9_-]*`, immutable),
optional `role` (doc-unique string; compile-time composition address — in/out
animation fragments target roles, the compiler resolves them to keys).

Any leaf value below may be a literal or an input binding
`{ "input": "key" }` of matching type; color-typed values additionally
accept a table reference `{ "color": "key" }` (§3). `enum` inputs are
declarable but not bindable in v1.

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

### image

```json
{ "kind": "image", "key": "avatar",
  "asset": "avatar",
  "frame": { "x": 420, "y": 640, "width": 240, "height": 240 },
  "fit": "cover",
  "cornerRadius": 120,
  "opacity": 1 }
```

- `asset` — key of an entry in the assets table (§4).
- `fit`: `cover` (default) | `contain` | `fill` — how the decoded image
  maps onto `frame`.
- `cornerRadius` — rounded clip of the frame; `min(width, height) / 2`
  yields a circle. Optional (0).
- `frame` and `fit` are layout — not animatable. Transform/opacity follow
  the standard vocabulary. Animated assets sample by the §4 rule; the
  node needs no extra fields for animation.
- Image-as-paint (image fills on shapes) is reserved (§9).

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
  when a `color`-property animator is present — see §6.
- `pill` — background box per WORD, behind glyphs, following per-word bounds
  (+padding, radius). Pill opacity/color animate per-unit like glyph props.
- `stroke`/`shadow`/`pill`/`activeFill` optional.
- Omitted style values default to: `align` left, `valign` top, `weight` 400,
  `slant` upright, `letterSpacing` 0, `lineHeight` 1.2.

Layout-affecting values (`frame`, `size`, `align`, `lineHeight`,
`letterSpacing`, `fontFamily`, `weight`, `content`) are NOT animatable —
paint and transform only (continuous animation never triggers re-layout).

## 6. Animators

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
| group, shape, text, image | `opacity`, `translateX`, `translateY`, `scale`, `scaleX`, `scaleY`, `rotate` |
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

## 7. Evaluation semantics

1. Parse strictly; validate (§8).
2. Decode assets once (at document load): frames composited
   (blend/dispose resolved) and premultiplied; held by the handle.
3. Bake springs to points curves.
4. Resolve input values: supplied signal or default; clamp per type.
   The C-ABI `t` argument is sugar for the `time` input; an explicit
   `time` signal wins.
5. Resolve the colors table in declaration order (override input if
   supplied, else the computed value); then resolve bindings and `{color}`
   references in node properties.
6. Apply animators in document order. For each: compute driver value
   (or per-unit weights), map through keyframes/ease, compose onto the
   current property value (`replace`/`add`/`multiply`), apply `amount`.
7. Build the scene: groups nest transforms; text lays out once (layout is
   static per input set); per-unit deltas apply at draw time per glyph
   run; animated assets sample their frame by the §4 rule.

Determinism: identical document + identical signals ⇒ identical pixels,
on every platform, forever. No randomness at render (stagger `random` is
seeded and baked at load), no clocks, no environment reads.

## 8. Validation (load-time hard errors)

- unknown field / unknown enum value anywhere
- `version` ≠ 1
- assets: duplicate `key`; unknown `kind`/`mime`; malformed base64 or a
  payload whose container signature contradicts `mime`; any §4 budget
  exceeded (checked from headers before decode); an `image` node's
  `asset` referencing an unknown key
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
- colors table: duplicate entry key; reference to an unknown or LATER
  entry; unknown `fn`; fn argument out of domain; `override` binding a
  non-color input; `{ "color": … }` reference to an unknown entry

`probe` returns `{ "version", "size", "inputs": [...], "roles": [...],
"assets": [...] }` (assets manifest per §4) for a valid document, or the
first validation error via the error channel.

## 9. Reserved for later versions (designed slots, no code)

Assets: `href` source form (host-resolved references registered like
fonts, for cross-document reuse), image-as-paint (image fills on shapes),
image-typed inputs (runtime image swapping), per-node time
remapping/speed/loop controls for animated assets.

Also reserved: group `clip` shapes, filter/effect nodes
(engine post-pass territory), converter chains between driver and curve,
`trigger`-type inputs, falloff-shape enums on stagger, grid staggers,
typographic-space per-unit `size`/`weight` animation, nested/component
documents, path morphing, dash patterns, additional color functions
(lighten/darken sugar, hue rotation).
