//! Spell checking for the messages being written.
//!
//! Each platform checks with what it has. On Linux that is the Hunspell dictionaries the
//! system has installed, which `flectar_spell` reads without keeping any of them in the app's
//! memory between checks. Windows and macOS have spell checkers of their own, which check in a
//! system process. The phones have no checker here yet, and the composer offers none there.
//!
//! Nothing is set up until a message is checked, and the last message to close lets go of it
//! all again, so reading mail costs nothing.

#[cfg(linux_desktop)]
mod hunspell;
#[cfg(target_os = "macos")]
mod macos;
mod names;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(linux_desktop)]
pub use hunspell::run_index_worker_if_requested;

use crate::rich_compose::{CODE, ComposeStyleRun, QUOTE_MARKER};
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

/// Whether this platform checks spelling.
const CHECKS_SPELLING: bool = cfg!(any(
    linux_desktop,
    target_os = "windows",
    target_os = "macos"
));

/// Whose spell checker this platform uses, for what Settings says about it.
const SPELLING_SYSTEM: &str = if cfg!(linux_desktop) {
    "hunspell"
} else if cfg!(target_os = "windows") {
    "windows"
} else if cfg!(target_os = "macos") {
    "macos"
} else {
    ""
};

thread_local! {
    static SPELLING: RefCell<Option<Rc<Spelling>>> = const { RefCell::new(None) };
}

/// A language spelling can be checked in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Language {
    /// How the checker and the settings name it, such as "en_US".
    pub(crate) id: String,
    /// Its name in the language itself, such as "English (United States)".
    pub(crate) name: String,
}

/// What checks spelling on this platform. Calls block, so they run off the UI thread.
pub(crate) trait Checker: Send + Sync {
    /// The installed languages, in the order to list them.
    fn languages(&self) -> Vec<Language>;
    /// Whether the system picks the languages itself, from the text and the user's settings,
    /// as macOS does. Such a checker lists none and gets none.
    fn picks_languages(&self) -> bool {
        false
    }
    /// The byte ranges of the words in `text` that every one of `languages` rejects, so a word
    /// in any of them passes. Words overlapping `skip` aren't checked, nor words in `ignored`.
    fn check(
        &self,
        text: &str,
        languages: &[String],
        skip: &[Range<usize>],
        ignored: &[String],
    ) -> Result<Vec<Range<usize>>, String>;
    /// Corrections for `word`, the likeliest first.
    fn suggest(&self, word: &str, languages: &[String]) -> Result<Vec<String>, String>;
    /// Accept `word` in `language` from now on.
    fn add_word(&self, word: &str, language: &str) -> Result<(), String>;
}

/// Spell checking as the user set it up, shared by every message being written.
pub(crate) struct Spelling {
    /// Where the checker keeps what it can rebuild, such as Linux's dictionary indexes.
    cache_dir: PathBuf,
    runtime: tokio::runtime::Handle,
    enabled: Cell<bool>,
    /// The languages the user chose, or `None` for the system's.
    chosen: RefCell<Option<Vec<String>>>,
    /// The checker, from the first check until the last message closes.
    checker: RefCell<Option<Arc<dyn Checker>>>,
    /// The installed languages, as last listed while messages were open.
    installed: RefCell<Vec<Language>>,
    /// Words added to the personal dictionary while messages were open, which pass even before
    /// the dictionary file has them.
    added: RefCell<Vec<String>>,
}

/// Set up spell checking for this process, without building anything yet. `cache_dir` is the
/// app's cache.
pub(crate) fn install(runtime: &tokio::runtime::Handle, cache_dir: &Path) {
    let spelling = Spelling {
        cache_dir: cache_dir.join("spelling"),
        runtime: runtime.clone(),
        enabled: Cell::new(true),
        chosen: RefCell::new(None),
        checker: RefCell::new(None),
        installed: RefCell::new(Vec::new()),
        added: RefCell::new(Vec::new()),
    };
    SPELLING.with(|slot| *slot.borrow_mut() = Some(Rc::new(spelling)));
}

#[cfg(linux_desktop)]
fn platform_checker(cache_dir: &Path) -> Option<Arc<dyn Checker>> {
    Some(Arc::new(hunspell::Hunspell::new(cache_dir.to_owned())))
}

#[cfg(target_os = "windows")]
fn platform_checker(_cache_dir: &Path) -> Option<Arc<dyn Checker>> {
    windows::WindowsSpelling::start().map(|checker| Arc::new(checker) as Arc<dyn Checker>)
}

#[cfg(target_os = "macos")]
fn platform_checker(_cache_dir: &Path) -> Option<Arc<dyn Checker>> {
    Some(Arc::new(macos::MacSpelling))
}

#[cfg(not(any(linux_desktop, target_os = "windows", target_os = "macos")))]
fn platform_checker(_cache_dir: &Path) -> Option<Arc<dyn Checker>> {
    None
}

/// Whether any language can be checked, found without keeping anything.
#[cfg(linux_desktop)]
fn has_dictionaries() -> bool {
    hunspell::has_dictionaries()
}

#[cfg(target_os = "windows")]
fn has_dictionaries() -> bool {
    windows::has_languages()
}

/// Every Mac checks spelling, and asking would start the app's spell checker for good.
#[cfg(target_os = "macos")]
fn has_dictionaries() -> bool {
    true
}

#[cfg(all(
    desktop,
    not(any(linux_desktop, target_os = "windows", target_os = "macos"))
))]
fn has_dictionaries() -> bool {
    false
}

/// Spell checking, once installed.
pub(crate) fn service() -> Option<Rc<Spelling>> {
    SPELLING.with(|slot| slot.borrow().clone())
}

/// The user's spelling settings, as loaded or changed. Open messages are checked again when
/// they change what counts as misspelled.
pub(crate) fn apply_settings(enabled: bool, languages: Option<Vec<String>>) {
    let Some(spelling) = service() else {
        return;
    };
    let changed =
        spelling.enabled.replace(enabled) != enabled || *spelling.chosen.borrow() != languages;
    *spelling.chosen.borrow_mut() = languages;
    if changed {
        crate::composer::spelling_changed();
    }
}

/// Show the spelling settings in the main window's settings.
pub(crate) fn apply_to_window(app: &crate::AppWindow) {
    let Some(spelling) = service() else {
        return;
    };
    app.set_spelling_available(spelling.available());
    app.set_spelling_enabled(spelling.enabled.get());
    app.set_spelling_system(SPELLING_SYSTEM.into());
}

/// Settings appeared: find out whether there's a language to check with, as installed now, and
/// pass it to `found`. Looking takes a moment on Windows, so it happens off the UI thread.
#[cfg(desktop)]
pub(crate) fn settings_shown(found: impl FnOnce(bool) + Send + 'static) {
    let Some(spelling) = service().filter(|spelling| spelling.available()) else {
        return;
    };
    spelling
        .runtime
        .spawn_blocking(move || found(has_dictionaries()));
}

impl Spelling {
    /// Whether this platform can check spelling at all.
    pub(crate) fn available(&self) -> bool {
        CHECKS_SPELLING
    }

    /// The checker, built the first time a message needs it.
    fn checker(&self) -> Option<Arc<dyn Checker>> {
        if !self.available() {
            return None;
        }
        let mut checker = self.checker.borrow_mut();
        if checker.is_none() {
            *checker = platform_checker(&self.cache_dir);
        }
        checker.clone()
    }

    /// Let go of the checker and everything it listed, once no message is being written.
    pub(crate) fn release(&self) {
        *self.checker.borrow_mut() = None;
        *self.installed.borrow_mut() = Vec::new();
        *self.added.borrow_mut() = Vec::new();
    }

    /// Whether the user wants spelling checked, where it can be.
    pub(crate) fn enabled(&self) -> bool {
        self.available() && self.enabled.get()
    }

    /// The chosen languages, or the system's when the user chose none.
    pub(crate) fn chosen_languages(&self) -> Option<Vec<String>> {
        self.chosen.borrow().clone()
    }

    /// Whether the system picks the languages, so there are none to choose here.
    pub(crate) fn picks_languages(&self) -> bool {
        self.checker()
            .is_some_and(|checker| checker.picks_languages())
    }

    /// The installed languages, as a check or the menu listed them again off the UI thread.
    pub(crate) fn set_installed(&self, installed: Vec<Language>) {
        *self.installed.borrow_mut() = installed;
    }

    /// List the installed languages again, off the UI thread: on Windows that asks the spell
    /// checker's own process.
    pub(crate) fn list_languages(&self) -> Option<tokio::task::JoinHandle<Vec<Language>>> {
        let checker = self.checker()?;
        Some(self.runtime.spawn_blocking(move || checker.languages()))
    }

    /// The installed languages, each with whether it is checked.
    pub(crate) fn languages(&self) -> Vec<(Language, bool)> {
        let active = self.active_languages();
        self.installed
            .borrow()
            .iter()
            .map(|language| (language.clone(), active.contains(&language.id)))
            .collect()
    }

    /// The installed languages that are checked: those the user chose, or the ones matching
    /// the system's languages.
    pub(crate) fn active_languages(&self) -> Vec<String> {
        active_languages(&self.installed.borrow(), self.chosen.borrow().as_deref())
    }

    /// The languages that result from turning `id` on or off.
    pub(crate) fn toggled(&self, id: &str) -> Vec<String> {
        let mut active = self.active_languages();
        match active.iter().position(|active| active == id) {
            Some(index) => {
                active.remove(index);
            }
            None => active.push(id.to_owned()),
        }
        active
    }

    /// The misspelled words of `text`, found off the UI thread, which also lists the installed
    /// languages again first when `relist`. `None` when spelling isn't checked.
    pub(crate) fn check(
        &self,
        text: String,
        skip: Vec<Range<usize>>,
        ignored: Vec<String>,
        relist: bool,
    ) -> Option<tokio::task::JoinHandle<Result<Checked, String>>> {
        if !self.enabled() {
            return None;
        }
        let checker = self.checker()?;
        let installed = self.installed.borrow().clone();
        let chosen = self.chosen.borrow().clone();
        let mut ignored = ignored;
        ignored.extend(self.added.borrow().iter().cloned());
        Some(self.runtime.spawn_blocking(move || {
            let installed = if relist {
                checker.languages()
            } else {
                installed
            };
            let languages = active_languages(&installed, chosen.as_deref());
            let found = if languages.is_empty() && !checker.picks_languages() {
                Vec::new()
            } else {
                checker.check(&text, &languages, &skip, &ignored)?
            };
            Ok(Checked {
                installed: relist.then_some(installed),
                found,
            })
        }))
    }

    /// Corrections for `word`, found off the UI thread.
    pub(crate) fn suggest(
        &self,
        word: String,
    ) -> Option<tokio::task::JoinHandle<Result<Vec<String>, String>>> {
        let checker = self.checker()?;
        let languages = self.active_languages();
        Some(
            self.runtime
                .spawn_blocking(move || checker.suggest(&word, &languages)),
        )
    }

    /// Accept `word` from now on, in the first checked language: the one the user writes in
    /// most, as they list it. A system that picks languages itself takes it for all of them.
    pub(crate) fn add_word(&self, word: String) {
        let Some(checker) = self.checker() else {
            return;
        };
        let Some(language) = self
            .active_languages()
            .first()
            .cloned()
            .or_else(|| checker.picks_languages().then(String::new))
        else {
            return;
        };
        self.added.borrow_mut().push(word.clone());
        self.runtime.spawn_blocking(move || {
            if let Err(error) = checker.add_word(&word, &language) {
                tracing::warn!(%error, "could not add a word to the personal dictionary");
            }
        });
    }
}

/// What a check found, and the installed languages when it listed them again.
pub(crate) struct Checked {
    pub(crate) installed: Option<Vec<Language>>,
    pub(crate) found: Vec<Range<usize>>,
}

/// The installed languages that are checked: those `chosen`, or the ones matching the system's
/// languages when the user chose none.
fn active_languages(installed: &[Language], chosen: Option<&[String]>) -> Vec<String> {
    match chosen {
        Some(chosen) => chosen
            .iter()
            .filter(|id| installed.iter().any(|language| &language.id == *id))
            .cloned()
            .collect(),
        None => default_languages(installed, &sys_locale::get_locales().collect::<Vec<_>>()),
    }
}

/// The installed languages matching the system's, one for each of its languages: the same
/// region when installed, otherwise the language's main one.
fn default_languages(installed: &[Language], locales: &[String]) -> Vec<String> {
    let mut chosen: Vec<String> = Vec::new();
    for locale in locales {
        let mut parts = locale.split(['-', '_']);
        let Some(language) = parts.next().filter(|language| {
            (2..=3).contains(&language.len()) && language.bytes().all(|b| b.is_ascii_alphabetic())
        }) else {
            continue;
        };
        let language = language.to_ascii_lowercase();
        let region = parts
            .find(|part| {
                part.len() == 2 || (part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit()))
            })
            .map(str::to_ascii_uppercase);
        let same_language: Vec<&Language> = installed
            .iter()
            .filter(|candidate| names::language_of(&candidate.id) == language)
            .collect();
        let exact = |id: &str| same_language.iter().find(|candidate| candidate.id == id);
        let main_region = names::main_region(&language);
        let best = region
            .as_ref()
            .and_then(|region| exact(&format!("{language}_{region}")))
            .or_else(|| {
                region.as_ref().and_then(|region| {
                    same_language
                        .iter()
                        .find(|candidate| names::region_of(&candidate.id) == Some(region.as_str()))
                })
            })
            .or_else(|| exact(&language))
            .or_else(|| main_region.and_then(|main| exact(&format!("{language}_{main}"))))
            .or_else(|| same_language.first());
        if let Some(best) = best
            && !chosen.contains(&best.id)
        {
            chosen.push(best.id.clone());
        }
    }
    chosen
}

/// The words a system checker flagged in `text`, as the other checkers would report them. Each
/// of `flagged` lists one language's misspellings as UTF-16 ranges, and a word counts when
/// every language flags it. Only the words `checkable_words` picks are kept, outside `skip` and
/// not `ignored`, so addresses, acronyms, quotes and code are left alone on every platform.
#[cfg(any(target_os = "windows", target_os = "macos", all(test, desktop)))]
fn system_misspellings(
    text: &str,
    flagged: &[Vec<Range<usize>>],
    skip: &[Range<usize>],
    ignored: &[String],
) -> Vec<Range<usize>> {
    let mut bytes = Vec::with_capacity(text.len() + 1);
    for (byte, character) in text.char_indices() {
        bytes.extend(std::iter::repeat_n(byte, character.len_utf16()));
    }
    bytes.push(text.len());
    let to_bytes = |range: &Range<usize>| Some(*bytes.get(range.start)?..*bytes.get(range.end)?);
    let mut languages = flagged
        .iter()
        .map(|ranges| ranges.iter().filter_map(to_bytes).collect::<Vec<_>>());
    let Some(first) = languages.next() else {
        return Vec::new();
    };
    let others: Vec<Vec<Range<usize>>> = languages.collect();
    let words = flectar_spell::checkable_words(text);
    first
        .into_iter()
        .filter(|range| range.start < range.end)
        .filter(|range| others.iter().all(|other| other.contains(range)))
        .filter(|range| {
            words
                .iter()
                .any(|word| range.start <= word.start && word.end <= range.end)
        })
        .filter(|range| {
            !skip
                .iter()
                .any(|skipped| skipped.start < range.end && range.start < skipped.end)
        })
        .filter(|range| {
            text.get(range.clone())
                .is_some_and(|word| !ignored.iter().any(|ignored| ignored == word))
        })
        .collect()
}

/// The parts of a message not to check: quoted lines, code, and the signature the composer
/// put in, which Thunderbird leaves out too.
pub(crate) fn unchecked_ranges(
    text: &str,
    styles: &[ComposeStyleRun],
    signature: Option<&str>,
) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        if line.starts_with(QUOTE_MARKER) {
            ranges.push(start..start + line.len());
        }
        start += line.len();
    }
    ranges.extend(
        styles
            .iter()
            .filter(|run| run.style.marks & CODE != 0)
            .map(|run| run.range.clone()),
    );
    if let Some(signature) = signature.filter(|signature| !signature.is_empty())
        && text.matches(signature).count() == 1
        && let Some(start) = text.find(signature)
    {
        ranges.push(start..start + signature.len());
    }
    ranges.sort_by_key(|range| range.start);
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installed(ids: &[&str]) -> Vec<Language> {
        ids.iter()
            .map(|id| Language {
                id: (*id).into(),
                name: (*id).into(),
            })
            .collect()
    }

    fn defaults(ids: &[&str], locales: &[&str]) -> Vec<String> {
        let locales: Vec<String> = locales.iter().map(|locale| (*locale).into()).collect();
        default_languages(&installed(ids), &locales)
    }

    #[test]
    fn nothing_is_built_until_a_message_needs_it_and_the_last_one_lets_go() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let cache = tempfile::tempdir().unwrap();
        install(runtime.handle(), cache.path());
        let spelling = service().unwrap();
        apply_settings(true, Some(vec!["en_US".into()]));
        assert!(spelling.checker.borrow().is_none());
        assert!(spelling.installed.borrow().is_empty());

        assert_eq!(spelling.picks_languages(), cfg!(target_os = "macos"));
        assert_eq!(spelling.checker.borrow().is_some(), CHECKS_SPELLING);
        spelling.release();
        assert!(spelling.checker.borrow().is_none());
        assert!(spelling.installed.borrow().is_empty());
        assert_eq!(spelling.installed.borrow().capacity(), 0);
    }

    #[test]
    fn system_checkers_follow_the_same_word_rules() {
        // UTF-16 ranges, as Windows and macOS report them: "Grüße" is five units, "😀" two.
        let text = "Grüße 😀 Teh www.exmaple.com NASA teh, recieve";
        let utf16 = |word: &str| {
            let start = text.find(word).unwrap();
            let before = text[..start].encode_utf16().count();
            before..before + word.encode_utf16().count()
        };
        let english = vec![
            utf16("Teh"),
            utf16("www.exmaple.com"),
            utf16("NASA"),
            utf16("teh"),
            utf16("recieve"),
        ];
        let german = vec![utf16("Teh"), utf16("teh"), utf16("recieve")];
        let quote = text.find("recieve").unwrap()..text.len();
        let found = system_misspellings(
            text,
            &[english.clone(), german],
            &[quote],
            &["teh".to_owned()],
        );
        let words: Vec<&str> = found.iter().map(|range| &text[range.clone()]).collect();
        // Addresses and capitals aren't checked, the quote is skipped, "teh" is ignored, and
        // a word only one language flags passes.
        assert_eq!(words, ["Teh"]);
        assert!(system_misspellings(text, &[], &[], &[]).is_empty());
        assert_eq!(
            system_misspellings(text, &[english], &[], &[]).len(),
            3,
            "Teh, teh and recieve"
        );
    }

    #[test]
    fn the_system_languages_pick_their_best_installed_dictionaries() {
        let all = [
            "de_AT",
            "de_CH_frami",
            "de_DE",
            "de_DE_frami",
            "en_AU",
            "en_GB",
            "en_US",
            "es_AR",
            "es_ES",
            "es_MX",
            "pt_BR",
            "pt_PT",
            "ru_RU",
        ];
        assert_eq!(defaults(&all, &["en-GB"]), ["en_GB"]);
        assert_eq!(defaults(&all, &["en-IE"]), ["en_US"]);
        assert_eq!(defaults(&all, &["en"]), ["en_US"]);
        assert_eq!(defaults(&all, &["de-DE", "en-US"]), ["de_DE", "en_US"]);
        assert_eq!(defaults(&all, &["de-CH"]), ["de_CH_frami"]);
        assert_eq!(defaults(&all, &["es-419"]), ["es_ES"]);
        assert_eq!(defaults(&all, &["pt"]), ["pt_BR"]);
        assert_eq!(defaults(&all, &["ru-RU", "ru"]), ["ru_RU"]);
        assert_eq!(defaults(&all, &["fr-FR", "C"]), Vec::<String>::new());
        assert_eq!(defaults(&["ca", "ca_valencia"], &["ca-ES"]), ["ca"]);
        assert_eq!(defaults(&["es"], &["es-MX"]), ["es"]);
    }

    #[test]
    fn quotes_code_and_the_inserted_signature_are_left_out() {
        let signature = "\n\n-- \nDean Tirok";
        let text = format!("Thanks for teh notes\n│ Quoted teh line\nRun cargo tset{signature}");
        let mut document = crate::rich_compose::RichComposeDocument::default();
        document.load(&text, 0, 0);
        let code = text.find("cargo").unwrap();
        document.format(
            "code",
            &text,
            code as i32,
            (code + "cargo tset".len()) as i32,
        );
        let ranges = unchecked_ranges(&text, document.style_runs(), Some(signature));
        let parts: Vec<&str> = ranges.iter().map(|range| &text[range.clone()]).collect();
        assert_eq!(parts, ["│ Quoted teh line\n", "cargo tset", signature]);

        // An edited signature is the user's own text again.
        let edited = text.replace("Dean", "Deen");
        assert_eq!(unchecked_ranges(&edited, &[], Some(signature)).len(), 1);
    }
}
