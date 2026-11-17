//! Font fallback for Han ideographs, ordered by language.
//!
//! Chinese, Japanese and Korean share the Han code points, but each region's
//! fonts cover a different set of them: Japanese fonts lack Simplified-only
//! forms such as 暂 and 务. fontique asks the system for a single family per
//! script and, for text without a language, the system answers with the font
//! of its own language. On a Japanese Mac the only Han fallback was Hiragino,
//! so Simplified Chinese text lost those characters (#304).
//!
//! Browsers keep a list of CJK languages and try the font of each in turn:
//! the content language, then the user's and the system's languages, then a
//! fixed order that puts Chinese first because its fonts cover the most
//! characters (Firefox `gfxPlatformFontList::AppendCJKPrefLangs` with
//! `font.cjk_pref_fallback_order`, Chromium `LayoutLocale::LocaleForHan`).
//! This module builds the same list from the family the system picks for each
//! language, so the UI and mail bodies fall back through all of them. Each
//! family is only opened when the ones before it lack a character.
//!
//! # When fontique changes
//!
//! Check this module on every parley/fontique (or Slint) upgrade. Upstream,
//! linebender/parley#689 makes fontique return the system's ordered list per
//! script and language and look up single characters it still lacks.
//!
//! - Once fontique returns several families per key (CoreText cascade list,
//!   DirectWrite and Fontconfig per character), missing characters are fixed
//!   there. Keep this module only for the order: Slint never passes the UI
//!   language to parley, so text without a language would follow the system
//!   language again. Drop it entirely when Slint lays out text in the UI
//!   language as well.
//! - `use_traditional_heiti` goes when fontique picks Heiti TC for Traditional
//!   Chinese itself, or when skrifa reads `hvgl` outlines and fontique stops
//!   dropping PingFang (googlefonts/fontations#1369, linebender/parley#929).
//! - `warm_up` goes with `HanFonts::discover`: it only makes the same CoreText
//!   lookups earlier.
//! - fontique files Simplified Chinese and text without a language under one
//!   key, so mail marked `zh-CN` follows the UI order. When it keeps them
//!   apart, give `zh-CN` its own key in `HanFonts::install`.
//! - `ComposeFallback` is about cosmic-text, not fontique: it stays while the
//!   composer uses cosmic-text.

use parley::fontique::{Collection, FallbackKey, FamilyId, Language, Script};
use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

const HAN: Script = Script::from_bytes(*b"Hani");

/// A language whose fonts draw Han ideographs in their own way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HanLanguage {
    Simplified,
    HongKong,
    Traditional,
    Japanese,
    Korean,
}

impl HanLanguage {
    /// Firefox's default `font.cjk_pref_fallback_order`, tried after the
    /// languages the user prefers.
    const FALLBACK_ORDER: [Self; 5] = [
        Self::Simplified,
        Self::HongKong,
        Self::Traditional,
        Self::Japanese,
        Self::Korean,
    ];

    fn index(self) -> usize {
        self as usize
    }

    fn tag(self) -> &'static str {
        match self {
            Self::Simplified => "zh-CN",
            Self::HongKong => "zh-HK",
            Self::Traditional => "zh-TW",
            Self::Japanese => "ja",
            Self::Korean => "ko",
        }
    }

    /// The CJK language a BCP 47 or POSIX locale names, such as `ja-JP`,
    /// `zh-Hant-HK` or `zh_TW.UTF-8`.
    pub(crate) fn from_locale(locale: &str) -> Option<Self> {
        let mut subtags = locale
            .split(['-', '_', '.', '@'])
            .filter(|part| !part.is_empty());
        let language = subtags.next()?;
        if language.eq_ignore_ascii_case("ja") {
            return Some(Self::Japanese);
        }
        if language.eq_ignore_ascii_case("ko") {
            return Some(Self::Korean);
        }
        if !language.eq_ignore_ascii_case("zh") {
            return None;
        }
        let mut script = None;
        let mut region = None;
        if let Some(subtag) = subtags.next() {
            if subtag.len() == 4 {
                script = Some(subtag.to_ascii_lowercase());
                region = subtags.next().map(str::to_ascii_lowercase);
            } else {
                region = Some(subtag.to_ascii_lowercase());
            }
        }
        let hong_kong = matches!(region.as_deref(), Some("hk" | "mo"));
        Some(match script.as_deref() {
            Some("hans") => Self::Simplified,
            Some("hant") if hong_kong => Self::HongKong,
            Some("hant") => Self::Traditional,
            _ if hong_kong => Self::HongKong,
            _ if region.as_deref() == Some("tw") => Self::Traditional,
            _ => Self::Simplified,
        })
    }
}

/// CJK languages in the order their fonts are tried: those named by
/// `preferred` (the app's language, then the system's), then the rest in
/// Firefox's default order.
pub(crate) fn han_order<'a>(preferred: impl IntoIterator<Item = &'a str>) -> Vec<HanLanguage> {
    let mut order = Vec::with_capacity(HanLanguage::FALLBACK_ORDER.len());
    let named = preferred.into_iter().filter_map(HanLanguage::from_locale);
    for language in named.chain(HanLanguage::FALLBACK_ORDER) {
        if !order.contains(&language) {
            order.push(language);
        }
    }
    order
}

/// The family the system picks for Han text in each CJK language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct HanFonts {
    families: [Option<FamilyId>; 5],
}

impl HanFonts {
    /// Ask the collection's system backend for each language's Han family.
    ///
    /// Run it before any text is laid out with `collection`. fontique files
    /// Simplified Chinese and text without a language under one key and
    /// keeps the first answer, so after a layout the Simplified Chinese
    /// family would be the system language's.
    pub(crate) fn discover(collection: &mut Collection) -> Self {
        let mut fonts = Self::default();
        for language in HanLanguage::FALLBACK_ORDER {
            let Ok(tag) = Language::parse(language.tag()) else {
                continue;
            };
            fonts.families[language.index()] = collection
                .fallback_families(FallbackKey::new(HAN, Some(&tag)))
                .next();
        }
        fonts.use_traditional_heiti(collection);
        fonts
    }

    /// fontique can't read PingFang's outlines, so on macOS it substitutes
    /// Heiti SC for every Chinese language: it picks the substitute by
    /// script, and the script of Han text is never Traditional. Heiti TC is
    /// installed alongside and draws the Traditional forms.
    #[cfg(target_vendor = "apple")]
    fn use_traditional_heiti(&mut self, collection: &mut Collection) {
        let Some(heiti_sc) = collection.family_id("Heiti SC") else {
            return;
        };
        let Some(heiti_tc) = collection.family_id("Heiti TC") else {
            return;
        };
        for language in [HanLanguage::HongKong, HanLanguage::Traditional] {
            let family = &mut self.families[language.index()];
            if *family == Some(heiti_sc) {
                *family = Some(heiti_tc);
            }
        }
    }

    #[cfg(not(target_vendor = "apple"))]
    fn use_traditional_heiti(&mut self, _collection: &mut Collection) {}

    fn family(&self, language: HanLanguage) -> Option<FamilyId> {
        self.families[language.index()]
    }

    /// Fall back through every language's family: text without a language
    /// (or in Simplified Chinese, which fontique files with it) in `order`,
    /// text in another CJK language through its own family first. Returns
    /// the families text without a language falls back to.
    pub(crate) fn install(
        &self,
        collection: &mut Collection,
        order: &[HanLanguage],
    ) -> Vec<FamilyId> {
        let chain: Vec<FamilyId> = order
            .iter()
            .filter_map(|language| self.family(*language))
            .fold(Vec::new(), push_new);
        if chain.is_empty() {
            return chain;
        }
        // Keep families someone else added, such as the platform faces
        // OpenHarmony hands over, after the chain.
        let default_key = FallbackKey::new(HAN, None);
        let added: Vec<FamilyId> = collection.fallback_families(default_key).collect();
        let default_chain = chain.into_iter().chain(added).fold(Vec::new(), push_new);
        collection.set_fallbacks(default_key, default_chain.iter().copied());

        let keys = [
            ("zh-HK", HanLanguage::HongKong),
            ("zh-MO", HanLanguage::HongKong),
            ("zh-TW", HanLanguage::Traditional),
            ("zh-SG", HanLanguage::Simplified),
            ("ja", HanLanguage::Japanese),
            ("ko", HanLanguage::Korean),
        ];
        for (tag, language) in keys {
            let Ok(tag) = Language::parse(tag) else {
                continue;
            };
            let families = self
                .family(language)
                .into_iter()
                .chain(default_chain.iter().copied())
                .fold(Vec::new(), push_new);
            collection.set_fallbacks(FallbackKey::new(HAN, Some(&tag)), families.into_iter());
        }
        default_chain
    }
}

fn push_new<T: PartialEq>(mut families: Vec<T>, family: T) -> Vec<T> {
    if !families.contains(&family) {
        families.push(family);
    }
    families
}

/// The app's UI language, once known.
static UI_LANGUAGE: Mutex<Option<String>> = Mutex::new(None);
/// Bumped when the UI language changes the order, so collections built
/// earlier follow it.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn current_order() -> Vec<HanLanguage> {
    let ui = UI_LANGUAGE
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let system: Vec<String> = sys_locale::get_locales().collect();
    han_order(ui.iter().chain(&system).map(String::as_str))
}

/// Make the lookups [`HanFonts::discover`] does on a background thread, so
/// the UI thread finds CoreText's answers cached when it installs the
/// fallback before the first frame. Cold, CoreText reads each language's font
/// for its first answer: on a Mac whose disk cache was emptied the five took
/// 75 to 90 ms. DirectWrite and Fontconfig answer in well under a
/// millisecond, so other systems skip this.
#[cfg(target_vendor = "apple")]
pub(crate) fn warm_up() {
    use objc2_core_foundation::{CFDictionary, CFRange, CFString};
    use objc2_core_text::{CTFont, CTFontDescriptor};
    use parley::fontique::ScriptExt;
    use std::ptr::{null, null_mut};

    let spawned = std::thread::Builder::new()
        .name("han-fonts".into())
        .spawn(|| {
            // CoreText autoreleases, and a new thread has no pool.
            objc2::rc::autoreleasepool(|_| {
                // fontique's lookup: an empty descriptor's font as the base,
                // the script's sample text, the language.
                // SAFETY: an empty dictionary with null callbacks, and a null
                // matrix for the identity transform, as fontique passes them.
                let Some(attributes) =
                    (unsafe { CFDictionary::new(None, null_mut(), null_mut(), 0, null(), null()) })
                else {
                    return;
                };
                let base = unsafe {
                    CTFont::with_font_descriptor(
                        &CTFontDescriptor::with_attributes(&attributes),
                        0.0,
                        null(),
                    )
                };
                let Some(sample) = HAN.sample() else {
                    return;
                };
                let text = CFString::from_str(sample);
                let range = CFRange {
                    location: 0,
                    length: text.length(),
                };
                for language in HanLanguage::FALLBACK_ORDER {
                    let tag = CFString::from_str(language.tag());
                    // SAFETY: the range covers `text`.
                    unsafe { CTFont::for_string_with_language(&base, &text, range, Some(&tag)) };
                }
            });
        });
    // Without the thread the UI thread makes the same lookups itself.
    drop(spawned);
}

#[cfg(not(target_vendor = "apple"))]
pub(crate) fn warm_up() {}

/// A collection whose Han fallback follows the UI language.
#[derive(Clone)]
pub(crate) struct HanFallback {
    fonts: HanFonts,
    generation: u64,
    chain: Vec<FamilyId>,
}

impl HanFallback {
    /// Discover and install. Call it on a collection no text was laid out
    /// with yet.
    pub(crate) fn install(collection: &mut Collection) -> Self {
        let fonts = HanFonts::discover(collection);
        Self::install_discovered(fonts, collection)
    }

    /// Install families [`HanFonts::discover`] found in `collection`.
    pub(crate) fn install_discovered(fonts: HanFonts, collection: &mut Collection) -> Self {
        let generation = GENERATION.load(Ordering::Acquire);
        let chain = fonts.install(collection, &current_order());
        Self {
            fonts,
            generation,
            chain,
        }
    }

    /// Reinstall when the UI language changed the order since the last
    /// install.
    pub(crate) fn follow(&mut self, collection: &mut Collection) {
        let generation = GENERATION.load(Ordering::Acquire);
        if generation != self.generation {
            self.chain = self.fonts.install(collection, &current_order());
            self.generation = generation;
        }
    }
}

thread_local! {
    /// The Slint collection's fallback, on the UI thread.
    static UI_FALLBACK: RefCell<Option<HanFallback>> = const { RefCell::new(None) };
}

/// Install the fallback in the UI's collection, before any text is laid out.
pub(crate) fn install_ui(collection: &mut Collection) {
    let fallback = HanFallback::install(collection);
    remember_ui_names(collection, &fallback.chain);
    UI_FALLBACK.with(|ui| *ui.borrow_mut() = Some(fallback));
}

/// Put the UI language's fonts first. `collection` is the UI's, which is
/// only asked for when the order changed.
pub(crate) fn set_ui_language(language: &str, collection: impl FnOnce() -> Collection) {
    let changed = {
        let mut ui = UI_LANGUAGE
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let previous = ui.as_deref().and_then(HanLanguage::from_locale);
        let changed = previous != HanLanguage::from_locale(language);
        *ui = Some(language.to_owned());
        changed
    };
    if !changed {
        return;
    }
    GENERATION.fetch_add(1, Ordering::AcqRel);
    UI_FALLBACK.with(|ui| {
        if let Some(fallback) = ui.borrow_mut().as_mut() {
            let mut collection = collection();
            fallback.follow(&mut collection);
            remember_ui_names(&mut collection, &fallback.chain);
        }
    });
}

/// The UI's Han fallback families by name, for the composer, whose
/// cosmic-text font database is separate.
static UI_NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());

fn remember_ui_names(collection: &mut Collection, chain: &[FamilyId]) {
    let names = chain
        .iter()
        .filter_map(|family| collection.family_name(*family).map(intern))
        .collect();
    *UI_NAMES.lock().unwrap_or_else(|error| error.into_inner()) = names;
}

/// cosmic-text takes fallback names as `&'static str`. The few Han family
/// names are kept for the life of the process, each once.
fn intern(name: &str) -> &'static str {
    static NAMES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
    let mut names = NAMES.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(known) = names.iter().find(|known| **known == name) {
        return known;
    }
    let leaked: &'static str = Box::leak(name.into());
    names.push(leaked);
    leaked
}

/// cosmic-text's fallback lists for the composer, with the UI's Han
/// families first, so typed CJK text uses the fonts the UI and mail bodies
/// draw it with. cosmic-text picks one family by the system language.
pub(crate) struct ComposeFallback {
    platform: cosmic_text::PlatformFallback,
    han: Vec<&'static str>,
}

impl ComposeFallback {
    pub(crate) fn new(locale: &str) -> Self {
        use cosmic_text::Fallback;
        let platform = cosmic_text::PlatformFallback;
        let ui = UI_NAMES
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let han = ui
            .into_iter()
            .chain(
                platform
                    .script_fallback(unicode_script::Script::Han, locale)
                    .iter()
                    .copied(),
            )
            .fold(Vec::new(), push_new);
        Self { platform, han }
    }
}

impl cosmic_text::Fallback for ComposeFallback {
    fn common_fallback(&self) -> &[&'static str] {
        self.platform.common_fallback()
    }

    fn forbidden_fallback(&self) -> &[&'static str] {
        self.platform.forbidden_fallback()
    }

    fn script_fallback(&self, script: unicode_script::Script, locale: &str) -> &[&'static str] {
        if script == unicode_script::Script::Han {
            &self.han
        } else {
            self.platform.script_fallback(script, locale)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use HanLanguage::*;
    use parley::fontique::{Blob, CollectionOptions, FontInfoOverride};
    use std::sync::Arc;

    #[test]
    fn locales_name_their_cjk_language() {
        let cases = [
            ("ja", Some(Japanese)),
            ("ja-JP", Some(Japanese)),
            ("ja_JP.UTF-8", Some(Japanese)),
            ("ko-KR", Some(Korean)),
            ("zh", Some(Simplified)),
            ("zh_Hans", Some(Simplified)),
            ("zh-Hans-HK", Some(Simplified)),
            ("zh-CN", Some(Simplified)),
            ("zh-SG", Some(Simplified)),
            ("zh-Hant", Some(Traditional)),
            ("zh-TW", Some(Traditional)),
            ("zh_TW.Big5", Some(Traditional)),
            ("zh-Hant-TW", Some(Traditional)),
            ("zh-HK", Some(HongKong)),
            ("zh-Hant-HK", Some(HongKong)),
            ("zh-MO", Some(HongKong)),
            ("en-JP", None),
            ("en", None),
            ("", None),
        ];
        for (locale, expected) in cases {
            assert_eq!(HanLanguage::from_locale(locale), expected, "{locale}");
        }
    }

    #[test]
    fn preferred_languages_come_first_then_chinese_before_japanese() {
        assert_eq!(
            han_order(["en", "es"]),
            vec![Simplified, HongKong, Traditional, Japanese, Korean]
        );
        // The setup in #304: a Simplified Chinese UI on a Japanese system.
        assert_eq!(
            han_order(["zh_Hans", "ja-JP", "en-US"]),
            vec![Simplified, Japanese, HongKong, Traditional, Korean]
        );
        assert_eq!(
            han_order(["en", "ja-JP", "ko-KR", "ja"]),
            vec![Japanese, Korean, Simplified, HongKong, Traditional]
        );
    }

    /// A collection without system fonts holding the bundled UI font under
    /// each of `names`.
    fn collection_with_families(names: &[&str]) -> (Collection, Vec<FamilyId>) {
        let mut collection = Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        });
        let families = names
            .iter()
            .map(|name| {
                let info = FontInfoOverride {
                    family_name: Some(name),
                    ..FontInfoOverride::default()
                };
                collection
                    .register_fonts(
                        Blob::new(Arc::new(crate::compose_editor::UI_FONT_DATA)),
                        Some(info),
                    )
                    .first()
                    .map(|(family, _)| *family)
                    .expect("bundled UI font registers")
            })
            .collect();
        (collection, families)
    }

    fn fallbacks(collection: &mut Collection, tag: Option<&str>) -> Vec<FamilyId> {
        let tag = tag.map(|tag| Language::parse(tag).unwrap());
        collection
            .fallback_families(FallbackKey::new(HAN, tag.as_ref()))
            .collect()
    }

    #[test]
    fn every_key_falls_back_through_every_language() {
        let (mut collection, ids) =
            collection_with_families(&["Han SC", "Han HK", "Han TC", "Han JP", "Han KR"]);
        let [sc, hk, tc, jp, kr] = [ids[0], ids[1], ids[2], ids[3], ids[4]];
        let fonts = HanFonts {
            families: [Some(sc), Some(hk), Some(tc), Some(jp), Some(kr)],
        };
        fonts.install(&mut collection, &han_order(["zh_Hans", "ja-JP"]));

        assert_eq!(fallbacks(&mut collection, None), [sc, jp, hk, tc, kr]);
        assert_eq!(
            fallbacks(&mut collection, Some("zh-CN")),
            [sc, jp, hk, tc, kr]
        );
        assert_eq!(
            fallbacks(&mut collection, Some("zh-SG")),
            [sc, jp, hk, tc, kr]
        );
        assert_eq!(fallbacks(&mut collection, Some("ja")), [jp, sc, hk, tc, kr]);
        assert_eq!(
            fallbacks(&mut collection, Some("zh-TW")),
            [tc, sc, jp, hk, kr]
        );
        assert_eq!(
            fallbacks(&mut collection, Some("zh-MO")),
            [hk, sc, jp, tc, kr]
        );
        assert_eq!(fallbacks(&mut collection, Some("ko")), [kr, sc, jp, hk, tc]);

        // A new UI language reorders the same families.
        fonts.install(&mut collection, &han_order(["en", "ko-KR"]));
        assert_eq!(fallbacks(&mut collection, None), [kr, sc, hk, tc, jp]);
        assert_eq!(fallbacks(&mut collection, Some("ja")), [jp, kr, sc, hk, tc]);
    }

    #[test]
    fn shared_families_and_added_families_appear_once() {
        let (mut collection, ids) = collection_with_families(&["Pan CJK", "Platform Han"]);
        let [pan, platform] = [ids[0], ids[1]];
        collection.set_fallbacks(FallbackKey::new(HAN, None), [platform].into_iter());
        let fonts = HanFonts {
            families: [Some(pan), Some(pan), None, Some(pan), None],
        };
        let chain = fonts.install(&mut collection, &han_order(["en"]));
        assert_eq!(chain, [pan, platform]);
        assert_eq!(fallbacks(&mut collection, None), [pan, platform]);
        assert_eq!(fallbacks(&mut collection, Some("ja")), [pan, platform]);
        assert_eq!(fallbacks(&mut collection, Some("zh-TW")), [pan, platform]);
    }

    #[test]
    fn the_composer_tries_the_ui_families_first_for_han_only() {
        use cosmic_text::Fallback;
        use unicode_script::Script;
        *UI_NAMES.lock().unwrap() = vec![intern("Heiti SC"), intern("Hiragino Sans")];
        let fallback = ComposeFallback::new("ja-JP");
        let platform = cosmic_text::PlatformFallback;
        let han = fallback.script_fallback(Script::Han, "ja-JP");
        assert_eq!(&han[..2], ["Heiti SC", "Hiragino Sans"]);
        for name in platform.script_fallback(Script::Han, "ja-JP") {
            assert_eq!(han.iter().filter(|known| *known == name).count(), 1);
        }
        assert_eq!(
            fallback.script_fallback(Script::Hangul, "ja-JP"),
            platform.script_fallback(Script::Hangul, "ja-JP")
        );
        assert_eq!(fallback.common_fallback(), platform.common_fallback());
        assert!(std::ptr::eq(intern("Heiti SC"), intern("Heiti SC")));
    }

    #[test]
    fn only_a_new_cjk_ui_language_reorders() {
        let generation = GENERATION.load(Ordering::Acquire);
        set_ui_language("en", || unreachable!("no UI collection in tests"));
        set_ui_language("es", || unreachable!());
        assert_eq!(GENERATION.load(Ordering::Acquire), generation);
        set_ui_language("zh_Hans", || unreachable!());
        assert_eq!(GENERATION.load(Ordering::Acquire), generation + 1);
        set_ui_language("zh_Hans", || unreachable!());
        set_ui_language("en", || unreachable!());
        assert_eq!(GENERATION.load(Ordering::Acquire), generation + 2);
    }

    #[test]
    fn without_system_families_the_collection_is_left_alone() {
        let (mut collection, ids) = collection_with_families(&["Platform Han"]);
        let fonts = HanFonts::discover(&mut collection);
        assert_eq!(fonts, HanFonts::default());
        collection.set_fallbacks(FallbackKey::new(HAN, None), [ids[0]].into_iter());
        assert!(
            fonts
                .install(&mut collection, &han_order(["ja"]))
                .is_empty()
        );
        assert_eq!(fallbacks(&mut collection, None), [ids[0]]);
        assert!(fallbacks(&mut collection, Some("ja")).is_empty());
    }
}
