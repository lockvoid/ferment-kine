//! `kine` — render a motion document from the command line, either raster
//! flavor. The behavioral probe: which flavor refuses a document it cannot
//! render honestly (an unregistered font family), and which one paints anyway.
//!
//!     kine <doc.json> [--backend cpu|gpu] [--font <path>]... [--fallback <family>]
//!          [-t <seconds>] [--signals <json>] [--size WxH] [-o <out.png>]
//!
//! Exit 0: rendered (path + byte count printed). Exit 1: the flavor refused —
//! the error prints verbatim. Exit 2: usage / IO trouble.

use std::process::exit;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut doc_path: Option<String> = None;
    let mut backend = "cpu".to_string();
    let mut fonts: Vec<String> = Vec::new();
    let mut t = 0.0f64;
    let mut signals = String::new();
    let mut size: Option<(u32, u32)> = None;
    let mut out_path: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--backend" => backend = expect(args.next(), "--backend needs cpu|gpu"),
            "--font" => fonts.push(expect(args.next(), "--font needs a path")),
            "--fallback" => {
                let family = expect(args.next(), "--fallback needs a family name");
                if let Err(error) = kine::oneshot::set_fallback_family(&family) {
                    eprintln!("{error}");
                    exit(2)
                }
                eprintln!("fallback family: {family}");
            }
            "-t" => {
                t = expect(args.next(), "-t needs seconds")
                    .parse()
                    .unwrap_or_else(|_| usage("-t needs a number"))
            }
            "--signals" => signals = expect(args.next(), "--signals needs json"),
            "--size" => {
                let raw = expect(args.next(), "--size needs WxH");
                let (w, h) = raw.split_once('x').unwrap_or_else(|| usage("--size needs WxH"));
                size = Some((
                    w.parse().unwrap_or_else(|_| usage("--size needs WxH")),
                    h.parse().unwrap_or_else(|_| usage("--size needs WxH")),
                ));
            }
            "-o" => out_path = Some(expect(args.next(), "-o needs a path")),
            other if doc_path.is_none() && !other.starts_with('-') => {
                doc_path = Some(other.to_string())
            }
            other => usage(&format!("unknown argument {other}")),
        }
    }

    let doc_path = doc_path.unwrap_or_else(|| usage("missing <doc.json>"));
    let doc = std::fs::read_to_string(&doc_path).unwrap_or_else(|e| {
        eprintln!("cannot read {doc_path}: {e}");
        exit(2)
    });

    for path in &fonts {
        let bytes = std::fs::read(path).unwrap_or_else(|e| {
            eprintln!("cannot read font {path}: {e}");
            exit(2)
        });
        match kine::oneshot::register_font(&bytes) {
            Ok(count) => eprintln!("registered {count} face(s) from {path}"),
            Err(error) => {
                eprintln!("font registration failed for {path}: {error}");
                exit(2)
            }
        }
    }

    let (width, height) = size.unwrap_or_else(|| doc_size(&doc));
    let result = match backend.as_str() {
        "cpu" => kine::oneshot::render_png_cpu(&doc, t, &signals, width, height),
        "gpu" => render_gpu(&doc, t, &signals, width, height),
        other => usage(&format!("unknown backend {other} (cpu|gpu)")),
    };

    match result {
        Ok(png) => {
            let out = out_path.unwrap_or_else(|| format!("{doc_path}.{backend}.png"));
            std::fs::write(&out, &png).unwrap_or_else(|e| {
                eprintln!("cannot write {out}: {e}");
                exit(2)
            });
            println!("{backend}: rendered {width}x{height} -> {out} ({} bytes)", png.len());
        }
        Err(error) => {
            eprintln!("{backend}: RENDER ERROR: {error}");
            exit(1);
        }
    }
}

#[cfg(feature = "gpu")]
fn render_gpu(doc: &str, t: f64, signals: &str, width: u32, height: u32) -> Result<Vec<u8>, String> {
    kine::oneshot::render_png_gpu(doc, t, signals, width, height)
}

#[cfg(not(feature = "gpu"))]
fn render_gpu(_: &str, _: f64, _: &str, _: u32, _: u32) -> Result<Vec<u8>, String> {
    eprintln!("built without the `gpu` feature — rebuild with --features gpu");
    exit(2)
}

/// The document's own canvas, when --size is not given.
fn doc_size(doc: &str) -> (u32, u32) {
    let parsed: serde_json::Value = serde_json::from_str(doc).unwrap_or(serde_json::Value::Null);
    (
        parsed["size"]["width"].as_u64().unwrap_or(512) as u32,
        parsed["size"]["height"].as_u64().unwrap_or(512) as u32,
    )
}

fn expect(value: Option<String>, message: &str) -> String {
    value.unwrap_or_else(|| usage(message))
}

fn usage(message: &str) -> ! {
    eprintln!("kine: {message}");
    eprintln!("usage: kine <doc.json> [--backend cpu|gpu] [--font <path>]... [-t <sec>] [--signals <json>] [--size WxH] [-o <out.png>]");
    exit(2)
}
