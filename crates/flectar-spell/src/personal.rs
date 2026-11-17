//! Words the user added to a dictionary.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

/// Words the user added, kept where Enchant keeps them so GNOME apps and this one share them:
/// `<dir>/<language>.dic`, one word per line.
#[derive(Clone, Debug)]
pub struct PersonalWords {
    dir: PathBuf,
}

impl PersonalWords {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// Enchant's directory: `$ENCHANT_CONFIG_DIR`, or `enchant` in the user's configuration
    /// directory. A Flatpak has a configuration directory of its own, so there the words are
    /// the app's alone.
    pub fn enchant() -> Option<Self> {
        std::env::var_os("ENCHANT_CONFIG_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| crate::dictionaries::config_home().map(|config| config.join("enchant")))
            .map(Self::new)
    }

    fn file(&self, language: &str) -> PathBuf {
        self.dir.join(format!("{language}.dic"))
    }

    pub fn words(&self, language: &str) -> Vec<String> {
        fs::read_to_string(self.file(language))
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|word| !word.is_empty() && !word.starts_with('#'))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn add(&self, language: &str, word: &str) -> io::Result<()> {
        let word = word.trim();
        if word.is_empty()
            || word.contains(['\n', '\r'])
            || self.words(language).iter().any(|w| w == word)
        {
            return Ok(());
        }
        fs::create_dir_all(&self.dir)?;
        let path = self.file(language);
        // Enchant writes each word on its own line; don't join one onto an unterminated last line.
        let needs_newline =
            fs::read(&path).is_ok_and(|text| text.last().is_some_and(|&byte| byte != b'\n'));
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        let line = if needs_newline {
            format!("\n{word}\n")
        } else {
            format!("{word}\n")
        };
        file.write_all(line.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn added_words_are_read_back_once_each() {
        let dir = tempfile::tempdir().unwrap();
        let personal = PersonalWords::new(dir.path().join("enchant"));
        assert!(personal.words("en_US").is_empty());
        personal.add("en_US", "Flectar").unwrap();
        personal.add("en_US", "Flectar").unwrap();
        personal.add("en_US", " Northwind ").unwrap();
        assert_eq!(personal.words("en_US"), ["Flectar", "Northwind"]);
        assert!(personal.words("de_DE").is_empty());

        // A file another app left without a final newline.
        std::fs::write(dir.path().join("enchant/de_DE.dic"), "Grüße").unwrap();
        personal.add("de_DE", "Flectar").unwrap();
        assert_eq!(personal.words("de_DE"), ["Grüße", "Flectar"]);
    }
}
