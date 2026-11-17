//! Spell checking with Hunspell dictionaries that keeps nothing in memory between checks.
//!
//! A dictionary's stems are parsed once into a word index ([`IndexCache::build`]), which an
//! application should do in a short-lived process. Each check then maps the index, parses the
//! dictionary's affix rules, checks and lets everything go. The stems stay in page cache, which
//! the system can reclaim, and the application's own memory is back where it was after each
//! check.

mod dictionaries;
mod index;
mod personal;
mod words;

use std::fmt;
use std::fs;
use std::io;
use std::ops::Range;

pub use dictionaries::{Dictionary, find_dictionaries, system_dictionary_dirs};
pub use index::IndexCache;
pub use personal::PersonalWords;
pub use words::checkable_words;

/// Why spelling couldn't be checked.
#[derive(Debug)]
pub enum CheckError {
    /// These dictionaries aren't indexed as they are installed now; see [`IndexCache::build`].
    Unindexed(Vec<Dictionary>),
    Io(io::Error),
}

impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unindexed(dictionaries) => {
                let names: Vec<_> = dictionaries.iter().map(|d| d.name.as_str()).collect();
                write!(f, "dictionaries not indexed yet: {}", names.join(", "))
            }
            Self::Io(error) => fmt::Display::fmt(error, f),
        }
    }
}

impl std::error::Error for CheckError {}

/// Checks spelling against indexed dictionaries, opening them for each call.
#[derive(Clone, Debug)]
pub struct Speller {
    cache: IndexCache,
    personal: Option<PersonalWords>,
}

impl Speller {
    pub fn new(cache: IndexCache, personal: Option<PersonalWords>) -> Self {
        Self { cache, personal }
    }

    pub fn cache(&self) -> &IndexCache {
        &self.cache
    }

    /// The byte ranges of the words in `text` that all of `dictionaries` reject, so a word in
    /// any of a message's languages passes. Words overlapping `skip`, such as quoted text and
    /// code, aren't checked, nor words `ignored` accepts.
    pub fn check(
        &self,
        text: &str,
        dictionaries: &[Dictionary],
        skip: &[Range<usize>],
        ignored: impl Fn(&str) -> bool,
    ) -> Result<Vec<Range<usize>>, CheckError> {
        if dictionaries.is_empty() {
            return Ok(Vec::new());
        }
        let opened = self.open(dictionaries)?;
        Ok(checkable_words(text)
            .into_iter()
            .filter(|range| {
                !skip
                    .iter()
                    .any(|skipped| skipped.start < range.end && range.start < skipped.end)
            })
            .filter(|range| {
                let word = &text[range.clone()];
                !ignored(word) && opened.iter().all(|dictionary| !dictionary.check(word))
            })
            .collect())
    }

    /// Suggestions for `word`, each dictionary's in turn, without repeats.
    pub fn suggest(
        &self,
        word: &str,
        dictionaries: &[Dictionary],
    ) -> Result<Vec<String>, CheckError> {
        let opened = self.open(dictionaries)?;
        let mut suggestions: Vec<String> = Vec::new();
        let mut found = Vec::new();
        for dictionary in &opened {
            dictionary.suggest(word, &mut found);
            for suggestion in found.drain(..) {
                if !suggestions.contains(&suggestion) {
                    suggestions.push(suggestion);
                }
            }
        }
        Ok(suggestions)
    }

    /// Accept `word` in `dictionary`'s language from now on.
    pub fn add_word(&self, word: &str, dictionary: &Dictionary) -> io::Result<()> {
        match &self.personal {
            Some(personal) => personal.add(dictionary.language(), word),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no personal dictionary to add the word to",
            )),
        }
    }

    fn open(&self, dictionaries: &[Dictionary]) -> Result<Vec<spellbook::Dictionary>, CheckError> {
        let mut opened = Vec::with_capacity(dictionaries.len());
        let mut unindexed = Vec::new();
        for dictionary in dictionaries {
            match self.open_one(dictionary).map_err(CheckError::Io)? {
                Some(open) => opened.push(open),
                None => unindexed.push(dictionary.clone()),
            }
        }
        if unindexed.is_empty() {
            Ok(opened)
        } else {
            Err(CheckError::Unindexed(unindexed))
        }
    }

    /// `dictionary` with its stems mapped from the cache and the user's words added, or `None`
    /// when it needs indexing first.
    fn open_one(&self, dictionary: &Dictionary) -> io::Result<Option<spellbook::Dictionary>> {
        let files = self.cache.files(dictionary)?;
        let opened =
            fs::read_to_string(&files.aff).and_then(|aff| Ok((aff, fs::File::open(&files.index)?)));
        let (aff, index) = match opened {
            Ok(files) => files,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        // SAFETY: The cache never writes to a file once it is in place: it writes each index
        // under a temporary name and renames it. Something else changing the file could make
        // reads return other bytes, which the word index tolerates since it checks every read,
        // or fault if it truncated the file, as with any mapped file.
        let map = unsafe { memmap2::Mmap::map(&index)? };
        let Ok(mut open) = spellbook::Dictionary::new_with_word_index(&aff, map) else {
            // Written in another format, or for other rules: it needs indexing again.
            return Ok(None);
        };
        if let Some(personal) = &self.personal {
            for word in personal.words(dictionary.language()) {
                // A slash would start flags in a dictionary line; checked words have none.
                if !word.contains('/') {
                    let _ = open.add(&word);
                }
            }
        }
        Ok(Some(open))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn vendored_en_us() -> Dictionary {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../spellbook/vendor/en_US");
        Dictionary {
            name: "en_US".into(),
            aff: dir.join("en_US.aff"),
            dic: dir.join("en_US.dic"),
        }
    }

    fn latin1_dictionary(dir: &Path) -> Dictionary {
        // "Straße" and "Grüße" encoded as ISO-8859-1, as many distributions ship German.
        let aff = b"SET ISO8859-1\nTRY esianrtolcdugmphbyfvkw\n".to_vec();
        let mut dic = b"2\nStra".to_vec();
        dic.push(0xDF);
        dic.extend_from_slice(b"e\nGr");
        dic.push(0xFC);
        dic.push(0xDF);
        dic.extend_from_slice(b"e\n");
        std::fs::write(dir.join("de_TEST.aff"), aff).unwrap();
        std::fs::write(dir.join("de_TEST.dic"), dic).unwrap();
        Dictionary {
            name: "de_TEST".into(),
            aff: dir.join("de_TEST.aff"),
            dic: dir.join("de_TEST.dic"),
        }
    }

    #[test]
    fn unindexed_dictionaries_are_reported_until_built() {
        let cache = tempfile::tempdir().unwrap();
        let speller = Speller::new(IndexCache::new(cache.path()), None);
        let en_us = vendored_en_us();
        assert!(matches!(
            speller.check("Teh cat", std::slice::from_ref(&en_us), &[], |_| false),
            Err(CheckError::Unindexed(list)) if list == [en_us.clone()]
        ));
        speller.cache().build(&en_us).unwrap();
        assert!(speller.cache().is_built(&en_us));
        let text = "Teh cat sat on teh mat, see https://exmaple.com.";
        let found = speller
            .check(text, std::slice::from_ref(&en_us), &[], |_| false)
            .unwrap();
        let words: Vec<&str> = found.iter().map(|range| &text[range.clone()]).collect();
        assert_eq!(words, ["Teh", "teh"]);
        let suggestions = speller
            .suggest("teh", std::slice::from_ref(&en_us))
            .unwrap();
        assert!(suggestions.iter().any(|s| s == "the"), "{suggestions:?}");
    }

    #[test]
    fn skipped_ranges_and_ignored_words_pass() {
        let cache = tempfile::tempdir().unwrap();
        let speller = Speller::new(IndexCache::new(cache.path()), None);
        let en_us = vendored_en_us();
        speller.cache().build(&en_us).unwrap();
        let text = "Teh reply\n> teh quote\nFlectar wrote";
        let quote = text.find('>').unwrap()..text.find("\nFlectar").unwrap();
        let found = speller
            .check(text, std::slice::from_ref(&en_us), &[quote], |word| {
                word == "Flectar"
            })
            .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(&text[found[0].clone()], "Teh");
    }

    #[test]
    fn a_word_passes_when_any_language_has_it() {
        let cache = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        let speller = Speller::new(IndexCache::new(cache.path()), None);
        let en_us = vendored_en_us();
        let german = latin1_dictionary(installed.path());
        for dictionary in [&en_us, &german] {
            speller.cache().build(dictionary).unwrap();
        }
        let text = "Grüße and greetings from the Straße, Grusse";
        let both = [en_us.clone(), german.clone()];
        let found = speller.check(text, &both, &[], |_| false).unwrap();
        let words: Vec<&str> = found.iter().map(|range| &text[range.clone()]).collect();
        assert_eq!(words, ["Grusse"]);
        let english_only = speller
            .check(text, std::slice::from_ref(&en_us), &[], |_| false)
            .unwrap();
        assert_eq!(english_only.len(), 3);
    }

    #[test]
    fn added_words_pass_from_the_next_check() {
        let cache = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        let speller = Speller::new(
            IndexCache::new(cache.path()),
            Some(PersonalWords::new(config.path().join("enchant"))),
        );
        let en_us = vendored_en_us();
        speller.cache().build(&en_us).unwrap();
        let check = || {
            speller
                .check("Flectar Mail", std::slice::from_ref(&en_us), &[], |_| false)
                .unwrap()
        };
        assert_eq!(check().len(), 1);
        speller.add_word("Flectar", &en_us).unwrap();
        assert!(check().is_empty());
        assert_eq!(
            std::fs::read_to_string(config.path().join("enchant/en_US.dic")).unwrap(),
            "Flectar\n"
        );
    }

    #[test]
    fn an_updated_dictionary_is_indexed_again_and_old_files_go() {
        let cache = tempfile::tempdir().unwrap();
        let installed = tempfile::tempdir().unwrap();
        let speller = Speller::new(IndexCache::new(cache.path()), None);
        let german = latin1_dictionary(installed.path());
        speller.cache().build(&german).unwrap();
        let listing = || {
            let mut names: Vec<PathBuf> = std::fs::read_dir(cache.path())
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect();
            names.sort();
            names
        };
        let before = listing();
        assert_eq!(before.len(), 2);

        // An update changes the file's size (and usually its time).
        let mut dic = std::fs::read(&german.dic).unwrap();
        dic.extend_from_slice(b"Haus\n");
        dic[0] = b'3';
        std::fs::write(&german.dic, dic).unwrap();
        assert!(!speller.cache().is_built(&german));
        assert!(matches!(
            speller.check("Haus", std::slice::from_ref(&german), &[], |_| false),
            Err(CheckError::Unindexed(_))
        ));
        speller.cache().build(&german).unwrap();
        let after = listing();
        assert_eq!(after.len(), 2);
        assert!(before.iter().all(|path| !after.contains(path)));
        assert!(
            speller
                .check("Haus", std::slice::from_ref(&german), &[], |_| false)
                .unwrap()
                .is_empty()
        );
    }
}
