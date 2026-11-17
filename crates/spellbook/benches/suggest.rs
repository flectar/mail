#![feature(test)]

extern crate test;

use once_cell::sync::Lazy;
use spellbook::Dictionary;
use test::{black_box, Bencher};

const EN_US_AFF: &str = include_str!("../vendor/en_US/en_US.aff");
const EN_US_DIC: &str = include_str!("../vendor/en_US/en_US.dic");

// The French dictionary has a much larger affix table than en_US (thousands of suffix rules
// versus dozens). Suggestion is dominated by checking generated candidate words against that
// table, so French exercises the affix-stripping hot paths that en_US barely touches. See
// <https://github.com/helix-editor/spellbook/issues/15>.
const FR_FR_AFF: &str = include_str!("../vendor/fr_FR/fr.aff");
const FR_FR_DIC: &str = include_str!("../vendor/fr_FR/fr.dic");

type RandomState = foldhash::fast::FixedState;
/// A random seed from a sample run. The values aren't important here: just that they're constant.
/// We don't want the benchmark outputs to reflect random changes to the seed.
const HASHER: RandomState = RandomState::with_seed(16553733157538299820);

static EN_US: Lazy<Dictionary<RandomState>> =
    Lazy::new(|| Dictionary::new_with_hasher(EN_US_AFF, EN_US_DIC, HASHER).unwrap());

static FR_FR: Lazy<Dictionary<RandomState>> =
    Lazy::new(|| Dictionary::new_with_hasher(FR_FR_AFF, FR_FR_DIC, HASHER).unwrap());

/// Runs `suggest` repeatedly, reusing the output `Vec` across iterations as a real caller would.
fn bench_suggest(b: &mut Bencher, dict: &Dictionary<RandomState>, word: &str) {
    let mut out = Vec::new();
    b.iter(|| {
        dict.suggest(black_box(word), &mut out);
        black_box(out.len())
    })
}

// en_US: with so few affixes, suggestion time here is dominated by ngram similarity and the
// cheaper edit-based methods rather than affix stripping.

#[bench]
fn en_us_uppercase_suggest(b: &mut Bencher) {
    // "ansi" -> "ANSI": a quick uppercase suggestion.
    bench_suggest(b, &EN_US, "ansi")
}

#[bench]
fn en_us_edit_suggest(b: &mut Bencher) {
    // A transposition that edit-based methods can correct ("example").
    bench_suggest(b, &EN_US, "exmaple")
}

#[bench]
fn en_us_ngram_suggest(b: &mut Bencher) {
    // A misspelling with no cheap edit hit, forcing the ngram pass over the word list.
    bench_suggest(b, &EN_US, "impecable")
}

// fr_FR: the affix-heavy path from issue #15.

#[bench]
fn fr_fr_correct_word_suggest(b: &mut Bencher) {
    bench_suggest(b, &FR_FR, "test")
}

#[bench]
fn fr_fr_impecable_suggest(b: &mut Bencher) {
    bench_suggest(b, &FR_FR, "impécable")
}

#[bench]
fn fr_fr_receptioniste_suggest(b: &mut Bencher) {
    bench_suggest(b, &FR_FR, "réceptioniste")
}

#[bench]
fn fr_fr_incorrect_word_suggest(b: &mut Bencher) {
    // No good suggestion exists, so every suggestion strategy runs to completion.
    bench_suggest(b, &FR_FR, "foobarbaz")
}
