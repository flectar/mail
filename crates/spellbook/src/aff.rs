pub(crate) mod parser;

use crate::{
    alloc::{
        borrow::Cow,
        boxed::Box,
        string::{String, ToString},
        vec::Vec,
    },
    AffixingMode, Flag, FlagSet, AT_COMPOUND_BEGIN, AT_COMPOUND_END, FULL_WORD,
};

use core::{marker::PhantomData, num::NonZeroU16, str::Chars};

pub(crate) const HIDDEN_HOMONYM_FLAG: Flag = unsafe { Flag::new_unchecked(u16::MAX) };
pub(crate) const MAX_SUGGESTIONS: usize = 16;

macro_rules! has_flag {
    ( $flags:expr, $flag:expr ) => {{
        match $flag {
            Some(flag) => $flags.contains(&flag),
            None => false,
        }
    }};
}

/// The representation of a flag in a `.dic` or `.aff` file.
///
/// This representation also decides how we encode flags into `Flag`. This is controlled by the
/// `FLAG` directive in a `.aff` file.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) enum FlagType {
    /// A single ascii character.
    ///
    /// This is the default representation if a `.aff` file does not specify another.
    #[default]
    Short,
    /// Two adjacent ascii characters.
    ///
    /// The french dictionary uses this. For example for some proper nouns like `Asimov/L'D'Q'`:
    /// `L'` is a flag, `D'` another, `Q'` another.
    Long,
    /// A number in the range `1..=65000`.
    ///
    /// We will approximate this to `2^16`. Numeric flags are separated by commas.
    /// For example `actionfilm/70,7,252,976` from the Danish dictionary.
    Numeric,
    /// One UTF8 character described by at most two bytes.
    Utf8,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) struct Condition {
    /// The input pattern.
    ///
    /// The condition string is not transformed or compiled into a different input. We'll iterate
    /// over it directly to attempt to match the pattern.
    ///
    /// This string is non-empty.
    pattern: Box<str>,
    /// The number of `char`s that the pattern describes.
    ///
    /// `Condition` is such a small subset of regex that we can tell only from a linear scan of
    /// the input how many characters we will attempt to match.
    chars: usize,
    /// Whether `pattern` is a plain literal: it contains no wildcard (`.`) or character class
    /// (`[...]`) syntax.
    ///
    /// The overwhelming majority of conditions in real dictionaries are plain literals (for
    /// example the French dictionary has thousands of suffix conditions like `uire` or `er` and
    /// only a few hundred with character classes). For a literal, matching reduces to a
    /// `str::starts_with`/`str::ends_with` check which is backed by an optimized `memcmp` rather
    /// than the general character-by-character matcher.
    is_literal: bool,
}

impl Condition {
    pub fn matches(&self, input: &str) -> bool {
        let mut input = input.chars().peekable();
        let mut pattern = self.pattern.chars().peekable();

        loop {
            match (pattern.next(), input.next()) {
                // If we're at the end of both inputs or the pattern is shorter, this is a match.
                (None, _) => return true,
                (Some(_), None) => return false,
                // Wildcard: skip the input character.
                (Some('.'), Some(_)) => (),
                // Character classes
                (Some('['), Some(input_ch)) => {
                    let mut found = false;
                    let negative = pattern.next_if_eq(&'^').is_some();

                    for ch in pattern.by_ref() {
                        if ch == ']' {
                            break;
                        }

                        if ch == input_ch {
                            found = true;
                        }
                    }

                    // If it's a positive character class and the character isn't a member,
                    // this is not a match.
                    if !negative && !found {
                        return false;
                    }
                    // If it's a negative character class and the character _is_ a member,
                    // this is not a match.
                    if negative && found {
                        return false;
                    }
                }
                // Literals: the pattern character must equal the input character.
                (Some(pattern_ch), Some(input_ch)) => {
                    if pattern_ch != input_ch {
                        return false;
                    }
                }
            }
        }
    }
}

/// Internal container type for a prefix or suffix.
#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) struct Affix<K> {
    /// The flag that words may use to reference this affix.
    pub flag: Flag,
    /// Whether the affix is compatible with the opposite affix. For example a word that has both
    /// a prefix and a suffix, both the prefix and suffix should have `crossproduct: true`.
    pub crossproduct: bool,
    /// What is stripped from the stem when the affix is applied.
    pub strip: Option<Box<str>>,
    /// What should be added when the affix is applied.
    pub add: Box<str>,
    /// Condition that the stem should be checked against to query if the affix is relevant.
    ///
    /// This is optional in Spellbook. Hunspell and Nuspell represent what we say is `None` as
    /// `"."`. It's a pattern that always matches the input since the input to `condition_matches`
    /// is never empty.
    condition: Option<Condition>,
    /// Continuation flags.
    ///
    /// These are included with the `add` in `.aff` files (separated by `/`).
    // TODO: document how they're used.
    pub flags: FlagSet,
    phantom_data: PhantomData<K>,
}

impl<K: AffixKind> Affix<K> {
    pub fn new(
        flag: Flag,
        crossproduct: bool,
        strip: Option<&str>,
        add: &str,
        condition: Option<&str>,
        flags: FlagSet,
    ) -> Result<Self, parser::ConditionError> {
        let condition = condition.map(str::parse).transpose()?;

        Ok(Self {
            flag,
            crossproduct,
            strip: strip.map(|str| str.into()),
            add: add.into(),
            flags,
            condition,
            phantom_data: PhantomData,
        })
    }

    pub fn appending(&self) -> K::Chars<'_> {
        K::chars(&self.add)
    }

    #[inline]
    pub fn is_modifying(&self) -> bool {
        self.strip.is_some() || !self.add.is_empty()
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) struct Pfx;
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) struct Sfx;

/// Rules for replacing characters at the beginning of a stem.
pub(crate) type Prefix = Affix<Pfx>;
/// Rules for replacing characters at the end of a stem.
pub(crate) type Suffix = Affix<Sfx>;

/// A helper trait that, together with `Pfx` and `Sfx`, allows generically reading either
/// characters of a `&str` forwards or backwards.
///
/// This is a textbook ["lending iterator"] which uses a generic associated type to express that
/// the lifetime of the iterator is bound only to the input word.
///
/// ["lending iterator"]: https://rust-lang.github.io/generic-associated-types-initiative/design_patterns/iterable.html
pub(crate) trait AffixKind {
    type Chars<'a>: Iterator<Item = char>
    where
        Self: 'a;

    fn chars(word: &str) -> Self::Chars<'_>;

    // Reversed form of `affix_NOT_valid` from Nuspell.
    fn is_valid<const MODE: AffixingMode>(affix: &Affix<Self>, options: &AffOptions) -> bool
    where
        Self: Sized;
}

impl AffixKind for Pfx {
    type Chars<'a> = Chars<'a>;

    fn chars(word: &str) -> Self::Chars<'_> {
        word.chars()
    }

    fn is_valid<const MODE: AffixingMode>(prefix: &Prefix, options: &AffOptions) -> bool {
        if MODE == FULL_WORD && has_flag!(prefix.flags, options.only_in_compound_flag) {
            return false;
        }

        if MODE == AT_COMPOUND_END && !has_flag!(prefix.flags, options.compound_permit_flag) {
            return false;
        }

        if MODE != FULL_WORD && has_flag!(prefix.flags, options.compound_forbid_flag) {
            return false;
        }

        true
    }
}

impl AffixKind for Sfx {
    type Chars<'a> = core::iter::Rev<Chars<'a>>;

    fn chars(word: &str) -> Self::Chars<'_> {
        word.chars().rev()
    }

    fn is_valid<const MODE: AffixingMode>(suffix: &Suffix, options: &AffOptions) -> bool {
        if MODE == FULL_WORD && has_flag!(suffix.flags, options.only_in_compound_flag) {
            return false;
        }

        if MODE == AT_COMPOUND_BEGIN && !has_flag!(suffix.flags, options.compound_permit_flag) {
            return false;
        }

        if MODE != FULL_WORD && has_flag!(suffix.flags, options.compound_forbid_flag) {
            return false;
        }

        true
    }
}

impl Prefix {
    /// Converts a word which starts with this `Prefix` to the word's stem.
    ///
    /// The prefix's `add` is removed from the beginning and replaced with the `strip`.
    ///
    /// Nuspell calls this `to_root`.
    ///
    /// # Panics
    ///
    /// This function `expect`s that the `Prefix`'s `add` is a prefix of the input `word`.
    pub fn to_stem<'a>(&self, word: &'a str) -> Cow<'a, str> {
        let stripped = word
            .strip_prefix(&*self.add)
            .expect("to_stem should only be called when the `add` is a prefix of the word");

        match &self.strip {
            Some(strip) => {
                let mut stem = strip.to_string();
                stem.push_str(stripped);
                Cow::Owned(stem)
            }
            None => Cow::Borrowed(stripped),
        }
    }

    /// Converts a stem into a word starting with this `Prefix`.
    ///
    /// This prefix's `strip` is removed from the beginning and replaced with the `add`. This is
    /// the inverse of `Prefix::to_stem`.
    ///
    /// Nuspell calls this `to_derived.`
    ///
    /// # Panics
    ///
    /// This function `expect`s that the given `word` starts with this `Prefix`'s `strip`, if this
    /// prefix has a `strip`.
    pub fn to_derived(&self, word: &str) -> String {
        let stripped = match &self.strip {
            Some(strip) => word
                .strip_prefix(&**strip)
                .expect("to_derived should only be called when `strip` is a prefix of the word"),
            None => word,
        };
        let mut stem = self.add.to_string();
        stem.push_str(stripped);
        stem
    }

    pub fn condition_matches(&self, word: &str) -> bool {
        let condition = match self.condition.as_ref() {
            Some(condition) => condition,
            None => return true,
        };

        // Length in bytes is greater than or equal to length in chars.
        if word.len() < condition.chars {
            return false;
        }

        // Fast path: a literal condition is just a prefix check on the word.
        if condition.is_literal {
            return word.starts_with(&*condition.pattern);
        }

        condition.matches(word)
    }
}

impl Suffix {
    /// Converts a word which ends with this `Suffix` to the word's stem.
    ///
    /// This suffix's `add` is removed from the end and replaced with the `strip`.
    ///
    /// Nuspell calls this `to_root`.
    ///
    /// # Panics
    ///
    /// This function `expect`s that the `Suffix`'s `add` is a suffix of the input `word`.
    pub fn to_stem<'a>(&self, word: &'a str) -> Cow<'a, str> {
        let stripped = word
            .strip_suffix(&*self.add)
            .expect("to_stem should only be called when the `add` is a suffix of the word");

        match self.strip.as_deref() {
            Some(strip) => {
                let mut stem = stripped.to_string();
                stem.push_str(strip);
                Cow::Owned(stem)
            }
            None => Cow::Borrowed(stripped),
        }
    }

    /// Converts a stem into a word starting with this `Suffix`.
    ///
    /// This suffix's `strip` is removed from the end and replaced with the `add`. This is
    /// the inverse of `Suffix::to_stem`.
    ///
    /// Nuspell calls this `to_derived.`
    ///
    /// # Panics
    ///
    /// This function `expect`s that the given `word` ends with this `Suffix`'s `strip`, if this
    /// suffix has a `strip`.
    pub fn to_derived(&self, word: &str) -> String {
        let mut stem = match &self.strip {
            Some(strip) => word
                .strip_suffix(&**strip)
                .expect("to_derived should only be called when `strip` is a prefix of the word"),
            None => word,
        }
        .to_string();
        stem.push_str(&self.add);
        stem
    }

    pub fn condition_matches(&self, word: &str) -> bool {
        let condition = match self.condition.as_ref() {
            Some(condition) => condition,
            None => return true,
        };

        // Length in bytes is greater than or equal to length in chars.
        let len_bytes = word.len();
        if len_bytes < condition.chars {
            return false;
        }

        // Fast path: a literal condition is just a suffix check on the word.
        if condition.is_literal {
            return word.ends_with(&*condition.pattern);
        }

        let (chars, bytes) = word
            .char_indices()
            .rev()
            .take(condition.chars)
            .fold((0, 0), |(chars, _bytes), (byte_index, _ch)| {
                (chars + 1, len_bytes - byte_index)
            });

        if chars < condition.chars {
            return false;
        }
        condition.matches(&word[word.len() - bytes..])
    }
}

pub(crate) type PrefixIndex = AffixIndex<Pfx>;
pub(crate) type SuffixIndex = AffixIndex<Sfx>;

/// A data structure for looking up any affixes which might match a given word.
///
/// The `AffixIndex` is one of two central data structures, along with the `WordList`. It
/// functions very similarly to a [radix tree], allowing efficient lookup of prefix or suffix
/// rules.
///
/// For example a prefix from `en_US.aff` for "re":
///
/// ```text
/// PFX A Y 1
/// PFX A   0     re         .
/// ```
///
/// That prefix strips nothing (`0`) from the beginning and adds "re" to the beginning of any
/// words it is applied to.
///
/// For prefixes, `affixes_of` returns an iterator over all of the `Prefix`es in the table which
/// have an `add` field which is a prefix of the search word.
///
/// This structure also searches from the end of the word when looking up suffixes. A suffix from
/// `en_US.aff`:
///
/// ```text
/// SFX D Y 4
/// SFX D   0     d          e
/// SFX D   y     ied        [^aeiou]y
/// SFX D   0     ed         [^ey]
/// SFX D   0     ed         [aeiou]y
/// ```
///
/// Any word in the word list with the "D" flag can try to apply these suffixes. For a word like
/// "aced," `affixes_of` would return the first, third and fourth suffixes, as `d`, `ed` and `ed`
/// are suffixes of "aced," but not the second (`ied`).
///
/// Internally this type is implemented using a sorted `Vec` of affixes - one table for prefixes
/// and one for suffixes. Iterating with `affixes_of` first emits all affixes with empty `add`
/// text. Then we look at the first character in the search string. We can constrain our search
/// to only the elements in the table that start with that character using a precomputed index
/// of characters to indices within the table. After considering the first character, we use
/// linear searches of the remaining table slice to constrain the search for each next character
/// in the search key.
///
/// [radix tree]: https://en.wikipedia.org/wiki/Radix_tree
// TODO: I originally tried a hashing-based approach using `hashbrown::raw::RawTable`. Lift that
// structure from the commit history and benchmark it against this Vec based approach.
#[derive(Debug, Clone)]
pub(crate) struct AffixIndex<C> {
    table: Box<[Affix<C>]>,
    first_char: Box<[char]>,
    prefix_idx_with_first_char: Box<[usize]>,
    pub all_flags: FlagSet,
}

impl<C: AffixKind> FromIterator<Affix<C>> for AffixIndex<C> {
    fn from_iter<T: IntoIterator<Item = Affix<C>>>(iter: T) -> Self {
        let table: Vec<_> = iter.into_iter().collect();
        table.into()
    }
}

impl<C: AffixKind> From<Vec<Affix<C>>> for AffixIndex<C> {
    fn from(mut table: Vec<Affix<C>>) -> Self {
        // Sort the table lexiographically by key. We will use this lexiographical ordering to
        // efficiently search in AffixesIter.
        table.sort_unstable_by(|a, b| a.appending().cmp(b.appending()));

        let mut first_char = Vec::new();
        let mut prefix_idx_with_first_char = Vec::new();

        // Seek through the sorted table to the first element where the key is non-empty.
        let mut first_char_idx = table.partition_point(|affix| affix.appending().next().is_none());
        while first_char_idx < table.len() {
            let ch = table[first_char_idx]
                .appending()
                .next()
                .expect("vec is sorted so empty keys are before the partition point");

            // Save the first character of the key and the index of the affix in the table that
            // starts off this character. We can use this while reading the AffixIndex to jump
            // ahead efficiently in the table.
            first_char.push(ch);
            prefix_idx_with_first_char.push(first_char_idx);

            match table[first_char_idx..].iter().position(|affix| {
                affix
                    .appending()
                    .next()
                    .expect("vec is sorted so empty keys are before the partition point")
                    > ch
            }) {
                Some(next_char_index) => first_char_idx += next_char_index,
                None => break,
            }
        }
        // Add an extra element to the end so that `prefix_idx_with_first_char` is always one
        // element longer than `first_char`.
        prefix_idx_with_first_char.push(table.len());

        let flags = table
            .iter()
            .flat_map(|affix| affix.flags.iter().copied())
            .collect::<Vec<Flag>>()
            .into();

        Self {
            table: table.into(),
            first_char: first_char.into(),
            prefix_idx_with_first_char: prefix_idx_with_first_char.into(),
            all_flags: flags,
        }
    }
}

impl<C: AffixKind> AffixIndex<C> {
    pub fn affixes_of<'index, 'word>(
        &'index self,
        word: &'word str,
    ) -> AffixesIter<'index, 'word, C> {
        AffixesIter {
            table: &self.table,
            first_char: &self.first_char,
            prefix_idx_with_first_char: &self.prefix_idx_with_first_char,
            chars: C::chars(word),
            chars_matched: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn iter(&self) -> core::slice::Iter<'_, Affix<C>> {
        self.table.iter()
    }
}

/// An iterator over the prefixes/suffixes of a given word.
pub(crate) struct AffixesIter<'index, 'word, C: AffixKind + 'static> {
    table: &'index [Affix<C>],
    first_char: &'index [char],
    prefix_idx_with_first_char: &'index [usize],
    chars: C::Chars<'word>,
    chars_matched: usize,
}

impl<'index, C: AffixKind> Iterator for AffixesIter<'index, '_, C> {
    type Item = &'index Affix<C>;

    fn next(&mut self) -> Option<Self::Item> {
        // TODO: revisit this type. I suspect it's faster to just use `str::starts_with` and
        // `str::ends_with` rather than char iterators. Also using char iterators shouldn't
        // be necessary I think - we could switch to bytes instead.

        // Return all affixes that append nothing first.
        if self.chars_matched == 0 {
            if self.table.is_empty() {
                return None;
            }

            let item = &self.table[0];
            if item.appending().next().is_some() {
                // The empty portion of the table is done.
                // Scan ahead to where the first character is.
                let ch = self.chars.next()?;
                let first_char_idx = self.first_char.iter().position(|c| *c == ch)?;

                // NOTE: `prefix_idx_with_first_char` always has at least one element and is
                // always one element longer than `first_char`, so we can safely index at `0`
                // and at whatever index we get from `first_char` plus one.
                let empty_offset = self.prefix_idx_with_first_char[0];
                // Constrain the bounds of the search to affixes that share the first letter
                // of the key. Offset by the number of affixes with empty `add` that we emitted
                // previously.
                let start = self.prefix_idx_with_first_char[first_char_idx] - empty_offset;
                let end = self.prefix_idx_with_first_char[first_char_idx + 1] - empty_offset;
                self.table = &self.table[start..end];
                self.chars_matched = 1;
            } else {
                self.table = &self.table[1..];
                return Some(item);
            }
        }

        loop {
            if self.table.is_empty() {
                return None;
            }

            // If the search key is exactly matched so far (up to the number of characters we've
            // seen), emit the item.
            let item = &self.table[0];
            if item.appending().count() == self.chars_matched {
                self.table = &self.table[1..];
                return Some(item);
            }

            // Look at the next character in the search key. Limit the search to the slice of
            // the table where the nth character for each affix matches this character of the
            // search key.
            let ch = self.chars.next()?;
            let target = Some(ch);

            // The table is sorted lexicographically by `appending()`. Within this sub-slice every
            // affix shares the first `chars_matched` characters with the search key, so the slice
            // is sorted by the character at position `chars_matched`. (Affixes whose key is
            // exactly `chars_matched` characters long sort first but have already been emitted and
            // removed from the front by the `count` check above.) That lets us binary search for
            // the range of affixes whose nth character matches, rather than scanning linearly.
            let start = self
                .table
                .partition_point(|affix| affix.appending().nth(self.chars_matched) < target);
            let end = self
                .table
                .partition_point(|affix| affix.appending().nth(self.chars_matched) <= target);
            if start == end {
                return None;
            }
            self.table = &self.table[start..end];

            self.chars_matched += 1;
        }
    }
}

/// A collection of patterns used to break words into smaller words.
///
/// This is internally represented with a single `table` which is partitioned into three sections:
/// one for patterns that apply at the beginning of words, one for patterns that can apply
/// anywhere in the middle of a word, and one for patterns that must apply to the end of a word.
///
// TODO: document how breaks are used and what the patterns mean.
#[derive(Debug, Clone)]
pub(crate) struct BreakTable {
    table: Box<[Box<str>]>,
    start_word_breaks_last_idx: usize,
    // Nuspell keeps the entries partitioned in the order "start, end, middle." I've re-arranged
    // this to be "start, middle, end" since I think it's more natural.
    middle_word_breaks_last_idx: usize,
}

impl BreakTable {
    fn new(breaks: &[&str]) -> Self {
        let mut start = Vec::new();
        let mut middle = Vec::new();
        let mut end = Vec::new();

        for &b in breaks.iter() {
            // Break patterns are parsed in a way that ensures they are non-empty.
            assert!(!b.is_empty());

            if let Some(b) = b.strip_prefix('^') {
                start.push(b.into());
            } else if let Some(b) = b.strip_suffix('$') {
                end.push(b.into());
            } else {
                middle.push(b.into());
            }
        }

        let mut table = start;
        let start_word_breaks_last_idx = table.len();
        table.append(&mut middle);
        let middle_word_breaks_last_idx = table.len();
        table.append(&mut end);

        Self {
            table: table.into_boxed_slice(),
            start_word_breaks_last_idx,
            middle_word_breaks_last_idx,
        }
    }

    #[inline]
    pub fn start_word_breaks(&self) -> impl Iterator<Item = &str> {
        self.table[..self.start_word_breaks_last_idx]
            .iter()
            .map(AsRef::as_ref)
    }

    #[inline]
    pub fn middle_word_breaks(&self) -> impl Iterator<Item = &str> {
        self.table[self.start_word_breaks_last_idx..self.middle_word_breaks_last_idx]
            .iter()
            .map(AsRef::as_ref)
    }

    #[inline]
    pub fn end_word_breaks(&self) -> impl Iterator<Item = &str> {
        self.table[self.middle_word_breaks_last_idx..]
            .iter()
            .map(AsRef::as_ref)
    }
}

/// An ordered sequence of replacement pairs.
///
/// This is very similar to break patterns both in terms of the language used to describe them
/// in aff files and in representation. It's an ordered sequence with the replacements that only
/// apply:
///
/// 1. To entire words.
/// 2. At the beginning of words.
/// 3. At the end of words.
/// 4. Anywhere in the word.
///
/// Otherwise though it's basically a `Vec<(String, String)>`.
#[derive(Debug, Clone)]
pub(crate) struct ReplacementTable {
    table: Box<[(Box<str>, Box<str>)]>,
    whole_word_replacements_last_idx: usize,
    start_word_replacements_last_idx: usize,
    end_word_replacements_last_idx: usize,
}

impl From<Vec<(&str, String)>> for ReplacementTable {
    fn from(replacements: Vec<(&str, String)>) -> Self {
        let mut whole = Vec::new();
        let mut start = Vec::new();
        let mut end = Vec::new();
        let mut anywhere = Vec::new();

        for (from, to) in replacements.into_iter() {
            // Replacements are parsed in a way that ensures they are non-empty.
            assert!(!from.is_empty() && !to.is_empty());

            if let Some(from) = from.strip_prefix('^') {
                if let Some(from) = from.strip_suffix('$') {
                    // Starts with '^' and ends with '$' matches the whole word.
                    // Seems to be rarely if ever used in practice.
                    whole.push((from.into(), to.into()));
                } else {
                    // Only starts with '^' - applies to the start of a word.
                    start.push((from.into(), to.into()));
                }
            } else if let Some(from) = from.strip_suffix('$') {
                // Only ends with '$' - applies to the end of a word.
                end.push((from.into(), to.into()));
            } else {
                // Doesn't have an anchor - applies anywhere in the word.
                anywhere.push((from.into(), to.into()));
            }
        }

        let mut table = whole;
        let whole_word_replacements_last_idx = table.len();
        table.append(&mut start);
        let start_word_replacements_last_idx = table.len();
        table.append(&mut end);
        let end_word_replacements_last_idx = table.len();
        table.append(&mut anywhere);

        Self {
            table: table.into_boxed_slice(),
            whole_word_replacements_last_idx,
            start_word_replacements_last_idx,
            end_word_replacements_last_idx,
        }
    }
}

impl ReplacementTable {
    #[inline]
    pub fn whole_word_replacements(&self) -> impl Iterator<Item = (&str, &str)> {
        self.table[..self.whole_word_replacements_last_idx]
            .iter()
            .map(|(from, to)| (from.as_ref(), to.as_ref()))
    }

    #[inline]
    pub fn start_word_replacements(&self) -> impl Iterator<Item = (&str, &str)> {
        self.table[self.whole_word_replacements_last_idx..self.start_word_replacements_last_idx]
            .iter()
            .map(|(from, to)| (from.as_ref(), to.as_ref()))
    }

    #[inline]
    pub fn end_word_replacements(&self) -> impl Iterator<Item = (&str, &str)> {
        self.table[self.start_word_replacements_last_idx..self.end_word_replacements_last_idx]
            .iter()
            .map(|(from, to)| (from.as_ref(), to.as_ref()))
    }

    #[inline]
    pub fn any_place_replacements(&self) -> impl Iterator<Item = (&str, &str)> {
        self.table[self.end_word_replacements_last_idx..]
            .iter()
            .map(|(from, to)| (from.as_ref(), to.as_ref()))
    }

    /// Whether the table only has whole word replacements.
    #[inline]
    pub fn has_only_whole_word_replacements(&self) -> bool {
        self.whole_word_replacements_last_idx == self.table.len()
    }
}

/// Individual rules of COMPOUNDRULE patterns.
///
/// Compound rules are a very small regex-like language for describing how stems might be joined
/// in a compound. Each element might be a flag, a zero-or-one wildcard (`?`) or a zero-or-more
/// wildcard (`*`). Typically dictionaries use these to describe how to compound numbers
/// together. The [Spylls docs for `CompoundRule`](https://spylls.readthedocs.io/en/latest/hunspell/data_aff.html?highlight=compound%20rule#spylls.hunspell.data.aff.CompoundRule)
/// have an excellent explanation of a common use-case for compound rules.
///
/// # Representation
///
/// Nuspell doesn't special case `*` and `?` modifiers. It stores the entire rule as a string
/// of `char16_t` (which is also Nuspell flag type). This is quite clever because it allows
/// Nuspell to only spend two bytes per element to store the rule. `CompoundRuleElement` is
/// 4 bytes in comparison. The tradeoff is ambiguity for some `FlagType` representations and more
/// complicated matching code. If a `.aff` file used `FlagType::Numeric`, `*` would be
/// indistinguishable from 42 and `?` indistinguishable from 63. In practice this doesn't seem to
/// be a problem. Nuspell looks ahead in the rule string to find wildcards when matching which is
/// not much more work but is more complicated to understand.
///
/// We use a `[CompoundRuleElement]` in Spellbook only for clarity. Few dictionaries use
/// compound rules and those that do use them tend to use 12 or fewer entries in the table, with
/// each rule being only a few elements long.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompoundRuleElement {
    pub flag: Flag,
    pub modifier: Option<CompoundRuleModifier>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompoundRuleModifier {
    ZeroOrOne,
    ZeroOrMore,
}

type CompoundRule = Box<[CompoundRuleElement]>;

fn compound_rule_matches(pattern: &[CompoundRuleElement], data: &[&FlagSet]) -> bool {
    use crate::alloc::vec;
    use CompoundRuleModifier::*;

    // TODO: try reimplementing with recursion instead of a Vec-based stack?
    let mut stack = vec![(0, 0)];

    while let Some((pattern_idx, data_idx)) = stack.pop() {
        if pattern_idx == pattern.len() {
            return data_idx == data.len();
        }

        // Nuspell does this a little differently. As mentioned in the `CompoundRuleElement` docs
        // they don't preprocess the rules so the wildcards remain in the strings as `*`/`?`
        // characters. It looks ahead (`pattern_idx + 1`) to find the modifier and when it skips
        // over the modifier it jumps ahead by 2. We jump ahead by one below and use the modifier
        // we parsed instead.

        let flag_matches = match data.get(data_idx) {
            Some(flagset) => flagset.contains(&pattern[pattern_idx].flag),
            None => false,
        };
        match pattern[pattern_idx].modifier {
            Some(ZeroOrOne) => {
                // Try as if the flag didn't match (allowed since the pattern may match 0 times).
                stack.push((pattern_idx + 1, data_idx));
                if flag_matches {
                    // Try as if it did match: consume the pattern token and move on.
                    stack.push((pattern_idx + 1, data_idx + 1));
                }
            }
            Some(ZeroOrMore) => {
                // Try as if the flag didn't match (allowed since the pattern may match 0 times).
                stack.push((pattern_idx + 1, data_idx));
                if flag_matches {
                    // Try as if it did match. Don't consume the pattern token because it can
                    // match infinitely.
                    stack.push((pattern_idx, data_idx + 1));
                }
            }
            None => {
                // Without a modifier, only continue searching if the flag matched.
                if flag_matches {
                    stack.push((pattern_idx + 1, data_idx + 1));
                }
            }
        }
    }

    false
}

/// A set of rules that can be used to detect whether constructed compounds are allowed.
///
/// TODO: talk about wildcards, show a compounding example.
#[derive(Debug, Clone)]
pub(crate) struct CompoundRuleTable {
    rules: Box<[CompoundRule]>,
    all_flags: FlagSet,
}

impl From<Vec<CompoundRule>> for CompoundRuleTable {
    fn from(rules: Vec<CompoundRule>) -> Self {
        let all_flags: Vec<_> = rules
            .iter()
            .flat_map(|rule| rule.iter().map(|el| el.flag))
            .collect();

        Self {
            rules: rules.into_boxed_slice(),
            all_flags: all_flags.into(),
        }
    }
}

impl CompoundRuleTable {
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Checks whether the given flagset has any flags in common with flags used in any compound
    /// rule.
    #[inline]
    pub fn has_any_flags(&self, flagset: &FlagSet) -> bool {
        self.all_flags.has_intersection(flagset)
    }

    /// Checks whether any rule in the compound table matches the sequence of flagsets.
    ///
    /// Also see Checker's `check_compound_with_rules_impl`. This function is passed a possible
    /// interpretation of a compound. Consider "10th" and the en_US dictionary. The checker might
    /// split up the word to be "1/n1" (L4) and "0th/pt" (L3). So this function would be passed
    /// `&[flagset!['n', '1'], flagset!['p', 't']]`. The matching logic in compound_rule_matches
    /// uses `FlagSet::contains` on these flagsets to look up whether the flagset matches the
    /// pattern. en_US defines the compounding for this as `n*1t` - `n` being a flag on any single
    /// digit, `*` meaning as many of those as you want (including none) and then a required `1`
    /// flag and `t` flag - which match the input. compound_rule_matches tries different
    /// interpretations of the modifiers to check if the pattern matches.
    pub fn any_rule_matches(&self, flagsets: &[&FlagSet]) -> bool {
        self.rules
            .iter()
            .any(|rule| compound_rule_matches(rule, flagsets))
    }
}

/// Storage for two strings within the allocation of one `Box<str>`.
#[derive(Debug, Clone)]
pub(crate) struct StrPair {
    inner: Box<str>,
    /// The `.len()` of the first string: the index of the partition between the first and
    /// second string.
    partition: usize,
}

impl StrPair {
    pub fn new(left: &str, right: &str) -> Self {
        let mut string = left.to_string();
        let partition = string.len();
        string.push_str(right);

        Self {
            inner: string.into(),
            partition,
        }
    }

    #[inline]
    pub fn full_str(&self) -> &str {
        &self.inner
    }

    #[inline]
    pub fn partition(&self) -> usize {
        self.partition
    }
}

#[derive(Debug, Clone)]
pub(crate) struct CompoundPattern {
    pub begin_end_chars: StrPair,
    // TODO: unimplemented. See Checker::check_compound_with_pattern_replacements.
    #[allow(dead_code)]
    pub replacement: Option<Box<str>>,
    pub first_word_flag: Option<Flag>,
    pub second_word_flag: Option<Flag>,
    pub match_first_only_unaffixed_or_zero_affixed: bool,
}

#[derive(Debug, Clone)]
struct Conversion {
    from: Box<str>,
    to: Box<str>,
    /// ICONV/OCONV use '_' at the end of the pattern text like '$' in regex: if the match only
    /// applies at the end of the word.
    anchor_end: bool,
}

/// A compact set of byte values, stored as a 256-bit bitset (32 bytes rather than the 256 bytes a
/// `[bool; 256]` would take). `contains` is a shift and mask, so the smaller footprint comes at
/// no measurable cost to the per-byte probe done while converting words.
#[derive(Debug, Clone, Default)]
struct ByteSet([u64; 4]);

impl ByteSet {
    fn insert(&mut self, byte: u8) {
        self.0[(byte >> 6) as usize] |= 1 << (byte & 63);
    }

    fn contains(&self, byte: u8) -> bool {
        self.0[(byte >> 6) as usize] & (1 << (byte & 63)) != 0
    }
}

/// The conversion table used by ICONV and OCONV rules.
///
/// This is nothing more than a sequence of `(from, to)` replacement pairs. Not many dictionaries
/// use this rule. en_US and a few others use it to replace magic apostrophes "’" with regular
/// ones. Others like french have quite a few rules to normalize similar looking and meaning
/// unicode representations of letters, like "à" becoming "à".
#[derive(Debug, Clone)]
pub(crate) struct ConversionTable {
    inner: Box<[Conversion]>,
    /// The set of bytes that any conversion pattern can start with.
    ///
    /// `convert` is called on every checked word (and every suggestion candidate), but most words
    /// contain none of the conversion patterns. Before walking the word we check whether it
    /// contains any byte that could begin a pattern; if not, there's nothing to convert and we
    /// return the word untouched. A 256-bit set indexed by `u8`; see [`ByteSet`].
    possible_start_bytes: ByteSet,
    /// Whether every conversion pattern contains at least one non-ASCII byte.
    ///
    /// When this holds, a purely ASCII word cannot contain any pattern as a substring, so there is
    /// nothing to convert. This is the common, cheap rejection for dictionaries like French whose
    /// conversion patterns are all accented/decomposed characters, ligatures and fancy
    /// apostrophes: a plain ASCII word like "test" skips conversion entirely. Note that the
    /// per-byte filter alone is not enough here - French stores patterns in decomposed form (e.g.
    /// "é" as "e" + a combining accent), so their starting byte is an ASCII vowel that ordinary
    /// words contain.
    all_patterns_non_ascii: bool,
}

impl From<Vec<(&str, &str)>> for ConversionTable {
    fn from(table: Vec<(&str, &str)>) -> Self {
        let mut possible_start_bytes = ByteSet::default();
        let mut inner: Vec<Conversion> = table
            .into_iter()
            .map(|(from, to)| {
                let (from, anchor_end) = if let Some(from) = from.strip_suffix('_') {
                    (from, true)
                } else {
                    (from, false)
                };
                if let Some(&byte) = from.as_bytes().first() {
                    possible_start_bytes.insert(byte);
                }
                Conversion {
                    to: to.into(),
                    from: from.into(),
                    anchor_end,
                }
            })
            .collect();

        // Sort ascending by the (stripped) pattern text. `convert` relies on this ordering to
        // binary search for the patterns sharing a given leading byte rather than scanning the
        // whole table for every position in every word.
        inner.sort_unstable_by(|a, b| a.from.cmp(&b.from));

        let all_patterns_non_ascii = !inner.is_empty() && inner.iter().all(|c| !c.from.is_ascii());

        Self {
            inner: inner.into(),
            possible_start_bytes,
            all_patterns_non_ascii,
        }
    }
}

impl ConversionTable {
    /// Finds the longest conversion whose pattern matches at the very start of `tail`.
    ///
    /// The table is sorted by pattern, so all patterns sharing `tail`'s first byte form a
    /// contiguous run which we locate with a binary search. Real conversion tables have only a
    /// handful of patterns per leading byte, so the subsequent scan of that run is short - far
    /// cheaper than the substring search over the whole table that a naive implementation would do
    /// at every position.
    fn match_at(&self, tail: &str) -> Option<&Conversion> {
        let first = *tail.as_bytes().first()?;

        // Patterns with an empty `from` (and any byte less than `first`) sort before this point and
        // are skipped. The run we want starts here and ends when the leading byte changes.
        let start = self
            .inner
            .partition_point(|c| c.from.as_bytes().first().copied() < Some(first));

        let mut best: Option<&Conversion> = None;
        for conversion in self.inner[start..].iter() {
            if conversion.from.as_bytes().first().copied() != Some(first) {
                break;
            }

            let matches = if conversion.anchor_end {
                // An end-anchored pattern only matches if it consumes the rest of the word.
                tail == conversion.from.as_ref()
            } else {
                tail.starts_with(conversion.from.as_ref())
            };

            // Favor the longest matching pattern.
            if matches && best.map_or(true, |b| conversion.from.len() > b.from.len()) {
                best = Some(conversion);
            }
        }

        best
    }

    /// Applies any ICONV/OCONV conversions to the given word.
    ///
    /// Walks the word left to right replacing the longest pattern that matches at each position
    /// (longer patterns are replaced before shorter ones). After a replacement, scanning resumes
    /// after the replacement text.
    pub fn convert<'a>(&self, word: &'a str) -> Cow<'a, str> {
        // Most dictionaries have no ICONV/OCONV rules at all (and even those that do usually leave
        // one of the two tables empty), so the table is empty far more often than not. Bail in O(1)
        // rather than scanning the word with the byte filter below.
        if self.inner.is_empty() {
            return Cow::Borrowed(word);
        }

        // Fast path: skip the whole walk when no pattern can possibly match. `convert` runs on
        // every checked word and every suggestion candidate, but the overwhelmingly common case is
        // that nothing matches.
        //
        // First, if every pattern needs a non-ASCII byte then an all-ASCII word matches none of
        // them. `str::is_ascii` is SIMD-optimized, so this is very cheap and handles the common
        // case for dictionaries like French. Otherwise fall back to checking whether the word
        // contains any byte a pattern could start with (which also covers an empty table).
        if self.all_patterns_non_ascii && word.is_ascii() {
            return Cow::Borrowed(word);
        }
        if !word
            .as_bytes()
            .iter()
            .any(|&byte| self.possible_start_bytes.contains(byte))
        {
            return Cow::Borrowed(word);
        }

        let mut word = Cow::Borrowed(word);

        let mut i = 0;
        while i < word.len() {
            let Some(conversion) = self.match_at(&word[i..]) else {
                // No pattern matches here; advance to the next character.
                i += word[i..].chars().next().map_or(1, |ch| ch.len_utf8());
                continue;
            };

            let from_len = conversion.from.len();
            let advance = conversion.to.len();
            let mut string = word.into_owned();
            string.replace_range(i..i + from_len, &conversion.to);
            word = Cow::Owned(string);
            i += advance;
        }

        word
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) enum CaseHandling {
    /// Turkish, Azerbaijani and Crimean Tartar. These locales require some extra replacement
    /// logic for i-like characters.
    Turkic,
    /// Anything else. These locales can use the standard library's case mapping functions.
    #[default]
    Standard,
}

impl CaseHandling {
    // Casing primitives.
    // The standard library covers us most of the way but there's a locale-specific casing rule
    // that applies to Turkic locales: Turkish, Azerbaijani and Crimean Tartar. In Turkic
    // locales, "i" is uppercased as "İ" and "I" is lowercased as "ı".
    // TODO: optimize. See the standard library's optimizations.

    pub fn lowercase(&self, word: &str) -> String {
        match self {
            Self::Turkic => word.replace('I', "ı").replace('İ', "i").to_lowercase(),
            Self::Standard => word.to_lowercase(),
        }
    }

    pub fn uppercase(&self, word: &str) -> String {
        match self {
            Self::Turkic => word.replace('i', "İ").replace('ı', "I").to_uppercase(),
            Self::Standard => word.to_uppercase(),
        }
    }

    pub fn titlecase(&self, word: &str) -> String {
        let mut output = String::with_capacity(word.len());
        let mut chars = word.chars();
        let first = chars.next().expect("non-empty input");
        match self {
            Self::Turkic if first == 'i' => output.push('İ'),
            Self::Turkic if first == 'ı' => output.push('I'),
            _ => output.extend(first.to_uppercase()),
        }
        for ch in chars {
            match self {
                Self::Turkic if ch == 'I' => output.push('ı'),
                Self::Turkic if ch == 'İ' => output.push('i'),
                _ => output.extend(ch.to_lowercase()),
            }
        }
        output
    }

    pub fn lower_first_char(&self, word: &str) -> String {
        let mut output = String::with_capacity(word.len());
        let mut chars = word.char_indices();
        let (_, first) = chars.next().expect("non-empty input");
        match self {
            Self::Turkic if first == 'I' => output.push('ı'),
            Self::Turkic if first == 'İ' => output.push('i'),
            _ => output.extend(first.to_lowercase()),
        }
        if let Some((idx, _)) = chars.next() {
            output.push_str(&word[idx..]);
        }
        output
    }

    pub fn upper_char_at(&self, word: &str, idx: usize) -> String {
        let mut output = String::with_capacity(word.len());
        output.push_str(&word[..idx]);
        let mut chars = word[idx..].char_indices();
        let (_, ch) = chars.next().expect("a char at the given index");
        match self {
            Self::Turkic if ch == 'ı' => output.push('I'),
            Self::Turkic if ch == 'i' => output.push('İ'),
            _ => output.extend(ch.to_uppercase()),
        }
        if let Some((next_idx, _)) = chars.next() {
            output.push_str(&word[idx + next_idx..]);
        }
        output
    }

    /// Checks whether `left` is equal to `right` when `right` is lowercase.
    pub fn is_char_eq_lowercase(&self, left: char, right: char) -> bool {
        match (self, left, right) {
            (Self::Turkic, 'ı', 'I') => return true,
            (Self::Turkic, 'i', 'İ') => return true,
            _ => (),
        }

        let mut lower_iter = right.to_lowercase();
        if lower_iter.len() != 1 {
            return false;
        }
        let lower = lower_iter.next().unwrap();
        left == lower
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SimilarityGroup {
    pub chars: Box<str>,
    pub strings: Box<[Box<str>]>,
}

#[derive(Debug, Clone)]
pub(crate) struct AffData {
    // checking options
    pub prefixes: PrefixIndex,
    pub suffixes: SuffixIndex,
    pub break_table: BreakTable,
    pub compound_rules: CompoundRuleTable,
    pub compound_syllable_vowels: Box<str>,
    pub compound_patterns: Box<[CompoundPattern]>,
    pub input_conversions: ConversionTable,
    pub output_conversions: ConversionTable,
    // locale TODO
    // suggestion options
    pub replacements: ReplacementTable,
    pub similarities: Box<[SimilarityGroup]>,
    // phonetic_table: PhoneticTable,
    pub ignore_chars: Box<[char]>,
    pub keyboard_closeness: Box<str>,
    pub try_chars: Box<str>,
    pub options: AffOptions,
    // Parsing options. These are preserved so that we can re-use them in `Dictionary::add`.
    pub flag_type: FlagType,
    pub flag_aliases: Box<[FlagSet]>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AffOptions {
    pub complex_prefixes: bool,
    pub fullstrip: bool,
    pub checksharps: bool,
    pub forbid_warn: bool,
    pub only_in_compound_flag: Option<Flag>,
    pub circumfix_flag: Option<Flag>,
    pub forbidden_word_flag: Flag,
    pub keep_case_flag: Option<Flag>,
    pub need_affix_flag: Option<Flag>,
    pub warn_flag: Option<Flag>,
    // compounding options
    pub compound_flag: Option<Flag>,
    pub compound_begin_flag: Option<Flag>,
    pub compound_middle_flag: Option<Flag>,
    pub compound_end_flag: Option<Flag>,
    // These `Option<NonZeroU16>`s represent counts or sizes and a zero value isn't accepted.
    // Being the same as a flag's representation is coincidence.
    pub compound_min_length: Option<NonZeroU16>,
    pub compound_max_word_count: Option<NonZeroU16>,
    pub compound_permit_flag: Option<Flag>,
    pub compound_forbid_flag: Option<Flag>,
    pub compound_root_flag: Option<Flag>,
    pub compound_force_uppercase_flag: Option<Flag>,
    pub compound_more_suffixes: bool,
    pub compound_check_duplicate: bool,
    pub compound_check_rep: bool,
    pub compound_check_case: bool,
    pub compound_check_triple: bool,
    pub compound_simplified_triple: bool,
    pub compound_syllable_num: bool,
    pub compound_syllable_max: Option<NonZeroU16>,
    pub max_compound_suggestions: u16,
    pub no_suggest_flag: Option<Flag>,
    pub substandard_flag: Option<Flag>,
    pub max_ngram_suggestions: u16,
    pub max_diff_factor: u16,
    pub only_max_diff: bool,
    pub no_split_suggestions: bool,
    pub suggest_with_dots: bool,
    pub case_handling: CaseHandling,
}

impl Default for AffOptions {
    fn default() -> Self {
        Self {
            // Hunspell:
            // // default flags
            // #define DEFAULTFLAGS 65510
            // #define FORBIDDENWORD 65510
            // #define ONLYUPCASEFLAG 65511
            complex_prefixes: Default::default(),
            fullstrip: Default::default(),
            checksharps: Default::default(),
            forbid_warn: Default::default(),
            only_in_compound_flag: Default::default(),
            circumfix_flag: Default::default(),
            forbidden_word_flag: Flag::new(65510).unwrap(),
            keep_case_flag: Default::default(),
            need_affix_flag: Default::default(),
            warn_flag: Default::default(),
            compound_flag: Default::default(),
            compound_begin_flag: Default::default(),
            compound_middle_flag: Default::default(),
            compound_end_flag: Default::default(),
            compound_min_length: Default::default(),
            compound_max_word_count: Default::default(),
            compound_permit_flag: Default::default(),
            compound_forbid_flag: Default::default(),
            compound_root_flag: Default::default(),
            compound_force_uppercase_flag: Default::default(),
            compound_more_suffixes: Default::default(),
            compound_check_duplicate: Default::default(),
            compound_check_rep: Default::default(),
            compound_check_case: Default::default(),
            compound_check_triple: Default::default(),
            compound_simplified_triple: Default::default(),
            compound_syllable_num: Default::default(),
            compound_syllable_max: Default::default(),
            max_compound_suggestions: 3,
            no_suggest_flag: Default::default(),
            substandard_flag: Default::default(),
            max_ngram_suggestions: 5,
            max_diff_factor: 5,
            only_max_diff: Default::default(),
            no_split_suggestions: Default::default(),
            suggest_with_dots: Default::default(),
            case_handling: Default::default(),
        }
    }
}

impl AffOptions {
    pub fn allows_compounding(&self) -> bool {
        self.compound_flag.is_some()
            || self.compound_begin_flag.is_some()
            || self.compound_middle_flag.is_some()
            || self.compound_end_flag.is_some()
    }
}

#[cfg(test)]
mod test {
    use super::*;

    macro_rules! flag {
        ( $x:expr ) => {{
            Flag::new($x as u16).unwrap()
        }};
    }
    macro_rules! flagset {
        () => {{
            FlagSet::default()
        }};
        ( $( $x:expr ),* ) => {
            {
                FlagSet::from( $crate::alloc::vec![ $( Flag::new( $x as u16 ).unwrap() ),* ] )
            }
        };
    }

    #[test]
    fn condition_matches() {
        // No special characters
        assert!("foo".parse::<Condition>().unwrap().matches("foo"));

        // Fast lane: the input is shorter (bytes) than the number of characters in the pattern.
        assert!(!"foo".parse::<Condition>().unwrap().matches("fo"));

        // Positive character class
        let condition = "xx[abc]x".parse::<Condition>().unwrap();
        assert!(condition.matches("xxax"));
        assert!(condition.matches("xxbx"));
        assert!(condition.matches("xxcx"));
        assert!(!condition.matches("xxdx"));

        // Negative character class
        let condition = "xx[^abc]x".parse::<Condition>().unwrap();
        assert!(!condition.matches("xxax"));
        assert!(!condition.matches("xxbx"));
        assert!(!condition.matches("xxcx"));
        assert!(condition.matches("xxdx"));
    }

    #[test]
    fn condition_nuspell_unit_test() {
        // Upstream: <https://github.com/nuspell/nuspell/blob/349e0d6bc68b776af035ca3ff664a7fc55d69387/tests/unit_test.cxx#L167-L299>
        // Our structure for Condition is different so we only port the prefix tests.
        let cond = "abcd".parse::<Condition>().unwrap();
        assert!(cond.matches("abcd"));
        assert!(cond.matches("abcdXYZ"));
        assert!(cond.matches("abcdБВГДШ\u{ABCD}\u{10ABCD}"));
        assert!(!cond.matches(""));
        assert!(!cond.matches("abc"));
        assert!(!cond.matches("abcX"));
        assert!(!cond.matches("XYZ"));
        assert!(!cond.matches("АаБбВвГгШш\u{ABCD}\u{10ABCD}"));

        let cond = "[vbn]".parse::<Condition>().unwrap();
        assert!(cond.matches("v"));
        assert!(cond.matches("vAAш"));
        assert!(cond.matches("b"));
        assert!(cond.matches("bBBш"));
        assert!(cond.matches("n"));
        assert!(cond.matches("nCCш"));
        assert!(!cond.matches(""));
        assert!(!cond.matches("Q"));
        assert!(!cond.matches("Qqqq"));
        assert!(!cond.matches("1342234"));
        assert!(!cond.matches("V"));
        assert!(!cond.matches("бвгдш"));

        let cond = "[бш\u{1234}]".parse::<Condition>().unwrap();
        assert!(cond.matches("б"));
        assert!(cond.matches("бeT"));
        assert!(cond.matches("ш"));
        assert!(cond.matches("шок"));
        assert!(cond.matches("\u{1234}кош"));
        assert!(!cond.matches(""));
        assert!(!cond.matches("Q"));
        assert!(!cond.matches("Qqqq"));
        assert!(!cond.matches("пан"));
        assert!(!cond.matches("\u{ABCD}\u{1234}"));
        assert!(!cond.matches("вбгдш"));

        let cond = "[^zш\u{1234}\u{10ABCD}]".parse::<Condition>().unwrap();
        assert!(!cond.matches("z"));
        assert!(!cond.matches("ш"));
        assert!(!cond.matches("\u{1234}"));
        assert!(!cond.matches("\u{10ABCD}"));
        assert!(!cond.matches("zљње"));
        assert!(!cond.matches("шabc"));
        assert!(!cond.matches("\u{1234} ytyty"));
        assert!(!cond.matches("\u{10ABCD} tytyty"));
        assert!(cond.matches("q"));
        assert!(cond.matches("r"));
        assert!(cond.matches("\u{FFFD}"));
        assert!(cond.matches("\u{10FFFF}"));
        assert!(cond.matches("qљње"));
        assert!(cond.matches("фabc"));
        assert!(cond.matches("\u{FFFD} ytyty"));
        assert!(cond.matches("\u{10FFFF} tytyty"));

        let cond = "abc АБВ..[zбш\u{1234}][^zш\u{1234}\u{10ABCD}]X"
            .parse::<Condition>()
            .unwrap();
        assert!(cond.matches("abc АБВ \u{2345}z\u{011111}X"));
        assert!(cond.matches("abc АБВ\u{2345} ш\u{011112}Xопop"));
        assert!(!cond.matches("abc ШШШ \u{2345}z\u{011111}X"));
        assert!(!cond.matches("abc АБВ\u{2345} t\u{011112}Xопop"));
        assert!(!cond.matches("abc АБВ \u{2345}z\u{1234}X"));
    }

    #[test]
    fn string_pair() {
        let pair = StrPair::new("foo", "bar");
        assert_eq!(pair.full_str(), "foobar");

        let pair = StrPair::new("", "");
        assert_eq!(pair.full_str(), "")
    }

    #[test]
    fn break_table_nuspell_unit_test() {
        // Upstream: <https://github.com/nuspell/nuspell/blob/349e0d6bc68b776af035ca3ff664a7fc55d69387/tests/unit_test.cxx#L100-L127>
        let table = BreakTable::new(&[
            "bsd", "zxc", "asd", "^bar", "^zoo", "^abc", "car$", "yoyo$", "air$",
        ]);

        let mut starts: Vec<_> = table.start_word_breaks().collect();
        starts.sort_unstable();
        assert_eq!(&["abc", "bar", "zoo"], starts.as_slice());

        let mut middles: Vec<_> = table.middle_word_breaks().collect();
        middles.sort_unstable();
        assert_eq!(&["asd", "bsd", "zxc"], middles.as_slice());

        let mut ends: Vec<_> = table.end_word_breaks().collect();
        ends.sort_unstable();
        assert_eq!(&["air", "car", "yoyo"], ends.as_slice());
    }

    #[test]
    fn prefix_suffix_nuspell_unit_test() {
        // Upstream: <https://github.com/nuspell/nuspell/blob/349e0d6bc68b776af035ca3ff664a7fc55d69387/tests/unit_test.cxx#L301-L313>
        let prefix = Prefix::new(flag!('F'), false, Some("qw"), "Qwe", None, flagset![]).unwrap();
        assert_eq!(prefix.to_derived("qwrty").as_str(), "Qwerty");
        assert_eq!(prefix.to_stem("Qwerty").as_ref(), "qwrty");

        let suffix = Suffix::new(flag!('F'), false, Some("ie"), "ying", None, flagset![]).unwrap();
        assert_eq!(suffix.to_derived("pie").as_str(), "pying");
        assert_eq!(suffix.to_stem("pying").as_ref(), "pie");
    }

    #[test]
    fn empty_affix_index() {
        let index: PrefixIndex = [].into_iter().collect();
        assert!(index.affixes_of("anything").next().is_none());

        let index: SuffixIndex = [].into_iter().collect();
        assert!(index.affixes_of("anything").next().is_none());
    }

    #[test]
    fn affix_index_prefix_multiset_nuspell_unit_test() {
        // Upstream: <https://github.com/nuspell/nuspell/blob/b37faff6ea630a4a1bfb22097d455224b4239f8e/tests/unit_test.cxx#L315-L329>
        fn prefix(add: &str) -> Prefix {
            Prefix::new(Flag::new(1).unwrap(), true, None, add, None, flagset![]).unwrap()
        }

        let index: PrefixIndex = [
            "", "a", "", "ab", "abx", "as", "asdf", "axx", "as", "bqwe", "ba", "rqwe",
        ]
        .into_iter()
        .map(prefix)
        .collect();

        let prefixes: Vec<_> = index
            .affixes_of("asdfg")
            .map(|prefix| prefix.add.as_ref())
            .collect();

        assert_eq!(&["", "", "a", "as", "as", "asdf"], prefixes.as_slice());
    }

    #[test]
    fn affix_index_suffix_multiset_nuspell_unit_test() {
        // Upstream: <https://github.com/nuspell/nuspell/blob/b37faff6ea630a4a1bfb22097d455224b4239f8e/tests/unit_test.cxx#L331-L345>
        fn suffix(add: &str) -> Suffix {
            Suffix::new(Flag::new(1).unwrap(), true, None, add, None, flagset![]).unwrap()
        }

        let index: SuffixIndex = [
            "", "", "a", "b", "b", "ab", "ub", "zb", "aub", "uub", "xub", "huub",
        ]
        .into_iter()
        .map(suffix)
        .collect();

        let suffixes: Vec<_> = index
            .affixes_of("ahahuub")
            .map(|suffix| suffix.add.as_ref())
            .collect();

        assert_eq!(
            &["", "", "b", "b", "ub", "uub", "huub"],
            suffixes.as_slice()
        );
    }

    #[test]
    fn affix_index_en_us_suffix_example() {
        // This suffix is from `en_US.aff`:
        //
        // SFX D Y 4
        // SFX D   0     d          e
        // SFX D   y     ied        [^aeiou]y
        // SFX D   0     ed         [^ey]
        // SFX D   0     ed         [aeiou]y
        let flag = Flag::new('D' as u16).unwrap();
        let suffix1 = Suffix::new(flag, true, None, "d", Some("e"), flagset![]).unwrap();
        let suffix2 =
            Suffix::new(flag, true, Some("y"), "ied", Some("[^aeiou]y"), flagset![]).unwrap();
        let suffix3 = Suffix::new(flag, true, None, "ed", Some("[^ey]"), flagset![]).unwrap();
        let suffix4 = Suffix::new(flag, true, None, "ed", Some("[aeiou]y"), flagset![]).unwrap();

        let index: SuffixIndex = [&suffix1, &suffix2, &suffix3, &suffix4]
            .into_iter()
            .cloned()
            .collect();

        // From `en_US.dic`: `ace/DSMG`. The "ace" stem can be turned into "aced" with the above
        // suffix rules, specifically the first rule (`suffix1`). However all of these suffixes
        // except `suffix2` are returned by `affixes_of`.
        let word = "aced";
        let affixes: Vec<&Suffix> = index.affixes_of(word).collect();
        assert_eq!(&[&suffix1, &suffix3, &suffix4], affixes.as_slice());

        // Note: even though the condition can match, we would also need to look up the produced
        // stem in the word list to confirm that "aced" is a valid word.

        let stem1 = suffix1.to_stem(word);
        assert_eq!(&stem1, "ace");
        assert!(suffix1.condition_matches(&stem1));

        let stem3 = suffix3.to_stem(word);
        assert_eq!(&stem3, "ac");
        assert!(suffix3.condition_matches(&stem3));

        let stem4 = suffix4.to_stem(word);
        assert_eq!(&stem4, "ac");
        assert!(!suffix4.condition_matches(&stem4));
    }

    fn compound_rule_matches(pattern: &[CompoundRuleElement], data: &str) -> bool {
        let flagsets: Vec<_> = data.chars().map(|ch| flagset!(ch)).collect();
        let borrowed: Vec<_> = flagsets.iter().collect();
        super::compound_rule_matches(pattern, &borrowed)
    }

    #[test]
    fn compound_rule_matches_literal() {
        let rule = parser::parse_compound_rule("abc", FlagType::default()).unwrap();

        assert!(compound_rule_matches(&rule, "abc"));

        assert!(!compound_rule_matches(&rule, "ac"));
        assert!(!compound_rule_matches(&rule, "abcd"));
    }

    #[test]
    fn compound_rule_matches_zero_or_one() {
        let rule = parser::parse_compound_rule("ab?c", FlagType::default()).unwrap();

        assert!(compound_rule_matches(&rule, "ac"));
        assert!(compound_rule_matches(&rule, "abc"));

        assert!(!compound_rule_matches(&rule, "ab"));
        assert!(!compound_rule_matches(&rule, "bc"));
        assert!(!compound_rule_matches(&rule, "abb"));
        assert!(!compound_rule_matches(&rule, "abbc"));
    }

    #[test]
    fn compound_rule_matches_zero_or_more() {
        let rule = parser::parse_compound_rule("ab*c", FlagType::default()).unwrap();

        assert!(compound_rule_matches(&rule, "ac"));
        assert!(compound_rule_matches(&rule, "abc"));
        assert!(compound_rule_matches(&rule, "abbc"));
        assert!(compound_rule_matches(&rule, "abbbc"));
        // etc.

        assert!(!compound_rule_matches(&rule, "ab"));
        assert!(!compound_rule_matches(&rule, "abb"));
        assert!(!compound_rule_matches(&rule, "abbcc"));
    }

    #[test]
    fn compound_rule_simple_regex_nuspell_unit_test() {
        // Upstream: <https://github.com/nuspell/nuspell/blob/349e0d6bc68b776af035ca3ff664a7fc55d69387/tests/unit_test.cxx#L384-L393>
        let rule = parser::parse_compound_rule("abc?de*ff", FlagType::default()).unwrap();

        assert!(compound_rule_matches(&rule, "abdff"));
        assert!(compound_rule_matches(&rule, "abcdff"));
        assert!(compound_rule_matches(&rule, "abdeeff"));
        assert!(compound_rule_matches(&rule, "abcdeff"));

        assert!(!compound_rule_matches(&rule, "abcdeeeefff"));
        assert!(!compound_rule_matches(&rule, "qwerty"));
    }

    #[test]
    fn casing_conversions_nuspell_unit_test() {
        // Upstream: <https://github.com/nuspell/nuspell/blob/6e46eb31708003fa02796ee8dc0c9e57248ba141/tests/unit_test.cxx#L440-L448>
        let word = "grüßEN";
        assert_eq!(&CaseHandling::default().lowercase(word), "grüßen");
        assert_eq!(&CaseHandling::default().uppercase(word), "GRÜSSEN");
        assert_eq!(&CaseHandling::default().titlecase(word), "Grüßen");

        let word = "isTAnbulI";
        assert_eq!(&CaseHandling::default().lowercase(word), "istanbuli");
        assert_eq!(&CaseHandling::default().uppercase(word), "ISTANBULI");
        assert_eq!(&CaseHandling::default().titlecase(word), "Istanbuli");
        assert_eq!(&CaseHandling::Turkic.lowercase(word), "istanbulı");
        assert_eq!(&CaseHandling::Turkic.uppercase(word), "İSTANBULI");
        assert_eq!(&CaseHandling::Turkic.titlecase(word), "İstanbulı");
    }
}
