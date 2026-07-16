//! Acceptance tests over the real extern-C boundary plus evaluator unit tests.
//! Golden PNGs are exact byte compares (vello_cpu is deterministic); regenerate
//! with KINE_REGEN_GOLDENS=1 only on pinned-version bumps.

use std::ffi::CString;
use std::path::PathBuf;

use crate::eval::{self, Curve, InterpSpace, MapKey, MapValue, RStagger, ValueMap};
use crate::schema::{Composite, StaggerFrom};
use crate::{
    kine_buf, kine_buf_free, kine_document_create, kine_document_free, kine_document_probe,
    kine_document_render_rgba, kine_last_error, kine_probe, kine_register_font,
    kine_render_document, kine_render_document_rgba, kine_version,
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

// Goldens are byte-exact on the architecture they were generated on (arm64 /
// NEON). On other arches SIMD rounding can diverge, so there we only check
// structural validity — the cross-arch bitwise verdict is an informational CI
// job, not a gate (see README + .github/workflows/ci.yml).
fn assert_golden(name: &str, png: &[u8], golden: &[u8]) {
    if cfg!(target_arch = "aarch64") {
        assert_eq!(png, golden, "{name} diverged from its golden");
    } else {
        assert!(png.starts_with(PNG_SIGNATURE), "{name}: not a PNG");
        assert!(!golden.is_empty(), "{name} golden missing");
    }
}

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
        (
            "color_dup_entry",
            include_str!("../tests/fixtures/invalid/color_dup_entry.json"),
            "duplicate color entry key \"a\"",
        ),
        (
            "color_later_ref",
            include_str!("../tests/fixtures/invalid/color_later_ref.json"),
            "unknown or later color entry \"b\"",
        ),
        (
            "color_unknown_ref",
            include_str!("../tests/fixtures/invalid/color_unknown_ref.json"),
            "unknown or later color entry \"nope\"",
        ),
        (
            "color_unknown_fn",
            include_str!("../tests/fixtures/invalid/color_unknown_fn.json"),
            "unknown color function \"lighten\"",
        ),
        (
            "color_amount_oob",
            include_str!("../tests/fixtures/invalid/color_amount_oob.json"),
            "within [0, 1]",
        ),
        (
            "color_contrast_one_candidate",
            include_str!("../tests/fixtures/invalid/color_contrast_one_candidate.json"),
            "at least 2 candidates",
        ),
        (
            "color_override_noncolor",
            include_str!("../tests/fixtures/invalid/color_override_noncolor.json"),
            "expected a color input",
        ),
        (
            "color_value_unknown_ref",
            include_str!("../tests/fixtures/invalid/color_value_unknown_ref.json"),
            "unknown color entry \"nope\"",
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
    assert_eq!(
        keys,
        [
            "time",
            "progress",
            "activations",
            "font",
            "foreground",
            "background",
            "accent",
            "borderColor"
        ]
    );
    assert_eq!(inputs[0]["type"], "time");
    assert_eq!(inputs[3]["default"], "Bebas Neue");
    assert_eq!(inputs[4]["type"], "color");
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
        assert_golden(name, &png, &golden);
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

// --- version ------------------------------------------------------------------------

#[test]
fn version_reports_crate_and_schema() {
    let text = unsafe { std::ffi::CStr::from_ptr(kine_version()) }
        .to_str()
        .unwrap();
    assert!(text.starts_with("kine "), "got: {text:?}");
    assert!(text.contains(env!("CARGO_PKG_VERSION")));
    assert!(text.contains("schema v1"));
}

// --- handles ------------------------------------------------------------------------

fn create(doc: &str) -> i64 {
    kine_document_create(CString::new(doc).unwrap().as_ptr())
}

fn render_handle_rgba(handle: i64, t: f64, signals: &str, w: u32, h: u32) -> Option<Vec<u8>> {
    let signals = CString::new(signals).unwrap();
    take(kine_document_render_rgba(handle, t, signals.as_ptr(), w, h))
}

#[test]
fn handle_lifecycle() {
    register_font();
    let handle = create(TEST_CARD);
    assert!(handle > 0, "create failed: {}", last_error());

    let interface: serde_json::Value = {
        let bytes = take(kine_document_probe(handle)).expect("probe failed");
        serde_json::from_slice(&bytes).unwrap()
    };
    assert_eq!(interface["roles"], serde_json::json!(["card", "text"]));

    let frame = render_handle_rgba(handle, 0.5, r#"{"progress":0.5}"#, 128, 128)
        .expect("handle render failed");
    assert_eq!(frame.len(), 128 * 128 * 4, "RGBA stride = width*4");

    kine_document_free(handle);
}

#[test]
fn invalid_document_yields_handle_zero() {
    let handle = create("{ not json");
    assert_eq!(handle, 0);
    assert!(!last_error().is_empty(), "a parse error must be reported");

    let missing = create("{}");
    assert_eq!(missing, 0);
    assert!(
        last_error().contains("missing field"),
        "got: {:?}",
        last_error()
    );
}

#[test]
fn use_after_free_errors_never_crashes() {
    register_font();
    let handle = create(TEST_CARD);
    assert!(handle > 0);
    kine_document_free(handle);
    kine_document_free(handle); // double free is a no-op

    assert!(render_handle_rgba(handle, 0.0, "{}", 64, 64).is_none());
    assert!(last_error().contains("freed"), "got: {:?}", last_error());
    assert!(take(kine_document_probe(handle)).is_none());
    // Never-issued handle behaves the same.
    assert!(render_handle_rgba(999_999, 0.0, "{}", 64, 64).is_none());
}

#[test]
fn concurrent_renders_one_handle() {
    register_font();
    let handle = create(TEST_CARD);
    assert!(handle > 0);
    let ok = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let threads: Vec<_> = (0..8)
        .map(|n| {
            let ok = ok.clone();
            std::thread::spawn(move || {
                for i in 0..20 {
                    let signals = format!(r#"{{"progress":{}}}"#, (i % 10) as f64 / 10.0);
                    if render_handle_rgba(handle, n as f64 + i as f64 * 0.01, &signals, 96, 96)
                        .is_some_and(|f| f.len() == 96 * 96 * 4)
                    {
                        ok.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(ok.load(std::sync::atomic::Ordering::Relaxed), 8 * 20);
    kine_document_free(handle);
}

#[test]
fn concurrent_renders_many_handles() {
    register_font();
    let threads: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(|| {
                let handle = create(TEST_CARD);
                assert!(handle > 0);
                for i in 0..15 {
                    let f = render_handle_rgba(handle, i as f64 * 0.1, "{}", 80, 80).unwrap();
                    assert_eq!(f.len(), 80 * 80 * 4);
                }
                kine_document_free(handle);
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }
}

#[test]
fn handle_cycles_do_not_leak_the_registry() {
    register_font();
    // 1000 create/render/free cycles. Assert the registry doesn't retain freed
    // handles — deterministic and isolation-proof (each id is checked directly,
    // ids are monotonic/unique), unlike a process-RSS gauge that concurrent
    // memory-heavy tests pollute.
    for i in 0..1000 {
        let handle = create(TEST_CARD);
        assert!(handle > 0);
        let _ = render_handle_rgba(handle, i as f64 * 0.001, "{}", 64, 64).unwrap();
        kine_document_free(handle);
        assert!(
            !crate::handle::contains(handle),
            "handle {handle} still live after free (registry leak)"
        );
    }
}

// --- rgba consistency ---------------------------------------------------------------

const OPAQUE_FILL: &str = include_str!("../tests/fixtures/features/opaque_fill.json");
const TRANSLUCENT_FILL: &str = include_str!("../tests/fixtures/features/translucent_fill.json");

fn render_rgba(doc: &str, w: u32, h: u32) -> Vec<u8> {
    let doc = CString::new(doc).unwrap();
    take(kine_render_document_rgba(
        doc.as_ptr(),
        0.0,
        std::ptr::null(),
        w,
        h,
    ))
    .unwrap_or_else(|| panic!("rgba render failed: {}", last_error()))
}

fn decode_png_rgba(png: &[u8]) -> (u32, u32, Vec<u8>) {
    let decoder = png::Decoder::new(std::io::Cursor::new(png));
    let mut reader = decoder.read_info().unwrap();
    let info = reader.info().clone();
    assert_eq!(info.color_type, png::ColorType::Rgba);
    let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
    let frame = reader.next_frame(&mut buf).unwrap();
    (info.width, info.height, buf[..frame.buffer_size()].to_vec())
}

fn premul(c: u8, a: u8) -> u8 {
    ((c as u16 * a as u16 + 127) / 255) as u8
}

#[test]
fn opaque_rgba_equals_decoded_png() {
    // On a fully opaque image, premultiplied == straight, so the raw RGBA bytes
    // and the decoded PNG bytes are identical.
    let (w, h) = (64u32, 48u32);
    let rgba = render_rgba(OPAQUE_FILL, w, h);
    let png = take(render(OPAQUE_FILL, 0.0, "{}", w, h)).unwrap();
    let (pw, ph, decoded) = decode_png_rgba(&png);
    assert_eq!((pw, ph), (w, h));
    assert!(
        rgba.chunks_exact(4).all(|px| px[3] == 255),
        "fixture must be opaque"
    );
    assert_eq!(rgba, decoded);
}

#[test]
fn translucent_rgba_is_premultiplied() {
    let (w, h) = (64u32, 48u32);
    let rgba = render_rgba(TRANSLUCENT_FILL, w, h);
    let png = take(render(TRANSLUCENT_FILL, 0.0, "{}", w, h)).unwrap();
    let (_, _, decoded) = decode_png_rgba(&png);

    // rgba is premultiplied; the PNG is straight alpha. Re-premultiplying the
    // decoded pixels must reproduce the raw bytes (within round-trip rounding).
    let mut saw_translucent = false;
    for (raw, straight) in rgba.chunks_exact(4).zip(decoded.chunks_exact(4)) {
        let a = straight[3];
        if (1..255).contains(&a) {
            saw_translucent = true;
            for ch in 0..3 {
                assert!(raw[ch] <= a, "premultiplied channel must be <= alpha");
                let expected = premul(straight[ch], a);
                assert!(
                    (raw[ch] as i32 - expected as i32).abs() <= 1,
                    "premul mismatch: raw {} vs expected {}",
                    raw[ch],
                    expected
                );
            }
        }
    }
    assert!(saw_translucent, "fixture must have translucent pixels");
}

// --- fuzz-lite: hostile inputs never abort ------------------------------------------

#[test]
fn hostile_inputs_error_but_never_abort() {
    register_font();
    let big = format!(r#"{{"junk":"{}"}}"#, "x".repeat(1_000_000));
    let deep = format!("{}{}", "[".repeat(5000), "]".repeat(5000));
    let huge_array = format!(r#"{{"inputs":[{}]}}"#, "0,".repeat(50_000));
    let hostile: Vec<&str> = vec![
        "",
        "   ",
        "not json at all",
        "{",
        "[1,2,3",
        "\"unterminated",
        "{\"version\":}",
        r#"{"version":1e999}"#,
        r#"{"version":1,"size":{"width":NaN,"height":10}}"#,
        r#"{"version":1,"size":{"width":Infinity,"height":10}}"#,
        "null",
        "true",
        "3.14",
        &big,
        &deep,
        &huge_array,
    ];
    // Every extern fn taking JSON must return the sentinel (never abort) — the
    // catch_unwind contract. We only assert we get here alive.
    for doc in &hostile {
        assert!(take(render(doc, 0.5, "{}", 32, 32)).is_none());
        assert!(take(render(doc, 0.5, doc, 32, 32)).is_none() || !doc.is_empty());
        let d = CString::new(*doc).unwrap_or_else(|_| CString::new("\\x00").unwrap());
        assert!(take(kine_render_document_rgba(
            d.as_ptr(),
            0.0,
            std::ptr::null(),
            32,
            32
        ))
        .is_none());
        assert!(probe(doc).is_none());
        assert_eq!(create(doc), 0);
    }

    // Invalid UTF-8 (0xFF) NUL-terminated, straight at the boundary.
    let bad_utf8: [std::ffi::c_char; 4] = [-1, -2, -3, 0];
    assert!(take(kine_render_document(
        bad_utf8.as_ptr(),
        0.0,
        std::ptr::null(),
        32,
        32
    ))
    .is_none());
    assert!(kine_document_create(bad_utf8.as_ptr()) == 0);
    assert!(last_error().contains("UTF-8"));
}

// --- per-feature goldens ------------------------------------------------------------

#[test]
fn feature_goldens() {
    register_font();
    // Enumerated so a new feature without a golden is a visible missing row.
    let features: &[(&str, &str)] = &[
        (
            "linear_gradient",
            include_str!("../tests/fixtures/features/linear_gradient.json"),
        ),
        (
            "radial_gradient",
            include_str!("../tests/fixtures/features/radial_gradient.json"),
        ),
        (
            "stroke_caps_joins",
            include_str!("../tests/fixtures/features/stroke_caps_joins.json"),
        ),
        (
            "opacity_layer",
            include_str!("../tests/fixtures/features/opacity_layer.json"),
        ),
        (
            "shadow",
            include_str!("../tests/fixtures/features/shadow.json"),
        ),
        ("pill", include_str!("../tests/fixtures/features/pill.json")),
        (
            "activefill",
            include_str!("../tests/fixtures/features/activefill.json"),
        ),
        (
            "glyph_transform",
            include_str!("../tests/fixtures/features/glyph_transform.json"),
        ),
    ];
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/features");
    let regen = std::env::var_os("KINE_REGEN_GOLDENS").is_some();
    if regen {
        std::fs::create_dir_all(&dir).unwrap();
    }
    for (name, doc) in features {
        // Render at the fixture's declared size, with fixed signals so the frame
        // is deterministic; fixtures without a `progress` input ignore it.
        let size: serde_json::Value = serde_json::from_str(doc).unwrap();
        let w = size["size"]["width"].as_u64().unwrap() as u32;
        let h = size["size"]["height"].as_u64().unwrap() as u32;
        let png = take(render(doc, 0.0, r#"{"progress":0.6}"#, w, h))
            .unwrap_or_else(|| panic!("{name} render failed: {}", last_error()));
        let path = dir.join(format!("{name}.png"));
        if regen {
            std::fs::write(&path, &png).unwrap();
            continue;
        }
        let golden = std::fs::read(&path)
            .unwrap_or_else(|_| panic!("{name} golden missing — run KINE_REGEN_GOLDENS=1"));
        assert_golden(name, &png, &golden);
    }
}

// --- evaluator: remaining gaps ------------------------------------------------------

#[test]
fn amount_lerps_toward_the_composed_value() {
    // amount = 0.5 halves the applied delta.
    assert_eq!(eval::composite_num(2.0, 10.0, Composite::Replace, 0.5), 6.0);
    assert_eq!(eval::composite_num(2.0, 10.0, Composite::Add, 0.5), 7.0);
    assert_eq!(eval::composite_num(4.0, 2.0, Composite::Multiply, 0.5), 6.0);
    assert_eq!(eval::composite_num(5.0, 99.0, Composite::Replace, 0.0), 5.0);
}

#[test]
fn stagger_index_mode_uses_declared_order() {
    let stagger = RStagger {
        driver_u: 0.5,
        total: Some(0.5),
        each: None,
        from: StaggerFrom::Index,
        ease: None,
        seed: 0,
        indices: vec![2, 0, 1],
    };
    let weights = eval::stagger_weights(&stagger, 3);
    // index 0 has rank 2 (latest), index 1 rank 0 (earliest) → weight[1] leads.
    assert!(weights[1] >= weights[2] && weights[2] >= weights[0]);
}

#[test]
fn activation_arrays_empty_short_long() {
    let doc = crate::schema::parse(TEST_CARD).unwrap();
    let compiled = crate::validate::validate(doc).unwrap();
    let cases = [
        (r#"{"activations":[]}"#, vec![]),
        (r#"{"activations":[0.5]}"#, vec![0.5]),
        (r#"{"activations":[1,2,3]}"#, vec![1.0, 1.0, 1.0]),
    ];
    for (json, expected) in cases {
        let signals: serde_json::Value = serde_json::from_str(json).unwrap();
        let values = eval::resolve_inputs(&compiled.doc, &signals, 0.0).unwrap();
        match &values["activations"] {
            eval::InputValue::UnitArray(a) => assert_eq!(*a, expected, "for {json}"),
            other => panic!("wrong type: {other:?}"),
        }
    }
}

#[test]
fn time_wraps_exactly_at_period_multiples() {
    register_font();
    // The card's wave has period 2; t=0, 2, 4 must be pixel-identical.
    let a = take(render(TEST_CARD, 0.0, "{}", 96, 96)).unwrap();
    let b = take(render(TEST_CARD, 2.0, "{}", 96, 96)).unwrap();
    let c = take(render(TEST_CARD, 4.0, "{}", 96, 96)).unwrap();
    assert_eq!(a, b);
    assert_eq!(b, c);
}

// --- colors (§3) --------------------------------------------------------------

const COLORS_DERIVATION: &str = include_str!("../tests/fixtures/colors_derivation.json");

#[test]
fn color_alpha_is_absolute() {
    let red = crate::validate::parse_color("#FF0000").unwrap();
    let out = eval::with_alpha(red, 0.5);
    assert!((out.components[3] - 0.5).abs() < 1e-6, "alpha replaced");
    assert!(
        (out.components[0] - red.components[0]).abs() < 1e-6,
        "rgb unchanged"
    );
    // Absolute, not multiplicative — a second alpha overwrites, doesn't compound.
    let out2 = eval::with_alpha(out, 0.2);
    assert!((out2.components[3] - 0.2).abs() < 1e-6);
}

#[test]
fn color_contrast_picks_farthest_lightness_ties_earlier() {
    use color::{AlphaColor, Oklab};
    let black = crate::validate::parse_color("#000000").unwrap();
    let white = crate::validate::parse_color("#FFFFFF").unwrap();
    let dark = crate::validate::parse_color("#141414").unwrap();
    let light = crate::validate::parse_color("#EDEDED").unwrap();
    assert_eq!(
        eval::contrast_pick(dark, &[black, white]).components,
        white.components
    );
    assert_eq!(
        eval::contrast_pick(light, &[black, white]).components,
        black.components
    );

    // Genuine tie: two candidates at equal Oklab lightness → the earlier wins.
    let c1 = AlphaColor::<Oklab>::new([0.6, 0.10, 0.0, 1.0]).convert::<color::Srgb>();
    let c2 = AlphaColor::<Oklab>::new([0.6, -0.10, 0.0, 1.0]).convert::<color::Srgb>();
    let base = AlphaColor::<Oklab>::new([0.9, 0.0, 0.0, 1.0]).convert::<color::Srgb>();
    assert_eq!(
        eval::contrast_pick(base, &[c1, c2]).components,
        c1.components
    );
}

#[test]
fn color_mix_endpoints_and_midpoint() {
    let a = crate::validate::parse_color("#000000").unwrap();
    let b = crate::validate::parse_color("#FFFFFF").unwrap();
    let space = InterpSpace::default();
    let at0 = eval::lerp_color(a, b, 0.0, space);
    let at1 = eval::lerp_color(a, b, 1.0, space);
    for i in 0..4 {
        assert!(
            (at0.components[i] - a.components[i]).abs() < 1e-4,
            "t=0 is a"
        );
        assert!(
            (at1.components[i] - b.components[i]).abs() < 1e-4,
            "t=1 is b"
        );
    }
    let mid = eval::lerp_color(a, b, 0.5, space);
    assert!(
        mid.components[0] > 0.05 && mid.components[0] < 0.95,
        "midpoint between"
    );
}

#[test]
fn probe_omits_the_private_color_table() {
    let interface = probe(TEST_CARD).unwrap();
    assert!(
        interface.get("colors").is_none(),
        "color table must stay private"
    );
    let keys: Vec<&str> = interface["inputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    // Seeds ARE the published interface.
    assert!(keys.contains(&"foreground") && keys.contains(&"accent"));
}

#[test]
fn colors_derivation_responds_to_seeds() {
    register_font();
    let reseeded = r##"{"foreground":"#101014","background":"#F5F0E8","accent":"#12B886"}"##;
    let default = take(render(COLORS_DERIVATION, 0.0, "{}", 240, 120)).unwrap();
    let reseed = take(render(COLORS_DERIVATION, 0.0, reseeded, 240, 120)).unwrap();
    assert!(default.starts_with(PNG_SIGNATURE));
    assert_ne!(default, reseed, "reseeding must change the derived colors");

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/features");
    let regen = std::env::var_os("KINE_REGEN_GOLDENS").is_some();
    for (name, png) in [
        ("derive_default.png", &default),
        ("derive_reseeded.png", &reseed),
    ] {
        let path = dir.join(name);
        if regen {
            std::fs::write(&path, png).unwrap();
            continue;
        }
        let golden = std::fs::read(&path).unwrap_or_else(|_| panic!("{name} missing"));
        assert_golden(name, png, &golden);
    }
}

#[test]
fn color_override_wins_only_when_supplied() {
    // The card's `border` = contrast(plateTop) with override borderColor.
    // Supplying borderColor changes the border; not supplying keeps the derived
    // value even though the input has a default.
    register_font();
    let base = take(render(TEST_CARD, 0.0, r#"{"progress":0.3}"#, 128, 128)).unwrap();
    let overridden = take(render(
        TEST_CARD,
        0.0,
        r##"{"progress":0.3,"borderColor":"#12B886"}"##,
        128,
        128,
    ))
    .unwrap();
    assert_ne!(base, overridden, "supplied override must change the render");
}

#[test]
fn render_area_is_capped_not_allocated() {
    // 65535 passes the per-dimension u16 guard, but 65535x65535 is ~17GB — the
    // total-area cap must reject it BEFORE allocating (this used to hang/OOM the
    // process). Rejection is instant; if this test ever hangs, the cap regressed.
    let buf = render(MINIMAL, 0.0, "{}", 65535, 65535);
    assert!(
        take(buf).is_none(),
        "huge area must be rejected, not allocated"
    );
    assert!(last_error().contains("exceeds"), "got: {:?}", last_error());
    // Just over the cap is rejected without allocating (8192*8193 > 8192^2); a
    // normal size still renders. (We avoid rendering at the 256 MB cap here so we
    // don't pollute the process-RSS leak test that runs concurrently.)
    assert!(
        take(render(MINIMAL, 0.0, "{}", 8192, 8193)).is_none(),
        "just over the cap must reject"
    );
    assert!(
        take(render(MINIMAL, 0.0, "{}", 512, 512)).is_some(),
        "normal size renders"
    );
}

// --- image assets (§4/§5) ---------------------------------------------------
//
// Fixtures are generated in-process by the `image` encoders (deterministic, no
// checked-in binaries) and round-tripped back through kine's own decode path.

use base64::Engine as _;
use image::codecs::gif::{GifEncoder, Repeat};
use image::codecs::png::PngEncoder;
use image::{Delay, ExtendedColorType, Frame, ImageEncoder, Rgba, RgbaImage};

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn solid(w: u32, h: u32, rgba: [u8; 4]) -> RgbaImage {
    RgbaImage::from_pixel(w, h, Rgba(rgba))
}

/// TL red, TR green, BL blue, BR white — an orientation-revealing test image.
fn quadrants(size: u32) -> RgbaImage {
    let half = size / 2;
    let mut img = RgbaImage::new(size, size);
    for y in 0..size {
        for x in 0..size {
            let c = match (x < half, y < half) {
                (true, true) => [220, 20, 20, 255],
                (false, true) => [20, 200, 20, 255],
                (true, false) => [20, 20, 220, 255],
                (false, false) => [240, 240, 240, 255],
            };
            img.put_pixel(x, y, Rgba(c));
        }
    }
    img
}

fn png_b64(img: &RgbaImage) -> String {
    let mut bytes = Vec::new();
    PngEncoder::new(&mut bytes)
        .write_image(
            img.as_raw(),
            img.width(),
            img.height(),
            ExtendedColorType::Rgba8,
        )
        .unwrap();
    b64(&bytes)
}

fn gif_b64(frames: &[RgbaImage], delay_ms: u32) -> String {
    let mut bytes = Vec::new();
    {
        // Fastest quantization (speed 30) — deterministic, and keeps the large
        // decoded-size fixture cheap to encode.
        let mut enc = GifEncoder::new_with_speed(&mut bytes, 30);
        enc.set_repeat(Repeat::Infinite).unwrap();
        for f in frames {
            let frame = Frame::from_parts(f.clone(), 0, 0, Delay::from_numer_denom_ms(delay_ms, 1));
            enc.encode_frame(frame).unwrap();
        }
    }
    b64(&bytes)
}

fn image_doc(w: u32, h: u32, mime: &str, data: &str, node: &str) -> String {
    format!(
        r#"{{"version":1,"size":{{"width":{w},"height":{h}}},"inputs":[{{"key":"time","type":"time","default":0}}],"assets":[{{"key":"img","kind":"image","mime":"{mime}","data":"{data}"}}],"root":{{"kind":"group","key":"root","children":[{node}]}}}}"#
    )
}

fn image_node(fit: &str, x: u32, y: u32, w: u32, h: u32, extra: &str) -> String {
    format!(
        r#"{{"kind":"image","key":"pic","asset":"img","frame":{{"x":{x},"y":{y},"width":{w},"height":{h}}},"fit":"{fit}"{extra}}}"#
    )
}

fn decode(png: &[u8]) -> RgbaImage {
    image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .unwrap()
        .to_rgba8()
}

fn px(img: &RgbaImage, x: u32, y: u32) -> [u8; 4] {
    img.get_pixel(x, y).0
}

fn is_red(p: [u8; 4]) -> bool {
    p[0] > 150 && p[1] < 90 && p[2] < 90
}
fn is_green(p: [u8; 4]) -> bool {
    p[1] > 130 && p[0] < 100 && p[2] < 100
}
fn is_blue(p: [u8; 4]) -> bool {
    p[2] > 150 && p[0] < 90 && p[1] < 90
}

fn render_image_doc(doc: &str, t: f64, w: u32, h: u32) -> Vec<u8> {
    take(render(doc, t, "{}", w, h)).unwrap_or_else(|| panic!("render failed: {}", last_error()))
}

#[test]
fn image_still_renders_into_frame() {
    let data = png_b64(&quadrants(2));
    let doc = image_doc(
        64,
        64,
        "image/png",
        &data,
        &image_node("fill", 0, 0, 64, 64, ""),
    );
    let out = render_image_doc(&doc, 0.0, 64, 64);
    let img = decode(&out);
    assert!(
        is_red(px(&img, 16, 16)),
        "TL should be red, got {:?}",
        px(&img, 16, 16)
    );
    assert!(
        is_green(px(&img, 48, 16)),
        "TR should be green, got {:?}",
        px(&img, 48, 16)
    );
    assert!(
        is_blue(px(&img, 16, 48)),
        "BL should be blue, got {:?}",
        px(&img, 16, 48)
    );
}

#[test]
fn animated_gif_samples_frame_by_time() {
    // Three solid frames, 100ms each → 300ms loop.
    let frames = [
        solid(4, 4, [220, 20, 20, 255]),
        solid(4, 4, [20, 200, 20, 255]),
        solid(4, 4, [20, 20, 220, 255]),
    ];
    let data = gif_b64(&frames, 100);
    let doc = image_doc(
        32,
        32,
        "image/gif",
        &data,
        &image_node("fill", 0, 0, 32, 32, ""),
    );

    let at = |t: f64| px(&decode(&render_image_doc(&doc, t, 32, 32)), 16, 16);
    assert!(is_red(at(0.0)), "t=0 → frame 0 (red), got {:?}", at(0.0));
    assert!(
        is_green(at(0.15)),
        "t=0.15 → frame 1 (green), got {:?}",
        at(0.15)
    );
    assert!(
        is_blue(at(0.25)),
        "t=0.25 → frame 2 (blue), got {:?}",
        at(0.25)
    );
    // t past the loop wraps: 0.35s → 350 mod 300 = 50ms → frame 0.
    assert!(
        is_red(at(0.35)),
        "t=0.35 wraps to frame 0 (red), got {:?}",
        at(0.35)
    );
}

#[test]
fn probe_reports_the_assets_manifest() {
    let still = png_b64(&quadrants(2));
    let anim = gif_b64(
        &[solid(2, 2, [255, 0, 0, 255]), solid(2, 2, [0, 255, 0, 255])],
        100,
    );
    let doc = format!(
        r#"{{"version":1,"size":{{"width":16,"height":16}},"assets":[{{"key":"a","kind":"image","mime":"image/png","data":"{still}"}},{{"key":"b","kind":"image","mime":"image/gif","data":"{anim}"}}],"root":{{"kind":"group","key":"root","children":[]}}}}"#
    );
    let interface = probe(&doc).unwrap_or_else(|| panic!("probe failed: {}", last_error()));
    let assets = interface["assets"].as_array().expect("assets manifest");
    assert_eq!(assets.len(), 2);
    assert_eq!(assets[0]["key"], "a");
    assert_eq!(assets[0]["mime"], "image/png");
    assert_eq!(assets[0]["animated"], false);
    assert_eq!(assets[1]["key"], "b");
    assert_eq!(assets[1]["animated"], true);
}

fn tall() -> RgbaImage {
    // 8 wide x 16 tall: top half red, bottom half blue (aspect ≠ a square frame).
    let mut img = RgbaImage::new(8, 16);
    for y in 0..16 {
        let c = if y < 8 {
            [220, 20, 20, 255]
        } else {
            [20, 20, 220, 255]
        };
        for x in 0..8 {
            img.put_pixel(x, y, Rgba(c));
        }
    }
    img
}

#[test]
fn corner_radius_clips_to_a_circle() {
    let data = png_b64(&quadrants(2));
    // cornerRadius = min(w,h)/2 → a circle inscribed in the 64x64 frame.
    let node = image_node("fill", 0, 0, 64, 64, r#","cornerRadius":32"#);
    let out = render_image_doc(&image_doc(64, 64, "image/png", &data, &node), 0.0, 64, 64);
    let img = decode(&out);
    assert!(
        px(&img, 1, 1)[3] < 30,
        "corner should be clipped transparent, got {:?}",
        px(&img, 1, 1)
    );
    assert!(
        px(&img, 32, 32)[3] > 200,
        "center should be opaque, got {:?}",
        px(&img, 32, 32)
    );
}

#[test]
fn fit_contain_letterboxes_transparent() {
    let data = png_b64(&tall()); // 8x16 into 64x64 → scaled 32x64, centered
    let node = image_node("contain", 0, 0, 64, 64, "");
    let img = decode(&render_image_doc(
        &image_doc(64, 64, "image/png", &data, &node),
        0.0,
        64,
        64,
    ));
    assert!(
        px(&img, 4, 32)[3] == 0,
        "left margin is letterbox (transparent), got {:?}",
        px(&img, 4, 32)
    );
    assert!(
        is_red(px(&img, 32, 16)),
        "top center red, got {:?}",
        px(&img, 32, 16)
    );
    assert!(
        is_blue(px(&img, 32, 48)),
        "bottom center blue, got {:?}",
        px(&img, 32, 48)
    );
}

#[test]
fn fit_cover_fills_the_frame() {
    let data = png_b64(&tall()); // 8x16 into 64x64 → scaled 64x128, cropped, fills
    let node = image_node("cover", 0, 0, 64, 64, "");
    let img = decode(&render_image_doc(
        &image_doc(64, 64, "image/png", &data, &node),
        0.0,
        64,
        64,
    ));
    for (x, y) in [(1, 1), (62, 1), (1, 62), (62, 62)] {
        assert!(
            px(&img, x, y)[3] > 200,
            "cover leaves no transparent corner at ({x},{y}), got {:?}",
            px(&img, x, y)
        );
    }
}

#[test]
fn frames_are_premultiplied() {
    // Straight-alpha red at 50% → premultiplied RGBA output ≈ (128,0,0,128).
    let data = png_b64(&solid(2, 2, [255, 0, 0, 128]));
    let node = image_node("fill", 0, 0, 8, 8, "");
    let bytes = render_rgba(&image_doc(8, 8, "image/png", &data, &node), 8, 8);
    let center = ((4 * 8 + 4) * 4) as usize;
    let p = &bytes[center..center + 4];
    assert!(
        (p[0] as i32 - 128).abs() <= 2,
        "premul red ≈128, got {}",
        p[0]
    );
    assert!(
        p[1] <= 2 && p[2] <= 2,
        "premul g/b ≈0, got {} {}",
        p[1],
        p[2]
    );
    assert!((p[3] as i32 - 128).abs() <= 2, "alpha ≈128, got {}", p[3]);
}

#[test]
fn zero_duration_frames_become_100ms() {
    // Two 0-delay frames → 100ms each (§4) → 200ms loop.
    let data = gif_b64(
        &[
            solid(2, 2, [220, 20, 20, 255]),
            solid(2, 2, [20, 20, 220, 255]),
        ],
        0,
    );
    let doc = image_doc(
        16,
        16,
        "image/gif",
        &data,
        &image_node("fill", 0, 0, 16, 16, ""),
    );
    let at = |t: f64| px(&decode(&render_image_doc(&doc, t, 16, 16)), 8, 8);
    assert!(is_red(at(0.05)), "50ms → frame 0, got {:?}", at(0.05));
    assert!(is_blue(at(0.15)), "150ms → frame 1, got {:?}", at(0.15));
}

// --- §8 validation / budgets ------------------------------------------------

fn asset_doc(mime: &str, data: &str) -> String {
    format!(
        r#"{{"version":1,"size":{{"width":16,"height":16}},"assets":[{{"key":"img","kind":"image","mime":"{mime}","data":"{data}"}}],"root":{{"kind":"group","key":"root","children":[]}}}}"#
    )
}

fn assert_rejected(doc: &str, expected: &str) {
    assert!(
        probe(doc).is_none(),
        "expected validation failure for {expected:?}"
    );
    let error = last_error();
    assert!(
        error.contains(expected),
        "error {error:?} does not contain {expected:?}"
    );
}

#[test]
fn asset_dimensions_over_2048_are_rejected() {
    let data = png_b64(&solid(2049, 1, [255, 0, 0, 255]));
    assert_rejected(&asset_doc("image/png", &data), "exceeds the 2048x2048");
}

#[test]
fn animated_asset_over_120_frames_is_rejected() {
    let frames: Vec<RgbaImage> = (0..121).map(|_| solid(1, 1, [0, 0, 0, 255])).collect();
    let data = gif_b64(&frames, 20);
    assert_rejected(&asset_doc("image/gif", &data), "120-frame");
}

#[test]
fn encoded_asset_over_2mb_is_rejected() {
    // ~2.1 MB after base64-decode; the size guard fires before signature/decode.
    let data = "A".repeat(2_800_000);
    assert_rejected(&asset_doc("image/png", &data), "per-asset limit");
}

#[test]
fn container_signature_must_match_mime() {
    let gif = gif_b64(&[solid(2, 2, [255, 0, 0, 255])], 100);
    assert_rejected(
        &asset_doc("image/png", &gif),
        "does not match declared mime",
    );
}

#[test]
fn malformed_base64_is_rejected() {
    assert_rejected(
        &asset_doc("image/png", "not*valid*base64"),
        "malformed base64",
    );
}

#[test]
fn duplicate_asset_keys_are_rejected() {
    let data = png_b64(&solid(2, 2, [255, 0, 0, 255]));
    let doc = format!(
        r#"{{"version":1,"size":{{"width":16,"height":16}},"assets":[{{"key":"img","kind":"image","mime":"image/png","data":"{data}"}},{{"key":"img","kind":"image","mime":"image/png","data":"{data}"}}],"root":{{"kind":"group","key":"root","children":[]}}}}"#
    );
    assert_rejected(&doc, "duplicate asset key");
}

#[test]
fn image_node_referencing_unknown_asset_is_rejected() {
    let data = png_b64(&solid(2, 2, [255, 0, 0, 255]));
    let node = r#"{"kind":"image","key":"pic","asset":"missing","frame":{"x":0,"y":0,"width":16,"height":16}}"#;
    assert_rejected(
        &image_doc(16, 16, "image/png", &data, node),
        "unknown asset \"missing\"",
    );
}

#[test]
fn image_node_accepts_transform_animators() {
    // A solid-red image at x∈[0,16) shifted +16 by a unit-driven translateX
    // (proves `image` is in the node-transform animator vocabulary and applies).
    let data = png_b64(&solid(2, 2, [220, 20, 20, 255]));
    let doc = format!(
        r#"{{"version":1,"size":{{"width":32,"height":16}},"inputs":[{{"key":"p","type":"unit","default":1}}],"assets":[{{"key":"img","kind":"image","mime":"image/png","data":"{data}"}}],"root":{{"kind":"group","key":"root","children":[{{"kind":"image","key":"pic","asset":"img","frame":{{"x":0,"y":0,"width":16,"height":16}},"fit":"fill"}}]}},"animators":[{{"target":"pic","property":"translateX","driver":"p","from":0,"to":16}}]}}"#
    );
    let img = decode(&render_image_doc(&doc, 0.0, 32, 16));
    assert!(
        is_red(px(&img, 24, 8)),
        "image shifted into x=24, got {:?}",
        px(&img, 24, 8)
    );
    assert!(
        px(&img, 4, 8)[3] == 0,
        "original x=4 now empty, got {:?}",
        px(&img, 4, 8)
    );
}

#[test]
fn animated_asset_handles_do_not_leak() {
    let data = gif_b64(
        &[
            solid(8, 8, [255, 0, 0, 255]),
            solid(8, 8, [0, 255, 0, 255]),
            solid(8, 8, [0, 0, 255, 255]),
        ],
        40,
    );
    let doc = image_doc(
        16,
        16,
        "image/gif",
        &data,
        &image_node("fill", 0, 0, 16, 16, ""),
    );
    let doc_c = CString::new(doc).unwrap();
    for i in 0..200 {
        let handle = kine_document_create(doc_c.as_ptr());
        assert!(handle > 0, "create failed: {}", last_error());
        let _ = take(kine_document_render_rgba(
            handle,
            i as f64 * 0.01,
            std::ptr::null(),
            16,
            16,
        ));
        kine_document_free(handle);
        assert!(
            !crate::handle::contains(handle),
            "handle {handle} (and its decoded frames) still live after free"
        );
    }
}

#[test]
fn malformed_animated_webp_errors_not_aborts() {
    // Valid RIFF/WEBP magic (passes the signature gate) with a garbage animated
    // body. The C-ABI boundary must return an error, never abort — a panic
    // through the iOS staticlib is a process kill. image-webp 0.2.5 fixes the
    // historical ANMF+ALPH+zero-VP8 panic (issue #182); this holds the line.
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&[0x1c, 0, 0, 0]);
    bytes.extend_from_slice(b"WEBP");
    bytes.extend_from_slice(b"VP8X");
    bytes.extend_from_slice(&[0x0a, 0, 0, 0, 0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let data = b64(&bytes);
    assert!(
        probe(&asset_doc("image/webp", &data)).is_none(),
        "malformed webp must be a clean error, not an abort"
    );
}

#[test]
fn decoded_animation_over_32mb_is_rejected() {
    // 3 frames at 2048x2048 = 48 MB decoded > 32 MB; rejected mid-decode with a
    // bounded peak (frames are pulled one at a time).
    let frame = solid(2048, 2048, [10, 20, 30, 255]);
    let data = gif_b64(&[frame.clone(), frame.clone(), frame], 40);
    assert_rejected(&asset_doc("image/gif", &data), "frames x w x h x 4");
}

#[test]
fn image_goldens() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/features");
    let regen = std::env::var_os("KINE_REGEN_GOLDENS").is_some();
    if regen {
        std::fs::create_dir_all(&dir).unwrap();
    }

    let still_doc = image_doc(
        64,
        64,
        "image/png",
        &png_b64(&quadrants(4)),
        &image_node("cover", 0, 0, 64, 64, r#","cornerRadius":32"#),
    );
    let frames = [
        solid(4, 4, [220, 20, 20, 255]),
        solid(4, 4, [20, 200, 20, 255]),
        solid(4, 4, [20, 20, 220, 255]),
    ];
    let anim_doc = image_doc(
        32,
        32,
        "image/gif",
        &gif_b64(&frames, 100),
        &image_node("fill", 0, 0, 32, 32, ""),
    );

    let cases = [
        ("image_still", render_image_doc(&still_doc, 0.0, 64, 64)),
        ("image_anim_t0", render_image_doc(&anim_doc, 0.0, 32, 32)),
        ("image_anim_t1", render_image_doc(&anim_doc, 0.15, 32, 32)),
    ];
    for (name, png) in &cases {
        let path = dir.join(format!("{name}.png"));
        if regen {
            std::fs::write(&path, png).unwrap();
            continue;
        }
        let golden = std::fs::read(&path)
            .unwrap_or_else(|_| panic!("{name} golden missing — run KINE_REGEN_GOLDENS=1"));
        assert_golden(name, png, &golden);
    }
}

// --- animated-WebP compositing regression fixtures --------------------------
//
// Pure Rust cannot ENCODE animated WebP, so these are libwebp-built binaries
// (crate/testdata/webp/regen.sh) decoded back through the normal document path.
// Each guards a blend/dispose class that broke in image-webp 0.2.4 (pinned away
// in Cargo.toml) — a future patch-rev bump that reintroduced the bug fails here.

const WEBP_DISPOSE_GHOST: &[u8] = include_bytes!("../testdata/webp/a_dispose_ghost.webp");
const WEBP_DISPOSE_LOSSY: &[u8] = include_bytes!("../testdata/webp/b_dispose_lossy.webp");
const WEBP_BLEND_MODES: &[u8] = include_bytes!("../testdata/webp/c_blend_modes.webp");

fn webp_doc(bytes: &[u8], w: u32, h: u32) -> String {
    image_doc(
        w,
        h,
        "image/webp",
        &b64(bytes),
        &image_node("fill", 0, 0, w, h, ""),
    )
}

fn close(actual: [u8; 4], expected: [u8; 4], tol: i32) -> bool {
    (0..4).all(|i| (actual[i] as i32 - expected[i] as i32).abs() <= tol)
}

/// libwebp animation "over" in straight alpha (integer), for the expected value.
fn blend_over(src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
    let (sa, da) = (src[3] as i32, dst[3] as i32);
    if sa == 255 {
        return src;
    }
    let dst_factor = da * (255 - sa) / 255;
    let blend_a = sa + dst_factor;
    if blend_a == 0 {
        return [0, 0, 0, 0];
    }
    let ch = |s: u8, d: u8| ((s as i32 * sa + d as i32 * dst_factor) / blend_a) as u8;
    [
        ch(src[0], dst[0]),
        ch(src[1], dst[1]),
        ch(src[2], dst[2]),
        blend_a as u8,
    ]
}

#[test]
fn webp_dispose_background_no_ghosting() {
    let doc = webp_doc(WEBP_DISPOSE_GHOST, 6, 6);
    // Frame 0 (t≈0.05): red 4x4 at (0,0), the rest transparent.
    let f0 = decode(&render_image_doc(&doc, 0.05, 6, 6));
    assert!(
        is_red(px(&f0, 1, 1)),
        "f0 red block, got {:?}",
        px(&f0, 1, 1)
    );
    assert!(
        px(&f0, 5, 5)[3] == 0,
        "f0 outside transparent, got {:?}",
        px(&f0, 5, 5)
    );
    // Frame 1 (t≈0.15): f0 disposed→background (transparent), only blue @(2,2).
    let f1 = decode(&render_image_doc(&doc, 0.15, 6, 6));
    assert!(
        px(&f1, 1, 1)[3] == 0,
        "no ghost: disposed f0 pixel transparent, got {:?}",
        px(&f1, 1, 1)
    );
    assert!(
        is_blue(px(&f1, 4, 4)),
        "f1 blue block, got {:?}",
        px(&f1, 4, 4)
    );
    assert!(
        is_blue(px(&f1, 3, 3)),
        "overlap shows f1 (blue) on top, got {:?}",
        px(&f1, 3, 3)
    );
}

#[test]
fn webp_dispose_then_lossy_no_alpha_frame() {
    let doc = webp_doc(WEBP_DISPOSE_LOSSY, 8, 8);
    // Frame 1 (t≈0.15): f0 (full red) disposed→transparent; lossy green 4x4 @(2,2).
    let f1 = decode(&render_image_doc(&doc, 0.15, 8, 8));
    assert!(
        px(&f1, 0, 0)[3] == 0,
        "disposed full-frame region cleared, got {:?}",
        px(&f1, 0, 0)
    );
    let g = px(&f1, 4, 4);
    assert!(
        g[1] > 180 && g[0] < 90 && g[2] < 90 && g[3] > 240,
        "lossy green frame, got {:?}",
        g
    );
    assert!(
        px(&f1, 7, 7)[3] == 0,
        "neighbour untouched (transparent), got {:?}",
        px(&f1, 7, 7)
    );
    // Frame 0 (t≈0.05): full red.
    let f0 = decode(&render_image_doc(&doc, 0.05, 8, 8));
    assert!(
        is_red(px(&f0, 4, 4)),
        "f0 full red, got {:?}",
        px(&f0, 4, 4)
    );
}

#[test]
fn webp_no_blend_vs_alpha_blend() {
    let doc = webp_doc(WEBP_BLEND_MODES, 8, 8);
    // Frame 2 (t≈0.25): f0 red@128 canvas; f1 NO_BLEND green@200 @(0,0,4,4);
    // f2 ALPHA_BLEND blue@128 @(0,4,4,4) over the red@128 beneath it.
    let f2 = decode(&render_image_doc(&doc, 0.25, 8, 8));
    // NO_BLEND replaces canvas pixels outright (ignores the red@128 beneath).
    assert!(
        close(px(&f2, 2, 2), [0, 220, 0, 200], 2),
        "no-blend replace = green@200, got {:?}",
        px(&f2, 2, 2)
    );
    // Untouched canvas keeps frame 0.
    assert!(
        close(px(&f2, 6, 2), [255, 0, 0, 128], 2),
        "untouched red@128, got {:?}",
        px(&f2, 6, 2)
    );
    // ALPHA_BLEND: blue@128 over red@128 (libwebp straight-alpha "over").
    let expected = blend_over([0, 0, 255, 128], [255, 0, 0, 128]);
    assert!(
        close(px(&f2, 2, 6), expected, 2),
        "alpha-blend over red@128 ≈ {expected:?}, got {:?}",
        px(&f2, 2, 6)
    );
}

#[test]
fn decoded_animation_at_exactly_32mb_is_accepted() {
    // 2 frames of 2048x2048 = 2*2048*2048*4 = 33554432 = exactly 32 MiB. The cap
    // is `> MAX`, so the boundary value is accepted (guards a `>`→`>=` regression).
    let frame = solid(2048, 2048, [10, 20, 30, 255]);
    let data = gif_b64(&[frame.clone(), frame], 40);
    let doc = image_doc(
        2048,
        2048,
        "image/gif",
        &data,
        &image_node("fill", 0, 0, 2048, 2048, ""),
    );
    assert!(
        probe(&doc).is_some(),
        "exactly-32MiB decoded must be accepted; err: {}",
        last_error()
    );
}
