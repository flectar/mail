// The parent module covers suggestions made by editing the input word (for example swapping two
// characters.)  This module instead covers "ngram suggestions" - a fancier and more expensive
// procedure.
//
// The basic idea of ngram suggestion is to find words in the dictionary similar to the input
// word. To do that we try to filter down the wordlist words in a multi step process.
//
// 1. Find 100 stems in the word list with the highest "ngram similarity" score to the input word.
// 2. Expand the prefixes and suffixes for those 100 stems and find the 200 expanded words with
//    the highest ngram similarity score to the input word.
// 3. Re-score the (up to) 200 best candidates based on weighted ngram similarity and other
//    bespoke metrics.
// 4. Push the most promising candidates to the `out` vec.
//
// Note that this is **very** expensive compared to regular edit based suggestions since we need
// to iterate on the word list and expand affixes.
//
// Ngram suggestions are also criticized as not very smart:
// <https://battlepenguin.com/tech/aspell-and-hunspell-a-tale-of-two-spell-checkers/>. Ngram
// suggestions are implemented for compatibility with Nuspell/Hunspell but we could consider
// adding other strategies as well, for example looking at the Aspell code.

// # Implementation note
//
// There's a theme in this module of reusing `Vec<T>` allocations rather than having functions
// return new `Vec<T>`s. This is slightly awkward but is necessary for performance. The
// `ngram_suggest` function in this module has a very hot loop iterating over every stem in the
// word list, so individual allocations add up and the performance boost from reusing allocations
// becomes noticeable.

use core::hash::BuildHasher;
use core::slice;

use crate::alloc::{collections::BinaryHeap, string::String, vec::Vec};

use crate::aff::{CaseHandling, HIDDEN_HOMONYM_FLAG, MAX_SUGGESTIONS};
use crate::{FlagSet, FULL_WORD};

use super::Suggester;

macro_rules! has_flag {
    ( $flags:expr, $flag:expr ) => {{
        match $flag {
            Some(flag) => $flags.contains(&flag),
            None => false,
        }
    }};
}

/// This struct is used as a wrapper for other data (for example stem+flagset) to organize a
/// min-heap with BinaryHeap. (`BinaryHeap` from the standard library is a max-heap and you need
/// to reverse the Ord of the type to use it as a min-heap.)
#[derive(Debug, PartialEq, Eq)]
struct MinScored<T: PartialEq + Eq> {
    score: isize,
    inner: T,
}

impl<T: PartialEq + Eq> Ord for MinScored<T> {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.score.cmp(&other.score).reverse()
    }
}

impl<T: PartialEq + Eq> PartialOrd<Self> for MinScored<T> {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<S: BuildHasher> Suggester<'_, S> {
    pub(super) fn ngram_suggest(&self, word_str: &str, out: &mut Vec<String>) {
        // First step: find 100 stems in the word list with the best ngram score.

        let mut word_buf = Vec::with_capacity(word_str.len());
        let word = CharsStr::new(word_str, &mut word_buf);
        // Overallocate so we probably don't need to reallocate in the loop:
        let mut stem_buf = Vec::with_capacity(word.len_chars() * 2);
        let mut lowercase_stem_buf = Vec::with_capacity(stem_buf.len());
        let mut roots = BinaryHeap::with_capacity(100);
        for entry @ (stem, flagset) in self.checker.words.iter() {
            if flagset.contains(&self.checker.aff.options.forbidden_word_flag)
                || has_flag!(flagset, self.checker.aff.options.no_suggest_flag)
                || has_flag!(flagset, self.checker.aff.options.only_in_compound_flag)
                || flagset.contains(&HIDDEN_HOMONYM_FLAG)
            {
                continue;
            }
            let stem = CharsStr::new(stem.as_str(), &mut stem_buf);
            let mut score =
                left_common_substring_length(&self.checker.aff.options.case_handling, word, stem)
                    as isize;

            // TODO: lowercase into buf so we can reuse this allocation? It would mean copying a
            // lot of code from the standard library unfortunately.
            let lowercase_stem = self
                .checker
                .aff
                .options
                .case_handling
                .lowercase(stem.as_str());
            let lowercase_stem = CharsStr::new(lowercase_stem.as_str(), &mut lowercase_stem_buf);
            score += ngram_similarity_longer_worse(3, word, lowercase_stem);

            let root = MinScored {
                score,
                inner: entry,
            };
            if roots.len() != 100 {
                roots.push(root);
            } else if roots.peek().is_some_and(|entry| score > entry.score) {
                // The heap has hit capacity. Drop the lowest scoring root and push this new
                // higher scored root.
                roots.pop();
                roots.push(root);
            }
        }

        // Calculate a somewhat low threshold score so that we can ignore bad suggestions in the
        // next steps.
        let mut mangled_word = String::new();
        let mut threshold = 0isize;
        for k_byte_idx in word.char_indices().skip(1).take(3) {
            let k_byte_idx = *k_byte_idx as usize;
            mangled_word.clear();
            mangled_word.push_str(&word_str[..k_byte_idx]);
            mangled_word.extend(word_str[k_byte_idx..].chars().enumerate().map(|(i, ch)| {
                if i % 4 == 0 {
                    '*'
                } else {
                    ch
                }
            }));
            threshold += ngram_similarity_any_mismatch(word.len_chars(), word, &mangled_word);
        }

        threshold /= 3;

        // Step two: expand the affixes for these wordlist entries, gathering the 200 highest
        // scoring candidates.

        let mut expanded_list = Vec::new();
        let mut expanded_cross_affix = Vec::new();
        let mut expanded_word_buf = Vec::with_capacity(word.len_chars() * 2);
        let mut guess_words = BinaryHeap::new();

        for MinScored {
            inner: (stem, flags),
            ..
        } in roots
        {
            expanded_cross_affix.clear();
            self.expand_stem_for_ngram(
                stem.as_str(),
                flags,
                word_str,
                &mut expanded_list,
                &mut expanded_cross_affix,
            );
            for expanded_word in expanded_list.drain(..) {
                let mut score = left_common_substring_length(
                    &self.checker.aff.options.case_handling,
                    word,
                    CharsStr::new(&expanded_word, &mut expanded_word_buf),
                ) as isize;

                let lower_expanded_word = self
                    .checker
                    .aff
                    .options
                    .case_handling
                    .lowercase(&expanded_word);
                score +=
                    ngram_similarity_any_mismatch(word.len_chars(), word, &lower_expanded_word);

                if score < threshold {
                    continue;
                }

                let guess_word = MinScored {
                    score,
                    inner: expanded_word,
                };
                if guess_words.len() != 200 {
                    guess_words.push(guess_word);
                } else if guess_words.peek().is_some_and(|entry| score > entry.score) {
                    guess_words.pop();
                    guess_words.push(guess_word);
                }
            }
        }

        // Step three: rescore these up to 200 potential matches based on a weighted ngram
        // calculation and other bespoke measurements.

        // Scratchpad vector used for calculating longest common subsequences. See
        // `longest_common_subsequence_length`.
        let mut lcs_state = Vec::new();
        // Nuspell questions whether or not the heap needs to be sorted before iterating.
        // For now, they sort the heap. I think Nuspell is correct to do so because the `break`
        // below could cause a different end behavior based on whether we're iterating on a sorted
        // or unsorted vec. Note that we are sorting in descending order here. In my manual
        // testing, using `BinaryHeap::into_vec` instead produces no noticeable difference.
        let mut guess_words = guess_words.into_sorted_vec();
        let mut lower_guess_word_buf = Vec::with_capacity(word.len_chars());
        // `iter_mut` because this loop modifies the `score`.
        for MinScored {
            score,
            inner: guess_word,
        } in guess_words.iter_mut()
        {
            let lower_guess_word = self.checker.aff.options.case_handling.lowercase(guess_word);
            let lower_guess_word = CharsStr::new(&lower_guess_word, &mut lower_guess_word_buf);

            let lcs = longest_common_subsequence_length(word, lower_guess_word, &mut lcs_state);

            if word.len_chars() == lower_guess_word.len_chars() && word.len_chars() == lcs {
                *score += 2000;
                break;
            }

            let mut ngram2 = ngram_similarity_any_mismatch_weighted(2, word, lower_guess_word);
            ngram2 += ngram_similarity_any_mismatch_weighted(2, lower_guess_word, word);
            let ngram4 = ngram_similarity_any_mismatch(4, word, lower_guess_word.as_str());

            let left_common = left_common_substring_length(
                &self.checker.aff.options.case_handling,
                word,
                lower_guess_word,
            );

            let (num_eq_chars_same_pos, eq_char_is_swapped) =
                count_eq_at_same_pos(word, lower_guess_word);

            *score = 2 * lcs as isize;
            *score -= (word.len_chars() as isize - lower_guess_word.len_chars() as isize).abs();
            *score += left_common as isize + ngram2 + ngram4;
            if num_eq_chars_same_pos != 0 {
                *score += 1;
            }
            if eq_char_is_swapped {
                *score += 10;
            }

            if 5 * ngram2
                < ((word.len_chars() + lower_guess_word.len_chars())
                    * (10 - self.checker.aff.options.max_diff_factor as usize))
                    as isize
            {
                *score -= 1000;
            }
        }

        // We've updated the scores (`iter_mut` above) so we need to re-sort the Vec.
        // Note that because of `MinScored<T>`'s `Ord` implementation the Vec is ordered by
        // score descending. (Normally a sort would be ascending.)
        guess_words.sort_unstable();

        // Step four: push the most promising of the candidates to `out`.

        let be_more_selective = guess_words.first().is_some_and(|guess| guess.score > 1000);
        let old_num_suggestions = out.len();
        let max_suggestions = MAX_SUGGESTIONS
            .min(old_num_suggestions + self.checker.aff.options.max_ngram_suggestions as usize);
        for MinScored {
            score,
            inner: guess_word,
        } in guess_words.into_iter()
        {
            if out.len() == max_suggestions {
                break;
            }
            // Note that we are iterating in descending score order, so this sets a minimum.
            if be_more_selective && score <= 1000 {
                break;
            }
            if score < -100
                && (old_num_suggestions != out.len() || self.checker.aff.options.only_max_diff)
            {
                break;
            }
            // Nuspell converts back to UTF-8 here but we've been working with UTF-8 all along.
            if out.iter().any(|sug| guess_word.contains(sug)) {
                if score < -100 {
                    break;
                } else {
                    continue;
                }
            }
            out.push(guess_word);
        }
    }

    fn expand_stem_for_ngram(
        &self,
        stem: &str,
        flags: &FlagSet,
        word: &str,
        expanded_list: &mut Vec<String>,
        cross_affix: &mut Vec<bool>,
    ) {
        expanded_list.clear();
        cross_affix.clear();

        if !has_flag!(flags, self.checker.aff.options.need_affix_flag) {
            expanded_list.push(String::from(stem));
            cross_affix.push(false);
        }

        if flags.is_empty() {
            return;
        }

        // TODO: investigate collecting `all_flags` (like we do for compounds IIRC) on the
        // prefixes and suffixes tables to see if we can disqualify flagsets faster?

        for suffix in self.checker.aff.suffixes.iter() {
            // Nuspell:
            // if (!cross_valid_inner_outer(flags, suffix))
            // 	continue;
            if !flags.contains(&suffix.flag) {
                continue;
            }
            if !self.checker.is_outer_affix_valid::<_, FULL_WORD>(suffix) {
                continue;
            }
            if self.checker.is_circumfix(suffix) {
                continue;
            }
            // Nuspell has a todo here:
            // > Suffixes marked with needaffix or circumfix should not just be skipped as we can
            // > later add prefix. This is not handled in hunspell, too.
            if suffix
                .strip
                .as_ref()
                .is_some_and(|suf| !stem.ends_with(&**suf))
            {
                continue;
            }
            if !suffix.condition_matches(stem) {
                continue;
            }
            if !suffix.add.is_empty() && !word.ends_with(&*suffix.add) {
                continue;
            }

            let expanded = suffix.to_derived(stem);
            expanded_list.push(expanded);
            cross_affix.push(suffix.crossproduct);
        }

        // Expand crossproduct words - prefixes for suffix-expanded words marked with
        // `crossproduct: true`.
        for i in 0..expanded_list.len() {
            if !cross_affix[i] {
                continue;
            }

            for prefix in self.checker.aff.prefixes.iter() {
                let suffixed_stem = &expanded_list[i];
                // if (!cross_valid_inner_outer(flags, prefix))
                // 	continue;
                if !flags.contains(&prefix.flag) {
                    continue;
                }
                if !self.checker.is_outer_affix_valid::<_, FULL_WORD>(prefix) {
                    continue;
                }
                if self.checker.is_circumfix(prefix) {
                    continue;
                }
                if prefix
                    .strip
                    .as_ref()
                    .is_some_and(|pre| !suffixed_stem.starts_with(&**pre))
                {
                    continue;
                }
                if !prefix.condition_matches(suffixed_stem) {
                    continue;
                }
                if !prefix.add.is_empty() && !word.starts_with(&*prefix.add) {
                    continue;
                }

                let expanded = prefix.to_derived(stem);
                expanded_list.push(expanded);
            }
        }

        for prefix in self.checker.aff.prefixes.iter() {
            // Nuspell:
            // if (!cross_valid_inner_outer(flags, prefix))
            // 	continue;
            if !flags.contains(&prefix.flag) {
                continue;
            }
            if !self.checker.is_outer_affix_valid::<_, FULL_WORD>(prefix) {
                continue;
            }
            if self.checker.is_circumfix(prefix) {
                continue;
            }
            if prefix
                .strip
                .as_ref()
                .is_some_and(|pre| !stem.starts_with(&**pre))
            {
                continue;
            }
            if !prefix.condition_matches(stem) {
                continue;
            }
            if !prefix.add.is_empty() && !word.starts_with(&*prefix.add) {
                continue;
            }

            let expanded = prefix.to_derived(stem);
            expanded_list.push(expanded);
        }
    }
}

/// A borrowed string (`&str`) wrapper which eagerly computes `core::str::char_indices`.
///
/// With this type it is cheap both to ask for the number of Unicode characters in the string
/// (`len_chars`) and to subslice the string by _character index_.
///
/// Compare with a regular `&str`: the number of characters can be counted with
/// `string.chars().count()` - a linear operation w.r.t. the length of the string. Subslicing
/// by character indices can't be done directly on a `&str`.
///
/// # Some discussion on UTF-8 vs. UTF-32
///
/// Nuspell uses UTF-32 for the ngram similarity section of the suggester because you can iterate
/// and index on characters easily. (In UTF-32 every index is a character because in Unicode you
/// need at most 32 bits to represent any character. (Note: not the same as a grapheme cluster
/// like 🏴‍☠️).) The C++ standard library seems to be better optimized for UTF-32 operations (I
/// believe a `std::u32_string` and equivalent string views), specifically
/// std::basic_string_view<char32_t>`'s `find` which is central to `ngram_similarity`. We now
/// use `str::contains(kgram)` equivalently and it performs much better than
/// `&[char]::starts_with` plus indexing.
///
/// Rust's UTF-8 methods are well optimized. Specifically, taking advantage of `memchr` and SIMD
/// to search `str`s. We retain a performance advantage by staying in UTF-8 and avoiding the more
/// generically (read: dumbly) implemented `Eq for &[T]` or `&[T: Eq]::starts_with`.
///
/// In Spellbook's history this module was first implemented to work with UTF-32. Switching to
/// UTF-8 yielded an impressive 25% speed up for suggesting wrong words like "exmaple" which end
/// up in the `ngram_similarity` loop.
///
/// <details><summary>What's curious about "exmaple" specifically?...</summary>
///
/// "ngram similarity" is a kind of 'layer cake' when it comes to performance. The more similar
/// the input strings, the more you have to compare. First you start at `k=1` and check how many
/// times each character in `left` appears in `right`. If that happens more than twice for any
/// character you check how many times any two-character (`k=2`) combo in `left` appears in
/// `right`. If that happens more than twice then you move onto layer 3 (`k=3`).
///
/// This means that when `left` contains two or more of a common letter for the language - like
/// "e" is in English - then the `ngram_similarity` function does much more work.
///
/// `ngram_similarity` relies on finding sub-`&str`s in `right`, so the faster that you can
/// determine whether a kgram of `left` appears in `right`, the faster `ngram_similarity` will be.
///
/// </details>
///
/// The original implementation eagerly collected `&str`s into `&[char]` once - paying the cost
/// of converting to UTF-32 once. This type is similar - we only run a character iterator over
/// the string once. But we can take advantage of optimized UTF-8 comparison tools in the standard
/// library.
///
/// # Lifetimes
///
/// For performance reasons this struct borrows a slab of memory from a `Vec<u16>` which is
/// hopefully instantiated many fewer times than this struct. See `CharsStr::new` docs.
#[derive(Clone, Copy)]
struct CharsStr<'s, 'i> {
    inner: &'s str,
    char_indices: &'i [u16],
}

impl<'s, 'i> CharsStr<'s, 'i> {
    /// Creates a `CharsStr`, borrowing the input `&str` and `slab`'s allocation.
    ///
    /// Taking in `slab` here is weird - it's not a nice API. We do it because otherwise we'd need
    /// to allocate a new, short-lived `Vec<u16>` for every stem in the WordList. Short lived
    /// allocations are bad for performance - `alloc`/`dealloc` from the Rust standard library
    /// (and their implementations way down to the kernel) are usually slow compared to
    /// stack-based operations. It's usually better to reuse a (probably overallocated) "block" /
    /// "slab" of memory, and, intuitively, the savings get better the more you reuse.
    ///
    /// Above in `ngram_suggest`, the vector used as `slab` is reused across iterations of
    /// stems in the wordlist for example. In fact we allocate a vector just for the stem's
    /// `CharsStr`s. Instead of allocating per iteration of that loop we allocate once and
    /// probably reallocate very rarely.
    fn new(s: &'s str, slab: &'i mut Vec<u16>) -> Self {
        let len_bytes = s.len();
        // Note: number of bytes is greater than or equal to number of chars, so all `as u16`
        // conversions are safe after this assertion. (Equal when the string is ASCII only.)
        assert!(len_bytes <= u16::MAX as usize);
        slab.clear();
        slab.extend(s.char_indices().map(|(i, _ch)| i as u16));
        // Push the length so that we can make exclusive ranges out of the windows of the
        // `char_indices` slice.
        slab.push(len_bytes as u16);

        Self {
            inner: s,
            char_indices: slab.as_slice(),
        }
    }

    const fn len_chars(&self) -> usize {
        // We push an extra element for the total `self.inner.len()`.
        self.char_indices.len() - 1
    }

    const fn is_empty(&self) -> bool {
        // As above, we pushed the extra element. So when `self.inner` is empty,
        // `self.char_indices` should be `&[0u16]`.
        self.char_indices.len() == 1
    }

    const fn as_str(&self) -> &str {
        self.inner
    }

    /// Returns a `&str` subslice containing all of the characters in the given _character_ range.
    ///
    /// Note that this method takes character indices and not byte indices.
    fn char_slice(&self, char_range: core::ops::Range<usize>) -> &str {
        let start_byte = self.char_indices[char_range.start] as usize;
        let end_byte = self.char_indices[char_range.end] as usize;
        // SAFETY: the caller is in charge of providing char indices that are in bounds of the
        // `self.char_indices` array. (Those accesses are bounds checked.) All byte indices in
        // `self.char_indices` are valid.
        // Unfortunately the bounds checks cost a noticeable amount on a flamegraph, so we prefer
        // the unsafe version to `&self.inner[start_byte..end_byte]`.
        unsafe { self.inner.get_unchecked(start_byte..end_byte) }
    }

    fn char_at(&self, char_idx: usize) -> &str {
        let start_byte = self.char_indices[char_idx] as usize;
        let end_byte = self.char_indices[char_idx + 1] as usize;

        // SAFETY: Same as above. All byte indices in `self.char_indices` are valid and the above
        // accesses are checked.
        unsafe { self.inner.get_unchecked(start_byte..end_byte) }
    }

    fn char_iter(&self) -> impl Iterator<Item = &'s str> + '_ {
        // SAFETY: as above, all byte indices in `self.char_indices` are valid. `slice::windows`
        // always produces valid indices into the slice, so all of these accesses can be done
        // unchecked safely.
        self.char_indices.windows(2).map(|idxs| unsafe {
            let start = *idxs.get_unchecked(0) as usize;
            let end = *idxs.get_unchecked(1) as usize;
            self.inner.get_unchecked(start..end)
        })
    }

    fn char_indices(&self) -> slice::Iter<'_, u16> {
        self.char_indices.iter()
    }
}

fn left_common_substring_length(
    case_handling: &CaseHandling,
    left: CharsStr,
    right: CharsStr,
) -> usize {
    let mut left_chars = left.as_str().chars();
    let mut right_chars = right.as_str().chars();

    let Some((l, r)) = left_chars.next().zip(right_chars.next()) else {
        return 0;
    };

    if l != r && !case_handling.is_char_eq_lowercase(l, r) {
        return 0;
    }

    index_of_mismatch(left_chars, right_chars)
        .map(|idx| idx + 1)
        .unwrap_or(left.len_chars())
}

fn index_of_mismatch<T: Eq, I: Iterator<Item = T>>(left: I, mut right: I) -> Option<usize> {
    left.enumerate().find_map(|(idx, l)| match right.next() {
        Some(r) if r == l => None,
        _ => Some(idx),
    })
}

fn ngram_similarity_longer_worse(n: usize, left: CharsStr, right: CharsStr) -> isize {
    if right.is_empty() {
        return 0;
    }
    let mut score = ngram_similarity(n, left, right.as_str());
    let d = (right.len_chars() as isize - left.len_chars() as isize) - 2;
    if d > 0 {
        score -= d;
    }
    score
}

// Nuspell calls this `ngram_similarity_low_level`.
fn ngram_similarity(n: usize, left: CharsStr, right: &str) -> isize {
    let n = n.min(left.len_chars());
    let mut score = 0;

    for k in 1..=n {
        let mut k_score = 0;
        for i in 0..=left.len_chars() - k {
            let kgram = left.char_slice(i..i + k);
            if right.contains(kgram) {
                k_score += 1;
            }
        }
        score += k_score;
        if k_score < 2 {
            break;
        }
    }

    score
}

fn ngram_similarity_any_mismatch(n: usize, left: CharsStr, right: &str) -> isize {
    if right.is_empty() {
        return 0;
    }
    let mut score = ngram_similarity(n, left, right);
    let d = (right.chars().count() as isize - left.len_chars() as isize).abs() - 2;
    if d > 0 {
        score -= d;
    }
    score
}

// Nuspell returns an isize.
fn longest_common_subsequence_length(
    left: CharsStr,
    right: CharsStr,
    state_buffer: &mut Vec<usize>,
) -> usize {
    state_buffer.clear();
    state_buffer.resize(right.len_chars(), 0);

    let mut row1_prev = 0;
    for l in left.char_iter() {
        row1_prev = 0;
        let mut row2_prev = 0;
        for (j, row2_current) in state_buffer.iter_mut().enumerate().take(right.len_chars()) {
            let row1_current = *row2_current;
            *row2_current = if l == right.char_at(j) {
                row1_prev + 1
            } else {
                row1_current.max(row2_prev)
            };
            row1_prev = row1_current;
            row2_prev = *row2_current;
        }
        row1_prev = row2_prev;
    }

    row1_prev
}

fn ngram_similarity_any_mismatch_weighted(n: usize, left: CharsStr, right: CharsStr) -> isize {
    if right.is_empty() {
        return 0;
    }
    let mut score = ngram_similarity_weighted(n, left, right.as_str());
    let d = (right.len_chars() as isize - left.len_chars() as isize).abs() - 2;
    if d > 0 {
        score -= d;
    }
    score
}

fn ngram_similarity_weighted(n: usize, left: CharsStr, right: &str) -> isize {
    let n = n.min(left.len_chars());
    let mut score = 0;

    for k in 1..=n {
        let mut k_score = 0;
        for i in 0..=left.len_chars() - k {
            let kgram = left.char_slice(i..i + k);
            if right.contains(kgram) {
                k_score += 1;
            } else {
                k_score -= 1;
                if i == 0 || i == left.len_chars() - k {
                    k_score -= 1;
                }
            }
        }
        score += k_score;
    }

    score
}

fn count_eq_at_same_pos(left: CharsStr, right: CharsStr) -> (usize, bool) {
    let n = left.len_chars().min(right.len_chars());
    let count = left
        .char_iter()
        .zip(right.char_iter())
        .filter(|(l, r)| l == r)
        .count();

    let mut is_swap = false;
    // Only two characters are not equal. Check if they were swapped.
    if left.len_chars() == right.len_chars() && n - count == 2 {
        let mut first_mismatch = None;
        for (l, r) in left.char_iter().zip(right.char_iter()) {
            if l != r {
                if let Some((l1, r1)) = first_mismatch {
                    is_swap = l1 == r && r1 == l;
                    break;
                }
                first_mismatch = Some((l, r));
            }
        }
    }

    (count, is_swap)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn index_of_mismatch_test() {
        assert_eq!(index_of_mismatch(b"abcd".iter(), b"abcd".iter()), None);
        assert_eq!(index_of_mismatch(b"abcd".iter(), b"abxy".iter()), Some(2));
        assert_eq!(index_of_mismatch(b"abcd".iter(), b"abc".iter()), Some(3));
        assert_eq!(index_of_mismatch(b"abc".iter(), b"abcd".iter()), None);
    }

    #[test]
    fn nagrm_similarity_test() {
        // Rebuilding the Spellchecker:
        // > ngram(3, 'actually', 'akchualy')
        // > 11 = a, c, u, a, l, l, y, ua, al, ly, ual
        let mut left_buf = Vec::new();
        let left = CharsStr::new("actually", &mut left_buf);
        assert_eq!(ngram_similarity(3, left, "akchualy"), 11);
    }

    #[test]
    fn longest_common_subsequence_length_test() {
        let mut left_buffer = Vec::new();
        let mut right_buffer = Vec::new();
        let mut state_buffer = Vec::new();
        assert_eq!(
            longest_common_subsequence_length(
                CharsStr::new("aaa", &mut left_buffer),
                CharsStr::new("aaa", &mut right_buffer),
                &mut state_buffer
            ),
            3
        );
        assert_eq!(
            longest_common_subsequence_length(
                CharsStr::new("aaaaa", &mut left_buffer),
                CharsStr::new("bbbaa", &mut right_buffer),
                &mut state_buffer
            ),
            2
        );
    }

    #[test]
    fn count_eq_at_same_pos_test() {
        let mut left_buffer = Vec::new();
        let mut right_buffer = Vec::new();
        assert_eq!(
            count_eq_at_same_pos(
                CharsStr::new("abcd", &mut left_buffer),
                CharsStr::new("abcd", &mut right_buffer),
            ),
            (4, false)
        );
        assert_eq!(
            count_eq_at_same_pos(
                CharsStr::new("abcd", &mut left_buffer),
                CharsStr::new("acbd", &mut right_buffer),
            ),
            (2, true)
        );
    }
}
