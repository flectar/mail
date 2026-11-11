//! Spelling with macOS's own spell checker. macOS checks in its spelling service, picks the
//! languages from the text and the ones the user set in System Settings, and shares learned
//! words with other apps.
//!
//! The spell checker is made and used on the main thread, as Chromium does: making it from
//! another thread waits on the main one and can deadlock. Checks are requested there and
//! answered on another thread, so the main thread only starts them.

use super::{Checker, Language};
use block2::RcBlock;
use objc2_app_kit::NSSpellChecker;
use objc2_foundation::{
    NSArray, NSInteger, NSOrthography, NSRange, NSString, NSTextCheckingResult, NSTextCheckingType,
};
use std::ops::Range;
use std::ptr::NonNull;
use std::sync::{Mutex, mpsc};
use std::time::Duration;

/// macOS answers well within this; it only keeps a lost answer from holding a message's
/// checks forever.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) struct MacSpelling;

/// Run `work` on the main thread and wait for its answer.
fn on_main<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    let (answer, result) = mpsc::sync_channel(1);
    slint::invoke_from_event_loop(move || {
        let _ = answer.send(work());
    })
    .map_err(|error| error.to_string())?;
    result
        .recv_timeout(ANSWER_TIMEOUT)
        .map_err(|_| "the spell checker didn't answer".to_owned())
}

impl Checker for MacSpelling {
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
        let (answer, result) = mpsc::sync_channel::<Vec<Range<usize>>>(1);
        let owned = super::checked_text(text, skip);
        on_main(move || {
            let checker = NSSpellChecker::sharedSpellChecker();
            let string = NSString::from_str(&owned);
            let whole = NSRange::new(0, string.length());
            // The handler runs on another thread and only copies plain ranges out.
            let answer = Mutex::new(Some(answer));
            let handler = RcBlock::new(
                move |_sequence: NSInteger,
                      results: NonNull<NSArray<NSTextCheckingResult>>,
                      _orthography: NonNull<NSOrthography>,
                      _words: NSInteger| {
                    // SAFETY: macOS passes a valid array for the duration of the call.
                    let results = unsafe { results.as_ref() };
                    let ranges = results
                        .iter()
                        .filter(|result| result.resultType() == NSTextCheckingType::Spelling)
                        .map(|result| {
                            let range = result.range();
                            range.location..range.location + range.length
                        })
                        .collect();
                    if let Some(answer) = answer.lock().ok().and_then(|mut answer| answer.take()) {
                        let _ = answer.send(ranges);
                    }
                },
            );
            // SAFETY: the string and the handler are retained for the request, and the
            // handler only reads its arguments.
            unsafe {
                checker.requestCheckingOfString_range_types_options_inSpellDocumentWithTag_completionHandler(
                    &string,
                    whole,
                    NSTextCheckingType::Spelling.0,
                    None,
                    0,
                    Some(&handler),
                );
            }
        })?;
        let flagged = result
            .recv_timeout(ANSWER_TIMEOUT)
            .map_err(|_| "the spell checker didn't answer".to_owned())?;
        Ok(super::system_misspellings(text, &[flagged], skip, ignored))
    }

    fn suggest(&self, word: &str, _languages: &[String]) -> Result<Vec<String>, String> {
        let owned = word.to_owned();
        on_main(move || {
            let checker = NSSpellChecker::sharedSpellChecker();
            let string = NSString::from_str(&owned);
            let whole = NSRange::new(0, string.length());
            let language = checker.language();
            checker
                .guessesForWordRange_inString_language_inSpellDocumentWithTag(
                    whole,
                    &string,
                    Some(&language),
                    0,
                )
                .map(|guesses| guesses.iter().map(|guess| guess.to_string()).collect())
                .unwrap_or_default()
        })
    }

    fn add_word(&self, word: &str, _language: &str) -> Result<(), String> {
        let owned = word.to_owned();
        on_main(move || {
            NSSpellChecker::sharedSpellChecker().learnWord(&NSString::from_str(&owned));
        })
    }
}
