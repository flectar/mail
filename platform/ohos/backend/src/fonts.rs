//! System font registration for the OHOS backend.
//!
//! OpenHarmony ships no Fontconfig and the fontique patch queue disables its
//! system backend, so the shared fontique collection starts empty. Fonts are
//! registered here from `/system/fonts` (the platform's font directory) and a
//! Han (CJK) fallback is wired so Chinese text renders in the Slint UI and in
//! Blitz email content.

use std::path::Path;
use std::sync::Arc;

use slint::fontique_011::fontique::{Blob, FallbackKey, GenericFamily, Script};

/// The platform font directory as seen from inside an app process.
const SYSTEM_FONT_DIR: &str = "/system/fonts";

/// Chinese faces used for the Han fallback when the family-name scan cannot
/// find them (Simplified first, Traditional second).
const HAN_FONT_FILES: [&str; 2] = [
    "/system/fonts/FZHeiT-SC-Regular.ttf",
    "/system/fonts/FZHeiT-TC-Regular.ttf",
];

/// Registers the platform fonts into Slint's shared fontique collection and
/// appends a Han fallback. Returns the number of Han fallback families.
pub fn register_system_fonts() -> usize {
    let dir = Path::new(SYSTEM_FONT_DIR);
    if !dir.is_dir() {
        return 0;
    }

    let mut collection = slint::fontique_011::shared_collection();
    collection.load_fonts_from_paths([dir]);

    // Find the Chinese faces by family-name fragment; fall back to registering
    // the known font files directly (their family ids come back from
    // `register_fonts`).
    let mut han_families = Vec::new();
    let names: Vec<String> = collection
        .family_names()
        .map(|name| name.to_string())
        .collect();
    for fragment in ["FZHeiT", "HYQiHei", "Source Han", "Noto Sans CJK"] {
        for name in &names {
            if name.contains(fragment)
                && let Some(id) = collection.family_id(name)
                && !han_families.contains(&id)
            {
                han_families.push(id);
            }
        }
        if !han_families.is_empty() {
            break;
        }
    }
    if han_families.is_empty() {
        for path in HAN_FONT_FILES {
            let Ok(bytes) = std::fs::read(path) else {
                continue;
            };
            for (family, _) in collection.register_fonts(Blob::new(Arc::new(bytes)), None) {
                if !han_families.contains(&family) {
                    han_families.push(family);
                }
            }
        }
    }

    // Fontconfig is unavailable on OHOS, so fontique starts with an empty
    // fallback table. Every script that is not Latin must be given the system
    // families explicitly; otherwise only Han text would fall back and Arabic,
    // Hebrew, Thai, Indic, Cyrillic, Greek, Armenian, Georgian, Hangul and
    // Japanese would render as tofu.
    const FALLBACK_SCRIPTS: [&[u8; 4]; 25] = [
        b"Arab", b"Hebr", b"Thai", b"Laoo", b"Khmr", b"Mymr", b"Tibt", b"Deva", b"Beng", b"Guru",
        b"Gujr", b"Orya", b"Taml", b"Telu", b"Knda", b"Mlym", b"Sinh", b"Ethi", b"Armn", b"Geor",
        b"Cyrl", b"Grek", b"Hang", b"Hira", b"Kana",
    ];
    let all_names: Vec<String> = collection
        .family_names()
        .map(|name| name.to_string())
        .collect();
    let all_families: Vec<_> = all_names
        .iter()
        .filter_map(|name| collection.family_id(name))
        .collect();
    if !all_families.is_empty() {
        for script in FALLBACK_SCRIPTS {
            collection.append_fallbacks(
                FallbackKey::new(Script::from_bytes(*script), None),
                all_families.iter().copied(),
            );
        }
        // A generic sans-serif request must not start with the Han face:
        // Latin text (email CSS `font-family: sans-serif`) would otherwise be
        // rendered by a CJK font with Latin glyphs. Prefer the known Latin UI
        // faces, then the remaining non-Han families, and keep Han only as the
        // last fallback below.
        let mut generic_families: Vec<_> = Vec::new();
        for preferred in ["Roboto", "Noto Sans", "HarmonyOS Sans"] {
            for name in &all_names {
                if name.contains(preferred)
                    && let Some(family) = collection.family_id(name)
                    && !generic_families.contains(&family)
                {
                    generic_families.push(family);
                }
            }
        }
        for family in all_families.iter().copied() {
            if !han_families.contains(&family) && !generic_families.contains(&family) {
                generic_families.push(family);
            }
        }
        if !generic_families.is_empty() {
            collection.append_generic_families(
                GenericFamily::SansSerif,
                generic_families.iter().copied(),
            );
        }
        crate::log::info(&format!(
            "fonts: {} families registered for {} non-Latin script fallbacks ({} non-Han sans families)",
            all_families.len(),
            FALLBACK_SCRIPTS.len(),
            generic_families.len()
        ));
    }

    if han_families.is_empty() {
        crate::log::info("fonts: no Han fallback family found");
        return 0;
    }

    // Han script fallback (the pattern documented on `shared_collection`).
    collection.append_fallbacks(
        FallbackKey::new(Script::from_bytes(*b"Hani"), None),
        han_families.iter().copied(),
    );
    // Keep the CJK face reachable for generic sans-serif selections (email CSS
    // `font-family: sans-serif` in Blitz) as the final fallback, after every
    // non-Han family.
    collection.append_generic_families(GenericFamily::SansSerif, han_families.iter().copied());

    han_families.len()
}
