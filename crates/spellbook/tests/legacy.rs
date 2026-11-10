/*
These are "legacy" tests (borrowing Nuspell's terminology) which are ported originally from
Hunspell's codebase. Each case has a `<case>.dic` and `<case>.aff` file which set up an example
dictionary and each case might have any of...

* `<case>.good`: a listing of words for which `Dictionary::check` should return `true`.
* `<case>.wrong`: a listing of words for which `Dictionary::check` should return `false`.
* `<case>.sug`: a listing of words which should be suggested for the given wrong word on the
  corresponding line in `<case>.wrong`, separated by commas and whitespace.

We use a simple declarative macro to create a `#[test]` for each case. The advantage of a
`#[test]` for each case is that a single test can fail but the others will run. We could use a
single test which globs for `.dic` files instead: that would make it very easy to add a case. But
these cases are meant just to ensure we have parity with the Hunspell/Nuspell test beds, so cases
should not be added often. The test runs faster without the glob (and requires no glob
dependency).
*/
use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
};

use spellbook::Dictionary;

macro_rules! check {
    ($case:ident) => {
        #[allow(non_snake_case)]
        #[test]
        fn $case() {
            let case = stringify!($case).strip_prefix("check_").unwrap();
            do_check_case(case);
        }
    };
}

fn do_check_case(case: &str) {
    let manifest_path = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let path = manifest_path.join("tests/legacy").join(case);
    let aff = read_to_string(path.with_extension("aff")).unwrap();
    let dic = read_to_string(path.with_extension("dic")).unwrap();
    let dict = Dictionary::new(&aff, &dic).unwrap();

    for good_word in fs::read_to_string(path.with_extension("good"))
        .iter()
        .flat_map(|text| text.lines())
    {
        let word = good_word.trim();
        assert!(
            dict.check(word),
            "expected {word:?} to be correct but it was incorrect"
        );
    }

    for wrong_word in fs::read_to_string(path.with_extension("wrong"))
        .iter()
        .flat_map(|text| text.lines())
    {
        let word = wrong_word.trim();
        assert!(
            !dict.check(word),
            "expected {word:?} to be incorrect but it was correct"
        );
    }
}

check!(check_1463589);
check!(check_1463589_utf);
check!(check_1592880);
check!(check_1695964);
check!(check_1706659);
check!(check_1975530);
check!(check_2970240);
check!(check_2970242);
check!(check_2999225);
check!(check_affixes);
check!(check_alias2);
check!(check_alias3);
check!(check_alias);
check!(check_allcaps2);
check!(check_allcaps3);
check!(check_allcaps);
check!(check_allcaps_utf);
check!(check_arabic);
check!(check_base);
check!(check_base_utf);
check!(check_breakdefault);
check!(check_break);
check!(check_breakoff);
check!(check_checkcompoundcase2);
check!(check_checkcompoundcase);
check!(check_checkcompoundcaseutf);
check!(check_checkcompounddup);
// Use CHECKCOMPOUNDPATTERN replacements which aren't implemented yet:
// check!(check_checkcompoundpattern2);
// check!(check_checkcompoundpattern3);
// check!(check_checkcompoundpattern4);
check!(check_checkcompoundpattern);
check!(check_checkcompoundrep);
check!(check_checkcompoundtriple);
check!(check_checksharps);
check!(check_checksharpsutf);
check!(check_circumfix);
check!(check_colons_in_words);
check!(check_complexprefixes2);
check!(check_complexprefixes);
check!(check_complexprefixesutf);
check!(check_compoundaffix2);
check!(check_compoundaffix3);
check!(check_compoundaffix);
check!(check_compoundflag);
check!(check_compoundrule2);
check!(check_compoundrule3);
check!(check_compoundrule4);
check!(check_compoundrule5);
check!(check_compoundrule6);
check!(check_compoundrule7);
check!(check_compoundrule8);
check!(check_compoundrule);
check!(check_conditionalprefix);
// Fails due to weird encoding of the aff/dic:
// check!(check_condition);
check!(check_condition_utf);
check!(check_digits_in_words);
check!(check_dotless_i);
// Fails due to weird encoding of the aff/dic:
// check!(check_encoding);
check!(check_flag);
check!(check_flaglong);
check!(check_flagnum);
check!(check_flagutf8);
check!(check_fogemorpheme);
check!(check_forbiddenword);
check!(check_forceucase);
check!(check_fullstrip);
check!(check_germancompounding);
check!(check_germancompoundingold);
check!(check_hu);
check!(check_i35725);
check!(check_i53643);
check!(check_i54633);
// Fails due to weird encoding of the aff/dic:
// check!(check_i54980);
check!(check_i58202);
check!(check_i68568);
check!(check_i68568utf);
check!(check_iconv2);
check!(check_iconv);
check!(check_ignore);
check!(check_ignoreutf);
check!(check_IJ);
check!(check_keepcase);
check!(check_korean);
check!(check_map);
check!(check_maputf);
// Presumably needs morphology support?
// check!(check_morph);
check!(check_needaffix2);
check!(check_needaffix3);
check!(check_needaffix4);
check!(check_needaffix5);
check!(check_needaffix);
check!(check_nepali);
check!(check_ngram_utf_fix);
check!(check_nosuggest);
check!(check_oconv);
check!(check_onlyincompound2);
check!(check_onlyincompound);
check!(check_opentaal_cpdpat2);
check!(check_opentaal_cpdpat);
check!(check_opentaal_forbiddenword1);
check!(check_opentaal_forbiddenword2);
check!(check_opentaal_keepcase);
check!(check_phone);
check!(check_rep);
check!(check_reputf);
check!(check_simplifiedtriple);
check!(check_slash);
check!(check_sug);
check!(check_sugutf);
check!(check_utf8_bom2);
check!(check_utf8_bom);
check!(check_utf8);
check!(check_utf8_nonbmp);
check!(check_utfcompound);
check!(check_warn);
check!(check_zeroaffix);

macro_rules! suggest {
    ($case:ident) => {
        #[allow(non_snake_case)]
        #[test]
        fn $case() {
            let case = stringify!($case).strip_prefix("suggest_").unwrap();
            do_suggest_case(case);
        }
    };
}

fn do_suggest_case(case: &str) {
    let manifest_path = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let path = manifest_path.join("tests/legacy").join(case);
    let aff = read_to_string(path.with_extension("aff")).unwrap();
    let dic = read_to_string(path.with_extension("dic")).unwrap();
    let dict = Dictionary::new(&aff, &dic).unwrap();

    let mut list_sugs = Vec::new();
    let mut sugs = Vec::new();
    for word in fs::read_to_string(path.with_extension("wrong"))
        .iter()
        .flat_map(|text| text.lines())
        .filter(|line| !line.is_empty())
    {
        assert!(
            !dict.check(word),
            "word {word:?} was in the .wrong file but was actually correct"
        );

        dict.suggest(word, &mut sugs);

        if !sugs.is_empty() {
            list_sugs.push(std::mem::take(&mut sugs));
        }
    }

    let mut expected_list_sugs = Vec::new();
    for line in fs::read_to_string(path.with_extension("sug"))
        .iter()
        .flat_map(|text| text.lines())
        .filter(|line| !line.is_empty())
    {
        let sugs: Vec<_> = line.split(", ").map(ToOwned::to_owned).collect();
        if !sugs.is_empty() {
            expected_list_sugs.push(sugs);
        }
    }

    assert_eq!(
        expected_list_sugs, list_sugs,
        "(left: expected, right: actual)"
    );
}

suggest!(suggest_1463589);
suggest!(suggest_1463589_utf);
suggest!(suggest_1695964);
suggest!(suggest_IJ);
suggest!(suggest_allcaps);
suggest!(suggest_allcaps_utf);
suggest!(suggest_allcaps2);
suggest!(suggest_base);
suggest!(suggest_base_utf);
suggest!(suggest_breakdefault);
suggest!(suggest_forceucase);
suggest!(suggest_i35725);
suggest!(suggest_i54633);
suggest!(suggest_i58202);
suggest!(suggest_keepcase);
suggest!(suggest_map);
suggest!(suggest_maputf);
suggest!(suggest_ngram_utf_fix);
suggest!(suggest_oconv);
suggest!(suggest_onlyincompound);
suggest!(suggest_opentaal_forbiddenword1);
suggest!(suggest_opentaal_forbiddenword2);
suggest!(suggest_opentaal_keepcase);
suggest!(suggest_rep);
suggest!(suggest_reputf);
suggest!(suggest_sug);
suggest!(suggest_sugutf);

// These are marked as failing in Nuspell:
// suggest!(suggest_checksharps);
// suggest!(suggest_checksharpsutf);
// suggest!(suggest_nosuggest);
// suggest!(suggest_phone);
// suggest!(suggest_utf8_nonbmp);

/// Reads the contents of a file into a String, handling detecting and decoding of non-UTF-8
/// contents.
fn read_to_string<P: AsRef<Path>>(path: P) -> io::Result<String> {
    // Adapted from Helix's document opening function:
    // <https://github.com/helix-editor/helix/blob/620dfceb849d6b68d41d4f7678bb4675009fef4d/helix-view/src/document.rs#L395>

    const BUF_SIZE: usize = 8 * 1024;
    let mut buf = [0u8; BUF_SIZE];
    let mut reader = File::open(path).unwrap();
    let read = reader.read(&mut buf).unwrap();
    assert_ne!(read, 0);

    // Guess encoding.
    let mut detector = chardetng::EncodingDetector::new();
    detector.feed(&buf, read < BUF_SIZE);
    let encoding = detector.guess(None, true);

    let mut decoder = encoding.new_decoder();
    let mut output = String::new();
    let mut slice = &buf[..read];
    let mut is_empty = read == 0;
    let mut total_written = 0usize;
    // Zero-initialized bytes are a valid str.
    let mut buf_out = [0u8; BUF_SIZE];
    let buf_str = unsafe { std::str::from_utf8_unchecked_mut(&mut buf_out) };
    loop {
        let mut total_read = 0usize;

        // An inner loop is necessary as it is possible that the input buffer
        // may not be completely decoded on the first `decode_to_str()` call
        // which would happen in cases where the output buffer is filled to
        // capacity.
        loop {
            let (result, read, written, ..) = decoder.decode_to_str(
                &slice[total_read..],
                &mut buf_str[total_written..],
                is_empty,
            );

            // These variables act as the read and write cursors of `buf` and `buf_str` respectively.
            // They are necessary in case the output buffer fills before decoding of the entire input
            // loop is complete. Otherwise, the loop would endlessly iterate over the same `buf` and
            // the data inside the output buffer would be overwritten.
            total_read += read;
            total_written += written;
            match result {
                encoding_rs::CoderResult::InputEmpty => {
                    assert_eq!(slice.len(), total_read);
                    break;
                }
                encoding_rs::CoderResult::OutputFull => {
                    assert!(slice.len() > total_read);
                    output.push_str(&buf_str[..total_written]);
                    total_written = 0;
                }
            }
        }
        // Once the end of the stream is reached, the output buffer is
        // flushed and the loop terminates.
        if is_empty {
            assert_eq!(reader.read(&mut buf)?, 0);
            output.push_str(&buf_str[..total_written]);
            break;
        }

        // Once the previous input has been processed and decoded, the next set of
        // data is fetched from the reader. The end of the reader is determined to
        // be when exactly `0` bytes were read from the reader, as per the invariants
        // of the `Read` trait.
        let read = reader.read(&mut buf)?;
        slice = &buf[..read];
        is_empty = read == 0;
    }

    Ok(output)
}
