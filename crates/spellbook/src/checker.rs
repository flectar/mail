use core::hash::BuildHasher;

use crate::{
    aff::{
        AffData, Affix, AffixKind, CompoundPattern, Pfx, Prefix, Sfx, Suffix, HIDDEN_HOMONYM_FLAG,
    },
    alloc::{fmt, string::String, vec::Vec},
    classify_casing, erase_chars, AffixingMode, Casing, Dictionary, Flag, FlagSet, Stem, WordList,
    AT_COMPOUND_BEGIN, AT_COMPOUND_END, AT_COMPOUND_MIDDLE, FULL_WORD, MAX_WORD_LEN,
};

macro_rules! has_flag {
    ( $flags:expr, $flag:expr ) => {{
        match $flag {
            Some(flag) => $flags.contains(&flag),
            None => false,
        }
    }};
}
macro_rules! flag {
    ( $x:expr ) => {{
        Flag::new($x as u16).unwrap()
    }};
}

/// A wrapper struct for a dictionary that allows customizing checking behavior.
#[derive(Clone, Copy)]
pub struct Checker<'a, S: BuildHasher> {
    pub(crate) words: &'a WordList<S>,
    pub(crate) aff: &'a AffData,

    check_lower_as_title: bool,
    check_lower_as_upper: bool,
}

impl<S: BuildHasher> fmt::Debug for Checker<'_, S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Checker")
            .field("words", &self.words.len())
            .field("check_lower_as_title", &self.check_lower_as_title)
            .field("check_lower_as_upper", &self.check_lower_as_upper)
            .finish_non_exhaustive()
    }
}

impl<'a, S: BuildHasher> Checker<'a, S> {
    pub(crate) fn new(dict: &'a Dictionary<S>) -> Self {
        Self {
            words: &dict.words,
            aff: &dict.aff_data,
            check_lower_as_title: false,
            check_lower_as_upper: false,
        }
    }

    /// Configures the `Checker` to check lowercase words in titlecase form.
    ///
    /// Normally lowercase words are checked in titlecase or uppercase forms. For example "Alice"
    /// is correct in the `en_US` dictionary but not "alice," and "RSVP" is correct but not
    /// "rsvp."
    ///
    /// # Example
    ///
    /// ```
    /// let aff = std::fs::read_to_string("./vendor/en_US/en_US.aff").unwrap();
    /// let dic = std::fs::read_to_string("./vendor/en_US/en_US.dic").unwrap();
    /// let dict = spellbook::Dictionary::new(&aff, &dic).unwrap();
    ///
    /// assert!(!dict.check("alice"));
    /// assert!(dict.checker().check_lower_as_title(true).check("alice"));
    /// ```
    pub fn check_lower_as_title(mut self, check_lower_as_title: bool) -> Self {
        self.check_lower_as_title = check_lower_as_title;
        self
    }

    /// Configures the `Checker` to check lowercase words in uppercase form.
    ///
    /// See `Checker::check_lower_as_title`.
    ///
    /// # Example
    ///
    /// ```
    /// let aff = std::fs::read_to_string("./vendor/en_US/en_US.aff").unwrap();
    /// let dic = std::fs::read_to_string("./vendor/en_US/en_US.dic").unwrap();
    /// let dict = spellbook::Dictionary::new(&aff, &dic).unwrap();
    ///
    /// assert!(!dict.check("rsvp"));
    /// assert!(dict.checker().check_lower_as_upper(true).check("rsvp"));
    /// ```
    pub fn check_lower_as_upper(mut self, check_lower_as_upper: bool) -> Self {
        self.check_lower_as_upper = check_lower_as_upper;
        self
    }

    /// Checks that the word is valid according to the dictionary.
    pub fn check(&self, word: &str) -> bool {
        if word.len() > MAX_WORD_LEN {
            return false;
        }

        let word = self.aff.input_conversions.convert(word);

        if word.is_empty() {
            return true;
        }

        // TODO: remove this abbreviation stuff and have the tokenizer take over
        // responsibility for it?
        let trimmed_word = word.trim_end_matches('.');
        let abbreviated = trimmed_word.len() != word.len();

        if is_number(trimmed_word) {
            return true;
        }

        let trimmed_word = erase_chars(trimmed_word, &self.aff.ignore_chars);

        if self.spell_break(&trimmed_word) {
            return true;
        }

        if abbreviated {
            // TODO: erase chars in `word` - or figure out abbreviation after ignore-chars.
            // TODO: only keep one `.`?
            return self.spell_break(&word);
        }

        false
    }

    /// Recursively breaks up a word according to the dictionary's `BREAK` rules and checks that
    /// each broken word is correct.
    fn spell_break(&self, word: &str) -> bool {
        self.do_spell_break(word, 0)
    }

    fn do_spell_break(&self, word: &str, depth: usize) -> bool {
        const MAX_DEPTH: usize = 9;

        if let Some(flags) = &self.spell_casing(word) {
            if flags.contains(&self.aff.options.forbidden_word_flag) {
                return false;
            }

            if self.aff.options.forbid_warn && has_flag!(flags, self.aff.options.warn_flag) {
                return false;
            }

            return true;
        }

        if depth == MAX_DEPTH {
            return false;
        }

        for pattern in self.aff.break_table.start_word_breaks() {
            if let Some(rest) = word.strip_prefix(pattern) {
                if self.do_spell_break(rest, depth + 1) {
                    return true;
                }
            }
        }

        for pattern in self.aff.break_table.end_word_breaks() {
            if let Some(rest) = word.strip_suffix(pattern) {
                if self.do_spell_break(rest, depth + 1) {
                    return true;
                }
            }
        }

        for pattern in self.aff.break_table.middle_word_breaks() {
            // Break the word into two - dropping the pattern - and check that both parts are
            // correct.
            if let Some((part1, part2)) = word.split_once(pattern) {
                // If the match is at the end of the string then it's not a middle word break.
                if part2.is_empty() {
                    continue;
                }

                if !self.do_spell_break(part1, depth + 1) {
                    continue;
                }

                if self.do_spell_break(part2, depth + 1) {
                    return true;
                }
            }
        }

        false
    }

    fn spell_casing(&self, word: &str) -> Option<&FlagSet> {
        let casing = classify_casing(word);
        match casing {
            Casing::None | Casing::Camel | Casing::Pascal => {
                if let Some(flags) =
                    self.check_word(word, Forceucase::default(), HiddenHomonym::default())
                {
                    return Some(flags);
                }

                if self.check_lower_as_title && !matches!(casing, Casing::Pascal) {
                    let title = self.aff.options.case_handling.titlecase(word);
                    if let Some(flags) = self.spell_casing_title(&title) {
                        return Some(flags);
                    }
                }

                if self.check_lower_as_upper {
                    let upper = self.aff.options.case_handling.uppercase(word);
                    if let Some(flags) = self.spell_casing_upper(&upper) {
                        return Some(flags);
                    }
                }

                None
            }
            Casing::All => self.spell_casing_upper(word),
            Casing::Init => self.spell_casing_title(word),
        }
    }

    pub(crate) fn check_word(
        &self,
        word: &str,
        allow_bad_forceucase: Forceucase,
        hidden_homonym: HiddenHomonym,
    ) -> Option<&FlagSet> {
        if let Some(flags) = self.check_simple_word(word, hidden_homonym) {
            return Some(flags);
        }

        self.check_compound::<AT_COMPOUND_BEGIN>(word, allow_bad_forceucase)
            .map(|result| result.flags)
    }

    fn spell_casing_upper(&self, word: &str) -> Option<&FlagSet> {
        // First try the word as-is.
        if let Some(flags) = self.check_word(
            word,
            Forceucase::AllowBadForceucase,
            HiddenHomonym::default(),
        ) {
            return Some(flags);
        }

        // Handle prefixes separated by an apostrophe. This is used for Catalan, French and
        // Italian. Nuspell gives the example: SANT'ELIA => Sant'+Elia. Find an apostrophe that
        // isn't at the very end of the word.
        if let Some(mut idx) = word.find('\'').filter(|idx| *idx != word.len() - 1) {
            // `find` returns the index of the found character. We want to include that apostrophe
            // in `part1` so move the index after the apostrophe.
            idx += '\''.len_utf8();
            // Slice up the word into two parts: part1 has the apostrophe and part2 is the rest.
            let part1 = &word[..idx];
            let part2 = &word[idx..];

            // Try two permutations: part1 as lowercase and part1 as titlecase with part2 always
            // being titlecase.
            let part2 = self.aff.options.case_handling.titlecase(part2);

            let mut lower = self.aff.options.case_handling.lowercase(part1);
            lower.push_str(&part2);
            if let Some(flags) = self.check_word(
                &lower,
                Forceucase::AllowBadForceucase,
                HiddenHomonym::default(),
            ) {
                return Some(flags);
            }

            let mut title = self.aff.options.case_handling.titlecase(part1);
            title.push_str(&part2);
            if let Some(flags) = self.check_word(
                &title,
                Forceucase::AllowBadForceucase,
                HiddenHomonym::default(),
            ) {
                return Some(flags);
            }
        }

        // Special-case for sharps (ß).
        if self.aff.options.checksharps && word.contains("SS") {
            if let Some(flags) = self.spell_sharps(&self.aff.options.case_handling.lowercase(word))
            {
                return Some(flags);
            }

            if let Some(flags) = self.spell_sharps(&self.aff.options.case_handling.titlecase(word))
            {
                return Some(flags);
            }
        }

        // Try as title-case.
        if let Some(flags) = self
            .check_word(
                &self.aff.options.case_handling.titlecase(word),
                Forceucase::AllowBadForceucase,
                HiddenHomonym::default(),
            )
            .filter(|flags| !has_flag!(flags, self.aff.options.keep_case_flag))
        {
            return Some(flags);
        }

        // Try as lowercase.
        self.check_word(
            &self.aff.options.case_handling.lowercase(word),
            Forceucase::AllowBadForceucase,
            HiddenHomonym::default(),
        )
        .filter(|flags| !has_flag!(flags, self.aff.options.keep_case_flag))
    }

    fn spell_sharps(&self, word: &str) -> Option<&FlagSet> {
        self.do_spell_sharps(&mut String::from(word), 0, 0, 0)
    }

    fn do_spell_sharps(
        &self,
        word: &mut String,
        position: usize,
        depth: usize,
        replacements: usize,
    ) -> Option<&FlagSet> {
        const MAX_DEPTH: usize = 5;
        const SHARP_S_BYTES: &[u8] = "ß".as_bytes();
        debug_assert_eq!(SHARP_S_BYTES.len(), 2);

        if let Some(idx) = word[position..].find("ss").filter(|_| depth < MAX_DEPTH) {
            // idx is the relative offset since we're subslicing 'word'. Turn it into the absolute
            // byte index of the first 's'.
            let idx = position + idx;

            // SAFETY: "ss" and "ß" are both two bytes long in UTF-8, so overwriting the bytes
            // cannot produce invalid UTF-8.
            unsafe {
                // Replace the "ss" with "ß".
                let bytes = word.as_mut_vec();
                bytes[idx] = SHARP_S_BYTES[0];
                bytes[idx + 1] = SHARP_S_BYTES[1];
            }
            // NOTE: we use `+ 2` here because "ss" (and "ß") is two bytes.
            if let Some(flags) = self.do_spell_sharps(word, idx + 2, depth + 1, replacements + 1) {
                return Some(flags);
            }
            // SAFETY: This is the opposite as above. They're both the same length so the bytes
            // can be overwritten without producing invalid UTF-8.
            unsafe {
                // Replace the "ß" with "ss" and continue the recursion to try another
                // permutation.
                let bytes = word.as_mut_vec();
                bytes[idx] = b's';
                bytes[idx + 1] = b's';
            }

            if let Some(flags) = self.do_spell_sharps(word, idx + 2, depth + 1, replacements + 1) {
                return Some(flags);
            }
        } else if replacements > 0 {
            return self.check_word(
                word,
                Forceucase::AllowBadForceucase,
                HiddenHomonym::default(),
            );
        }
        None
    }

    fn spell_casing_title(&self, word: &str) -> Option<&FlagSet> {
        // First try the word as-is.
        if let Some(flags) = self.check_word(
            word,
            Forceucase::AllowBadForceucase,
            HiddenHomonym::SkipHiddenHomonym,
        ) {
            return Some(flags);
        }

        // Then try lowercase.
        let lower_word = self.aff.options.case_handling.lowercase(word);
        let flags = self.check_word(
            &lower_word,
            Forceucase::AllowBadForceucase,
            HiddenHomonym::default(),
        )?;

        // Nuspell: with CHECKSHARPS, ß is allowed too in KEEPCASE words with title case.
        if has_flag!(flags, self.aff.options.keep_case_flag)
            && !(self.aff.options.checksharps && lower_word.contains('ß'))
        {
            return None;
        }

        Some(flags)
    }

    pub(crate) fn check_simple_word(
        &self,
        word: &str,
        hidden_homonym: HiddenHomonym,
    ) -> Option<&FlagSet> {
        for (_stem, flags) in self.words.get(word) {
            if has_flag!(flags, self.aff.options.need_affix_flag) {
                continue;
            }
            if has_flag!(flags, self.aff.options.only_in_compound_flag) {
                continue;
            }
            if hidden_homonym.skip() && flags.contains(&HIDDEN_HOMONYM_FLAG) {
                continue;
            }
            return Some(flags);
        }

        if let Some(form) = self.strip_suffix_only::<FULL_WORD>(word, hidden_homonym) {
            return Some(form.flags);
        }

        if let Some(form) = self.strip_prefix_only::<FULL_WORD>(word, hidden_homonym) {
            return Some(form.flags);
        }

        if let Some(form) =
            self.strip_prefix_then_suffix_commutative::<FULL_WORD>(word, hidden_homonym)
        {
            return Some(form.flags);
        }

        if self.aff.options.complex_prefixes {
            if let Some(form) = self.strip_prefix_then_prefix(word, hidden_homonym) {
                return Some(form.flags);
            }

            if let Some(form) = self.strip_suffix_then_2_prefixes(word, hidden_homonym) {
                return Some(form.flags);
            }

            if let Some(form) = self.strip_prefix_then_suffix_then_prefix(word, hidden_homonym) {
                return Some(form.flags);
            }

            // strip_2_prefixes_then_suffix (slow and unused, commented out)
        } else {
            if let Some(form) = self.strip_suffix_then_suffix(word, hidden_homonym) {
                return Some(form.flags);
            }

            if let Some(form) = self.strip_prefix_then_2_suffixes(word, hidden_homonym) {
                return Some(form.flags);
            }

            if let Some(form) = self.strip_suffix_then_prefix_then_suffix(word, hidden_homonym) {
                return Some(form.flags);
            }

            // strip_2_suffixes_then_prefix (slow and unused, commented out)
        }

        None
    }

    fn strip_suffix_only<const MODE: AffixingMode>(
        &self,
        word: &str,
        hidden_homonym: HiddenHomonym,
    ) -> Option<AffixForm<'a>> {
        for suffix in self.aff.suffixes.affixes_of(word) {
            if !self.is_outer_affix_valid::<_, MODE>(suffix) {
                continue;
            }

            if !suffix.add.is_empty()
                && MODE == AT_COMPOUND_END
                && has_flag!(suffix.flags, self.aff.options.only_in_compound_flag)
            {
                continue;
            }

            if self.is_circumfix(suffix) {
                continue;
            }

            let stem = suffix.to_stem(word);

            if !suffix.condition_matches(&stem) {
                continue;
            }

            for (stem, flags) in self.words.get(stem.as_ref()) {
                // Nuspell:
                // if (!cross_valid_inner_outer(word_flags, e))
                // 	continue;
                if !flags.contains(&suffix.flag) {
                    continue;
                }

                if MODE == FULL_WORD && has_flag!(flags, self.aff.options.only_in_compound_flag) {
                    continue;
                }

                if hidden_homonym.skip() && flags.contains(&HIDDEN_HOMONYM_FLAG) {
                    continue;
                }

                if !self.is_valid_inside_compound::<MODE>(flags)
                    && !self.is_valid_inside_compound::<MODE>(&suffix.flags)
                {
                    continue;
                }

                return Some(AffixForm {
                    stem,
                    flags,
                    prefixes: Default::default(),
                    suffixes: [Some(suffix), None],
                });
            }
        }

        None
    }

    fn strip_prefix_only<const MODE: AffixingMode>(
        &self,
        word: &str,
        hidden_homonym: HiddenHomonym,
    ) -> Option<AffixForm<'a>> {
        for prefix in self.aff.prefixes.affixes_of(word) {
            if !self.is_outer_affix_valid::<_, MODE>(prefix) {
                continue;
            }

            if !prefix.add.is_empty()
                && MODE == AT_COMPOUND_END
                && has_flag!(prefix.flags, self.aff.options.only_in_compound_flag)
            {
                continue;
            }

            if self.is_circumfix(prefix) {
                continue;
            }

            let stem = prefix.to_stem(word);

            if !prefix.condition_matches(&stem) {
                continue;
            }

            for (stem, flags) in self.words.get(stem.as_ref()) {
                // Nuspell:
                // if (!cross_valid_inner_outer(word_flags, e))
                // 	continue;
                if !flags.contains(&prefix.flag) {
                    continue;
                }

                if MODE == FULL_WORD && has_flag!(flags, self.aff.options.only_in_compound_flag) {
                    continue;
                }

                if hidden_homonym.skip() && flags.contains(&HIDDEN_HOMONYM_FLAG) {
                    continue;
                }

                if !self.is_valid_inside_compound::<MODE>(flags)
                    && !self.is_valid_inside_compound::<MODE>(&prefix.flags)
                {
                    continue;
                }

                return Some(AffixForm {
                    stem,
                    flags,
                    prefixes: [Some(prefix), None],
                    suffixes: Default::default(),
                });
            }
        }

        None
    }

    // Reversed form of Nuspell's `outer_affix_NOT_valid`
    pub(crate) fn is_outer_affix_valid<K: AffixKind, const MODE: AffixingMode>(
        &self,
        affix: &Affix<K>,
    ) -> bool {
        if !K::is_valid::<MODE>(affix, &self.aff.options) {
            return false;
        }

        if has_flag!(affix.flags, self.aff.options.need_affix_flag) {
            return false;
        }

        true
    }

    pub(crate) fn is_circumfix<K: AffixKind>(&self, affix: &Affix<K>) -> bool {
        has_flag!(affix.flags, self.aff.options.circumfix_flag)
    }

    fn is_valid_inside_compound<const MODE: AffixingMode>(&self, flags: &FlagSet) -> bool {
        let is_compound = has_flag!(flags, self.aff.options.compound_flag);

        match MODE {
            AT_COMPOUND_BEGIN
                if !is_compound && !has_flag!(flags, self.aff.options.compound_begin_flag) =>
            {
                false
            }
            AT_COMPOUND_MIDDLE
                if !is_compound && !has_flag!(flags, self.aff.options.compound_middle_flag) =>
            {
                false
            }
            AT_COMPOUND_END
                if !is_compound && !has_flag!(flags, self.aff.options.compound_end_flag) =>
            {
                false
            }
            _ => true,
        }
    }

    fn strip_prefix_then_suffix_commutative<const MODE: AffixingMode>(
        &self,
        word: &str,
        hidden_homonym: HiddenHomonym,
    ) -> Option<AffixForm<'a>> {
        for prefix in self.aff.prefixes.affixes_of(word) {
            if !prefix.crossproduct {
                continue;
            }

            if !Pfx::is_valid::<MODE>(prefix, &self.aff.options) {
                continue;
            }

            let stem_without_prefix = prefix.to_stem(word);

            if !prefix.condition_matches(&stem_without_prefix) {
                continue;
            }

            // Inlined translation of `strip_pfx_then_sfx_comm_2` from Nuspell.
            let has_needaffix_prefix = has_flag!(prefix.flags, self.aff.options.need_affix_flag);
            let is_circumfix_prefix = self.is_circumfix(prefix);

            for suffix in self.aff.suffixes.affixes_of(&stem_without_prefix) {
                if !suffix.crossproduct {
                    continue;
                }

                if !Sfx::is_valid::<MODE>(suffix, &self.aff.options) {
                    continue;
                }

                let has_needaffix_suffix =
                    has_flag!(suffix.flags, self.aff.options.need_affix_flag);

                if has_needaffix_prefix && has_needaffix_suffix {
                    continue;
                }

                if is_circumfix_prefix != self.is_circumfix(suffix) {
                    continue;
                }

                let stem_without_suffix = suffix.to_stem(&stem_without_prefix);

                if !suffix.condition_matches(&stem_without_suffix) {
                    continue;
                }

                for (stem, flags) in self.words.get(stem_without_suffix.as_ref()) {
                    let valid_cross_prefix_outer = !has_needaffix_prefix
                        && flags.contains(&suffix.flag)
                        && (suffix.flags.contains(&prefix.flag) || flags.contains(&prefix.flag));

                    let valid_cross_suffix_outer = !has_needaffix_suffix
                        && flags.contains(&prefix.flag)
                        && (prefix.flags.contains(&suffix.flag) || flags.contains(&suffix.flag));

                    if !valid_cross_prefix_outer && !valid_cross_suffix_outer {
                        continue;
                    }

                    if MODE == FULL_WORD && has_flag!(flags, self.aff.options.only_in_compound_flag)
                    {
                        continue;
                    }

                    if hidden_homonym.skip() && flags.contains(&HIDDEN_HOMONYM_FLAG) {
                        continue;
                    }

                    if !self.is_valid_inside_compound::<MODE>(flags)
                        && !self.is_valid_inside_compound::<MODE>(&suffix.flags)
                        && !self.is_valid_inside_compound::<MODE>(&prefix.flags)
                    {
                        continue;
                    }

                    return Some(AffixForm {
                        stem,
                        flags,
                        prefixes: [Some(prefix), None],
                        suffixes: [Some(suffix), None],
                    });
                }
            }
        }

        None
    }

    fn strip_suffix_then_suffix(
        &self,
        word: &str,
        hidden_homonym: HiddenHomonym,
    ) -> Option<AffixForm<'a>> {
        // Nuspell notes that this is a fast-lane and doesn't affect correctness.
        if self.aff.suffixes.all_flags.is_empty() {
            return None;
        }

        for outer_suffix in self.aff.suffixes.affixes_of(word) {
            // Another fast lane:
            if !self.aff.suffixes.all_flags.contains(&outer_suffix.flag) {
                continue;
            }

            if !self.is_outer_affix_valid::<_, FULL_WORD>(outer_suffix) {
                continue;
            }

            if self.is_circumfix(outer_suffix) {
                continue;
            }

            let stem = outer_suffix.to_stem(word);

            if !outer_suffix.condition_matches(&stem) {
                continue;
            }

            // Inline version of strip_sfx_then_sfx_2. Assume `AffixingMode::FullWord` here.
            for inner_suffix in self.aff.suffixes.affixes_of(stem.as_ref()) {
                // Nuspell:
                // if (!cross_valid_inner_outer(word_flags, se2))
                // 	continue;
                if !inner_suffix.flags.contains(&outer_suffix.flag) {
                    continue;
                }

                if !Sfx::is_valid::<FULL_WORD>(inner_suffix, &self.aff.options) {
                    continue;
                }
                if self.is_circumfix(inner_suffix) {
                    continue;
                }

                let stem2 = inner_suffix.to_stem(&stem);

                if !inner_suffix.condition_matches(&stem2) {
                    continue;
                }

                for (stem, flags) in self.words.get(stem2.as_ref()) {
                    if !flags.contains(&inner_suffix.flag) {
                        continue;
                    }
                    // Note: assumed `AffixingMode::FullWord`
                    if has_flag!(flags, self.aff.options.only_in_compound_flag) {
                        continue;
                    }

                    if hidden_homonym.skip() && flags.contains(&HIDDEN_HOMONYM_FLAG) {
                        continue;
                    }

                    return Some(AffixForm {
                        stem,
                        flags,
                        prefixes: Default::default(),
                        suffixes: [Some(outer_suffix), Some(inner_suffix)],
                    });
                }
            }
        }

        None
    }

    fn strip_prefix_then_prefix(
        &self,
        word: &str,
        hidden_homonym: HiddenHomonym,
    ) -> Option<AffixForm<'a>> {
        // Fastlane
        if self.aff.prefixes.all_flags.is_empty() {
            return None;
        }

        for outer_prefix in self.aff.prefixes.affixes_of(word) {
            // Fastlane
            if !self.aff.prefixes.all_flags.contains(&outer_prefix.flag) {
                continue;
            }

            if !self.is_outer_affix_valid::<_, FULL_WORD>(outer_prefix) {
                continue;
            }

            if self.is_circumfix(outer_prefix) {
                continue;
            }

            let stem = outer_prefix.to_stem(word);

            if !outer_prefix.condition_matches(&stem) {
                continue;
            }

            // Inline version of strip_pfx_then_pfx_2. Assume `AffixingMode::FullWord` here.
            for inner_prefix in self.aff.prefixes.affixes_of(stem.as_ref()) {
                if !inner_prefix.flags.contains(&outer_prefix.flag) {
                    continue;
                }

                if !Pfx::is_valid::<FULL_WORD>(inner_prefix, &self.aff.options) {
                    continue;
                }
                if self.is_circumfix(inner_prefix) {
                    continue;
                }

                let stem2 = inner_prefix.to_stem(&stem);

                if !inner_prefix.condition_matches(&stem2) {
                    continue;
                }

                for (stem, flags) in self.words.get(stem2.as_ref()) {
                    if !flags.contains(&inner_prefix.flag) {
                        continue;
                    }
                    // Note: assumed `AffixingMode::FullWord`
                    if has_flag!(flags, self.aff.options.only_in_compound_flag) {
                        continue;
                    }

                    if hidden_homonym.skip() && flags.contains(&HIDDEN_HOMONYM_FLAG) {
                        continue;
                    }

                    return Some(AffixForm {
                        stem,
                        flags,
                        prefixes: [Some(outer_prefix), Some(inner_prefix)],
                        suffixes: Default::default(),
                    });
                }
            }
        }

        None
    }

    fn strip_prefix_then_2_suffixes(
        &self,
        word: &str,
        hidden_homonym: HiddenHomonym,
    ) -> Option<AffixForm<'a>> {
        // Fastlane
        if self.aff.suffixes.all_flags.is_empty() {
            return None;
        }

        // Strip the prefix.
        for prefix in self.aff.prefixes.affixes_of(word) {
            if !prefix.crossproduct {
                continue;
            }

            if !self.is_outer_affix_valid::<_, FULL_WORD>(prefix) {
                continue;
            }

            // TODO: check whether the condition matches while producing the stem - avoid allocations
            let stem = prefix.to_stem(word);

            if !prefix.condition_matches(&stem) {
                continue;
            }

            // Strip the outer suffix.
            for outer_suffix in self.aff.suffixes.affixes_of(&stem) {
                // Fastlane
                if !self.aff.suffixes.all_flags.contains(&outer_suffix.flag) {
                    continue;
                }

                if !outer_suffix.crossproduct {
                    continue;
                }

                if !Sfx::is_valid::<FULL_WORD>(outer_suffix, &self.aff.options) {
                    continue;
                }

                if self.is_circumfix(prefix) != self.is_circumfix(outer_suffix) {
                    continue;
                }

                let stem2 = outer_suffix.to_stem(&stem);

                if !outer_suffix.condition_matches(&stem2) {
                    continue;
                }

                // Strip the inner suffix.
                for inner_suffix in self.aff.suffixes.affixes_of(&stem2) {
                    if !inner_suffix.flags.contains(&outer_suffix.flag) {
                        continue;
                    }

                    if !Sfx::is_valid::<FULL_WORD>(inner_suffix, &self.aff.options) {
                        continue;
                    }

                    if self.is_circumfix(inner_suffix) {
                        continue;
                    }

                    let stem3 = inner_suffix.to_stem(&stem2);

                    if !inner_suffix.condition_matches(&stem3) {
                        continue;
                    }

                    // Check that the fully stripped word is a stem in the dictionary.
                    for (stem, flags) in self.words.get(stem3.as_ref()) {
                        if !outer_suffix.flags.contains(&prefix.flag)
                            && !flags.contains(&prefix.flag)
                        {
                            continue;
                        }

                        if !flags.contains(&inner_suffix.flag) {
                            continue;
                        }

                        // Note: assumed `AffixingMode::FullWord`
                        if has_flag!(flags, self.aff.options.only_in_compound_flag) {
                            continue;
                        }

                        if hidden_homonym.skip() && flags.contains(&HIDDEN_HOMONYM_FLAG) {
                            continue;
                        }

                        return Some(AffixForm {
                            stem,
                            flags,
                            prefixes: [Some(prefix), None],
                            suffixes: [Some(outer_suffix), Some(inner_suffix)],
                        });
                    }
                }
            }
        }

        None
    }

    /// Strip an outer suffix, then a prefix and then an inner suffix.
    ///
    /// This is very similar to the above function but what we're checking is slightly different.
    ///
    /// * `strip_prefix_then_2_suffixes` we care that (AND):
    ///     * the inner suffix's flags contains the outer suffix's flag
    ///     * either (OR):
    ///         * the outer suffix's flags contains the prefix's flag
    ///         * the word flags contain the prefix's flag
    /// * `strip_suffix_then_prefix_then_suffix` we care that (AND):
    ///     * either (OR):
    ///         * the inner suffix's flags contains the outer suffix's flag
    ///         * the prefix's flags contains the outer suffix's flag
    ///     * either (OR):
    ///         * the inner suffix's flag contains the prefix's flag
    ///         * the word flags contain the prefix's flag
    ///
    /// By stripping a suffix first and then a prefix in this function, we allow words where the
    /// inner suffix's contination flags might contain the prefix's flag, or where the prefix's
    /// flag allows the outer suffix.
    fn strip_suffix_then_prefix_then_suffix(
        &self,
        word: &str,
        hidden_homonym: HiddenHomonym,
    ) -> Option<AffixForm<'a>> {
        // Fastlane
        if self.aff.prefixes.all_flags.is_empty() && self.aff.suffixes.all_flags.is_empty() {
            return None;
        }

        for outer_suffix in self.aff.suffixes.affixes_of(word) {
            // Fastlane
            if !self.aff.prefixes.all_flags.contains(&outer_suffix.flag)
                && !self.aff.suffixes.all_flags.contains(&outer_suffix.flag)
            {
                continue;
            }

            if !outer_suffix.crossproduct {
                continue;
            }

            if !self.is_outer_affix_valid::<_, FULL_WORD>(outer_suffix) {
                continue;
            }

            let stem = outer_suffix.to_stem(word);

            if !outer_suffix.condition_matches(&stem) {
                continue;
            }

            for prefix in self.aff.prefixes.affixes_of(&stem) {
                if !prefix.crossproduct {
                    continue;
                }

                if !Pfx::is_valid::<FULL_WORD>(prefix, &self.aff.options) {
                    continue;
                }

                let stem2 = prefix.to_stem(&stem);

                if !prefix.condition_matches(&stem) {
                    continue;
                }

                let prefix_flags_contains_outer_suffix_flag =
                    prefix.flags.contains(&outer_suffix.flag);

                // Inlined version of Nuspell's `strip_s_p_s_3`. Here kitty kitty.
                for inner_suffix in self.aff.suffixes.affixes_of(&stem2) {
                    if !inner_suffix.crossproduct {
                        continue;
                    }

                    if !inner_suffix.flags.contains(&outer_suffix.flag)
                        && !prefix_flags_contains_outer_suffix_flag
                    {
                        continue;
                    }

                    if !Sfx::is_valid::<FULL_WORD>(inner_suffix, &self.aff.options) {
                        continue;
                    }

                    let prefix_circumfix = self.is_circumfix(prefix);
                    let inner_suffix_circumfix = self.is_circumfix(inner_suffix);
                    let outer_suffix_circumfix = self.is_circumfix(outer_suffix);
                    let circumfix_ok_1 =
                        (prefix_circumfix == outer_suffix_circumfix) && !inner_suffix_circumfix;
                    let circumfix_ok_2 =
                        (prefix_circumfix == inner_suffix_circumfix) && !outer_suffix_circumfix;
                    if !circumfix_ok_1 && !circumfix_ok_2 {
                        continue;
                    }

                    let stem3 = inner_suffix.to_stem(&stem2);

                    if !inner_suffix.condition_matches(&stem3) {
                        continue;
                    }

                    for (stem, flags) in self.words.get(stem3.as_ref()) {
                        if !inner_suffix.flags.contains(&prefix.flag)
                            && !flags.contains(&prefix.flag)
                        {
                            continue;
                        }

                        if !flags.contains(&inner_suffix.flag) {
                            continue;
                        }

                        // Note: assumed `AffixingMode::FullWord`
                        if has_flag!(flags, self.aff.options.only_in_compound_flag) {
                            continue;
                        }

                        if hidden_homonym.skip() && flags.contains(&HIDDEN_HOMONYM_FLAG) {
                            continue;
                        }

                        return Some(AffixForm {
                            stem,
                            flags,
                            prefixes: [Some(prefix), None],
                            suffixes: [Some(outer_suffix), Some(inner_suffix)],
                        });
                    }
                }
            }
        }

        None
    }

    fn strip_suffix_then_2_prefixes(
        &self,
        word: &str,
        hidden_homonym: HiddenHomonym,
    ) -> Option<AffixForm<'a>> {
        // Fastlane
        if self.aff.suffixes.all_flags.is_empty() {
            return None;
        }

        // Strip the suffix.
        for suffix in self.aff.suffixes.affixes_of(word) {
            if !suffix.crossproduct {
                continue;
            }

            if !self.is_outer_affix_valid::<_, FULL_WORD>(suffix) {
                continue;
            }

            let stem = suffix.to_stem(word);

            if !suffix.condition_matches(&stem) {
                continue;
            }

            // Strip the outer prefix.
            for outer_prefix in self.aff.prefixes.affixes_of(&stem) {
                // Fastlane
                if !self.aff.suffixes.all_flags.contains(&outer_prefix.flag) {
                    continue;
                }

                if !outer_prefix.crossproduct {
                    continue;
                }

                if !Pfx::is_valid::<FULL_WORD>(outer_prefix, &self.aff.options) {
                    continue;
                }

                if self.is_circumfix(suffix) != self.is_circumfix(outer_prefix) {
                    continue;
                }

                let stem2 = outer_prefix.to_stem(&stem);

                if !outer_prefix.condition_matches(&stem2) {
                    continue;
                }

                // Strip the inner prefix.
                for inner_prefix in self.aff.prefixes.affixes_of(&stem2) {
                    if !inner_prefix.flags.contains(&outer_prefix.flag) {
                        continue;
                    }

                    if !Pfx::is_valid::<FULL_WORD>(inner_prefix, &self.aff.options) {
                        continue;
                    }

                    if self.is_circumfix(inner_prefix) {
                        continue;
                    }

                    let stem3 = inner_prefix.to_stem(&stem2);

                    if !inner_prefix.condition_matches(&stem3) {
                        continue;
                    }

                    // Check that the fully stripped word is a stem in the dictionary.
                    for (stem, flags) in self.words.get(stem3.as_ref()) {
                        if !outer_prefix.flags.contains(&suffix.flag)
                            && !flags.contains(&suffix.flag)
                        {
                            continue;
                        }

                        if !flags.contains(&inner_prefix.flag) {
                            continue;
                        }

                        // Note: assumed `AffixingMode::FullWord`
                        if has_flag!(flags, self.aff.options.only_in_compound_flag) {
                            continue;
                        }

                        if hidden_homonym.skip() && flags.contains(&HIDDEN_HOMONYM_FLAG) {
                            continue;
                        }

                        return Some(AffixForm {
                            stem,
                            flags,
                            prefixes: [Some(outer_prefix), Some(inner_prefix)],
                            suffixes: [Some(suffix), None],
                        });
                    }
                }
            }
        }

        None
    }

    /// Strip an outer prefix, then a suffix and then an inner prefix.
    ///
    /// This is very similar to the above function but what we're checking is slightly different.
    ///
    /// * `strip_suffix_then_2_prefixes` we care that (AND):
    ///     * the inner prefix's flags contains the outer prefix's flag
    ///     * either (OR):
    ///         * the outer prefix's flags contains the suffix's flag
    ///         * the word flags contains the suffix's flag
    /// * `strip_prefix_then_suffix_then_prefix` we care that (AND):
    ///     * either (OR):
    ///         * the inner prefix's flags includes the outer prefix's flag
    ///         * the suffix's flags contains the outer prefix's flag
    ///     * either (OR):
    ///       * the inner prefix's flags contains the suffix's flag
    ///       * the word flags contains the suffix's flag
    ///
    /// By stripping a prefix first and then a suffix in this function, we allow words where the
    /// inner prefix's continuation flag might contain the suffix's flag, or where the suffix's
    /// flag allows the outer prefix.
    fn strip_prefix_then_suffix_then_prefix(
        &self,
        word: &str,
        hidden_homonym: HiddenHomonym,
    ) -> Option<AffixForm<'a>> {
        // Fastlane
        if self.aff.prefixes.all_flags.is_empty() && self.aff.suffixes.all_flags.is_empty() {
            return None;
        }

        for outer_prefix in self.aff.prefixes.affixes_of(word) {
            // Fastlane
            if !self.aff.prefixes.all_flags.contains(&outer_prefix.flag)
                && !self.aff.suffixes.all_flags.contains(&outer_prefix.flag)
            {
                continue;
            }

            if !outer_prefix.crossproduct {
                continue;
            }

            if !self.is_outer_affix_valid::<_, FULL_WORD>(outer_prefix) {
                continue;
            }

            let stem = outer_prefix.to_stem(word);

            if !outer_prefix.condition_matches(&stem) {
                continue;
            }

            for suffix in self.aff.suffixes.affixes_of(&stem) {
                if !suffix.crossproduct {
                    continue;
                }

                if !Sfx::is_valid::<FULL_WORD>(suffix, &self.aff.options) {
                    continue;
                }

                let stem2 = suffix.to_stem(&stem);

                if !suffix.condition_matches(&stem) {
                    continue;
                }

                // TODO: can we eagerly evaluate this in other loops?
                let suffix_flags_contains_outer_prefix_flag =
                    suffix.flags.contains(&outer_prefix.flag);

                for inner_prefix in self.aff.prefixes.affixes_of(&stem2) {
                    if !inner_prefix.crossproduct {
                        continue;
                    }

                    if !inner_prefix.flags.contains(&outer_prefix.flag)
                        && !suffix_flags_contains_outer_prefix_flag
                    {
                        continue;
                    }

                    if !Pfx::is_valid::<FULL_WORD>(inner_prefix, &self.aff.options) {
                        continue;
                    }

                    let prefix_circumfix = self.is_circumfix(suffix);
                    let inner_suffix_circumfix = self.is_circumfix(inner_prefix);
                    let outer_suffix_circumfix = self.is_circumfix(outer_prefix);
                    let circumfix_ok_1 =
                        (prefix_circumfix == outer_suffix_circumfix) && !inner_suffix_circumfix;
                    let circumfix_ok_2 =
                        (prefix_circumfix == inner_suffix_circumfix) && !outer_suffix_circumfix;
                    if !circumfix_ok_1 && !circumfix_ok_2 {
                        continue;
                    }

                    let stem3 = inner_prefix.to_stem(&stem2);

                    if !inner_prefix.condition_matches(&stem3) {
                        continue;
                    }

                    for (stem, flags) in self.words.get(stem3.as_ref()) {
                        if !inner_prefix.flags.contains(&suffix.flag)
                            && !flags.contains(&suffix.flag)
                        {
                            continue;
                        }

                        if !flags.contains(&inner_prefix.flag) {
                            continue;
                        }

                        // Note: assumed `AffixingMode::FullWord`
                        if has_flag!(flags, self.aff.options.only_in_compound_flag) {
                            continue;
                        }

                        if hidden_homonym.skip() && flags.contains(&HIDDEN_HOMONYM_FLAG) {
                            continue;
                        }

                        return Some(AffixForm {
                            stem,
                            flags,
                            prefixes: [Some(outer_prefix), Some(inner_prefix)],
                            suffixes: [Some(suffix), None],
                        });
                    }
                }
            }
        }

        None
    }

    // Compounding

    pub(crate) fn check_compound<const MODE: AffixingMode>(
        &self,
        word: &str,
        allow_bad_forceucase: Forceucase,
    ) -> Option<CompoundingResult<'_>> {
        if self.aff.options.allows_compounding() {
            // Note that Nuspell passes along basically a `&mut String`. We can avoid that by
            // subslicing the word `&str`. Also see `check_compound_with_rules`.
            if let Some(result) = self.check_compound_impl::<MODE>(word, 0, 0, allow_bad_forceucase)
            {
                return Some(result);
            }
        }

        if !self.aff.compound_rules.is_empty() {
            return self.check_compound_with_rules(word, allow_bad_forceucase);
        }

        None
    }

    fn check_compound_impl<const MODE: AffixingMode>(
        &self,
        word: &str,
        start_pos: usize,
        num_parts: usize,
        allow_bad_forceucase: Forceucase,
    ) -> Option<CompoundingResult<'_>> {
        let min_chars = self
            .aff
            .options
            .compound_min_length
            .map(|len| len.get())
            .unwrap_or(3) as usize;

        for i in bytes_within(word, start_pos, min_chars) {
            if let Some(result) = self.check_compound_classic::<MODE>(
                word,
                start_pos,
                i,
                num_parts,
                allow_bad_forceucase,
            ) {
                return Some(result);
            }

            if let Some(result) = self.check_compound_with_pattern_replacements::<MODE>(
                word,
                start_pos,
                i,
                num_parts,
                allow_bad_forceucase,
            ) {
                return Some(result);
            }
        }

        None
    }

    fn check_compound_classic<const MODE: AffixingMode>(
        &self,
        word: &str,
        start_pos: usize,
        i: usize,
        mut num_part: usize,
        forceucase: Forceucase,
    ) -> Option<CompoundingResult<'_>> {
        // There's an odd bit of code in Nuspell here that declares `old_num_part` and sets it
        // to `num_part` instead of declaring it below. The variable isn't used between here and
        // the declaration below but Nuspell sets it here anyways. Nuspell must declare
        // `old_num_part` here because it uses goto labels and it would cause a compilation error
        // to declare it later, even though the variable is unused in the `goto` labels.

        // Nuspell uses `goto` with similar names as these functions. Admittedly it's kinda nice -
        // calling all of these other functions is a little verbose. We could use a macro to cut
        // down on the text but I'd rather have the code be unfancy.
        // TODO: investigate just using a big chain of `||`s.

        let part1_entry = self.check_word_in_compound::<MODE>(&word[start_pos..i])?;
        if part1_entry
            .flags
            .contains(&self.aff.options.forbidden_word_flag)
        {
            return None;
        }
        if self.aff.options.compound_check_triple && are_three_chars_equal(word, i) {
            return None;
        }
        if self.aff.options.compound_check_case && has_uppercase_at_compound_word_boundary(word, i)
        {
            return None;
        }
        num_part += part1_entry.num_words_modifier as usize;
        if has_flag!(part1_entry.flags, self.aff.options.compound_root_flag) {
            num_part += 1;
        }

        let Some(part2_entry) = self.check_word_in_compound::<AT_COMPOUND_END>(&word[i..]) else {
            return self.check_compound_classic_try_recursive(
                word,
                start_pos,
                i,
                num_part,
                forceucase,
                part1_entry,
            );
        };
        if part2_entry
            .flags
            .contains(&self.aff.options.forbidden_word_flag)
        {
            return self.check_compound_classic_try_recursive(
                word,
                start_pos,
                i,
                num_part,
                forceucase,
                part1_entry,
            );
        }
        if self.is_compound_forbidden_by_patterns(word, i, &part1_entry, &part2_entry) {
            return self.check_compound_classic_try_recursive(
                word,
                start_pos,
                i,
                num_part,
                forceucase,
                part1_entry,
            );
        }
        if self.aff.options.compound_check_duplicate && part1_entry == part2_entry {
            return self.check_compound_classic_try_recursive(
                word,
                start_pos,
                i,
                num_part,
                forceucase,
                part1_entry,
            );
        }
        if self.aff.options.compound_check_rep && self.is_rep_similar(&word[start_pos..]) {
            return self.check_compound_classic_try_recursive(
                word,
                start_pos,
                i,
                num_part,
                forceucase,
                part1_entry,
            );
        }
        if forceucase.forbid()
            && has_flag!(
                part2_entry.flags,
                self.aff.options.compound_force_uppercase_flag
            )
        {
            return self.check_compound_classic_try_recursive(
                word,
                start_pos,
                i,
                num_part,
                forceucase,
                part1_entry,
            );
        }

        let old_num_part = num_part;
        num_part += part2_entry.num_words_modifier as usize;
        if has_flag!(part2_entry.flags, self.aff.options.compound_root_flag) {
            num_part += 1;
        }
        if self
            .aff
            .options
            .compound_max_word_count
            .is_some_and(|max| num_part + 1 >= max.get().into())
        {
            // Nuspell: "is not Hungarian"
            if self.aff.compound_syllable_vowels.is_empty() {
                return None;
            }

            let mut num_syllable = self.count_syllables(word);
            num_syllable += part2_entry.num_syllable_modifier as usize;
            if num_syllable
                > self
                    .aff
                    .options
                    .compound_syllable_max
                    .map(|n| n.get().into())
                    .unwrap_or(0)
            {
                num_part = old_num_part;
                return self.check_compound_classic_try_recursive(
                    word,
                    start_pos,
                    i,
                    num_part,
                    forceucase,
                    part1_entry,
                );
            }
        }

        Some(part1_entry)
    }

    fn check_compound_classic_try_recursive(
        &self,
        word: &str,
        start_pos: usize,
        i: usize,
        num_part: usize,
        forceucase: Forceucase,
        part1_entry: CompoundingResult<'a>,
    ) -> Option<CompoundingResult<'_>> {
        let Some(part2_entry) =
            self.check_compound_impl::<AT_COMPOUND_MIDDLE>(word, i, num_part + 1, forceucase)
        else {
            return self.check_compound_classic_try_simplified_triple(
                word,
                start_pos,
                i,
                num_part,
                forceucase,
                part1_entry,
            );
        };
        if self.is_compound_forbidden_by_patterns(word, i, &part1_entry, &part2_entry) {
            return self.check_compound_classic_try_simplified_triple(
                word,
                start_pos,
                i,
                num_part,
                forceucase,
                part1_entry,
            );
        }
        // Commented out in Nuspell:
        // if self.aff.options.compound_check_duplicate && part1_entry == part2_entry {
        //     return self.check_compound_classic_try_simplified_triple(
        //         word,
        //         start_pos,
        //         i,
        //         num_part,
        //         forceucase,
        //         part1_entry,
        //     );
        // }
        if self.aff.options.compound_check_rep {
            if self.is_rep_similar(&word[start_pos..]) {
                return self.check_compound_classic_try_simplified_triple(
                    word,
                    start_pos,
                    i,
                    num_part,
                    forceucase,
                    part1_entry,
                );
            }
            let part2_word = part2_entry.stem.as_ref();
            if word
                .get(i..)
                .is_some_and(|tail| tail.starts_with(part2_word))
                && self.is_rep_similar(&word[start_pos..i + part2_word.len()])
            {
                return self.check_compound_classic_try_simplified_triple(
                    word,
                    start_pos,
                    i,
                    num_part,
                    forceucase,
                    part1_entry,
                );
            }
        }

        Some(part1_entry)
    }

    fn check_compound_classic_try_simplified_triple(
        &self,
        word: &str,
        start_pos: usize,
        i: usize,
        num_part: usize,
        forceucase: Forceucase,
        part1_entry: CompoundingResult<'a>,
    ) -> Option<CompoundingResult<'_>> {
        if !self.aff.options.compound_simplified_triple {
            return None;
        }

        let (prev_cp_idx, prev_cp) = prev_codepoint(word, i)?;
        if prev_cp_idx == 0 {
            return None;
        }
        let (_, prev_cp2) = prev_codepoint(word, i)?;
        if prev_cp != prev_cp2 {
            return None;
        }
        // TODO: pass a `&mut String` instead of allocating a new one?
        let mut word_with_triple = String::from(word);
        word_with_triple.insert(prev_cp_idx, prev_cp);
        let part = &word_with_triple[i..];
        let Some(part2_entry) = self.check_word_in_compound::<AT_COMPOUND_END>(part) else {
            return self.check_compound_classic_try_simplified_triple_recursive(
                word_with_triple,
                word,
                start_pos,
                i,
                num_part,
                forceucase,
                part1_entry,
            );
        };
        if part2_entry
            .flags
            .contains(&self.aff.options.forbidden_word_flag)
        {
            return self.check_compound_classic_try_simplified_triple_recursive(
                word_with_triple,
                word,
                start_pos,
                i,
                num_part,
                forceucase,
                part1_entry,
            );
        }
        if self.is_compound_forbidden_by_patterns(&word_with_triple, i, &part1_entry, &part2_entry)
        {
            return self.check_compound_classic_try_simplified_triple_recursive(
                word_with_triple,
                word,
                start_pos,
                i,
                num_part,
                forceucase,
                part1_entry,
            );
        }
        if self.aff.options.compound_check_duplicate && part1_entry == part2_entry {
            return self.check_compound_classic_try_simplified_triple_recursive(
                word_with_triple,
                word,
                start_pos,
                i,
                num_part,
                forceucase,
                part1_entry,
            );
        }
        // NOTE: `word` instead of `word_with_triple` is intentional here. Nuspell: "The added
        // char above should not be checked for rep similarity, instead check the original word."
        if self.aff.options.compound_check_rep && self.is_rep_similar(&word[start_pos..]) {
            return self.check_compound_classic_try_simplified_triple_recursive(
                word_with_triple,
                word,
                start_pos,
                i,
                num_part,
                forceucase,
                part1_entry,
            );
        }
        if forceucase.forbid()
            && has_flag!(
                part2_entry.flags,
                self.aff.options.compound_force_uppercase_flag
            )
        {
            return self.check_compound_classic_try_simplified_triple_recursive(
                word_with_triple,
                word,
                start_pos,
                i,
                num_part,
                forceucase,
                part1_entry,
            );
        }

        if self
            .aff
            .options
            .compound_max_word_count
            .is_some_and(|max| num_part > max.get().into())
        {
            return None;
        }
        Some(part1_entry)
    }

    #[allow(clippy::too_many_arguments)]
    fn check_compound_classic_try_simplified_triple_recursive(
        &self,
        word_with_triple: String,
        word: &str,
        start_pos: usize,
        i: usize,
        num_part: usize,
        forceucase: Forceucase,
        part1_entry: CompoundingResult<'a>,
    ) -> Option<CompoundingResult<'_>> {
        let part2_entry = self.check_compound_impl::<AT_COMPOUND_MIDDLE>(
            &word_with_triple,
            i,
            num_part + 1,
            forceucase,
        )?;
        if self.is_compound_forbidden_by_patterns(&word_with_triple, i, &part1_entry, &part2_entry)
        {
            return None;
        }
        // Commented out in Nuspell:
        // if self.aff.options.compound_check_duplicate && part1_entry == part2_entry {
        //     return None;
        // }
        if self.aff.options.compound_check_rep {
            if self.is_rep_similar(&word[start_pos..]) {
                return None;
            }

            let part2_word = part2_entry.stem.as_ref();
            if word
                .get(i..)
                .is_some_and(|tail| tail.starts_with(part2_word))
                && self.is_rep_similar(&word[start_pos..i + part2_word.len()])
            {
                return None;
            }
        }

        Some(part1_entry)
    }

    // about the pub(crate) - supposedly the suggester will need this.
    pub(crate) fn is_rep_similar(&self, word: &str) -> bool {
        // Whole word replacements can be checked without allocating - we can just borrow the
        // `to` from the replacement pair.
        for (from, to) in self.aff.replacements.whole_word_replacements() {
            if word == from
                && self
                    .check_simple_word(to, HiddenHomonym::SkipHiddenHomonym)
                    .is_some()
            {
                return true;
            }
        }

        // Otherwise we'll need to manipulate `word` and replace some characters. Create a
        // `String` we'll use as a scratchpad and reuse that throughout. To avoid a needless
        // allocation, first check whether there are any other replacements in the table:
        if self.aff.replacements.has_only_whole_word_replacements() {
            return false;
        }

        let mut scratch = String::from(word);

        for (from, to) in self.aff.replacements.start_word_replacements() {
            if word.starts_with(from) {
                scratch.replace_range(..from.len(), to);
                if self
                    .check_simple_word(&scratch, HiddenHomonym::SkipHiddenHomonym)
                    .is_some()
                {
                    return true;
                }
                scratch.replace_range(..to.len(), from);
            }
        }

        debug_assert_eq!(&scratch, word);

        for (from, to) in self.aff.replacements.end_word_replacements() {
            if word.ends_with(from) {
                let idx = word.len() - from.len();
                scratch.replace_range(idx.., to);
                if self
                    .check_simple_word(&scratch, HiddenHomonym::SkipHiddenHomonym)
                    .is_some()
                {
                    return true;
                }
                scratch.replace_range(idx.., from);
            }
        }

        debug_assert_eq!(&scratch, word);

        for (from, to) in self.aff.replacements.any_place_replacements() {
            for (idx, _) in word.match_indices(from) {
                scratch.replace_range(idx..idx + from.len(), to);
                if self
                    .check_simple_word(&scratch, HiddenHomonym::SkipHiddenHomonym)
                    .is_some()
                {
                    return true;
                }
                scratch.replace_range(idx..idx + to.len(), from);
            }
        }

        debug_assert_eq!(&scratch, word);

        false
    }

    fn check_compound_with_pattern_replacements<const MODE: AffixingMode>(
        &self,
        _word: &str,
        _start_pos: usize,
        _i: usize,
        _num_parts: usize,
        _allow_bad_forceucase: Forceucase,
    ) -> Option<CompoundingResult<'_>> {
        // TODO: find a dictionary that uses CHECKCOMPOUNDPATTERN with replacements.
        // This function does nothing unless we have a compound pattern with a replacement (as
        // you might guess from the function name).
        None
    }

    fn check_word_in_compound<const MODE: AffixingMode>(
        &self,
        word: &str,
    ) -> Option<CompoundingResult<'_>> {
        let compound_flag = match MODE {
            AT_COMPOUND_BEGIN => self.aff.options.compound_begin_flag,
            AT_COMPOUND_MIDDLE => self.aff.options.compound_middle_flag,
            AT_COMPOUND_END => self.aff.options.compound_end_flag,
            _ => None,
        };

        for (stem, flags) in self.words.get(word) {
            if has_flag!(flags, self.aff.options.need_affix_flag) {
                continue;
            }

            if !has_flag!(flags, self.aff.options.compound_flag) && !has_flag!(flags, compound_flag)
            {
                continue;
            }

            if flags.contains(&HIDDEN_HOMONYM_FLAG) {
                continue;
            }

            let num_syllable_modifier = self.calculate_syllable_modifier::<MODE>(flags);

            return Some(CompoundingResult {
                stem,
                flags,
                num_words_modifier: Default::default(),
                num_syllable_modifier,
                affixed_and_modified: Default::default(),
            });
        }

        if let Some(affix_form) =
            self.strip_suffix_only::<MODE>(word, HiddenHomonym::SkipHiddenHomonym)
        {
            // `strip_suffix_only` always produces one suffix.
            let suffix = affix_form.suffixes[0].unwrap();
            let num_syllable_modifier =
                self.calculate_syllable_modifier_with_suffix::<MODE>(affix_form.flags, suffix);
            return Some(CompoundingResult {
                stem: affix_form.stem,
                flags: affix_form.flags,
                num_words_modifier: Default::default(),
                num_syllable_modifier,
                affixed_and_modified: suffix.is_modifying(),
            });
        }

        if let Some(affix_form) =
            self.strip_prefix_only::<MODE>(word, HiddenHomonym::SkipHiddenHomonym)
        {
            // `strip_prefix_only` always produces one prefix.
            let prefix = affix_form.prefixes[0].unwrap();
            let num_words_modifier = self.calculate_num_words_modifier(prefix);
            return Some(CompoundingResult {
                stem: affix_form.stem,
                flags: affix_form.flags,
                num_words_modifier,
                num_syllable_modifier: Default::default(),
                affixed_and_modified: prefix.is_modifying(),
            });
        }

        if let Some(affix_form) = self
            .strip_prefix_then_suffix_commutative::<MODE>(word, HiddenHomonym::SkipHiddenHomonym)
        {
            // `strip_prefix_then_suffix_commutative` always produces one suffix and one prefix.
            let prefix = affix_form.prefixes[0].unwrap();
            let num_words_modifier = self.calculate_num_words_modifier(prefix);
            let suffix = affix_form.suffixes[0].unwrap();
            let num_syllable_modifier =
                self.calculate_syllable_modifier_with_suffix::<MODE>(affix_form.flags, suffix);
            return Some(CompoundingResult {
                stem: affix_form.stem,
                flags: affix_form.flags,
                num_words_modifier,
                num_syllable_modifier,
                affixed_and_modified: prefix.is_modifying() || suffix.is_modifying(),
            });
        }

        None
    }

    fn calculate_num_words_modifier(&self, prefix: &Prefix) -> u16 {
        if self.aff.compound_syllable_vowels.is_empty() {
            return 0;
        }

        // TODO: introduce a short circuiting `has_syllable` function?
        let syllables = self.count_syllables(&prefix.add);
        syllables.max(1) as u16
    }

    fn calculate_syllable_modifier<const MODE: AffixingMode>(&self, flags: &FlagSet) -> i16 {
        // TODO: what do these flags mean?
        let subtract_syllable = MODE == AT_COMPOUND_END
            && !self.aff.compound_syllable_vowels.is_empty()
            && flags.contains(&flag!('I'))
            && !flags.contains(&flag!('J'));

        0 - subtract_syllable as i16
    }

    fn calculate_syllable_modifier_with_suffix<const MODE: AffixingMode>(
        &self,
        flags: &FlagSet,
        suffix: &Suffix,
    ) -> i16 {
        if MODE != AT_COMPOUND_END || self.aff.compound_syllable_vowels.is_empty() {
            return 0;
        }

        let append = &suffix.add;
        // TODO: count syllables
        let mut num_syllable_mod = 0 - self.count_syllables(append) as i16;
        // TODO: what are these hardcoded checks?
        let suffix_extra = append.ends_with('i')
            && matches!(append.chars().nth_back(1), Some(c) if c != 'y' && c != 't');
        num_syllable_mod -= suffix_extra as i16;

        if self.aff.options.compound_syllable_num {
            match char::from_u32(suffix.flag.get() as u32) {
                Some('c') => num_syllable_mod += 2,
                Some('J') => num_syllable_mod += 1,
                Some('I') => {
                    num_syllable_mod += flags.contains(&flag!('J')) as i16;
                }
                _ => (),
            }
        }

        num_syllable_mod
    }

    fn count_syllables(&self, word: &str) -> usize {
        fn count_appearances_of(haystack: &str, needles: &str) -> usize {
            let mut occurrences = 0;

            for ch in haystack.chars() {
                occurrences += needles.matches(ch).count();
            }

            occurrences
        }

        count_appearances_of(word, &self.aff.compound_syllable_vowels)
    }

    fn is_compound_forbidden_by_patterns(
        &self,
        word: &str,
        i: usize,
        first: &CompoundingResult,
        second: &CompoundingResult,
    ) -> bool {
        fn match_compound_pattern(
            pattern: &CompoundPattern,
            word: &str,
            i: usize,
            first: &CompoundingResult,
            second: &CompoundingResult,
        ) -> bool {
            let partition = pattern.begin_end_chars.partition();
            if i < partition {
                return false;
            }
            if !word
                .get(i - partition..)
                .is_some_and(|comp| comp.starts_with(pattern.begin_end_chars.full_str()))
            {
                return false;
            }
            if pattern
                .first_word_flag
                .is_some_and(|flag| !first.flags.contains(&flag))
            {
                return false;
            }
            if pattern
                .second_word_flag
                .is_some_and(|flag| !second.flags.contains(&flag))
            {
                return false;
            }
            if pattern.match_first_only_unaffixed_or_zero_affixed && first.affixed_and_modified {
                return false;
            }

            true
        }

        self.aff
            .compound_patterns
            .iter()
            .any(|pattern| match_compound_pattern(pattern, word, i, first, second))
    }

    /// Checks whether the word might be a compound according to the aff's `COMPOUNDRULE` rules.
    ///
    /// This function recursively tries to slice up the word into parts which might exist in the
    /// dictionary. For example for the compound "10th" it splits it up as "1" and "0th" or
    /// compound "202nd" it splits it up as "2", "0" and "2nd". It tries to split up the word
    /// in any permutation based on the minimum number of characters required for a compound part
    /// (the `COMPOUNDMIN`) so many other splits are tried as well.
    fn check_compound_with_rules(
        &self,
        word: &str,
        forceucase: Forceucase,
    ) -> Option<CompoundingResult<'_>> {
        self.check_compound_with_rules_impl(word, &mut Vec::new(), 0, forceucase)
    }

    fn check_compound_with_rules_impl(
        &self,
        word: &str,
        words_data: &mut Vec<&'a FlagSet>,
        start_pos: usize,
        forceucase: Forceucase,
    ) -> Option<CompoundingResult<'_>> {
        // Notes on differences from Nuspell's version of this function:
        //
        // It passes basically a `&mut String` with this function which is used with C++'s
        // `basic_string::assign` to hold a substring and that is used to look up in the word
        // list. We can avoid that by subslicing the `word: &str` with the same indices. See
        // where we call `WordList::get` below.
        //
        // There's a `try_recursive` label that Nuspell jumps to in order to recurse or continue
        // to the next iteration of the loop. We just do the recursion/continuing instead of
        // jumping (since that doesn't exist in Rust). It also uses `AT_SCOPE_EXIT` to pop the
        // part flags off of the `words_data` stack. We do that manually.
        let min_chars = self
            .aff
            .options
            .compound_min_length
            .map(|n| n.get() as usize)
            .unwrap_or(3);
        // Note that this cannot be zero but we cast it to a usize for easier use.
        debug_assert_ne!(min_chars, 0);

        for i in bytes_within(word, start_pos, min_chars) {
            let Some((part1_stem, part1_flags)) =
                self.words.get(&word[start_pos..i]).find(|(_stem, flags)| {
                    !has_flag!(flags, self.aff.options.need_affix_flag)
                        && self.aff.compound_rules.has_any_flags(flags)
                })
            else {
                continue;
            };
            words_data.push(part1_flags);

            // Check whether the remainder of the word is a valid final part.
            let direct_match = if let Some((_part2_stem, part2_flags)) =
                self.words.get(&word[i..]).find(|(_stem, flags)| {
                    !has_flag!(flags, self.aff.options.need_affix_flag)
                        && self.aff.compound_rules.has_any_flags(flags)
                }) {
                words_data.push(part2_flags);
                let ok = self.aff.compound_rules.any_rule_matches(words_data)
                    && !(forceucase.forbid()
                        && has_flag!(part2_flags, self.aff.options.compound_force_uppercase_flag));
                words_data.pop();
                ok
            } else {
                false
            };

            if direct_match {
                words_data.pop();
                return Some(CompoundingResult::new(part1_stem, part1_flags));
            }

            // Recurse with part1 still in words_data, mirroring Nuspell's AT_SCOPE_EXIT
            // which keeps part1 on the stack through the try_recursive label.
            let result = self.check_compound_with_rules_impl(word, words_data, i, forceucase);
            words_data.pop();
            if let Some(r) = result {
                return Some(r);
            }
        }

        None
    }
}

/// Checks if the input word is a number.
///
/// Numbers may have a leading `-` and can have separators within the number of `,`, `.` or `-`,
/// but not more than one separating digits.
fn is_number(word: &str) -> bool {
    let word = word.strip_prefix('-').unwrap_or(word);
    if word.is_empty() {
        return false;
    }

    let mut separated = true;
    for ch in word.chars() {
        match ch {
            '0'..='9' => separated = false,
            '.' | '-' | ',' if !separated => separated = true,
            _ => return false,
        }
    }

    !separated
}

/// Checks if the three chars around byte index `idx` are the same.
///
/// If `idx` is at the end of the word, this means the last three chars. Otherwise this means the
/// character at byte `idx`, the previous character and the next character.
///
/// # Panics
/// `idx` is assumed to be at a valid UTF-8 boundary within the word.
fn are_three_chars_equal(word: &str, idx: usize) -> bool {
    let mut chars_ahead = word[idx..].chars();
    let mut chars_behind = word[..idx].chars().rev();
    let ch = match chars_ahead.next() {
        Some(c) => c,
        None => return false,
    };
    let prev_ch = match chars_behind.next() {
        Some(c) => c,
        None => return false,
    };

    if prev_ch == ch {
        if let Some(next_ch) = chars_ahead.next() {
            if next_ch == ch {
                return true;
            }
        }

        if let Some(prev_prev_ch) = chars_behind.next() {
            if prev_prev_ch == ch {
                return true;
            }
        }
    }

    false
}

/// Checks if the character at the byte index is uppercase and the prior character is alphabetic,
/// or vice versa.
///
/// # Panics
/// `idx` is assumed to be at a valid UTF-8 boundary within the word.
fn has_uppercase_at_compound_word_boundary(word: &str, idx: usize) -> bool {
    let ch = match word[idx..].chars().next() {
        Some(c) => c,
        None => return false,
    };
    let prev_ch = match word[..idx].chars().next_back() {
        Some(c) => c,
        None => return false,
    };

    (ch.is_uppercase() && prev_ch.is_alphabetic()) || (prev_ch.is_uppercase() && ch.is_alphabetic())
}

fn prev_codepoint(word: &str, byte: usize) -> Option<(usize, char)> {
    word[..byte].char_indices().next_back()
}

/// An iterator over each byte index in the given `word` within the given margin.
///
/// For example in "abcd" with a margin of 1, this will yield `[1, 2]` (0-indexed).
///
/// `start` must be a valid byte index within `word` and `margin` must be non-zero.
fn bytes_within(word: &str, start: usize, margin: usize) -> impl Iterator<Item = usize> + '_ {
    // TODO: this seems unnecessarily complex. Can we just use prev/next_codepoint
    // helpers instead?.
    fn byte_range(word: &str, start: usize, margin: usize) -> Option<core::ops::Range<usize>> {
        let start_byte = word[start..]
            .char_indices()
            .nth(margin)
            .map(|(byte, _)| byte + start)?;
        // We want the byte _after_ the `nth - 1` character from the back because the  range is
        // exclusive on the right-hand side. We need an exclusive range  because `&word[n..=n]`
        // has surprising behavior: it's equivalent to `&word[n..n + 1]` which is not correct when
        // `n + 1` is not a valid byte index.
        //
        // When looking at the surrounding one character this must be the total word  length then
        // (again - it's an exclusive range on the right). Using a saturating subtraction here
        // to avoid a panic from `- 2` would not be equivalent.
        let end_byte = if margin == 1 {
            word.len()
        } else {
            word.char_indices()
                .rev()
                .take_while(|(byte, _)| *byte >= start_byte)
                .nth(margin - 2)
                .map(|(byte, _)| byte)?
        };
        Some(start_byte..end_byte)
    }

    debug_assert_ne!(margin, 0);

    byte_range(word, start, margin)
        .into_iter()
        .flat_map(move |range| {
            word[range.clone()]
                .char_indices()
                .map(move |(byte, _)| byte + range.start)
        })
}

// TODO: rename?
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
pub(crate) enum Forceucase {
    #[default]
    ForbidBadForceucase,
    AllowBadForceucase,
}

impl Forceucase {
    #[inline]
    const fn forbid(&self) -> bool {
        matches!(self, Self::ForbidBadForceucase)
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
pub(crate) enum HiddenHomonym {
    #[default]
    AcceptHiddenHomonym,
    SkipHiddenHomonym,
}

impl HiddenHomonym {
    #[inline]
    const fn skip(&self) -> bool {
        matches!(self, Self::SkipHiddenHomonym)
    }
}

// Similar to Nuspell's AffixingResult
#[derive(Debug)]
pub(crate) struct AffixForm<'aff> {
    stem: &'aff Stem,
    flags: &'aff FlagSet,
    // Up to 2 prefixes and/or 2 suffixes allowed.
    prefixes: [Option<&'aff Prefix>; 2],
    suffixes: [Option<&'aff Suffix>; 2],
}

// TODO: docs.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CompoundingResult<'aff> {
    stem: &'aff Stem,
    flags: &'aff FlagSet,
    num_words_modifier: u16,
    num_syllable_modifier: i16,
    affixed_and_modified: bool,
}

impl<'aff> CompoundingResult<'aff> {
    pub fn new(stem: &'aff Stem, flags: &'aff FlagSet) -> Self {
        Self {
            stem,
            flags,
            num_words_modifier: Default::default(),
            num_syllable_modifier: Default::default(),
            affixed_and_modified: Default::default(),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::EN_US;

    #[test]
    fn are_three_chars_equal_test() {
        assert!(are_three_chars_equal("aaa", 1));
        // The three chars are equal but not around byte idx 1 or 4.
        assert!(!are_three_chars_equal("baaab", 1));
        assert!(are_three_chars_equal("baaab", 2));
        assert!(are_three_chars_equal("baaab", 3));
        assert!(!are_three_chars_equal("baaab", 4));

        assert!(!are_three_chars_equal("", 0));
        assert!(!are_three_chars_equal("ab", 1));
    }

    #[test]
    fn is_number_nuspell_unit_test() {
        // Upstream: <https://github.com/nuspell/nuspell/blob/349e0d6bc68b776af035ca3ff664a7fc55d69387/tests/unit_test.cxx#L461-L471>

        assert!(!is_number(""));
        assert!(is_number("1234567890"));
        assert!(is_number("-1234567890"));
        assert!(is_number("123.456.78-9,0"));
        assert!(is_number("-123.456.78-9,0"));
        assert!(!is_number("123..456.78-9,0"));
        assert!(!is_number("123.456.-78-9,0"));
        assert!(!is_number("123..456.78-9-,0"));
        // trailing separator
        assert!(!is_number("1."));
        assert!(!is_number("1,2,"));
    }

    #[test]
    fn huge_word_is_rejected() {
        assert!(!EN_US.check(&"hello".repeat(MAX_WORD_LEN)));
    }

    #[test]
    fn empty_word_is_accepted() {
        assert!(EN_US.check(""));
    }

    #[test]
    fn check_number_test() {
        assert!(EN_US.check("123456789"));
    }

    #[test]
    fn check_exact_word_in_the_wordlist_test() {
        // adventure/DRSMZG (en_US.dic line 11021)
        assert!(EN_US.check("adventure"));
    }

    #[test]
    fn check_exact_words_in_the_wordlist_with_break_patterns_test() {
        // en_US uses the default break patterns: `^-`, `-`, `-$`.
        // All of these words are stems in the dictionary.
        assert!(EN_US.check("-any"));
        assert!(EN_US.check("anyway-anywhere"));
        assert!(EN_US.check("ace-"));
    }

    #[test]
    fn check_word_with_single_suffix() {
        // Suffix 'V' from en_US.aff:
        // SFX V N 2
        // SFX V   e     ive        e
        // SFX V   0     ive        [^e]

        // concuss/V (en_US.dic line 17451)
        assert!(EN_US.check("concussive"));
        // regenerate/V (en_US.dic line 38722)
        assert!(EN_US.check("regenerative"));
    }

    #[test]
    fn check_word_with_single_prefix() {
        // Prefix 'A' from en_US.aff
        // PFX A Y 1
        // PFX A   0     re         .

        // route/ADSG (en_US.dic line 39619)
        assert!(EN_US.check("reroute"));
    }

    #[test]
    fn check_other_casings() {
        // "drink" is a stem and should be recognized in any case.
        assert!(EN_US.check("drink"));
        assert!(EN_US.check("Drink"));
        assert!(EN_US.check("DRINK"));
        // Achilles/M from en_US.dic line 116, should be recognized in titlecase and uppercase but
        // not lowercase.
        assert!(!EN_US.check("achilles"));
        assert!(EN_US.check("Achilles"));
        assert!(EN_US.check("ACHILLES"));
        // BBQ from en_US.dic line 810, should only be recognized in uppercase.
        assert!(!EN_US.check("bbq"));
        assert!(!EN_US.check("Bbq"));
        assert!(EN_US.check("BBQ"));
    }

    #[test]
    fn check_sharp_upper_casing() {
        let aff = r#"
        CHECKSHARPS
        "#;

        // aussaß/Z from de_DE_frami.dic line 118186. It has both a "ss" and a "ß".
        // Äußern/SJm from line 195. It's title-cased.
        // bass/Tpozm from line 120101. Doesn't use ß.
        let dic = r#"3
        aussaß
        Äußern
        bass
        "#;

        let dict = Dictionary::new(aff, dic).unwrap();

        assert!(dict.check("aussaß"));
        assert!(dict.check("Aussaß"));
        assert!(dict.check("AUSSASS"));

        // Lowercase is not allowed but uppercase and title-case are.
        assert!(!dict.check("äußern"));
        assert!(dict.check("Äußern"));
        assert!(dict.check("ÄUSSERN"));

        assert!(dict.check("bass"));
        assert!(dict.check("Bass"));
        assert!(dict.check("BASS"));
        assert!(!dict.check("baß"));

        // Not in the dictionary:
        assert!(!dict.check("bassa"));
        assert!(!dict.check("Bassa"));
        assert!(!dict.check("BASSA"));
    }

    #[test]
    fn check_apostrophe_upper_casing() {
        let aff = "";
        // from it_IT
        let dic = r#"4
        cent'anni
        d'Intelvi
        Anch'Egli
        anch'Ella
        "#;

        let dict = Dictionary::new(aff, dic).unwrap();

        assert!(dict.check("cent'anni"));
        assert!(dict.check("d'Intelvi"));
        assert!(dict.check("Anch'Egli"));
        assert!(dict.check("anch'Ella"));

        assert!(dict.check("CENT'ANNI"));
        assert!(dict.check("D'INTELVI"));
        assert!(dict.check("ANCH'EGLI"));
        assert!(dict.check("ANCH'ELLA"));
    }

    #[test]
    fn iconv_test() {
        // Magic quotes are converted by en_US.
        assert!(EN_US.check("can’t"));

        // Both from en_ZA
        let aff = r#"
        ICONV 2
        ICONV ﬃ ffi
        ICONV ﬄ ffl
        "#;
        let dic = r#"2
        affine
        affluent/Y
        "#;
        let dict = Dictionary::new(aff, dic).unwrap();
        assert!(dict.check("affine"));
        assert!(dict.check("aﬃne"));
        assert!(dict.check("affluent"));
        assert!(dict.check("aﬄuent"));
    }

    #[test]
    fn false_prefix_test() {
        // "un" is a prefix in en_US and "drink" is a stem. "drink"'s flags don't allow you to use
        // the "un" prefix though, so this word isn't correct.
        assert!(!EN_US.check("undrink"));
        assert!(!EN_US.check("undrinks"));
    }

    #[test]
    fn check_word_with_prefix_and_suffix() {
        // Prefix 'U' from en_US.aff
        // PFX U Y 1
        // PFX U   0     un         .

        // Suffix 'D' from en_US.aff
        // SFX D Y 4
        // SFX D   0     d          e
        // SFX D   y     ied        [^aeiou]y
        // SFX D   0     ed         [^ey]
        // SFX D   0     ed         [aeiou]y

        // earth/UDYG (en_US.dic line 20997)
        assert!(EN_US.check("unearthed"));
    }

    #[test]
    fn check_word_with_double_suffix() {
        // en_US doesn't use continuation flags in prefixes or suffixes so we need to create a
        // small custom dictionary to check this. We'll use part of es_ANY, suffixes 'S' and 'J'
        // and prefix 'k' trimmed to only the clauses we need.
        let aff = r#"
        SFX J Y 1
        SFX J le ilidad/S ble

        SFX S Y 2
        SFX S 0 s [aceéfgiíkoóptuúw]
        SFX S 0 es [bdhíjlmrúxy]

        PFX k Y 1
        PFX k 0 in [^blpr]
        "#;

        // es_ANY.dic line 48787
        // es_ANY.dic line 63299
        let dic = r#"2
        perdurable/JS
        trazable/kSJ
        "#;

        let dict = Dictionary::new(aff, dic).unwrap();

        // Stem
        assert!(dict.check("perdurable"));
        // Single suffix 'J'
        assert!(dict.check("perdurabilidad"));
        // Single suffix 'S'
        assert!(dict.check("perdurables"));
        // Double suffix. 'S' is the outer suffix then 'J' is the inner suffix.
        assert!(dict.check("perdurabilidades"));

        // Stem
        assert!(dict.check("trazable"));
        // Single prefix
        assert!(dict.check("intrazable"));
        // Single suffix 'J'
        assert!(dict.check("trazabilidad"));
        // Single suffix 'S'
        assert!(dict.check("trazables"));
        // Prefix and suffix 'J'
        assert!(dict.check("intrazabilidad"));
        // Prefix and suffix 'S'
        assert!(dict.check("intrazables"));
        // Double suffix. 'S' is the outer suffix then 'J' is the inner suffix.
        assert!(dict.check("trazabilidades"));
        // Prefix and double suffix. 'S' is the outer suffix then 'J' is the inner suffix.
        assert!(dict.check("intrazabilidades"));
    }

    #[test]
    fn check_word_with_suffix_then_prefix_then_suffix() {
        // I'm not sure if any real dictionaries use this feature. If you can find one please
        // open an issue or PR to update this unit test.
        // 'o' is the outer suffix. It's a continuation of 'i', the inner suffix, so it's only
        // valid when the 'i' suffix is also present. 'p' is just a regular prefix.
        let aff = r#"
        SFX o Y 1
        SFX o 0 suf2 .

        SFX i Y 1
        SFX i 0 suf1/o .

        PFX p Y 1
        PFX p 0 pre .
        "#;

        let dic = r#"1
        stem/pi
        "#;

        let dict = Dictionary::new(aff, dic).unwrap();

        assert!(dict.check("stem"));
        assert!(dict.check("prestem"));
        assert!(dict.check("prestemsuf1"));
        assert!(dict.check("prestemsuf1suf2"));

        assert!(!dict.check("stemsuf2"));
        assert!(!dict.check("prestemsuf2"));
    }

    #[test]
    fn complex_prefixes_test() {
        // I believe no real dictionaries use this feature ("COMPLEXPREFIXES"). So we'll have to
        // make our own with fake prefixes/suffixes.
        let aff = r#"
        COMPLEXPREFIXES

        FLAG long

        PFX p1 Y 1
        PFX p1 0 pfx1 .

        PFX p2 Y 1
        PFX p2 0 pfx2/p1 .

        SFX s1 Y 1
        SFX s1 0 suf1 .

        SFX s2 Y 1
        SFX s2 0 suf2/p1 .
        "#;

        let dic = r#"2
        stem1/p1p2s1
        stem2/p2s2
        "#;

        let dict = Dictionary::new(aff, dic).unwrap();
        assert!(dict.aff_data.options.complex_prefixes);

        assert!(dict.check("stem1"));
        assert!(dict.check("pfx1stem1"));
        assert!(dict.check("pfx2stem1"));
        assert!(dict.check("pfx1pfx2stem1"));
        assert!(!dict.check("pfx2pfx1stem1"));
        assert!(dict.check("pfx1pfx2stem1suf1"));

        assert!(dict.check("stem2"));
        // p1 prefix is not allowed unless the s2 suffix is present.
        assert!(!dict.check("pfx1stem2"));
        assert!(dict.check("pfx1stem2suf2"));
        assert!(dict.check("pfx1pfx2stem2suf2"));
        assert!(!dict.check("pfx2pfx1stem2suf2"));
    }

    #[test]
    fn en_us_compounding() {
        // Examples given alongside the definitions of the compound rules in en_US.aff.
        // `n*1t`
        assert!(EN_US.check("10th"));
        assert!(EN_US.check("11th"));
        assert!(EN_US.check("12th"));
        assert!(EN_US.check("57614th"));
        // `n*mp`
        assert!(EN_US.check("21st"));
        assert!(EN_US.check("22nd"));
        assert!(EN_US.check("123rd"));
        assert!(EN_US.check("1234th"));
    }

    #[test]
    fn check_lower_as_other_casings() {
        assert!(!EN_US.check("alice"));
        assert!(!EN_US.check("rsvp"));
        assert!(EN_US.checker().check_lower_as_title(true).check("alice"));
        assert!(EN_US.checker().check_lower_as_upper(true).check("rsvp"));
    }

    #[test]
    fn emoji_pfx_flag_test() {
        // See <https://github.com/titoBouzout/Dictionaries/blob/80a5112e41b21ade9d00b837c05b0d06280f138f/Spanish.aff#L75-L77>
        let aff = r#"
        FLAG UTF-8

        PFX 🔭 Y 2
        PFX 🔭 0 macro [^r]
        PFX 🔭 0 macror r
        "#;

        let dic = r#"1
        concierto/hS🔭
        "#;

        let dict = Dictionary::new(aff, dic).unwrap();

        assert!(dict.check("macroconcierto"));
    }

    #[test]
    fn rep_panic() {
        // <https://github.com/blopker/codebook/issues/72>
        let aff = r#"
        # At least this many letters in each word within a compound:
        COMPOUNDMIN 1
        # Don't allow autogenerated compounds that are very similar
        # to some word in the dictionary:
        CHECKCOMPOUNDREP
        # For suffixes used to create first part of a compound
        COMPOUNDPERMITFLAG W
        # May appear first in compound words:
        COMPOUNDBEGIN X
        # May be middle elements in compound words:
        COMPOUNDMIDDLE U
        # May appear last in compound words:
        COMPOUNDEND Y

        REP 1
        REP ochmed$ _och_med

        SFX  f  N  2
        SFX  f  a  s/WUZ  a
        SFX  f  0  s/WUZ  [^a]

        "#;
        let dic = r#"3
        träd/ABDXY
        algoritm/ADHTXY
        sökning/ADfGvY
        "#;
        let dict = Dictionary::new(aff, dic).unwrap();
        assert!(dict.check("trädsökningsalgoritm"));
    }

    #[test]
    fn crossproduct_n() {
        let aff = r#"
        SFX A N 1
        SFX A 0 ed .
        PFX B Y 1
        PFX B 0 re .
        "#;
        let dic = r#"1
        walk/AB
        "#;
        let dict = Dictionary::new(aff, dic).unwrap();
        // SFX A is N (no cross-product), so "rewalked" should NOT be accepted.
        assert!(!dict.check("rewalked"));
    }

    #[test]
    fn compound_simplified_triple_rep_panic() {
        // SIMPLIFIEDTRIPLE compound checking creates word_with_triple by inserting a character
        // into the original word. check_compound_impl runs against word_with_triple, so
        // part2_entry.stem may be longer than the corresponding slice in the original word.
        // When part2_word.len() covers the full tail of word_with_triple, indexing
        // word[i..i + part2_word.len()] panics because i + part2_word.len() > word.len().
        //
        // "bbabcge" trace (i=3, word_with_triple="bbaabcge"):
        //   part2 found: "abcg" matches stem "abcde" via the SFX rule (5 chars)
        //   word[3..3+5] = word[3..8] but word.len() = 7 -> panic
        let aff = r#"
        COMPOUNDBEGIN B
        COMPOUNDMIDDLE M
        COMPOUNDEND E
        COMPOUNDMIN 1
        SIMPLIFIEDTRIPLE
        CHECKCOMPOUNDREP
        SFX S Y 1
        SFX S de g .
        "#;
        let dic = r#"3
        bba/B
        abcde/MS
        e/E
        "#;
        let dict = Dictionary::new(aff, dic).unwrap();
        dict.check("bbabcge");
    }

    #[test]
    fn compound_rule_three_part() {
        // "foobarbaz" is a valid 3-part compound matching rule "ABC".
        // "barbaz/B" causes part2 to be found at the first split ("foo"|"barbaz"),
        // but [A,B] doesn't match "ABC". Part1's flags must still be present when
        // recursing so the deeper split "foo"+"bar"+"baz" can match [A,B,C].
        let aff = r#"
        COMPOUNDRULE 1
        COMPOUNDRULE ABC
        "#;
        let dic = r#"4
        foo/A
        bar/B
        baz/C
        barbaz/B
        "#;
        let dict = Dictionary::new(aff, dic).unwrap();
        assert!(dict.check("foobarbaz"));
    }
}
