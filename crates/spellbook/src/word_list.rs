//! The stems of a dictionary with their flags.
//!
//! Stems parsed from a `.dic` file live in a [`HashBag`]. A dictionary can instead read them in
//! place from a word index, a file written by [`crate::Dictionary::word_index`]: an application
//! can map that file into memory, so the word list takes up page cache, which the system can
//! reclaim, rather than heap. Words added later always go to the hash bag.
//!
//! A word index is little-endian:
//!
//! | Bytes | Contents |
//! | --- | --- |
//! | 8 | [`MAGIC`] |
//! | 8 | FNV-1a hash of the `.aff` file the flags were parsed with |
//! | 4 | number of entries |
//! | 4 | length of the stem pool |
//! | 4 | length of the flag pool |
//! | 4 | zero |
//! | 12 per entry | stem offset (4), stem length (2), flag count (2), flag offset (4) |
//! | | stem pool: UTF-8 stems, sorted by their bytes |
//! | | flag pool: each stem's sorted flags, two bytes each |
//!
//! Entries are sorted by stem, and a stem's homonyms keep the order the hash bag yields them in.
//! Reading never trusts the file: each read is bounds checked, and a damaged entry is skipped.

use alloc::{boxed::Box, collections::BTreeMap, vec::Vec};
use core::{cmp::Ordering, fmt, hash::BuildHasher, num::NonZeroU16};

use crate::{hash_bag::HashBag, FlagSet, Flags, Stem};

pub(crate) const MAGIC: &[u8; 8] = b"SBINDEX1";
const HEADER_LEN: usize = 32;
const ENTRY_LEN: usize = 12;

/// FNV-1a, to tie a word index to the `.aff` file its flags came from.
pub(crate) fn aff_hash(aff: &str) -> u64 {
    aff.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

pub(crate) struct WordList<S> {
    parsed: HashBag<Stem, FlagSet, S>,
    index: Option<WordIndex>,
}

impl<S: Clone> Clone for WordList<S> {
    fn clone(&self) -> Self {
        // The index's bytes belong to the caller and can't be duplicated, so a clone keeps only
        // the parsed words. `Dictionary::clone` documents this.
        Self {
            parsed: self.parsed.clone(),
            index: None,
        }
    }
}

impl<S> WordList<S> {
    pub fn len(&self) -> usize {
        self.parsed.len() + self.index.as_ref().map_or(0, |index| index.entries)
    }

    /// Every stem with its flags, indexed ones first.
    pub fn iter(&self) -> impl Iterator<Item = (&str, Flags<'_>)> {
        self.index.iter().flat_map(WordIndex::iter).chain(
            self.parsed
                .iter()
                .map(|(stem, flags)| (stem.as_str(), Flags::from(flags))),
        )
    }
}

impl<S: BuildHasher> WordList<S> {
    pub fn with_capacity_and_hasher(capacity: usize, build_hasher: S) -> Self {
        Self {
            parsed: HashBag::with_capacity_and_hasher(capacity, build_hasher),
            index: None,
        }
    }

    pub fn set_index(&mut self, index: WordIndex) {
        self.index = Some(index);
    }

    pub fn insert(&mut self, stem: Stem, flags: FlagSet) {
        self.parsed.insert(stem, flags);
    }

    /// The entries for `word`, indexed ones first.
    pub fn get<'list, 'word>(
        &'list self,
        word: &'word str,
    ) -> impl Iterator<Item = (&'list str, Flags<'list>)> + use<'list, 'word, S> {
        self.index
            .iter()
            .flat_map(move |index| index.get(word))
            .chain(
                self.parsed
                    .get(word)
                    .map(|(stem, flags)| (stem.as_str(), Flags::from(flags))),
            )
    }

    /// The flags of the parsed entries for `word`. Indexed entries can't be changed.
    pub fn get_mut<'list, 'word>(
        &'list mut self,
        word: &'word str,
    ) -> impl Iterator<Item = &'list mut FlagSet> + use<'list, 'word, S> {
        self.parsed.get_mut(word)
    }

    /// This list in the word index format.
    pub fn to_index(&self, aff_hash: u64) -> Vec<u8> {
        let mut stems: Vec<&str> = self.iter().map(|(stem, _)| stem).collect();
        stems.sort_unstable();
        stems.dedup();

        let mut entries = Vec::with_capacity(self.len() * ENTRY_LEN);
        let mut pool = Vec::new();
        let mut flag_pool = Vec::new();
        let mut flag_offsets: BTreeMap<Vec<u16>, u32> = BTreeMap::new();
        let mut count = 0u32;
        for stem in stems {
            let stem_start = u32::try_from(pool.len()).expect("stem pool fits a word index");
            let stem_len = u16::try_from(stem.len()).expect("stems are shorter than 64 KiB");
            pool.extend_from_slice(stem.as_bytes());
            for (_, flags) in self.get(stem) {
                let flags: Vec<u16> = flags.iter().map(NonZeroU16::get).collect();
                let flag_count =
                    u16::try_from(flags.len()).expect("flag sets have at most 65535 flags");
                let flag_start = *flag_offsets.entry(flags).or_insert_with_key(|flags| {
                    let start =
                        u32::try_from(flag_pool.len()).expect("flag pool fits a word index");
                    for flag in flags {
                        flag_pool.extend_from_slice(&flag.to_le_bytes());
                    }
                    start
                });
                entries.extend_from_slice(&stem_start.to_le_bytes());
                entries.extend_from_slice(&stem_len.to_le_bytes());
                entries.extend_from_slice(&flag_count.to_le_bytes());
                entries.extend_from_slice(&flag_start.to_le_bytes());
                count += 1;
            }
        }

        let mut index =
            Vec::with_capacity(HEADER_LEN + entries.len() + pool.len() + flag_pool.len());
        index.extend_from_slice(MAGIC);
        index.extend_from_slice(&aff_hash.to_le_bytes());
        index.extend_from_slice(&count.to_le_bytes());
        index.extend_from_slice(
            &u32::try_from(pool.len())
                .expect("stem pool fits")
                .to_le_bytes(),
        );
        index.extend_from_slice(
            &u32::try_from(flag_pool.len())
                .expect("flag pool fits")
                .to_le_bytes(),
        );
        index.extend_from_slice(&0u32.to_le_bytes());
        index.extend_from_slice(&entries);
        index.extend_from_slice(&pool);
        index.extend_from_slice(&flag_pool);
        index
    }
}

/// Why a word index can't be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordIndexError {
    /// The bytes aren't a word index in this format.
    Format,
    /// The index was written for a different `.aff` file.
    OtherAffix,
}

impl fmt::Display for WordIndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Format => "not a word index in a known format",
            Self::OtherAffix => "the word index was written for a different .aff file",
        })
    }
}

/// A word index in bytes the caller owns, often a memory-mapped file.
pub(crate) struct WordIndex {
    bytes: Box<dyn AsRef<[u8]> + Send + Sync>,
    entries: usize,
    pool_start: usize,
    flags_start: usize,
}

struct Entry {
    stem_start: usize,
    stem_len: usize,
    flag_count: usize,
    flag_start: usize,
}

fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at.checked_add(4)?)?
        .try_into()
        .ok()
        .map(u32::from_le_bytes)
}

impl WordIndex {
    pub fn new(
        bytes: Box<dyn AsRef<[u8]> + Send + Sync>,
        aff_hash: u64,
    ) -> Result<Self, WordIndexError> {
        let data = (*bytes).as_ref();
        if data.get(..8) != Some(MAGIC.as_slice()) || data.len() < HEADER_LEN {
            return Err(WordIndexError::Format);
        }
        let hash = data[8..16]
            .try_into()
            .map(u64::from_le_bytes)
            .map_err(|_| WordIndexError::Format)?;
        if hash != aff_hash {
            return Err(WordIndexError::OtherAffix);
        }
        let field = |at| {
            read_u32(data, at)
                .map(|value| value as usize)
                .ok_or(WordIndexError::Format)
        };
        let entries = field(16)?;
        let pool_len = field(20)?;
        let flags_len = field(24)?;
        let pool_start = entries
            .checked_mul(ENTRY_LEN)
            .and_then(|len| len.checked_add(HEADER_LEN))
            .ok_or(WordIndexError::Format)?;
        let flags_start = pool_start
            .checked_add(pool_len)
            .ok_or(WordIndexError::Format)?;
        let end = flags_start
            .checked_add(flags_len)
            .ok_or(WordIndexError::Format)?;
        if end != data.len() || flags_len % 2 != 0 {
            return Err(WordIndexError::Format);
        }
        Ok(Self {
            bytes,
            entries,
            pool_start,
            flags_start,
        })
    }

    fn data(&self) -> &[u8] {
        (*self.bytes).as_ref()
    }

    fn entry(&self, i: usize) -> Option<Entry> {
        let data = self.data();
        let at = HEADER_LEN + i.checked_mul(ENTRY_LEN)?;
        let raw = data.get(at..at + ENTRY_LEN)?;
        Some(Entry {
            stem_start: u32::from_le_bytes(raw[0..4].try_into().ok()?) as usize,
            stem_len: u16::from_le_bytes(raw[4..6].try_into().ok()?) as usize,
            flag_count: u16::from_le_bytes(raw[6..8].try_into().ok()?) as usize,
            flag_start: u32::from_le_bytes(raw[8..12].try_into().ok()?) as usize,
        })
    }

    fn stem_bytes(&self, entry: &Entry) -> Option<&[u8]> {
        let start = self.pool_start.checked_add(entry.stem_start)?;
        let end = start.checked_add(entry.stem_len)?;
        if end > self.flags_start {
            return None;
        }
        self.data().get(start..end)
    }

    fn resolve(&self, entry: &Entry) -> Option<(&str, Flags<'_>)> {
        let stem = core::str::from_utf8(self.stem_bytes(entry)?).ok()?;
        let start = self.flags_start.checked_add(entry.flag_start)?;
        let end = start.checked_add(entry.flag_count.checked_mul(2)?)?;
        Some((stem, Flags::from_le_bytes(self.data().get(start..end)?)))
    }

    /// The first entry whose stem is not less than `word`.
    fn lower_bound(&self, word: &[u8]) -> usize {
        let (mut low, mut high) = (0, self.entries);
        while low < high {
            let middle = low + (high - low) / 2;
            let less = self
                .entry(middle)
                .and_then(|entry| self.stem_bytes(&entry))
                .is_some_and(|stem| stem.cmp(word) == Ordering::Less);
            if less {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        low
    }

    pub fn get<'index, 'word>(
        &'index self,
        word: &'word str,
    ) -> impl Iterator<Item = (&'index str, Flags<'index>)> + use<'index, 'word> {
        let start = self.lower_bound(word.as_bytes());
        (start..self.entries)
            .map_while(move |i| {
                let entry = self.entry(i)?;
                (self.stem_bytes(&entry)? == word.as_bytes()).then_some(entry)
            })
            .filter_map(move |entry| self.resolve(&entry))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, Flags<'_>)> {
        (0..self.entries).filter_map(move |i| self.resolve(&self.entry(i)?))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::Flag;

    #[test]
    fn damaged_indexes_are_rejected_or_read_safely() {
        let bag_entries = [
            ("cat", alloc::vec![1u16, 4]),
            ("cat", alloc::vec![2]),
            ("dog", alloc::vec![3]),
        ];
        let mut list = WordList::with_capacity_and_hasher(4, crate::DefaultHashBuilder::default());
        for (stem, flags) in bag_entries.iter() {
            let flags: Vec<Flag> = flags.iter().map(|flag| Flag::new(*flag).unwrap()).collect();
            list.insert(Stem::from(*stem), FlagSet::from(flags));
        }
        let bytes = list.to_index(7);
        assert!(matches!(
            WordIndex::new(Box::new(bytes.clone()), 8),
            Err(WordIndexError::OtherAffix)
        ));
        let mut truncated = bytes.clone();
        truncated.pop();
        assert!(matches!(
            WordIndex::new(Box::new(truncated), 7),
            Err(WordIndexError::Format)
        ));

        let index = WordIndex::new(Box::new(bytes.clone()), 7).unwrap();
        let cats: Vec<Vec<u16>> = index
            .get("cat")
            .map(|(_, flags)| flags.iter().map(NonZeroU16::get).collect())
            .collect();
        assert_eq!(cats.len(), 2);
        assert!(index.get("cow").next().is_none());

        // A stem offset pointing past the pool skips that entry instead of panicking.
        let mut damaged = bytes;
        damaged[HEADER_LEN..HEADER_LEN + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        let index = WordIndex::new(Box::new(damaged), 7).unwrap();
        assert!(index.iter().count() < 3);
        let _ = index.get("cat").count();
    }
}
