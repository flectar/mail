//! Spell checking for the messages being written.
//!
//! Each platform checks with what it has. On Linux that is the Hunspell dictionaries the
//! system has installed, which `flectar_spell` reads without keeping any of them in the app's
//! memory between checks. The other platforms have no checker here yet, and the composer
//! offers none there.

#[cfg(linux_desktop)]
mod hunspell;
mod names;

#[cfg(linux_desktop)]
pub use hunspell::run_index_worker_if_requested;

use crate::rich_compose::{CODE, ComposeStyleRun, QUOTE_MARKER};
use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

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
    checker: Option<Arc<dyn Checker>>,
    runtime: tokio::runtime::Handle,
    enabled: Cell<bool>,
    /// The languages the user chose, or `None` for the system's.
    chosen: RefCell<Option<Vec<String>>>,
    /// The installed languages, as last listed.
    installed: RefCell<Vec<Language>>,
    /// The system's languages, most preferred first, as BCP 47 tags.
    locales: Vec<String>,
    /// Words added to the personal dictionary since the app started, which pass even before
    /// the dictionary file has them.
    added: RefCell<Vec<String>>,
}

/// Set up spell checking for this process. `cache_dir` holds what the checker can rebuild,
/// such as Linux's dictionary indexes.
pub(crate) fn install(runtime: &tokio::runtime::Handle, cache_dir: &std::path::Path) {
    let checker = platform_checker(cache_dir);
    let spelling = Spelling::new(
        checker,
        runtime.clone(),
        sys_locale::get_locales().collect(),
    );
    SPELLING.with(|slot| *slot.borrow_mut() = Some(Rc::new(spelling)));
}

#[cfg(linux_desktop)]
fn platform_checker(cache_dir: &std::path::Path) -> Option<Arc<dyn Checker>> {
    Some(Arc::new(hunspell::Hunspell::new(
        cache_dir.join("spelling"),
    )))
}

#[cfg(not(linux_desktop))]
fn platform_checker(_cache_dir: &std::path::Path) -> Option<Arc<dyn Checker>> {
    None
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
    spelling.refresh_languages();
    app.set_spelling_available(spelling.available());
    app.set_spelling_enabled(spelling.enabled.get());
    app.set_spelling_installed(!spelling.installed.borrow().is_empty());
}

impl Spelling {
    fn new(
        checker: Option<Arc<dyn Checker>>,
        runtime: tokio::runtime::Handle,
        locales: Vec<String>,
    ) -> Self {
        Self {
            checker,
            runtime,
            enabled: Cell::new(true),
            chosen: RefCell::new(None),
            installed: RefCell::new(Vec::new()),
            locales,
            added: RefCell::new(Vec::new()),
        }
    }

    /// Whether this platform can check spelling at all.
    pub(crate) fn available(&self) -> bool {
        self.checker.is_some()
    }

    /// Whether the user wants spelling checked, where it can be.
    pub(crate) fn enabled(&self) -> bool {
        self.available() && self.enabled.get()
    }

    /// The chosen languages, or the system's when the user chose none.
    pub(crate) fn chosen_languages(&self) -> Option<Vec<String>> {
        self.chosen.borrow().clone()
    }

    /// List the installed languages again, for dictionaries installed or removed since.
    pub(crate) fn refresh_languages(&self) {
        if let Some(checker) = &self.checker {
            *self.installed.borrow_mut() = checker.languages();
        }
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
        let installed = self.installed.borrow();
        match &*self.chosen.borrow() {
            Some(chosen) => chosen
                .iter()
                .filter(|id| installed.iter().any(|language| &language.id == *id))
                .cloned()
                .collect(),
            None => default_languages(&installed, &self.locales),
        }
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

    /// The misspelled words of `text`, found off the UI thread. `None` when spelling isn't
    /// checked.
    pub(crate) fn check(
        &self,
        text: String,
        skip: Vec<Range<usize>>,
        ignored: Vec<String>,
    ) -> Option<tokio::task::JoinHandle<Result<Vec<Range<usize>>, String>>> {
        if !self.enabled() {
            return None;
        }
        let checker = Arc::clone(self.checker.as_ref()?);
        let languages = self.active_languages();
        let mut ignored = ignored;
        ignored.extend(self.added.borrow().iter().cloned());
        Some(self.runtime.spawn_blocking(move || {
            if languages.is_empty() {
                return Ok(Vec::new());
            }
            checker.check(&text, &languages, &skip, &ignored)
        }))
    }

    /// Corrections for `word`, found off the UI thread.
    pub(crate) fn suggest(
        &self,
        word: String,
    ) -> Option<tokio::task::JoinHandle<Result<Vec<String>, String>>> {
        let checker = Arc::clone(self.checker.as_ref()?);
        let languages = self.active_languages();
        Some(
            self.runtime
                .spawn_blocking(move || checker.suggest(&word, &languages)),
        )
    }

    /// Accept `word` from now on, in the first checked language: the one the user writes in
    /// most, as they list it.
    pub(crate) fn add_word(&self, word: String) {
        let (Some(checker), Some(language)) =
            (&self.checker, self.active_languages().first().cloned())
        else {
            return;
        };
        let checker = Arc::clone(checker);
        self.added.borrow_mut().push(word.clone());
        self.runtime.spawn_blocking(move || {
            if let Err(error) = checker.add_word(&word, &language) {
                tracing::warn!(%error, "could not add a word to the personal dictionary");
            }
        });
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
