//! Acceptance tests over the real extern-C boundary plus evaluator unit tests.
//! Golden PNGs are exact byte compares (vello_cpu is deterministic); regenerate
//! with KINE_REGEN_GOLDENS=1 only on pinned-version bumps.

use std::ffi::CString;
use std::path::PathBuf;

use crate::eval::{self, Curve, InterpSpace, MapKey, MapValue, RStagger, ValueMap};
use crate::schema::{Composite, StaggerFrom};
use crate::{
    kine_buf, kine_buf_free, kine_last_error, kine_probe, kine_register_font, kine_render_document,
};

const FONT: &[u8] = include_bytes!("../testdata/font.ttf");
const TEST_CARD: &str = include_str!("../tests/fixtures/test_card.json");
const DECORATIONS: &str = include_str!("../tests/fixtures/text_decorations.json");
const MINIMAL: &str = include_str!("../tests/fixtures/minimal.json");

fn register_font() {
    let code = kine_register_font(FONT.as_ptr(), FONT.len());
    assert_eq!(code, 0, "font registration failed: {}", last_error());
}

fn last_error() -> String {
    let ptr = kine_last_error();
    if ptr.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

fn take(buf: kine_buf) -> Option<Vec<u8>> {
    if buf.ptr.is_null() {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(buf.ptr, buf.len) }.to_vec();
    kine_buf_free(buf);
    Some(bytes)
}

fn render(doc: &str, t: f64, signals: &str, w: u32, h: u32) -> kine_buf {
    let doc = CString::new(doc).unwrap();
    let signals = CString::new(signals).unwrap();
    kine_render_document(doc.as_ptr(), t, signals.as_ptr(), w, h)
}

fn probe(doc: &str) -> Option<serde_json::Value> {
    let doc = CString::new(doc).unwrap();
    take(kine_probe(doc.as_ptr())).map(|bytes| serde_json::from_slice(&bytes).unwrap())
}

const PNG_SIGNATURE: &[u8] = &[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

// --- §6 validation table ---------------------------------------------------------

#[test]
fn invalid_fixtures_report_pathed_errors() {
    let table: &[(&str, &str, &str)] = &[
        (
            "unknown_field",
            include_str!("../tests/fixtures/invalid/unknown_field.json"),
            "unknown field `extra`",
        ),
        (
            "unknown_enum",
            include_str!("../tests/fixtures/invalid/unknown_enum.json"),
            "unknown variant `hexagon`",
        ),
        (
            "duplicate_json_key",
            include_str!("../tests/fixtures/invalid/duplicate_json_key.json"),
            "duplicate key \"version\"",
        ),
        (
            "bad_version",
            include_str!("../tests/fixtures/invalid/bad_version.json"),
            "version: must be 1",
        ),
        (
            "assets_nonempty",
            include_str!("../tests/fixtures/invalid/assets_nonempty.json"),
            "assets: must be []",
        ),
        (
            "dup_node_key",
            include_str!("../tests/fixtures/invalid/dup_node_key.json"),
            "duplicate node key \"dot\"",
        ),
        (
            "dup_role",
            include_str!("../tests/fixtures/invalid/dup_role.json"),
            "duplicate role \"card\"",
        ),
        (
            "dup_input_key",
            include_str!("../tests/fixtures/invalid/dup_input_key.json"),
            "duplicate input key \"progress\"",
        ),
        (
            "unknown_target",
            include_str!("../tests/fixtures/invalid/unknown_target.json"),
            "unknown node key \"nope\"",
        ),
        (
            "units_on_shape",
            include_str!("../tests/fixtures/invalid/units_on_shape.json"),
            "sub-unit targets",
        ),
        (
            "bad_property",
            include_str!("../tests/fixtures/invalid/bad_property.json"),
            "not animatable",
        ),
        (
            "undeclared_driver",
            include_str!("../tests/fixtures/invalid/undeclared_driver.json"),
            "undeclared input \"nope\"",
        ),
        (
            "wrong_driver_type",
            include_str!("../tests/fixtures/invalid/wrong_driver_type.json"),
            "expected unit or time",
        ),
        (
            "time_driver_no_period",
            include_str!("../tests/fixtures/invalid/time_driver_no_period.json"),
            "requires period",
        ),
        (
            "kf_not_increasing",
            include_str!("../tests/fixtures/invalid/kf_not_increasing.json"),
            "strictly increasing",
        ),
        (
            "kf_missing_endpoints",
            include_str!("../tests/fixtures/invalid/kf_missing_endpoints.json"),
            "at: 0 first",
        ),
        (
            "kf_first_ease",
            include_str!("../tests/fixtures/invalid/kf_first_ease.json"),
            "keyframes[0].ease",
        ),
        (
            "unit_default_oob",
            include_str!("../tests/fixtures/invalid/unit_default_oob.json"),
            "outside [0, 1]",
        ),
        (
            "malformed_color",
            include_str!("../tests/fixtures/invalid/malformed_color.json"),
            "malformed color",
        ),
        (
            "stops_nonmonotone",
            include_str!("../tests/fixtures/invalid/stops_nonmonotone.json"),
            "monotone",
        ),
        (
            "enum_default_not_in_values",
            include_str!("../tests/fixtures/invalid/enum_default_not_in_values.json"),
            "not one of the declared values",
        ),
        (
            "text_without_content",
            include_str!("../tests/fixtures/invalid/text_without_content.json"),
            "missing field `content`",
        ),
        (
            "shape_no_fill_no_stroke",
            include_str!("../tests/fixtures/invalid/shape_no_fill_no_stroke.json"),
            "requires fill or stroke",
        ),
        (
            "binding_type_mismatch",
            include_str!("../tests/fixtures/invalid/binding_type_mismatch.json"),
            "type mismatch",
        ),
        (
            "stagger_random_no_seed",
            include_str!("../tests/fixtures/invalid/stagger_random_no_seed.json"),
            "requires seed",
        ),
        (
            "color_composite_add",
            include_str!("../tests/fixtures/invalid/color_composite_add.json"),
            "not supported for color properties",
        ),
        (
            "pill_props_without_pill",
            include_str!("../tests/fixtures/invalid/pill_props_without_pill.json"),
            "require a pill",
        ),
        (
            "karaoke_without_activefill",
            include_str!("../tests/fixtures/invalid/karaoke_without_activefill.json"),
            "requires activeFill",
        ),
    ];
    for (name, fixture, expected) in table {
        let result = probe(fixture);
        assert!(
            result.is_none(),
            "{name}: expected validation failure, got {result:?}"
        );
        let error = last_error();
        assert!(
            error.contains(expected),
            "{name}: error {error:?} does not contain {expected:?}"
        );
    }
}

// --- probe ------------------------------------------------------------------------

#[test]
fn probe_reports_the_declared_interface() {
    let interface = probe(TEST_CARD).unwrap_or_else(|| panic!("probe failed: {}", last_error()));
    assert_eq!(interface["version"], 1);
    assert_eq!(interface["size"]["width"], 512.0);
    assert_eq!(interface["roles"], serde_json::json!(["card", "text"]));

    let inputs = interface["inputs"].as_array().unwrap();
    let keys: Vec<&str> = inputs.iter().map(|i| i["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["time", "progress", "activations", "font"]);
    assert_eq!(inputs[0]["type"], "time");
    assert_eq!(inputs[3]["default"], "Bebas Neue");
}

#[test]
fn probe_accepts_the_minimal_document() {
    let interface = probe(MINIMAL).unwrap_or_else(|| panic!("probe failed: {}", last_error()));
    assert_eq!(interface["inputs"], serde_json::json!([]));
    assert_eq!(interface["roles"], serde_json::json!([]));
}

// --- rendering ----------------------------------------------------------------------

#[test]
fn renders_the_test_card_document() {
    register_font();
    let png = take(render(TEST_CARD, 0.5, r#"{"progress":0.5}"#, 512, 512))
        .unwrap_or_else(|| panic!("render failed: {}", last_error()));
    assert!(png.starts_with(PNG_SIGNATURE));

    let decoder = png::Decoder::new(std::io::Cursor::new(png.as_slice()));
    let mut reader = decoder.read_info().expect("PNG did not decode");
    let info = reader.info().clone();
    assert_eq!((info.width, info.height), (512, 512));

    let mut buf = vec![0u8; reader.output_buffer_size().expect("buffer size")];
    let frame = reader.next_frame(&mut buf).expect("frame decode failed");
    let pixels = &buf[..frame.buffer_size()];
    assert_eq!(info.color_type, png::ColorType::Rgba);
    let total = (info.width * info.height) as usize;
    let opaque = pixels.chunks_exact(4).filter(|px| px[3] > 0).count();
    assert!(
        opaque * 10 > total,
        "expected >10% non-transparent pixels, got {opaque}/{total}"
    );
}

#[test]
fn t_and_signals_change_the_output() {
    register_font();
    let a = take(render(TEST_CARD, 0.0, r#"{"progress":0.0}"#, 256, 256)).unwrap();
    let b = take(render(TEST_CARD, 0.7, r#"{"progress":0.8}"#, 256, 256)).unwrap();
    let c = take(render(
        TEST_CARD,
        0.7,
        r#"{"progress":0.8,"activations":[1,0.5]}"#,
        256,
        256,
    ))
    .unwrap();
    assert_ne!(a, b, "t/progress changed nothing");
    assert_ne!(b, c, "activations changed nothing");
}

#[test]
fn time_driver_wraps_at_period() {
    register_font();
    // The card's wave has period 2 — t and t+2 must render identically.
    let a = take(render(TEST_CARD, 0.35, "{}", 128, 128)).unwrap();
    let b = take(render(TEST_CARD, 2.35, "{}", 128, 128)).unwrap();
    assert_eq!(a, b, "time driver did not wrap at the period");
}

#[test]
fn renders_are_deterministic() {
    register_font();
    let a = take(render(TEST_CARD, 0.4, r#"{"progress":0.6}"#, 300, 300)).unwrap();
    let b = take(render(TEST_CARD, 0.4, r#"{"progress":0.6}"#, 300, 300)).unwrap();
    assert_eq!(a, b, "same doc+signals must render identical bytes");
}

#[test]
fn renders_text_decorations() {
    register_font();
    let a = take(render(DECORATIONS, 0.0, r#"{"progress":0.1}"#, 256, 256))
        .unwrap_or_else(|| panic!("render failed: {}", last_error()));
    let b = take(render(DECORATIONS, 0.0, r#"{"progress":0.9}"#, 256, 256)).unwrap();
    assert!(a.starts_with(PNG_SIGNATURE));
    assert_ne!(a, b, "pill/rotate driven by progress changed nothing");
}

#[test]
fn golden_renders() {
    register_font();
    let cases: &[(&str, f64, &str)] = &[
        ("card_t0_p0.png", 0.0, r#"{"progress":0}"#),
        ("card_t05_p05.png", 0.5, r#"{"progress":0.5}"#),
        ("card_t05_p1.png", 0.5, r#"{"progress":1}"#),
    ];
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens");
    let regen = std::env::var_os("KINE_REGEN_GOLDENS").is_some();
    for (name, t, signals) in cases {
        let png = take(render(TEST_CARD, *t, signals, 512, 512))
            .unwrap_or_else(|| panic!("render failed: {}", last_error()));
        let path = dir.join(name);
        if regen {
            std::fs::write(&path, &png).unwrap();
            continue;
        }
        let golden = std::fs::read(&path)
            .unwrap_or_else(|_| panic!("{name} missing — run with KINE_REGEN_GOLDENS=1"));
        assert_eq!(
            png, golden,
            "{name} diverged from golden (pinned versions unchanged?)"
        );
    }
}

// --- error boundary -------------------------------------------------------------------

#[test]
fn empty_document_fails_validation_not_abort() {
    let buf = render("{}", 0.0, "{}", 64, 64);
    assert!(take(buf).is_none());
    assert!(
        last_error().contains("missing field"),
        "got: {:?}",
        last_error()
    );
}

#[test]
fn unregistered_font_family_is_a_render_error() {
    register_font();
    let doc = r##"{ "version": 1, "size": { "width": 64, "height": 64 },
      "root": { "kind": "text", "key": "t", "content": "X",
        "frame": { "x": 0, "y": 0, "width": 64, "height": 64 },
        "style": { "fontFamily": "Nope Sans", "size": 20, "fill": "#FFFFFF" } } }"##;
    let buf = render(doc, 0.0, "{}", 64, 64);
    assert!(take(buf).is_none());
    assert!(
        last_error().contains("not registered"),
        "got: {:?}",
        last_error()
    );
}

#[test]
fn zero_size_is_rejected_without_aborting() {
    let buf = render(MINIMAL, 0.0, "{}", 0, 64);
    assert!(take(buf).is_none());
    assert!(last_error().contains("non-zero"), "got: {:?}", last_error());
}

#[test]
fn invalid_utf8_is_rejected_without_aborting() {
    let bytes: [std::ffi::c_char; 3] = [-1, -2, 0];
    let buf = kine_render_document(bytes.as_ptr(), 0.0, std::ptr::null(), 64, 64);
    assert!(take(buf).is_none());
    assert!(last_error().contains("UTF-8"), "got: {:?}", last_error());
}

#[test]
fn null_document_is_rejected() {
    let buf = kine_render_document(std::ptr::null(), 0.0, std::ptr::null(), 64, 64);
    assert!(take(buf).is_none());
    assert!(last_error().contains("null"), "got: {:?}", last_error());
}

// --- evaluator units --------------------------------------------------------------------

#[test]
fn curves_evaluate_canonically() {
    assert_eq!(Curve::Linear.eval(0.25), 0.25);
    assert_eq!(Curve::Hold.eval(0.99), 0.0);
    assert_eq!(Curve::Hold.eval(1.0), 1.0);
    // ease-in-out is symmetric and passes through 0.5.
    let inout = Curve::Bezier {
        x1: 0.42,
        y1: 0.0,
        x2: 0.58,
        y2: 1.0,
    };
    assert!((inout.eval(0.5) - 0.5).abs() < 1e-4);
    assert!(inout.eval(0.25) < 0.25, "ease-in-out starts slow");
    let points = Curve::Points(vec![[0.0, 0.0], [0.5, 1.5], [1.0, 1.0]]);
    assert!(
        (points.eval(0.25) - 0.75).abs() < 1e-9,
        "piecewise-linear midpoint"
    );
    assert!((points.eval(0.5) - 1.5).abs() < 1e-9, "overshoot preserved");
}

#[test]
fn composites_stack_in_document_order() {
    let mut value = 1.0;
    value = eval::composite_num(value, 2.0, Composite::Replace, 1.0);
    value = eval::composite_num(value, 3.0, Composite::Add, 1.0);
    value = eval::composite_num(value, 2.0, Composite::Multiply, 1.0);
    assert_eq!(value, 10.0, "replace(2) → add(3)=5 → multiply(2)=10");

    // amount lerps between previous and composed.
    let half = eval::composite_num(4.0, 8.0, Composite::Replace, 0.5);
    assert_eq!(half, 6.0);
}

#[test]
fn keyframe_map_applies_arriving_ease_per_segment() {
    let map = ValueMap {
        keys: vec![
            MapKey {
                at: 0.0,
                value: MapValue::Num(0.0),
                ease: None,
            },
            MapKey {
                at: 0.5,
                value: MapValue::Num(10.0),
                ease: Some(Curve::Hold),
            },
            MapKey {
                at: 1.0,
                value: MapValue::Num(20.0),
                ease: None,
            },
        ],
    };
    let space = InterpSpace::default();
    let MapValue::Num(v) = map.eval(0.25, space) else {
        panic!()
    };
    assert_eq!(
        v, 0.0,
        "hold keeps the previous keyframe value through the segment"
    );
    let MapValue::Num(v) = map.eval(0.5, space) else {
        panic!()
    };
    assert_eq!(v, 10.0);
    let MapValue::Num(v) = map.eval(0.75, space) else {
        panic!()
    };
    assert_eq!(v, 15.0, "linear default on the second segment");
}

fn stagger(from: StaggerFrom, u: f64, seed: u64) -> RStagger {
    RStagger {
        driver_u: u,
        total: Some(0.5),
        each: None,
        from,
        ease: None,
        seed,
        indices: vec![],
    }
}

#[test]
fn stagger_windows_offset_by_rank() {
    let weights = eval::stagger_weights(&stagger(StaggerFrom::Start, 0.5, 0), 5);
    assert_eq!(weights.len(), 5);
    assert!(
        (weights[0] - 1.0).abs() < 1e-9,
        "first unit finished at u=0.5 (window 0.5)"
    );
    assert_eq!(weights[4], 0.0, "last unit starts at offset 0.5");
    for pair in weights.windows(2) {
        assert!(pair[0] >= pair[1], "from start: earlier units lead");
    }

    let end = eval::stagger_weights(&stagger(StaggerFrom::End, 0.5, 0), 5);
    let reversed: Vec<f64> = weights.iter().rev().copied().collect();
    assert_eq!(end, reversed, "from end mirrors from start");

    let center = eval::stagger_weights(&stagger(StaggerFrom::Center, 0.4, 0), 5);
    assert!(center[2] > center[0], "center unit leads for from: center");
    assert_eq!(center[0], center[4], "symmetric edges");
}

#[test]
fn stagger_random_is_seed_deterministic() {
    let a = eval::stagger_weights(&stagger(StaggerFrom::Random, 0.4, 42), 8);
    let b = eval::stagger_weights(&stagger(StaggerFrom::Random, 0.4, 42), 8);
    let c = eval::stagger_weights(&stagger(StaggerFrom::Random, 0.4, 43), 8);
    assert_eq!(a, b, "same seed → same order");
    assert_ne!(a, c, "different seed → different order");
}

#[test]
fn activation_weights_tolerate_short_and_long_arrays() {
    // Short arrays default missing units to 0; long arrays ignore extras.
    let doc = crate::schema::parse(TEST_CARD).unwrap();
    let compiled = crate::validate::validate(doc).unwrap();
    let signals: serde_json::Value =
        serde_json::from_str(r#"{"activations":[1,0.5,2,-1]}"#).unwrap();
    let values = eval::resolve_inputs(&compiled.doc, &signals, 0.0).unwrap();
    match &values["activations"] {
        eval::InputValue::UnitArray(a) => {
            assert_eq!(a, &vec![1.0, 0.5, 1.0, 0.0], "elements clamp to [0,1]")
        }
        other => panic!("wrong type: {other:?}"),
    }
}

#[test]
fn input_resolution_clamps_and_wires_t_sugar() {
    let doc = crate::schema::parse(TEST_CARD).unwrap();
    let compiled = crate::validate::validate(doc).unwrap();

    let signals: serde_json::Value = serde_json::from_str(r#"{"progress":1.7}"#).unwrap();
    let values = eval::resolve_inputs(&compiled.doc, &signals, 3.25).unwrap();
    assert!(
        matches!(values["progress"], eval::InputValue::Unit(v) if v == 1.0),
        "unit clamps"
    );
    assert!(
        matches!(values["time"], eval::InputValue::Time(v) if v == 3.25),
        "t sugar fills time"
    );

    let explicit: serde_json::Value = serde_json::from_str(r#"{"time":9.0}"#).unwrap();
    let values = eval::resolve_inputs(&compiled.doc, &explicit, 3.25).unwrap();
    assert!(
        matches!(values["time"], eval::InputValue::Time(v) if v == 9.0),
        "explicit time wins"
    );
}
