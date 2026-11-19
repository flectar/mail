//! Which words another version of a text changes (#241): the edits that turn
//! one into the other, word by word, so that the composer can put an AI
//! server's revision of a draft in place of the words it changes, and the
//! words it keeps keep their formatting.

use std::{collections::HashMap, ops::Range};
use unicode_segmentation::UnicodeSegmentation;

/// One change: the text in the old version that the new one replaces, and
/// what replaces it, as byte ranges into each.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WordEdit {
    pub(crate) old: Range<usize>,
    pub(crate) new: Range<usize>,
}

/// Past this many pairs of words to compare, the text from the first change
/// to the last is replaced as a whole: comparing them would take too long.
/// Each pair costs two bits, so this is 4 MB at most.
const MOST_PAIRS: usize = 16_000_000;

const MATCH: u8 = 0;
const OLD_ONLY: u8 = 1;
const NEW_ONLY: u8 = 2;

/// The words of `text`, its runs of white space and its punctuation, by the
/// Unicode word boundaries: a word in Chinese or Japanese is a character.
fn tokens(text: &str) -> Vec<Range<usize>> {
    text.split_word_bound_indices()
        .map(|(at, token)| at..at + token.len())
        .collect()
}

/// The words of `text` at `ranges` as numbers, the same for the same word.
fn numbered<'a>(
    numbers: &mut HashMap<&'a str, u32>,
    text: &'a str,
    ranges: &[Range<usize>],
) -> Vec<u32> {
    ranges
        .iter()
        .map(|range| {
            let next = u32::try_from(numbers.len()).unwrap_or(u32::MAX);
            *numbers.entry(&text[range.clone()]).or_insert(next)
        })
        .collect()
}

/// The edits that turn `old` into `new`, in order, none touching another:
/// the fewest words each, as the longest run of words they share in order
/// shows.
pub(crate) fn word_edits(old: &str, new: &str) -> Vec<WordEdit> {
    let a = tokens(old);
    let b = tokens(new);
    let same = |i: usize, j: usize| old[a[i].clone()] == new[b[j].clone()];

    // What both start and end with needs no comparing.
    let mut prefix = 0;
    while prefix < a.len() && prefix < b.len() && same(prefix, prefix) {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < a.len() - prefix
        && suffix < b.len() - prefix
        && same(a.len() - 1 - suffix, b.len() - 1 - suffix)
    {
        suffix += 1;
    }
    let a_mid = prefix..a.len() - suffix;
    let b_mid = prefix..b.len() - suffix;
    let old_at = |token: usize| a.get(token).map_or(old.len(), |range| range.start);
    let new_at = |token: usize| b.get(token).map_or(new.len(), |range| range.start);
    if a_mid.is_empty() && b_mid.is_empty() {
        return Vec::new();
    }
    let whole = || {
        vec![WordEdit {
            old: old_at(a_mid.start)..old_at(a_mid.end),
            new: new_at(b_mid.start)..new_at(b_mid.end),
        }]
    };
    let (n, m) = (a_mid.len(), b_mid.len());
    if n == 0 || m == 0 || n.saturating_mul(m) > MOST_PAIRS {
        return whole();
    }

    // Each distinct word becomes a number, so comparing two is cheap.
    let mut numbers = HashMap::new();
    let old_words = numbered(&mut numbers, old, &a[a_mid.clone()]);
    let new_words = numbered(&mut numbers, new, &b[b_mid.clone()]);

    // The longest common subsequence, a row of lengths at a time, with the
    // way each cell was reached kept in two bits for walking back.
    let mut steps = vec![0u8; (n * m).div_ceil(4)];
    let set = |steps: &mut [u8], cell: usize, step: u8| {
        steps[cell / 4] |= step << ((cell % 4) * 2);
    };
    let get = |steps: &[u8], cell: usize| (steps[cell / 4] >> ((cell % 4) * 2)) & 0b11;
    let mut above = vec![0u32; m + 1];
    let mut row = vec![0u32; m + 1];
    for (i, old_word) in old_words.iter().enumerate() {
        row[0] = 0;
        for (j, new_word) in new_words.iter().enumerate() {
            let cell = i * m + j;
            if old_word == new_word {
                row[j + 1] = above[j] + 1;
                set(&mut steps, cell, MATCH);
            } else if above[j + 1] >= row[j] {
                row[j + 1] = above[j + 1];
                set(&mut steps, cell, OLD_ONLY);
            } else {
                row[j + 1] = row[j];
                set(&mut steps, cell, NEW_ONLY);
            }
        }
        std::mem::swap(&mut above, &mut row);
    }

    // Walking back from the end gives the words both keep, last first.
    let mut kept = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        match get(&steps, (i - 1) * m + (j - 1)) {
            MATCH => {
                kept.push((i - 1, j - 1));
                i -= 1;
                j -= 1;
            }
            OLD_ONLY => i -= 1,
            _ => j -= 1,
        }
    }
    kept.reverse();

    // The gaps between the words both keep are the edits.
    let mut edits = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (ki, kj) in kept.into_iter().chain(std::iter::once((n, m))) {
        if ki > i || kj > j {
            edits.push(WordEdit {
                old: old_at(prefix + i)..old_at(prefix + ki),
                new: new_at(prefix + j)..new_at(prefix + kj),
            });
        }
        i = ki + 1;
        j = kj + 1;
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(old: &str, new: &str, edits: &[WordEdit]) -> String {
        let mut text = old.to_owned();
        for edit in edits.iter().rev() {
            text.replace_range(edit.old.clone(), &new[edit.new.clone()]);
        }
        text
    }

    fn changes(old: &str, new: &str) -> Vec<(String, String)> {
        let edits = word_edits(old, new);
        assert_eq!(apply(old, new, &edits), new);
        assert!(
            edits
                .windows(2)
                .all(|pair| pair[0].old.end < pair[1].old.start)
        );
        edits
            .iter()
            .map(|edit| {
                (
                    old[edit.old.clone()].to_owned(),
                    new[edit.new.clone()].to_owned(),
                )
            })
            .collect()
    }

    #[test]
    fn only_the_words_that_differ_change() {
        assert_eq!(
            changes(
                "Hi Anna, teh report is attachd. Thanks!",
                "Hi Anna, the report is attached. Thanks!"
            ),
            [
                ("teh".to_owned(), "the".to_owned()),
                ("attachd".to_owned(), "attached".to_owned())
            ]
        );
        assert_eq!(
            changes("see you tomorow", "See you tomorrow."),
            [
                ("see".to_owned(), "See".to_owned()),
                ("tomorow".to_owned(), "tomorrow.".to_owned())
            ]
        );
    }

    #[test]
    fn words_go_in_and_out() {
        assert_eq!(
            changes("I will send it Monday.", "I will send it on Monday."),
            [("".to_owned(), "on ".to_owned())]
        );
        assert_eq!(
            changes("It is really very good.", "It is good."),
            [("really very ".to_owned(), "".to_owned())]
        );
        assert!(changes("Same text.", "Same text.").is_empty());
        assert_eq!(changes("", "New"), [("".to_owned(), "New".to_owned())]);
        assert_eq!(changes("Old", ""), [("Old".to_owned(), "".to_owned())]);
    }

    #[test]
    fn lines_and_other_scripts_keep_their_places() {
        changes(
            "• first item\n• secnd item\n\nBest,\nAnna",
            "• First item\n• Second item\n\nBest,\nAnna",
        );
        // Chinese splits into characters, so one wrong character is the
        // change.
        assert_eq!(
            changes("我们明天见面吧", "我们明天見面吧"),
            [("见".to_owned(), "見".to_owned())]
        );
        changes(
            "Hi \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} and \u{FFFC} here",
            "Hello \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467} and \u{FFFC} there",
        );
    }

    #[test]
    fn a_whole_rewrite_still_comes_out_right() {
        changes(
            "Could you maybe send me the numbers for last quarter when you get a chance?",
            "Please send the Q3 numbers by Friday.",
        );
    }
}
