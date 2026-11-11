//! Spelling with iOS's own spell checker, UITextChecker, which checks in the app, one language
//! at a time. iOS's text fields check in the languages the user writes in, with no choice of
//! their own, so this checks in the preferred languages from Settings that it has dictionaries
//! for, and a word passes when any of them has it. Words the user adds, iOS keeps.
//!
//! It works on the main thread only, so checks go there in turns of a few milliseconds, a piece
//! of the text in one language at a time, and the app keeps drawing in between. One checker
//! serves while messages are being written, as Apple suggests one for each document, and goes
//! when the last one closes.

use crate::spelling::{Checker, Language};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_foundation::{NSInteger, NSNotFound, NSRange, NSString};
use objc2_ui_kit::UITextChecker;
use std::cell::RefCell;
use std::ops::Range;
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// iOS answers well within this; it only keeps a lost answer from holding a message's checks
/// forever.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);
/// How long one turn of the main thread checks before letting it draw. In text of another
/// language, every word is a misspelling, and each one costs a call.
const TURN: Duration = Duration::from_millis(4);

thread_local! {
    /// The checker while messages are being written, on the main thread.
    static CHECKER: RefCell<Option<Retained<UITextChecker>>> = const { RefCell::new(None) };
}

pub(crate) struct IosSpelling;

/// iOS's spell checker, for the first message being written.
pub(crate) fn spell_checker() -> Option<Arc<dyn Checker>> {
    Some(Arc::new(IosSpelling))
}

/// Run `work` on the main thread and wait for its answer.
fn on_main<T: Send + 'static>(
    work: impl FnOnce(MainThreadMarker) -> T + Send + 'static,
) -> Result<T, String> {
    let (answer, result) = mpsc::sync_channel(1);
    slint::invoke_from_event_loop(move || {
        if let Some(main) = MainThreadMarker::new() {
            let _ = answer.send(work(main));
        }
    })
    .map_err(|error| error.to_string())?;
    result
        .recv_timeout(ANSWER_TIMEOUT)
        .map_err(|_| "the spell checker didn't answer".to_owned())
}

/// The checker, made the first time a message needs it.
fn checker(main: MainThreadMarker) -> Retained<UITextChecker> {
    CHECKER.with(|slot| {
        slot.borrow_mut()
            .get_or_insert_with(|| UITextChecker::new(main))
            .clone()
    })
}

impl Drop for IosSpelling {
    /// The last message closed: let go of the checker, on the main thread.
    fn drop(&mut self) {
        let release = || {
            CHECKER.with(|slot| slot.borrow_mut().take());
        };
        if MainThreadMarker::new().is_some() {
            release();
        } else {
            let _ = slint::invoke_from_event_loop(release);
        }
    }
}

/// The preferred languages from Settings that iOS can check, as the app's identifiers.
fn checked_languages(main: MainThreadMarker) -> Vec<String> {
    let available: Vec<Language> = UITextChecker::availableLanguages(main)
        .iter()
        .map(|language| Language {
            id: language.to_string(),
            name: String::new(),
        })
        .collect();
    crate::spelling::system_languages(&available)
}

/// The UTF-16 ranges `language` flags in `string`, which is `length` units long, from `start`
/// on until `turn` is over, and where to go on from, if not at the end.
fn misspelled(
    checker: &UITextChecker,
    string: &NSString,
    length: usize,
    language: &NSString,
    mut start: usize,
    turn: Duration,
) -> (Vec<Range<usize>>, Option<usize>) {
    let ends = std::time::Instant::now() + turn;
    let mut found = Vec::new();
    while start < length {
        if std::time::Instant::now() >= ends {
            return (found, Some(start));
        }
        let range = checker.rangeOfMisspelledWordInString_range_startingAt_wrap_language(
            string,
            NSRange::new(0, length),
            start as NSInteger,
            false,
            language,
        );
        if range.location == NSNotFound as usize || range.length == 0 || range.location < start {
            break;
        }
        found.push(range.location..range.location + range.length);
        start = range.location + range.length;
    }
    (found, None)
}

impl Checker for IosSpelling {
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
        let languages = on_main(checked_languages)?;
        if languages.is_empty() {
            return Ok(Vec::new());
        }
        let checked = crate::spelling::checked_text(text, skip);
        let mut flagged: Vec<Vec<Range<usize>>> = vec![Vec::new(); languages.len()];
        // A piece in one language at a time, each turn of the main thread going on from where
        // the last one stopped.
        for (piece, place) in crate::spelling::checked_pieces(&checked) {
            for (language, all) in languages.iter().zip(flagged.iter_mut()) {
                let mut from = Some(0);
                while let Some(start) = from {
                    let part = checked[piece.clone()].to_owned();
                    let language = language.clone();
                    let (found, next) = on_main(move |main| {
                        let string = NSString::from_str(&part);
                        let length = string.length();
                        let language = NSString::from_str(&language);
                        misspelled(&checker(main), &string, length, &language, start, TURN)
                    })?;
                    all.extend(
                        found
                            .into_iter()
                            .map(|range| range.start + place..range.end + place),
                    );
                    from = next;
                }
            }
        }
        Ok(crate::spelling::system_misspellings(
            text, &flagged, skip, ignored,
        ))
    }

    fn suggest(&self, word: &str, _languages: &[String]) -> Result<Vec<String>, String> {
        let word = word.to_owned();
        on_main(move |main| {
            let languages = checked_languages(main);
            let checker = checker(main);
            let string = NSString::from_str(&word);
            let whole = NSRange::new(0, string.length());
            let mut suggestions: Vec<String> = Vec::new();
            for language in &languages {
                let Some(guesses) = checker.guessesForWordRange_inString_language(
                    whole,
                    &string,
                    &NSString::from_str(language),
                ) else {
                    continue;
                };
                for guess in guesses.iter() {
                    let guess = guess.to_string();
                    if !suggestions.contains(&guess) {
                        suggestions.push(guess);
                    }
                }
            }
            suggestions
        })
    }

    fn add_word(&self, word: &str, _language: &str) -> Result<(), String> {
        let word = word.to_owned();
        on_main(move |main| UITextChecker::learnWord(&NSString::from_str(&word), main))
    }
}
