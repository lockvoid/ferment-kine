//! Kine — a Rust render core for motion documents.
//!
//! A motion document is a pure function `(inputs) → scene` (see docs/SCHEMA.md);
//! this crate parses, validates, evaluates and rasterizes it to pixels over the
//! Vello CPU renderer. The only supported surface is the extern-C boundary in
//! [`capi`] (mirrored by `include/kine.h`). The same crate compiles to a
//! `cdylib` for FFI language bindings and to a `staticlib` for native app
//! embedding — one API surface, many hosts. All logic lives in [`render`] and
//! [`fonts`]; `capi` is glue only.

mod assets;
mod bubble;
mod capi;
mod error;
mod eval;
mod fonts;
mod handle;
mod log;
mod render;
mod schema;
mod validate;

pub use capi::*;

#[cfg(test)]
mod tests;
