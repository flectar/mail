//! Spelling with the Hunspell dictionaries installed on the system, as Enchant finds them for
//! GNOME and KDE apps, and words the user added in Enchant's personal word lists.
//!
//! Parsing a dictionary takes as much memory as its word list, up to 18 MiB for German, so a
//! short-lived copy of the app indexes each one into the cache, once per installed version.
//! Each check then maps the index and keeps nothing once it returns.

use super::{Checker, Language};
use flectar_spell::{CheckError, Dictionary, IndexCache, PersonalWords, Speller};
use std::collections::HashSet;
use std::ffi::OsStr;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

/// The argument that makes the app index one dictionary and exit.
const INDEX_ARGUMENT: &str = "--index-spelling-dictionary";
/// Indexing the largest dictionaries takes under half a second.
const INDEX_TIMEOUT: Duration = Duration::from_secs(60);

/// Builds the index of a dictionary into a cache directory.
type Indexer = fn(&Path, &Dictionary) -> Result<(), String>;

pub(crate) struct Hunspell {
    speller: Speller,
    cache_dir: PathBuf,
    dirs: Vec<PathBuf>,
    indexer: Indexer,
    /// The dictionaries as last listed.
    dictionaries: Mutex<Vec<Dictionary>>,
    /// Dictionaries that couldn't be indexed, as installed then.
    failed: Mutex<HashSet<Installed>>,
    /// Held while indexing, so two messages don't index the same dictionary twice.
    indexing: Mutex<()>,
}

/// A dictionary as installed at one time.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Installed {
    dictionary: Dictionary,
    size: u64,
    modified: Option<SystemTime>,
}

impl Installed {
    fn of(dictionary: &Dictionary) -> Self {
        let metadata = std::fs::metadata(&dictionary.dic).ok();
        Self {
            dictionary: dictionary.clone(),
            size: metadata.as_ref().map_or(0, std::fs::Metadata::len),
            modified: metadata.and_then(|metadata| metadata.modified().ok()),
        }
    }
}

impl Hunspell {
    pub(crate) fn new(cache_dir: PathBuf) -> Self {
        Self::with(
            cache_dir,
            flectar_spell::system_dictionary_dirs(),
            PersonalWords::enchant(),
            index_in_helper,
        )
    }

    fn with(
        cache_dir: PathBuf,
        dirs: Vec<PathBuf>,
        personal: Option<PersonalWords>,
        indexer: Indexer,
    ) -> Self {
        Self {
            speller: Speller::new(IndexCache::new(&cache_dir), personal),
            cache_dir,
            dirs,
            indexer,
            dictionaries: Mutex::new(Vec::new()),
            failed: Mutex::new(HashSet::new()),
            indexing: Mutex::new(()),
        }
    }

    /// The listed dictionaries for `languages`, in that order, leaving out any that failed to
    /// index as installed now.
    fn dictionaries(&self, languages: &[String]) -> Vec<Dictionary> {
        let listed = self.dictionaries.lock().unwrap_or_else(|e| e.into_inner());
        let failed = self.failed.lock().unwrap_or_else(|e| e.into_inner());
        languages
            .iter()
            .filter_map(|id| listed.iter().find(|dictionary| &dictionary.name == id))
            .filter(|dictionary| !failed.contains(&Installed::of(dictionary)))
            .cloned()
            .collect()
    }

    fn index(&self, unindexed: &[Dictionary]) {
        let _indexing = self.indexing.lock().unwrap_or_else(|e| e.into_inner());
        for dictionary in unindexed {
            // Another message may have indexed it while this one waited.
            if self.speller.cache().is_built(dictionary) {
                continue;
            }
            if let Err(error) = (self.indexer)(&self.cache_dir, dictionary) {
                tracing::warn!(%error, dictionary = %dictionary.name, "spelling dictionary unusable");
                self.failed
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(Installed::of(dictionary));
            }
        }
    }

    /// Run `checked` on the dictionaries for `languages`, indexing those that need it first.
    fn with_dictionaries<T>(
        &self,
        languages: &[String],
        checked: impl Fn(&[Dictionary]) -> Result<T, CheckError>,
    ) -> Result<T, String> {
        match checked(&self.dictionaries(languages)) {
            Err(CheckError::Unindexed(unindexed)) => {
                self.index(&unindexed);
                checked(&self.dictionaries(languages)).map_err(|error| error.to_string())
            }
            result => result.map_err(|error| error.to_string()),
        }
    }
}

impl Checker for Hunspell {
    fn languages(&self) -> Vec<Language> {
        let found = flectar_spell::find_dictionaries(&self.dirs);
        let mut languages: Vec<Language> = found
            .iter()
            .map(|dictionary| Language {
                id: dictionary.name.clone(),
                name: super::names::display_name(&dictionary.name),
            })
            .collect();
        languages.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        *self.dictionaries.lock().unwrap_or_else(|e| e.into_inner()) = found;
        languages
    }

    fn check(
        &self,
        text: &str,
        languages: &[String],
        skip: &[Range<usize>],
        ignored: &[String],
    ) -> Result<Vec<Range<usize>>, String> {
        self.with_dictionaries(languages, |dictionaries| {
            self.speller.check(text, dictionaries, skip, |word| {
                ignored.iter().any(|ignored| ignored == word)
            })
        })
    }

    fn suggest(&self, word: &str, languages: &[String]) -> Result<Vec<String>, String> {
        self.with_dictionaries(languages, |dictionaries| {
            self.speller.suggest(word, dictionaries)
        })
    }

    fn add_word(&self, word: &str, language: &str) -> Result<(), String> {
        let listed = self.dictionaries.lock().unwrap_or_else(|e| e.into_inner());
        let dictionary = listed
            .iter()
            .find(|dictionary| dictionary.name == language)
            .ok_or_else(|| format!("no {language} dictionary is installed"))?;
        self.speller
            .add_word(word, dictionary)
            .map_err(|error| error.to_string())
    }
}

/// Index `dictionary` in a copy of the app started for that alone, which returns its memory
/// to the system when it exits.
fn index_in_helper(cache_dir: &Path, dictionary: &Dictionary) -> Result<(), String> {
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut child = std::process::Command::new(executable)
        .arg(INDEX_ARGUMENT)
        .arg(cache_dir)
        .arg(&dictionary.name)
        .arg(&dictionary.aff)
        .arg(&dictionary.dic)
        .env_clear()
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("indexing ended with {status}"))
            };
        }
        if started.elapsed() > INDEX_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            return Err("indexing took too long".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Called by the desktop launcher before anything else: with the reserved argument, index the
/// dictionary the other arguments name and return the exit code.
pub fn run_index_worker_if_requested() -> Option<i32> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(OsStr::new(INDEX_ARGUMENT)) {
        return None;
    }
    let (Some(cache_dir), Some(name), Some(aff), Some(dic), None) = (
        args.next(),
        args.next(),
        args.next(),
        args.next(),
        args.next(),
    ) else {
        return Some(2);
    };
    let Ok(name) = name.into_string() else {
        return Some(2);
    };
    // SAFETY: These calls only lower this process's priority and turn off its core dumps.
    unsafe {
        let core = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        libc::setrlimit(libc::RLIMIT_CORE, &core);
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
    let dictionary = Dictionary {
        name,
        aff: aff.into(),
        dic: dic.into(),
    };
    Some(i32::from(
        IndexCache::new(PathBuf::from(cache_dir))
            .build(&dictionary)
            .is_err(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index_here(cache_dir: &Path, dictionary: &Dictionary) -> Result<(), String> {
        IndexCache::new(cache_dir)
            .build(dictionary)
            .map_err(|error| error.to_string())
    }

    fn installed_english(dir: &Path) {
        let vendored = Path::new(env!("CARGO_MANIFEST_DIR")).join("crates/spellbook/vendor/en_US");
        for extension in ["aff", "dic"] {
            std::fs::copy(
                vendored.join(format!("en_US.{extension}")),
                dir.join(format!("en_US.{extension}")),
            )
            .unwrap();
        }
    }

    #[test]
    fn dictionaries_are_indexed_on_first_use_and_checked_from_then_on() {
        let system = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let config = tempfile::tempdir().unwrap();
        installed_english(system.path());
        let checker = Hunspell::with(
            cache.path().to_owned(),
            vec![system.path().to_owned()],
            Some(PersonalWords::new(config.path())),
            index_here,
        );
        let languages = checker.languages();
        assert_eq!(languages.len(), 1);
        assert_eq!(languages[0].id, "en_US");
        let english = vec!["en_US".to_owned()];

        let text = "Teh numbers for Flectar are in";
        let found = checker.check(text, &english, &[], &[]).unwrap();
        let words: Vec<&str> = found.iter().map(|range| &text[range.clone()]).collect();
        assert_eq!(words, ["Teh", "Flectar"]);
        assert!(std::fs::read_dir(cache.path()).unwrap().count() >= 2);

        let ignored = checker
            .check(text, &english, &[], &["Teh".to_owned()])
            .unwrap();
        assert_eq!(ignored.len(), 1);
        checker.add_word("Flectar", "en_US").unwrap();
        assert_eq!(checker.check(text, &english, &[], &[]).unwrap().len(), 1);
        let suggestions = checker.suggest("teh", &english).unwrap();
        assert!(suggestions.iter().any(|s| s == "the"), "{suggestions:?}");

        // Languages that aren't installed check nothing.
        assert!(
            checker
                .check(text, &["xx_XX".to_owned()], &[], &[])
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_dictionary_that_fails_to_index_is_left_out_until_it_changes() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
        fn fail(_: &Path, _: &Dictionary) -> Result<(), String> {
            ATTEMPTS.fetch_add(1, Ordering::Relaxed);
            Err("broken".into())
        }
        let system = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        installed_english(system.path());
        let checker = Hunspell::with(
            cache.path().to_owned(),
            vec![system.path().to_owned()],
            None,
            fail,
        );
        checker.languages();
        let english = vec!["en_US".to_owned()];
        // Nothing is left to check with, and the dictionary isn't tried again.
        for _ in 0..2 {
            assert!(checker.check("Teh", &english, &[], &[]).unwrap().is_empty());
        }
        assert_eq!(ATTEMPTS.load(Ordering::Relaxed), 1);
        // An updated dictionary is.
        let dic = system.path().join("en_US.dic");
        let mut words = std::fs::read(&dic).unwrap();
        words.extend_from_slice(b"Flectar\n");
        std::fs::write(&dic, words).unwrap();
        assert!(checker.check("Teh", &english, &[], &[]).unwrap().is_empty());
        assert_eq!(ATTEMPTS.load(Ordering::Relaxed), 2);
    }
}
