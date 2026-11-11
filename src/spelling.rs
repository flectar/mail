//! Spell checking for the messages being written.
//!
//! Each platform checks with what it has. On Linux that is the Hunspell dictionaries the
//! system has installed, which `flectar_spell` reads without keeping any of them in the app's
//! memory between checks. Windows, macOS and iOS have spell checkers of their own: Windows and
//! macOS check in a system process, iOS in the app. On Android the host brings the spell
//! checker the user chose in the system's settings, usually the keyboard's, which checks in
//! its own process.
//!
//! Nothing is set up until a message is checked, and the last message to close lets go of it
//! all again, so reading mail costs nothing.

#[cfg(linux_desktop)]
mod hunspell;
#[cfg(target_os = "macos")]
mod macos;
pub(crate) mod names;
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

/// Whether the app has a checker of its own for this platform. A host may bring one too.
#[cfg(desktop)]
const CHECKS_SPELLING: bool = cfg!(any(
    linux_desktop,
    target_os = "windows",
    target_os = "macos"
));
#[cfg(mobile)]
const CHECKS_SPELLING: bool = crate::platform::CHECKS_SPELLING;

/// Whose spell checker this platform uses, for what Settings says about it.
#[cfg(desktop)]
const SPELLING_SYSTEM: &str = if cfg!(linux_desktop) {
    "hunspell"
} else if cfg!(target_os = "windows") {
    "windows"
} else if cfg!(target_os = "macos") {
    "macos"
} else {
    ""
};
#[cfg(mobile)]
const SPELLING_SYSTEM: &str = crate::platform::NAME;

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

/// A spell checker the host brings, for a platform the app has none of its own for: Android's,
/// which the user chooses in the system's settings and which checks in its own process. The
/// system picks the language.
pub trait SpellingHost: Send + Sync + 'static {
    /// Whether the system checks spelling for apps now, for what Settings says. Called off the
    /// UI thread.
    fn available(&self) -> bool;
    /// Start checking, for a message's first check. Called on the UI thread, so the work waits
    /// for the session's calls. Dropping the session, once the last message closes, lets go of
    /// the system's checker.
    fn open(&self) -> Option<Box<dyn SpellingSession>>;
}

/// The host's spell checker while messages are being written. Calls block, and are made off the
/// UI thread.
pub trait SpellingSession: Send + Sync {
    /// The UTF-16 ranges of the misspelled words in `text`.
    fn misspelled(&self, text: &str) -> Result<Vec<Range<usize>>, String>;
    /// Corrections for `word`, the likeliest first.
    fn suggest(&self, word: &str) -> Result<Vec<String>, String>;
    /// Offer to add `word` to the dictionary the system's apps share.
    fn add_word(&self, word: &str) -> Result<(), String>;
}

/// The host's spell checker, checked by the same rules as the others.
struct HostChecker(Box<dyn SpellingSession>);

impl Checker for HostChecker {
    fn languages(&self) -> Vec<Language> {
        Vec::new()
    }

    fn picks_languages(&self) -> bool {
        true
    }

    fn check(
        &self,
        text: &str,
        _languages: &[String],
        skip: &[Range<usize>],
        ignored: &[String],
    ) -> Result<Vec<Range<usize>>, String> {
        let checked = checked_text(text, skip);
        let mut flagged = Vec::new();
        for (piece, place) in checked_pieces(&checked) {
            let found = self.0.misspelled(&checked[piece])?;
            flagged.extend(
                found
                    .into_iter()
                    .map(|range| range.start + place..range.end + place),
            );
        }
        Ok(system_misspellings(text, &[flagged], skip, ignored))
    }

    fn suggest(&self, word: &str, _languages: &[String]) -> Result<Vec<String>, String> {
        self.0.suggest(word)
    }

    fn add_word(&self, word: &str, _language: &str) -> Result<(), String> {
        self.0.add_word(word)
    }
}

/// Spell checking as the user set it up, shared by every message being written.
pub(crate) struct Spelling {
    /// Where the checker keeps what it can rebuild, such as Linux's dictionary indexes.
    cache_dir: PathBuf,
    runtime: tokio::runtime::Handle,
    /// The host's spell checker, where the app has none of its own.
    host: Option<Arc<dyn SpellingHost>>,
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
/// app's cache, and `host` the host's spell checker, if it brings one.
pub(crate) fn install(
    runtime: &tokio::runtime::Handle,
    cache_dir: &Path,
    host: Option<Arc<dyn SpellingHost>>,
) {
    let spelling = Spelling {
        cache_dir: cache_dir.join("spelling"),
        runtime: runtime.clone(),
        host,
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

#[cfg(mobile)]
fn platform_checker(_cache_dir: &Path) -> Option<Arc<dyn Checker>> {
    crate::platform::spell_checker()
}

#[cfg(all(
    desktop,
    not(any(linux_desktop, target_os = "windows", target_os = "macos"))
))]
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

/// iOS always has languages to check; elsewhere the host says.
#[cfg(mobile)]
fn has_dictionaries() -> bool {
    CHECKS_SPELLING
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

/// Settings appeared: find out whether there's a language to check with, as installed now, or
/// whether the host's spell checker is turned on, and pass it to `found`. Looking takes a
/// moment on Windows, so it happens off the UI thread.
pub(crate) fn settings_shown(found: impl FnOnce(bool) + Send + 'static) {
    let Some(spelling) = service().filter(|spelling| spelling.available()) else {
        return;
    };
    let host = spelling.host.clone();
    spelling.runtime.spawn_blocking(move || {
        found(match host {
            Some(host) => host.available(),
            None => has_dictionaries(),
        })
    });
}

impl Spelling {
    /// Whether this platform can check spelling at all.
    pub(crate) fn available(&self) -> bool {
        CHECKS_SPELLING || self.host.is_some()
    }

    /// The checker, built the first time a message needs it.
    fn checker(&self) -> Option<Arc<dyn Checker>> {
        if !self.available() {
            return None;
        }
        let mut checker = self.checker.borrow_mut();
        if checker.is_none() {
            *checker = match &self.host {
                Some(host) => host
                    .open()
                    .map(|session| Arc::new(HostChecker(session)) as Arc<dyn Checker>),
                None => platform_checker(&self.cache_dir),
            };
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
        None => system_languages(installed),
    }
}

/// The installed languages matching the system's languages, as checked when the user chose none.
pub(crate) fn system_languages(installed: &[Language]) -> Vec<String> {
    default_languages(installed, &sys_locale::get_locales().collect::<Vec<_>>())
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

/// The text a system checker gets: `text` with the parts not to check blanked out, so quotes
/// and the signature stay in the app, while every other character keeps its UTF-16 place.
pub(crate) fn checked_text(text: &str, skip: &[Range<usize>]) -> String {
    let mut checked = String::with_capacity(text.len());
    for (byte, character) in text.char_indices() {
        if character == '\n' || !skip.iter().any(|range| range.contains(&byte)) {
            checked.push(character);
        } else {
            checked.extend(std::iter::repeat_n(' ', character.len_utf16()));
        }
    }
    checked
}

/// The most UTF-16 units a system checker gets at once where checks go a part at a time: on
/// iOS a turn of the main thread, on Android a request to the spell checker's process.
const PIECE_UNITS: usize = 2048;

/// `text` in pieces of at most `most` UTF-16 units that keep words whole. A piece ends after its
/// last line break, or else after its last space, and only a run without either is cut where it
/// reaches `most`.
fn pieces(text: &str, most: usize) -> Vec<Range<usize>> {
    let mut pieces = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let mut units = 0;
        let mut end = text.len();
        let (mut line_break, mut space) = (None, None);
        for (offset, character) in text[start..].char_indices() {
            units += character.len_utf16();
            if units > most {
                end = line_break.or(space).unwrap_or(start + offset);
                break;
            }
            let after = start + offset + character.len_utf8();
            if character == '\n' {
                line_break = Some(after);
            } else if character.is_whitespace() {
                space = Some(after);
            }
        }
        if end == start {
            // Not even one character fits; it goes alone.
            end += text[start..].chars().next().map_or(1, char::len_utf8);
        }
        pieces.push(start..end);
        start = end;
    }
    pieces
}

/// The pieces of `checked` with something in them to check, each with its place in UTF-16
/// units.
pub(crate) fn checked_pieces(checked: &str) -> Vec<(Range<usize>, usize)> {
    let (mut place, mut counted) = (0, 0);
    let mut found = Vec::new();
    for piece in pieces(checked, PIECE_UNITS) {
        place += checked[counted..piece.start].encode_utf16().count();
        counted = piece.start;
        if !checked[piece.clone()].trim().is_empty() {
            found.push((piece, place));
        }
    }
    found
}

/// The words a system checker flagged in `text`, as the other checkers would report them. Each
/// of `flagged` lists one language's misspellings as UTF-16 ranges, and a word counts when
/// every language flags it. Only the words `checkable_words` picks are kept, outside `skip` and
/// not `ignored`, so addresses, acronyms, quotes and code are left alone on every platform.
pub(crate) fn system_misspellings(
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
        install(runtime.handle(), cache.path(), None);
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

    #[test]
    fn system_checkers_get_no_quotes_and_the_same_places() {
        let text = "Grüße 😀 teh\n│ Quoted teh line\nrecieve";
        let quote = text.find('│').unwrap()..text.find("recieve").unwrap();
        let checked = checked_text(text, std::slice::from_ref(&quote));
        let blank = " ".repeat("│ Quoted teh line".encode_utf16().count());
        assert_eq!(checked, format!("Grüße 😀 teh\n{blank}\nrecieve"));
        assert_eq!(checked.encode_utf16().count(), text.encode_utf16().count());
        let place = |text: &str| text[..text.find("recieve").unwrap()].encode_utf16().count();
        assert_eq!(place(&checked), place(text));
    }

    #[test]
    fn long_text_goes_to_the_system_in_whole_words() {
        let line = "Grüße 😀 aus dem Büro, bis morgen.\n";
        let text = format!("{}{}{}", line.repeat(3), "wort ".repeat(9), "x".repeat(30));
        let most = 24;
        let found = pieces(&text, most);
        assert_eq!(found.first().map(|piece| piece.start), Some(0));
        assert_eq!(found.last().map(|piece| piece.end), Some(text.len()));
        for pair in found.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
        }
        for piece in &found {
            assert!(text[piece.clone()].encode_utf16().count() <= most);
        }
        let parts: Vec<&str> = found.iter().map(|piece| &text[piece.clone()]).collect();
        // A line too long for a piece ends after a space, the rest after the line break, and
        // only the run without a space is cut.
        assert_eq!(
            parts,
            [
                "Grüße 😀 aus dem Büro, ",
                "bis morgen.\n",
                "Grüße 😀 aus dem Büro, ",
                "bis morgen.\n",
                "Grüße 😀 aus dem Büro, ",
                "bis morgen.\n",
                "wort wort wort wort ",
                "wort wort wort wort ",
                "wort ",
                "xxxxxxxxxxxxxxxxxxxxxxxx",
                "xxxxxx",
            ]
        );

        let checked = format!("{}\n{}", " ".repeat(10), "teh");
        let places = checked_pieces(&checked);
        assert_eq!(places, [(0..checked.len(), 0)]);
    }

    /// A host's spell checker that flags "teh" and says what it was given.
    struct TestHost {
        opened: Arc<std::sync::atomic::AtomicUsize>,
        closed: Arc<std::sync::atomic::AtomicUsize>,
        texts: Arc<std::sync::Mutex<Vec<String>>>,
    }

    struct TestSession {
        closed: Arc<std::sync::atomic::AtomicUsize>,
        texts: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl SpellingHost for TestHost {
        fn available(&self) -> bool {
            true
        }

        fn open(&self) -> Option<Box<dyn SpellingSession>> {
            self.opened
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(Box::new(TestSession {
                closed: self.closed.clone(),
                texts: self.texts.clone(),
            }))
        }
    }

    impl SpellingSession for TestSession {
        fn misspelled(&self, text: &str) -> Result<Vec<Range<usize>>, String> {
            self.texts.lock().unwrap().push(text.to_owned());
            let utf16: Vec<u16> = text.encode_utf16().collect();
            let teh: Vec<u16> = "teh".encode_utf16().collect();
            Ok(utf16
                .windows(teh.len())
                .enumerate()
                .filter(|(_, window)| *window == teh.as_slice())
                .map(|(start, _)| start..start + teh.len())
                .collect())
        }

        fn suggest(&self, _word: &str) -> Result<Vec<String>, String> {
            Ok(vec!["the".into()])
        }

        fn add_word(&self, _word: &str) -> Result<(), String> {
            Ok(())
        }
    }

    impl Drop for TestSession {
        fn drop(&mut self) {
            self.closed
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn a_hosts_checker_opens_for_the_first_check_and_closes_with_the_last_message() {
        use std::sync::atomic::Ordering;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let cache = tempfile::tempdir().unwrap();
        let host = TestHost {
            opened: Default::default(),
            closed: Default::default(),
            texts: Default::default(),
        };
        let (opened, closed, texts) =
            (host.opened.clone(), host.closed.clone(), host.texts.clone());
        install(runtime.handle(), cache.path(), Some(Arc::new(host)));
        let spelling = service().unwrap();
        apply_settings(true, None);
        assert!(spelling.available());
        assert_eq!(opened.load(Ordering::SeqCst), 0);

        // The system picks the language, so the menu lists none.
        assert!(spelling.picks_languages());
        let text = "Grüße teh\n│ Quoted teh\nteh";
        let quote = text.find('│').unwrap()..text.rfind("teh").unwrap();
        let task = spelling
            .check(text.into(), vec![quote], Vec::new(), true)
            .unwrap();
        let checked = runtime.block_on(task).unwrap().unwrap();
        let words: Vec<Range<usize>> = checked.found;
        let first = text.find("teh").unwrap();
        let last = text.rfind("teh").unwrap();
        assert_eq!(words, [first..first + 3, last..last + 3]);
        assert_eq!(checked.installed, Some(Vec::new()));
        // The quote never left the app.
        assert!(!texts.lock().unwrap()[0].contains("Quoted"));
        assert_eq!(opened.load(Ordering::SeqCst), 1);

        let suggestions = runtime.block_on(spelling.suggest("teh".into()).unwrap());
        assert_eq!(suggestions.unwrap().unwrap(), ["the"]);
        spelling.release();
        assert_eq!(closed.load(Ordering::SeqCst), 1);
    }
}
