//! Word indexes: each dictionary's stems, parsed once and cached for mapping.

use std::borrow::Cow;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::Dictionary;

/// Where word indexes are cached, with a UTF-8 copy of each dictionary's affix rules.
#[derive(Clone, Debug)]
pub struct IndexCache {
    dir: PathBuf,
}

/// The cached files for one dictionary.
pub(crate) struct CachedFiles {
    pub aff: PathBuf,
    pub index: PathBuf,
}

impl IndexCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The files for `dictionary` as installed now. Their names change when either of its
    /// files does, so an updated dictionary is indexed again.
    pub(crate) fn files(&self, dictionary: &Dictionary) -> io::Result<CachedFiles> {
        let mut hash = Fnv::default();
        for path in [&dictionary.aff, &dictionary.dic] {
            let metadata = fs::metadata(path)?;
            hash.write(path.as_os_str().as_encoded_bytes());
            hash.write(&metadata.len().to_le_bytes());
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |time| time.as_nanos());
            hash.write(&modified.to_le_bytes());
        }
        let stem = format!("{}-{:016x}", dictionary.name, hash.0);
        Ok(CachedFiles {
            aff: self.dir.join(format!("{stem}.aff")),
            index: self.dir.join(format!("{stem}.idx")),
        })
    }

    /// Whether `dictionary` is indexed as installed now.
    pub fn is_built(&self, dictionary: &Dictionary) -> bool {
        self.files(dictionary)
            .is_ok_and(|files| files.aff.is_file() && files.index.is_file())
    }

    /// Index `dictionary` into this cache.
    ///
    /// Parsing a dictionary takes as much memory as its word list, up to 18 MiB for German, so
    /// an application should run this in a short-lived process and keep its own memory clear.
    pub fn build(&self, dictionary: &Dictionary) -> io::Result<()> {
        let files = self.files(dictionary)?;
        let aff = fs::read(&dictionary.aff)?;
        let dic = fs::read(&dictionary.dic)?;
        let encoding = declared_encoding(&aff)?;
        let aff = with_utf8_set(&decode(&aff, encoding)?);
        let dic = decode(&dic, encoding)?;
        let index = spellbook::Dictionary::new(&aff, &dic)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?
            .word_index();
        drop(dic);
        fs::create_dir_all(&self.dir)?;
        // The rules first: a check needs both, and looks for the index.
        write_atomically(&files.aff, aff.as_bytes())?;
        write_atomically(&files.index, &index)?;
        self.remove_others(dictionary, &files);
        Ok(())
    }

    /// Drop the files of earlier installs of `dictionary`.
    fn remove_others(&self, dictionary: &Dictionary, current: &CachedFiles) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let prefix = format!("{}-", dictionary.name);
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(rest) = name.strip_prefix(&prefix) else {
                continue;
            };
            // Only `<name>-<16 hex digits>.<ext>`, so `en-…` never removes `en_US-…`.
            let is_ours = rest.len() == 20
                && rest.as_bytes()[..16].iter().all(u8::is_ascii_hexdigit)
                && (rest.ends_with(".aff") || rest.ends_with(".idx"));
            if is_ours && path != current.aff && path != current.index {
                let _ = fs::remove_file(path);
            }
        }
    }
}

/// The encoding a `.aff` file declares with `SET`, ISO-8859-1 when it declares none.
fn declared_encoding(aff: &[u8]) -> io::Result<&'static encoding_rs::Encoding> {
    let label = aff
        .split(|&byte| byte == b'\n')
        .find_map(|line| {
            let line = line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(line);
            let rest = line.strip_prefix(b"SET")?;
            let label = rest
                .split(|b| b.is_ascii_whitespace())
                .find(|part| !part.is_empty())?;
            std::str::from_utf8(label).ok()
        })
        .unwrap_or("ISO8859-1");
    // Hunspell's names for code pages differ from the Web's in one case.
    let web_label = match label.to_ascii_lowercase().as_str() {
        "microsoft-cp1251" => Cow::Borrowed("windows-1251"),
        other => Cow::Owned(other.to_owned()),
    };
    encoding_rs::Encoding::for_label(web_label.as_bytes()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported dictionary encoding {label}"),
        )
    })
}

fn decode(bytes: &[u8], encoding: &'static encoding_rs::Encoding) -> io::Result<String> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    encoding
        .decode_without_bom_handling_and_without_replacement(bytes)
        .map(Cow::into_owned)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("dictionary text is not valid {}", encoding.name()),
            )
        })
}

/// `aff` declaring UTF-8, as it now is.
fn with_utf8_set(aff: &str) -> String {
    let mut out = String::with_capacity(aff.len());
    for line in aff.split_inclusive('\n') {
        let declares = line
            .trim_start_matches('\u{feff}')
            .strip_prefix("SET")
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_whitespace()));
        if declares {
            out.push_str("SET UTF-8\n");
        } else {
            out.push_str(line);
        }
    }
    out
}

fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let result = (|| {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// FNV-1a, for cache file names.
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.0 = (self.0 ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodings_come_from_the_set_line() {
        assert_eq!(
            declared_encoding(b"SET UTF-8\n").unwrap(),
            encoding_rs::UTF_8
        );
        assert_eq!(
            declared_encoding(b"# comment\nSET ISO8859-1\nTRY e\n").unwrap(),
            encoding_rs::WINDOWS_1252
        );
        assert_eq!(
            declared_encoding(b"SET KOI8-R\n").unwrap(),
            encoding_rs::KOI8_R
        );
        assert_eq!(
            declared_encoding(b"SET microsoft-cp1251\n").unwrap(),
            encoding_rs::WINDOWS_1251
        );
        assert_eq!(
            declared_encoding(b"TRY abc\n").unwrap(),
            encoding_rs::WINDOWS_1252
        );
        assert!(declared_encoding(b"SET ISCII-DEVANAGARI\n").is_err());
        assert_eq!(
            with_utf8_set("# x\nSET ISO8859-1\r\nSETTINGS stay\n"),
            "# x\nSET UTF-8\nSETTINGS stay\n"
        );
    }
}
