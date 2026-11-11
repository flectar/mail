//! Spelling with Windows' own spell checker. Windows checks in a process of its own, and the
//! words the user adds go to Windows' lists, which every app that checks spelling shares.
//!
//! Its COM objects stay on the thread that made them, so one thread holds them while messages
//! are being written and lets go of them, and of COM, when the last one closes.

use super::{Checker, Language};
use std::collections::HashMap;
use std::ops::Range;
use std::sync::mpsc;
use std::time::Duration;
use windows::Win32::Globalization::{
    CORRECTIVE_ACTION_GET_SUGGESTIONS, CORRECTIVE_ACTION_REPLACE, IEnumSpellingError,
    ISpellChecker, ISpellCheckerFactory, ISpellingError, SpellCheckerFactory,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoFreeUnusedLibrariesEx,
    CoInitializeEx, CoTaskMemFree, CoUninitialize, IEnumString,
};
use windows::core::{HSTRING, PWSTR};

/// Windows answers within the RPC timeouts; this only keeps a stuck call from holding a
/// message's checks forever.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

type Job = Box<dyn FnOnce(Option<&mut Session>) + Send>;

/// The spell checker's objects, on the thread that made them.
struct Session {
    factory: ISpellCheckerFactory,
    checkers: HashMap<String, ISpellChecker>,
}

pub(crate) struct WindowsSpelling {
    jobs: mpsc::Sender<Job>,
}

impl WindowsSpelling {
    /// Start the thread that talks to Windows' spell checker. It ends when this is dropped.
    pub(crate) fn start() -> Option<Self> {
        let (jobs, receiver) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("spelling".into())
            .spawn(move || serve(&receiver))
            .ok()?;
        Some(Self { jobs })
    }

    /// Run `job` with the spell checker's objects and wait for its answer.
    fn run<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Session) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        let (answer, result) = mpsc::sync_channel(1);
        self.jobs
            .send(Box::new(move |session| {
                let _ = answer.send(match session {
                    Some(session) => job(session),
                    None => Err("Windows has no spell checker here".into()),
                });
            }))
            .map_err(|_| "the spell checker stopped".to_owned())?;
        result
            .recv_timeout(ANSWER_TIMEOUT)
            .map_err(|_| "the spell checker didn't answer".to_owned())?
    }
}

/// The spell checker's thread: joins the multithreaded apartment, as Microsoft's spell checker
/// sample does, makes the factory, and runs jobs until the checker is dropped.
fn serve(jobs: &mpsc::Receiver<Job>) {
    // SAFETY: called once on this thread, before any other COM use on it.
    let joined = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    if let Err(error) = joined.ok() {
        tracing::warn!(%error, "spelling: could not start COM");
        while let Ok(job) = jobs.recv() {
            job(None);
        }
        return;
    }
    // SAFETY: COM is initialized on this thread, and the class id is Windows' own.
    let factory: windows::core::Result<ISpellCheckerFactory> =
        unsafe { CoCreateInstance(&SpellCheckerFactory, None, CLSCTX_INPROC_SERVER) };
    match factory {
        Ok(factory) => {
            let mut session = Session {
                factory,
                checkers: HashMap::new(),
            };
            while let Ok(job) = jobs.recv() {
                job(Some(&mut session));
            }
            // The objects go before COM does on this thread.
            drop(session);
        }
        Err(error) => {
            tracing::warn!(%error, "spelling: Windows has no spell checker");
            while let Ok(job) = jobs.recv() {
                job(None);
            }
        }
    }
    // The spell checker's library goes with the objects, rather than after COM's usual delay.
    // SAFETY: plain COM housekeeping on a thread in the apartment.
    unsafe { CoFreeUnusedLibrariesEx(0, None) };
    // SAFETY: balances the successful CoInitializeEx above, on the same thread.
    unsafe { CoUninitialize() };
}

impl Session {
    fn checker(&mut self, language: &str) -> Result<&ISpellChecker, String> {
        if !self.checkers.contains_key(language) {
            // SAFETY: the factory is used on the thread that made it.
            let checker = unsafe { self.factory.CreateSpellChecker(&HSTRING::from(language)) }
                .map_err(|error| error.to_string())?;
            self.checkers.insert(language.to_owned(), checker);
        }
        Ok(&self.checkers[language])
    }

    fn languages(&self) -> Result<Vec<String>, String> {
        // SAFETY: the factory is used on the thread that made it.
        let tags =
            unsafe { self.factory.SupportedLanguages() }.map_err(|error| error.to_string())?;
        Ok(strings(&tags))
    }

    /// The UTF-16 ranges `language` flags in `text`, as Chromium counts them: words with
    /// corrections, not repeated words.
    fn misspelled(&mut self, language: &str, text: &HSTRING) -> Result<Vec<Range<usize>>, String> {
        let checker = self.checker(language)?;
        // SAFETY: the checker is used on the thread that made it, and the text outlives the
        // call.
        let errors: IEnumSpellingError =
            unsafe { checker.ComprehensiveCheck(text) }.map_err(|error| error.to_string())?;
        let mut ranges = Vec::new();
        loop {
            let mut error: Option<ISpellingError> = None;
            // SAFETY: the enumerator writes one error into `error`, or none at the end.
            let next = unsafe { errors.Next(&mut error) };
            let Some(error) = error.filter(|_| next.is_ok()) else {
                break;
            };
            // SAFETY: reads the error's own fields.
            let fields = unsafe { (error.StartIndex(), error.Length(), error.CorrectiveAction()) };
            let (Ok(start), Ok(length), Ok(action)) = fields else {
                continue;
            };
            if action == CORRECTIVE_ACTION_GET_SUGGESTIONS || action == CORRECTIVE_ACTION_REPLACE {
                let start = start as usize;
                ranges.push(start..start + length as usize);
            }
        }
        Ok(ranges)
    }
}

/// Every string an enumerator gives, freeing each as COM asks.
fn strings(enumerator: &IEnumString) -> Vec<String> {
    let mut found = Vec::new();
    loop {
        let mut item = [PWSTR::null()];
        let mut fetched = 0;
        // SAFETY: the enumerator writes at most one string into `item`.
        let next = unsafe { enumerator.Next(&mut item, Some(&mut fetched)) };
        if next.is_err() || fetched == 0 || item[0].is_null() {
            break;
        }
        // SAFETY: the string is valid until it is freed just below.
        if let Ok(text) = unsafe { item[0].to_string() } {
            found.push(text);
        }
        // SAFETY: COM allocated the string for the caller to free.
        unsafe { CoTaskMemFree(Some(item[0].as_ptr().cast())) };
    }
    found
}

/// Whether Windows can check any language, asked on a thread of its own that leaves nothing
/// behind.
pub(super) fn has_languages() -> bool {
    WindowsSpelling::start()
        .and_then(|spelling| spelling.run(|session| session.languages()).ok())
        .is_some_and(|languages| !languages.is_empty())
}

impl Checker for WindowsSpelling {
    fn languages(&self) -> Vec<Language> {
        let tags = self
            .run(|session| session.languages())
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "spelling: no Windows languages");
                Vec::new()
            });
        let mut languages: Vec<Language> = tags
            .into_iter()
            .map(|tag| Language {
                name: super::names::display_name(&tag),
                id: tag,
            })
            .collect();
        languages.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        languages
    }

    fn check(
        &self,
        text: &str,
        languages: &[String],
        skip: &[Range<usize>],
        ignored: &[String],
    ) -> Result<Vec<Range<usize>>, String> {
        let wide = HSTRING::from(text);
        let languages = languages.to_vec();
        let flagged = self.run(move |session| {
            languages
                .iter()
                .map(|language| session.misspelled(language, &wide))
                .collect::<Result<Vec<_>, _>>()
        })?;
        Ok(super::system_misspellings(text, &flagged, skip, ignored))
    }

    fn suggest(&self, word: &str, languages: &[String]) -> Result<Vec<String>, String> {
        let word = HSTRING::from(word);
        let languages = languages.to_vec();
        self.run(move |session| {
            let mut suggestions: Vec<String> = Vec::new();
            for language in &languages {
                let checker = session.checker(language)?;
                // SAFETY: the checker is used on the thread that made it.
                let Ok(found) = (unsafe { checker.Suggest(&word) }) else {
                    continue;
                };
                for suggestion in strings(&found) {
                    if !suggestions.contains(&suggestion) {
                        suggestions.push(suggestion);
                    }
                }
            }
            Ok(suggestions)
        })
    }

    fn add_word(&self, word: &str, language: &str) -> Result<(), String> {
        let word = HSTRING::from(word);
        let language = language.to_owned();
        self.run(move |session| {
            let checker = session.checker(&language)?;
            // SAFETY: the checker is used on the thread that made it.
            unsafe { checker.Add(&word) }.map_err(|error| error.to_string())
        })
    }
}
