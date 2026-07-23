//! Global font collection. Bundled fonts only — fontique's `system` feature is
//! off, so nothing is ever pulled from the host OS; the only fonts that exist
//! are the ones registered through `kine_register_font`. This is what lets the
//! every host renders identically from the same registered fonts.

use std::sync::{Arc, OnceLock, RwLock};

use fontique::{Blob, Collection, CollectionOptions, SourceCache};
use parley::FontContext;

struct Registry {
    collection: Collection,
    /// Registered family names, in registration order. The phase-0 test card
    /// renders with the first one.
    families: Vec<String>,
}

static REGISTRY: OnceLock<RwLock<Registry>> = OnceLock::new();

fn registry() -> &'static RwLock<Registry> {
    REGISTRY.get_or_init(|| {
        RwLock::new(Registry {
            collection: Collection::new(CollectionOptions {
                shared: false,
                system_fonts: false,
            }),
            families: Vec::new(),
        })
    })
}

/// Register font bytes. Returns the number of families discovered (0 = the data
/// held no usable fonts). Re-registering the same bytes is harmless — fontique
/// keys registered faces by their data.
pub fn register(bytes: &[u8]) -> Result<usize, String> {
    let blob = Blob::new(Arc::new(bytes.to_vec()));
    let mut reg = registry()
        .write()
        .map_err(|_| "font registry lock poisoned".to_string())?;

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
            if !reg.families.contains(&name) {
                reg.families.push(name);
            }
            count += 1;
        }
    }
    Ok(count)
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
