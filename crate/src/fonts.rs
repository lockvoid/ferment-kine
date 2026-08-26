//! Global font collection. Bundled fonts only — fontique's `system` feature is
//! off, so nothing is ever pulled from the host OS; the only fonts that exist
//! are the embedded default plus the ones registered through
//! `kine_register_font`. This is what lets every host render identically from
//! the same registered fonts.
//!
//! Two built-in behaviors ride registration:
//!  - **Inter is embedded** (`assets/Inter.ttf`, OFL) and registered at
//!    collection init — a document naming "Inter" renders on every host with
//!    zero setup, and the "no fonts registered" failure class is gone.
//!  - Any registered family whose name contains "emoji" is mapped to
//!    fontique's `GenericFamily::Emoji`. Parley queries that generic
//!    automatically for emoji clusters (shape-time fallback), so hosts get
//!    color-emoji cascade by simply registering a system emoji font
//!    (Apple Color Emoji / Noto Color Emoji).

use std::sync::{Arc, OnceLock, RwLock};

use fontique::{Blob, Collection, CollectionOptions, GenericFamily, SourceCache};
use parley::FontContext;

/// Inter variable (upstream Google Fonts build, OFL — `assets/Inter-OFL.txt`).
/// The engine's guaranteed default family.
const EMBEDDED_INTER: &[u8] = include_bytes!("../assets/Inter.ttf");

struct Registry {
    collection: Collection,
    /// Registered family names, in registration order. The phase-0 test card
    /// renders with the first one.
    families: Vec<String>,
}

static REGISTRY: OnceLock<RwLock<Registry>> = OnceLock::new();

fn registry() -> &'static RwLock<Registry> {
    REGISTRY.get_or_init(|| {
        let mut reg = Registry {
            collection: Collection::new(CollectionOptions {
                shared: false,
                system_fonts: false,
            }),
            families: Vec::new(),
        };
        // Embedded default. Known-good bytes — a failure here would be a
        // build corruption; surface it through the log sink, never a panic
        // from library init.
        if let Err(error) = register_into(&mut reg, EMBEDDED_INTER) {
            crate::log::emit(
                crate::log::ERROR,
                &format!("embedded Inter failed to register: {error}"),
            );
        }
        RwLock::new(reg)
    })
}

/// Register font bytes. Returns the number of families discovered (0 = the data
/// held no usable fonts). Re-registering the same bytes is harmless — fontique
/// keys registered faces by their data.
pub fn register(bytes: &[u8]) -> Result<usize, String> {
    let mut reg = registry()
        .write()
        .map_err(|_| "font registry lock poisoned".to_string())?;
    register_into(&mut reg, bytes)
}

fn register_into(reg: &mut Registry, bytes: &[u8]) -> Result<usize, String> {
    let blob = Blob::new(Arc::new(bytes.to_vec()));
    let registered = reg.collection.register_fonts(blob, None);
    if registered.is_empty() {
        return Err("no fonts found in the provided data".to_string());
    }

    let mut count = 0;
    for (family_id, _) in registered {
        if let Some(name) = reg.collection.family_name(family_id) {
            let name = name.to_string();
            // Announce the NAME-TABLE family — the key documents must resolve
            // by. A host whose catalog name differs sees the mismatch here.
            crate::log::emit(
                crate::log::INFO,
                &format!("registered font family \"{name}\""),
            );
            // Emoji-capable families become the shape-time emoji fallback:
            // parley queries `GenericFamily::Emoji` for emoji clusters on
            // its own — the mapping is all it needs. Name-based detection
            // covers every platform emoji font (Apple Color Emoji, Noto
            // Color Emoji, Segoe UI Emoji).
            if name.to_lowercase().contains("emoji") {
                reg.collection
                    .append_generic_families(GenericFamily::Emoji, std::iter::once(family_id));
                crate::log::emit(
                    crate::log::INFO,
                    &format!("mapped \"{name}\" as the emoji fallback family"),
                );
            }
            if !reg.families.contains(&name) {
                reg.families.push(name);
            }
            count += 1;
        }
    }
    Ok(count)
}

/// Of `families`, the ones the registered collection cannot serve — the
/// probe's `missingFonts`. Resolution matches the render path
/// (`collection.family_id` by name); an empty registry reports every family
/// missing. Order follows the input.
pub fn missing(families: &[String]) -> Vec<String> {
    let Some(mut ctx) = context() else {
        return families.to_vec();
    };
    families
        .iter()
        .filter(|family| ctx.collection.family_id(family).is_none())
        .cloned()
        .collect()
}

/// A `FontContext` seeded with the registered collection. `None` when no font
/// has been registered yet. Families are resolved by name from the document.
pub fn context() -> Option<FontContext> {
    let reg = registry().read().ok()?;
    if reg.families.is_empty() {
        return None;
    }
    Some(FontContext {
        collection: reg.collection.clone(),
        source_cache: SourceCache::default(),
    })
}
