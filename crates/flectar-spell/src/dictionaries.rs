//! Finding the Hunspell dictionaries installed on the system.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// A Hunspell dictionary: affix rules in a `.aff` file and stems in a `.dic` file.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Dictionary {
    /// Its file name without the extension, as Enchant names it: `en_US`, `de_DE_frami`.
    pub name: String,
    pub aff: PathBuf,
    pub dic: PathBuf,
}

impl Dictionary {
    /// Its language and region without any variant, `de_DE` for `de_DE_frami`: the name
    /// Enchant keeps the user's added words under.
    pub fn language(&self) -> &str {
        let mut parts = self.name.splitn(3, '_');
        let language = parts.next().unwrap_or_default();
        match parts.next() {
            Some(region)
                if (region.len() == 2 && region.bytes().all(|b| b.is_ascii_uppercase()))
                    || (region.len() == 3 && region.bytes().all(|b| b.is_ascii_digit())) =>
            {
                &self.name[..language.len() + 1 + region.len()]
            }
            _ => language,
        }
    }
}

/// Where Linux keeps Hunspell dictionaries, in the order Enchant looks: the user's own, each
/// `$XDG_DATA_DIRS` entry (which in a Flatpak covers the runtime's dictionaries), then
/// `/usr/share/myspell`, where some distributions still install them.
pub fn system_dictionary_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(config) = config_home() {
        dirs.push(config.join("enchant/hunspell"));
    }
    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.extend(std::env::split_paths(&data_dirs).map(|dir| dir.join("hunspell")));
    dirs.push("/usr/share/myspell".into());
    dirs.push("/usr/share/myspell/dicts".into());
    dirs
}

/// `$XDG_CONFIG_HOME`, or `~/.config`.
pub(crate) fn config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
}

/// The dictionaries in `dirs`, sorted by name. When two directories hold a dictionary of the
/// same name, the earlier one wins, as in Enchant.
pub fn find_dictionaries(dirs: &[PathBuf]) -> Vec<Dictionary> {
    let mut seen = HashSet::new();
    let mut found = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().into_string().ok()?;
                name.strip_suffix(".dic").map(str::to_owned)
            })
            .collect();
        names.sort_unstable();
        for name in names {
            // Hyphenation patterns and thesauri share the extension in some layouts.
            if name.starts_with("hyph_") || name.starts_with("th_") || seen.contains(&name) {
                continue;
            }
            let dic = dir.join(format!("{name}.dic"));
            let aff = dir.join(format!("{name}.aff"));
            if dic.is_file() && aff.is_file() {
                seen.insert(name.clone());
                found.push(Dictionary { name, aff, dic });
            }
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_drop_variants() {
        let named = |name: &str| Dictionary {
            name: name.into(),
            aff: PathBuf::new(),
            dic: PathBuf::new(),
        };
        assert_eq!(named("en_US").language(), "en_US");
        assert_eq!(named("de_DE_frami").language(), "de_DE");
        assert_eq!(named("es_419").language(), "es_419");
        assert_eq!(named("ru").language(), "ru");
        assert_eq!(named("ca_valencia").language(), "ca");
    }

    #[test]
    fn earlier_directories_win_and_other_files_are_ignored() {
        let user = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        for (dir, name) in [
            (user.path(), "en_US"),
            (system.path(), "en_US"),
            (system.path(), "de_DE"),
            (system.path(), "hyph_de_DE"),
        ] {
            std::fs::write(dir.join(format!("{name}.dic")), "1\nword\n").unwrap();
            std::fs::write(dir.join(format!("{name}.aff")), "SET UTF-8\n").unwrap();
        }
        // A word list without affix rules isn't a dictionary.
        std::fs::write(system.path().join("fr.dic"), "1\nmot\n").unwrap();

        let found = find_dictionaries(&[
            user.path().to_owned(),
            PathBuf::from("/nonexistent/hunspell"),
            system.path().to_owned(),
        ]);
        let names: Vec<_> = found.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["de_DE", "en_US"]);
        assert!(found[1].dic.starts_with(user.path()));
    }
}
