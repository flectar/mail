//! The parser for `.aff` and `.dic` files.
//!
//! This parser takes inspiration from Zspell: <https://github.com/pluots/zspell/blob/71ae77932f2cc67f593d846a26a9ffd7cf6d0412/crates/zspell/src/parser_affix.rs>
//! and Nuspell. The Rust bits are mostly Zspell: folding over a const slice of parser functions.
//! Rather than trying to parse directly into the `Aff_Data` type, Nuspell has a sort of
//! scratch-pad data structure for the initial parsing. This works well for types like
//! `AffixIndex` which are most efficient to build all-at-once from all elements rather than
//! constructively by inserting each element.

use core::{
    fmt,
    hash::BuildHasher,
    iter::{Enumerate, Peekable},
    num::NonZeroU16,
    str::{FromStr, SplitWhitespace},
};

use hashbrown::HashMap;

use crate::{
    aff::CompoundRuleModifier,
    alloc::{
        string::{String, ToString},
        vec::Vec,
    },
    erase_chars, Casing, Flag, FlagSet, Stem, WordList,
};

use super::{
    AffData, AffOptions, BreakTable, CaseHandling, CompoundPattern, CompoundRule, Condition,
    FlagType, Prefix, SimilarityGroup, Suffix, HIDDEN_HOMONYM_FLAG,
};

// Cap the allocation size a dictionary can suggest to ~1 million.
const MAX_HINT: usize = 1 << 20;

type Result<T> = core::result::Result<T, ParseDictionaryError>;
type ParseResult = Result<()>;

#[derive(Debug)]
struct AffLineParser<'aff> {
    options: AffOptions,
    // options only used for parsing:
    flag_type: FlagType,
    // encoding: Encoding,
    flag_aliases: Vec<FlagSet>,
    // wordchars: String, deprecated?
    replacements: Vec<(&'aff str, String)>,
    similarities: Vec<SimilarityGroup>,
    input_conversions: Vec<(&'aff str, &'aff str)>,
    output_conversions: Vec<(&'aff str, &'aff str)>,
    break_patterns: Vec<&'aff str>,
    compound_syllable_vowels: &'aff str,
    ignore_chars: Vec<char>,
    try_chars: &'aff str,
    keyboard_closeness: &'aff str,
    prefixes: Vec<Prefix>,
    suffixes: Vec<Suffix>,
    compound_rules: Vec<CompoundRule>,
    compound_patterns: Vec<CompoundPattern>,
}

impl Default for AffLineParser<'_> {
    fn default() -> Self {
        use crate::alloc::vec;

        Self {
            options: Default::default(),
            flag_type: Default::default(),
            flag_aliases: Default::default(),
            replacements: Default::default(),
            similarities: Default::default(),
            input_conversions: Default::default(),
            output_conversions: Default::default(),
            break_patterns: vec!["^-", "-", "-$"],
            compound_syllable_vowels: Default::default(),
            ignore_chars: Default::default(),
            try_chars: Default::default(),
            keyboard_closeness: Default::default(),
            prefixes: Default::default(),
            suffixes: Default::default(),
            compound_rules: Default::default(),
            compound_patterns: Default::default(),
        }
    }
}

type Parser = for<'aff> fn(&mut AffLineParser<'aff>, &mut Lines<'aff>) -> ParseResult;

// These parsers are only used for the `.aff` file's contents. The `.dic` file is handled ad-hoc.
const AFF_PARSERS: &[(&str, Parser)] = &[
    ("FLAG", parse_flag_type),
    // Flags
    ("FORBIDDENWORD", parse_forbidden_word_flag),
    ("CIRCUMFIX", parse_circumfix_flag),
    ("KEEPCASE", parse_keep_case_flag),
    ("NEEDAFFIX", parse_need_affix_flag),
    ("NOSUGGEST", parse_no_suggest_flag),
    ("SUBSTANDARD", parse_substandard_flag),
    ("WARN", parse_warn_flag),
    ("COMPOUNDFLAG", parse_compound_flag),
    ("COMPOUNDBEGIN", parse_compound_begin_flag),
    ("COMPOUNDMIDDLE", parse_compound_middle_flag),
    ("COMPOUNDEND", parse_compound_end_flag),
    ("ONLYINCOMPOUND", parse_only_in_compound_flag),
    ("COMPOUNDPERMITFLAG", parse_compound_permit_flag),
    ("COMPOUNDFORBIDFLAG", parse_compound_forbid_flag),
    ("COMPOUNDROOT", parse_compound_root_flag),
    ("FORCEUCASE", parse_compound_force_uppercase_flag),
    // Bools
    ("COMPLEXPREFIXES", parse_complex_prefixes),
    ("FULLSTRIP", parse_fullstrip),
    ("CHECKSHARPS", parse_checksharps),
    ("FORBIDWARN", parse_forbid_warn),
    ("COMPOUNDMORESUFFIXES", parse_compound_more_suffixes),
    ("CHECKCOMPOUNDDUP", parse_compound_check_duplicate),
    ("CHECKCOMPOUNDREP", parse_compound_check_rep),
    ("CHECKCOMPOUNDCASE", parse_compound_check_case),
    ("CHECKCOMPOUNDTRIPLE", parse_compound_check_triple),
    ("SIMPLIFIEDTRIPLE", parse_compound_simplified_triple),
    ("SYLLABLENUM", parse_compound_syllable_num),
    ("ONLYMAXDIFF", parse_only_max_diff),
    ("NOSPLITSUGS", parse_no_split_suggestions),
    ("SUGSWITHDOTS", parse_suggest_with_dots),
    // "Shorts" as Nuspell calls them (u16s in Spellbook)
    ("COMPOUNDMIN", parse_compound_min_length),
    ("COMPOUNDWORDMAX", parse_compound_max_word_count),
    ("MAXCPDSUGS", parse_max_compound_suggestions),
    ("MAXNGRAMSUGS", parse_max_ngram_suggestions),
    ("MAXDIFF", parse_max_diff_factor),
    // Strings
    ("IGNORE", parse_ignore_chars),
    ("KEY", parse_keyboard_closeness),
    ("TRY", parse_try_chars),
    ("LANG", parse_language),
    // String pairs
    // TODO: phonetic replacements
    ("REP", parse_replacements),
    ("ICONV", parse_input_conversions),
    ("OCONV", parse_output_conversions),
    // Remaining complicated structures
    ("BREAK", parse_break_patterns),
    ("COMPOUNDSYLLABLE", parse_compound_syllable),
    ("AF", parse_flag_aliases),
    ("PFX", parse_prefix_table),
    ("SFX", parse_suffix_table),
    ("COMPOUNDRULE", parse_compound_rule_table),
    ("CHECKCOMPOUNDPATTERN", parse_compound_pattern_table),
    ("MAP", parse_map),
];

// TODO: encoding? Or just require all dictionaries to be UTF-8?
pub(crate) fn parse<'aff, 'dic, S: BuildHasher + Clone>(
    aff_text: &'aff str,
    dic_text: &'dic str,
    build_hasher: S,
) -> Result<(WordList<S>, AffData)> {
    // First parse the aff file.
    let mut lines = Lines::<'aff>::new(aff_text, ParseDictionaryErrorSource::Aff);
    let mut aff_parsers =
        HashMap::with_capacity_and_hasher(AFF_PARSERS.len(), build_hasher.clone());
    aff_parsers.extend(AFF_PARSERS);
    let mut cx = AffLineParser::<'aff>::default();

    while !lines.is_finished() {
        if let Some(parser) = lines.next_word().and_then(|key| aff_parsers.get(key)) {
            (parser)(&mut cx, &mut lines)?;
        }

        lines.advance_line()
    }

    // Then parse the dic file into a WordList.
    let mut lines = Lines::<'dic>::new(dic_text, ParseDictionaryErrorSource::Dic);
    let row_count = lines
        .take_exactly_one_word()?
        .parse::<usize>()
        .map_err(|err| lines.error(ParseDictionaryErrorKind::MalformedNumber(err)))?;
    let mut words = WordList::with_capacity_and_hasher(row_count.min(MAX_HINT), build_hasher);

    while !lines.is_finished() {
        lines.advance_line();

        let Some(line) = lines.line().map(|l| l.trim()).filter(|l| !l.is_empty()) else {
            // Empty lines are skipped.
            continue;
        };

        if line.starts_with('/') && line.len() > 1 {
            // Some dictionaries like it_IT use a leading slash as a comment.
            // The empty word isn't a valid entry in the word list so it doesn't matter which
            // flags come after it.
            continue;
        }

        if line.len() > u16::MAX as usize {
            return Err(lines.error(ParseDictionaryErrorKind::WordTooLong));
        }

        let (word, flagset) =
            parse_dic_line(line, cx.flag_type, &cx.flag_aliases, &cx.ignore_chars)
                .map_err(|err| lines.error(ParseDictionaryErrorKind::MalformedFlag(err)))?;
        // Normalize out Pascal and Camel cases (and uppercase when there are no flags) by
        // converting them to titlecase and setting the hidden homonym flag.
        let casing = crate::classify_casing(word.as_ref());
        if (matches!(casing, Casing::Pascal | Casing::Camel)
            && !flagset.contains(&cx.options.forbidden_word_flag))
            || (matches!(casing, Casing::All) && !flagset.is_empty())
        {
            let word = cx.options.case_handling.titlecase(word.as_ref()).into();
            words.insert(word, flagset.with_flag(HIDDEN_HOMONYM_FLAG));
        }
        words.insert(word, flagset);
    }

    if !cx.ignore_chars.is_empty() {
        for prefix in cx.prefixes.iter_mut() {
            let add = erase_chars(&prefix.add, &cx.ignore_chars);
            prefix.add = add.into();
        }
        for suffix in cx.suffixes.iter_mut() {
            let add = erase_chars(&suffix.add, &cx.ignore_chars);
            suffix.add = add.into();
        }
    }

    // Collect everything into AffData.
    let aff_data = AffData {
        prefixes: cx.prefixes.into(),
        suffixes: cx.suffixes.into(),
        break_table: BreakTable::new(&cx.break_patterns),
        replacements: cx.replacements.into(),
        similarities: cx.similarities.into(),
        input_conversions: cx.input_conversions.into(),
        output_conversions: cx.output_conversions.into(),
        compound_rules: cx.compound_rules.into(),
        compound_syllable_vowels: cx.compound_syllable_vowels.into(),
        compound_patterns: cx.compound_patterns.into(),
        ignore_chars: cx.ignore_chars.into(),
        keyboard_closeness: cx.keyboard_closeness.into(),
        try_chars: cx.try_chars.into(),
        options: cx.options,
        flag_type: cx.flag_type,
        flag_aliases: cx.flag_aliases.into(),
    };
    Ok((words, aff_data))
}

fn parse_flag_type(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    let word = lines.take_exactly_one_word()?;

    word.parse::<FlagType>()
        .map(|flag_type| cx.flag_type = flag_type)
        .map_err(|err| lines.error(ParseDictionaryErrorKind::UnknownFlagType(err)))
}

fn parse_forbidden_word_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.forbidden_word_flag = flag)
}

fn parse_circumfix_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.circumfix_flag = Some(flag))
}

fn parse_keep_case_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.keep_case_flag = Some(flag))
}

fn parse_need_affix_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.need_affix_flag = Some(flag))
}

fn parse_no_suggest_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.no_suggest_flag = Some(flag))
}

fn parse_substandard_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.substandard_flag = Some(flag))
}

fn parse_warn_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.warn_flag = Some(flag))
}

fn parse_compound_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.compound_flag = Some(flag))
}

fn parse_compound_begin_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.compound_begin_flag = Some(flag))
}

fn parse_compound_middle_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.compound_middle_flag = Some(flag))
}

fn parse_compound_end_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.compound_end_flag = Some(flag))
}

fn parse_only_in_compound_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.only_in_compound_flag = Some(flag))
}

fn parse_compound_permit_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.compound_permit_flag = Some(flag))
}

fn parse_compound_forbid_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.compound_forbid_flag = Some(flag))
}

fn parse_compound_root_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.compound_root_flag = Some(flag))
}

fn parse_compound_force_uppercase_flag(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_flag(cx)
        .map(|flag| cx.options.compound_force_uppercase_flag = Some(flag))
}

fn parse_complex_prefixes(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines.parse_bool().map(|b| cx.options.complex_prefixes = b)
}

fn parse_fullstrip(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines.parse_bool().map(|b| cx.options.fullstrip = b)
}

fn parse_checksharps(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines.parse_bool().map(|b| cx.options.checksharps = b)
}

fn parse_forbid_warn(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines.parse_bool().map(|b| cx.options.forbid_warn = b)
}

fn parse_compound_more_suffixes(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_bool()
        .map(|b| cx.options.compound_more_suffixes = b)
}

fn parse_compound_check_duplicate(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_bool()
        .map(|b| cx.options.compound_check_duplicate = b)
}

fn parse_compound_check_rep(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_bool()
        .map(|b| cx.options.compound_check_rep = b)
}

fn parse_compound_check_case(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_bool()
        .map(|b| cx.options.compound_check_case = b)
}

fn parse_compound_check_triple(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_bool()
        .map(|b| cx.options.compound_check_triple = b)
}

fn parse_compound_simplified_triple(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_bool()
        .map(|b| cx.options.compound_simplified_triple = b)
}

fn parse_compound_syllable_num(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_bool()
        .map(|b| cx.options.compound_syllable_num = b)
}

fn parse_only_max_diff(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines.parse_bool().map(|b| cx.options.only_max_diff = b)
}

fn parse_no_split_suggestions(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_bool()
        .map(|b| cx.options.no_split_suggestions = b)
}

fn parse_suggest_with_dots(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines.parse_bool().map(|b| cx.options.suggest_with_dots = b)
}

fn parse_compound_min_length(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_short()
        .map(|s| cx.options.compound_min_length = NonZeroU16::new(s.max(1)))
}

fn parse_compound_max_word_count(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_short()
        .map(|s| cx.options.compound_max_word_count = NonZeroU16::new(s))
}

fn parse_max_compound_suggestions(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_short()
        .map(|s| cx.options.max_compound_suggestions = s)
}

fn parse_max_ngram_suggestions(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines
        .parse_short()
        .map(|s| cx.options.max_ngram_suggestions = s)
}

fn parse_max_diff_factor(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    let s = lines.parse_short()?;
    cx.options.max_diff_factor = if s > 10 { 5 } else { s };
    Ok(())
}

fn parse_ignore_chars<'a>(cx: &mut AffLineParser<'a>, lines: &mut Lines<'a>) -> ParseResult {
    lines
        .take_exactly_one_word()
        .map(|word| cx.ignore_chars = word.chars().collect())
}

fn parse_keyboard_closeness<'a>(cx: &mut AffLineParser<'a>, lines: &mut Lines<'a>) -> ParseResult {
    lines
        .take_exactly_one_word()
        .map(|word| cx.keyboard_closeness = word)
}

fn parse_try_chars<'a>(cx: &mut AffLineParser<'a>, lines: &mut Lines<'a>) -> ParseResult {
    lines
        .take_exactly_one_word()
        .map(|word| cx.try_chars = word)
}

fn parse_language<'a>(cx: &mut AffLineParser<'a>, lines: &mut Lines<'a>) -> ParseResult {
    lines.take_exactly_one_word().map(|word| {
        cx.options.case_handling = if matches!(word, "tr" | "tr_TR" | "az" | "crh") {
            CaseHandling::Turkic
        } else {
            CaseHandling::default()
        }
    })
}

fn parse_replacements<'aff>(cx: &mut AffLineParser<'aff>, lines: &mut Lines<'aff>) -> ParseResult {
    lines.parse_table2("REP", |str1, str2| {
        cx.replacements.push((str1, str2.replace('_', " ")));
        Ok(())
    })
}

fn parse_input_conversions<'aff>(
    cx: &mut AffLineParser<'aff>,
    lines: &mut Lines<'aff>,
) -> ParseResult {
    lines.parse_table2("ICONV", |str1, str2| {
        cx.input_conversions.push((str1, str2));
        Ok(())
    })
}

fn parse_output_conversions<'aff>(
    cx: &mut AffLineParser<'aff>,
    lines: &mut Lines<'aff>,
) -> ParseResult {
    lines.parse_table2("OCONV", |str1, str2| {
        cx.output_conversions.push((str1, str2));
        Ok(())
    })
}

fn parse_break_patterns<'aff>(
    cx: &mut AffLineParser<'aff>,
    lines: &mut Lines<'aff>,
) -> ParseResult {
    // This field has a non-empty default. You can turn off the default break patterns by
    // setting `BREAK 0` in the aff file.
    cx.break_patterns.clear();
    lines.parse_table1("BREAK", |str| {
        cx.break_patterns.push(str);
        Ok(())
    })
}

fn parse_compound_syllable<'aff>(
    cx: &mut AffLineParser<'aff>,
    lines: &mut Lines<'aff>,
) -> ParseResult {
    // TODO: can this just use parse_table2?
    // Takes the shape COMPOUNDSYLLABLE <compound_syllable_max> <compound_syllable_vowels>
    let mut words = match lines.words.take() {
        Some(words) => words,
        None => {
            return Err(lines.error(ParseDictionaryErrorKind::MismatchedArity {
                expected: 2,
                actual: 0,
            }))
        }
    };

    let max = match words.next() {
        Some(word) => word,
        None => {
            return Err(lines.error(ParseDictionaryErrorKind::MismatchedArity {
                expected: 2,
                actual: 0,
            }))
        }
    };
    let vowels = match words.next() {
        Some(word) => word,
        None => {
            return Err(lines.error(ParseDictionaryErrorKind::MismatchedArity {
                expected: 2,
                actual: 1,
            }))
        }
    };

    cx.options.compound_syllable_max = max
        .parse::<u16>()
        .map(NonZeroU16::new)
        .map_err(|err| lines.error(ParseDictionaryErrorKind::MalformedNumber(err)))?;
    cx.compound_syllable_vowels = vowels;

    Ok(())
}

fn parse_flag_aliases(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines.parse_table1("AF", |alias| {
        let flagset = parse_flags_from_str(cx.flag_type, alias)?;
        cx.flag_aliases.push(flagset);
        Ok(())
    })
}

fn parse_prefix_table(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines.parse_affix_table(
        "PFX",
        cx.flag_type,
        |flag, crossproduct, strip, add, condition, flagset_str| {
            let flagset = decode_flagset(flagset_str, cx.flag_type, &cx.flag_aliases)?;
            let prefix = Prefix::new(flag, crossproduct, strip, add, condition, flagset)?;
            cx.prefixes.push(prefix);
            Ok(())
        },
    )
}

fn parse_suffix_table(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines.parse_affix_table(
        "SFX",
        cx.flag_type,
        |flag, crossproduct, strip, add, condition, flagset_str| {
            let flagset = decode_flagset(flagset_str, cx.flag_type, &cx.flag_aliases)?;
            let suffix = Suffix::new(flag, crossproduct, strip, add, condition, flagset)?;
            cx.suffixes.push(suffix);
            Ok(())
        },
    )
}

fn parse_compound_rule_table(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines.parse_table1("COMPOUNDRULE", |word| {
        let rule = parse_compound_rule(word, cx.flag_type)?;
        cx.compound_rules.push(rule);
        Ok(())
    })
}

fn parse_compound_pattern_table(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    // A parser that works basically like `parse_table2` but allows for an optional third column,
    // and trims comments since some dictionaries seem to use them.
    let row_count = lines
        .take_exactly_one_word()?
        .parse::<usize>()
        .map_err(|err| lines.error(ParseDictionaryErrorKind::MalformedNumber(err)))?;

    for row in 1..=row_count {
        lines.advance_line();
        if lines.is_finished() || lines.next_word() != Some("CHECKCOMPOUNDPATTERN") {
            return Err(lines.error(ParseDictionaryErrorKind::MismatchedRowCount {
                expected: row_count,
                actual: row,
            }));
        }

        let mut words = match lines.words.take() {
            Some(words) => words,
            None => {
                return Err(lines.error(ParseDictionaryErrorKind::MismatchedArity {
                    expected: 2,
                    actual: 0,
                }))
            }
        };

        let first_word_end = match words.next() {
            Some(word) => word,
            None => {
                return Err(lines.error(ParseDictionaryErrorKind::MismatchedArity {
                    expected: 2,
                    actual: 0,
                }))
            }
        };
        let second_word_begin = match words.next() {
            Some(word) => word,
            None => {
                return Err(lines.error(ParseDictionaryErrorKind::MismatchedArity {
                    expected: 2,
                    actual: 1,
                }))
            }
        };
        let replacement = words.next();

        let (first_word_end, first_word_flag) = split_word_and_flagset_naive(first_word_end);
        let (second_word_begin, second_word_flag) = split_word_and_flagset_naive(second_word_begin);
        let first_word_flag = (!first_word_flag.is_empty())
            .then(|| parse_flag_from_str(cx.flag_type, first_word_flag))
            .transpose()
            .map_err(|err| lines.error(ParseDictionaryErrorKind::MalformedFlag(err)))?;
        let second_word_flag = (!second_word_flag.is_empty())
            .then(|| parse_flag_from_str(cx.flag_type, second_word_flag))
            .transpose()
            .map_err(|err| lines.error(ParseDictionaryErrorKind::MalformedFlag(err)))?;
        let (first_word_end, match_first_only_unaffixed_or_zero_affixed) = if first_word_end == "0"
        {
            ("", true)
        } else {
            (first_word_end, false)
        };
        let begin_end_chars = super::StrPair::new(first_word_end, second_word_begin);

        cx.compound_patterns.push(CompoundPattern {
            begin_end_chars,
            replacement: replacement.map(|word| word.into()),
            first_word_flag,
            second_word_flag,
            match_first_only_unaffixed_or_zero_affixed,
        });
    }

    Ok(())
}

impl From<&str> for SimilarityGroup {
    fn from(s: &str) -> Self {
        let mut chars = String::new();
        let mut strings = Vec::new();
        let mut i = 0;
        loop {
            let j = s[i..].find('(').map(|idx| idx + i);
            chars.push_str(&s[i..j.unwrap_or(s.len())]);
            let Some(j) = j else {
                break;
            };
            i = j + 1;
            let Some(j) = s[i..].find(')').map(|idx| idx + i) else {
                break;
            };
            // NOTE: we diverge from Nuspell here since I think it's incorrect. Here it pushes
            // to `chars` if `j - i == 1` but that makes `(o)` behave differently than `(ö)` for
            // example since 'ö' is more than one byte long in UTF-8. Instead we count chars here
            // and push to `strings` if it's more than one char.
            let substr = &s[i..j];
            let mut chs = substr.chars();
            let has_no_chars = chs.next().is_none();
            let has_exactly_one_char = chs.next().is_none();
            if has_exactly_one_char {
                // Note that this only pushes one character.
                chars.push_str(substr);
            } else if !has_no_chars {
                strings.push(substr.into());
            }
            i = j + 1;
        }

        SimilarityGroup {
            chars: chars.into(),
            strings: strings.into(),
        }
    }
}

fn parse_map(cx: &mut AffLineParser, lines: &mut Lines) -> ParseResult {
    lines.parse_table1("MAP", |s| {
        cx.similarities.push(s.into());
        Ok(())
    })
}

struct Lines<'text> {
    lines: Peekable<Enumerate<core::str::Lines<'text>>>,
    /// The words on the current line split by whitespace.
    words: Option<SplitWhitespace<'text>>,
    source: ParseDictionaryErrorSource,
}

impl<'text> Lines<'text> {
    fn new(text: &'text str, source: ParseDictionaryErrorSource) -> Self {
        // Skip the UTF-8 BOM if there is one.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let mut lines = text.lines().enumerate().peekable();
        while lines
            .next_if(|(_line_no, line)| line.trim_start().starts_with('#'))
            .is_some()
        {}
        let words = lines.peek().map(|(_line_no, line)| line.split_whitespace());

        Self {
            lines,
            words,
            source,
        }
    }

    fn is_finished(&mut self) -> bool {
        self.lines.peek().is_none()
    }

    fn advance_line(&mut self) {
        self.lines.next();
        while self
            .lines
            .next_if(|(_line_no, line)| line.trim_start().starts_with('#'))
            .is_some()
        {}
        self.words = self
            .lines
            .peek()
            .map(|(_line_no, line)| line.split_whitespace());
    }

    fn line(&mut self) -> Option<&str> {
        self.lines.peek().map(|(_line_no, line)| *line)
    }

    fn next_word(&mut self) -> Option<&str> {
        let mut words = self.words.take()?;
        let word = words.next()?;
        self.words = Some(words);
        Some(word)
    }

    fn take_exactly_one_word(&mut self) -> Result<&'text str> {
        let mut words = self.words.take().ok_or_else(|| {
            self.error(ParseDictionaryErrorKind::MismatchedArity {
                expected: 1,
                actual: 0,
            })
        })?;
        let word = words.next().ok_or_else(|| {
            self.error(ParseDictionaryErrorKind::MismatchedArity {
                expected: 1,
                actual: 0,
            })
        })?;
        self.words = Some(words);
        Ok(word)
    }

    fn parse_flag(&mut self, cx: &AffLineParser) -> Result<Flag> {
        let word = self.take_exactly_one_word()?;
        parse_flag_from_str(cx.flag_type, word)
            .map_err(|err| self.error(ParseDictionaryErrorKind::MalformedFlag(err)))
    }

    fn parse_bool(&mut self) -> Result<bool> {
        Ok(true)
    }

    fn parse_short(&mut self) -> Result<u16> {
        let word = self.take_exactly_one_word()?;
        word.parse::<u16>()
            .map_err(|err| self.error(ParseDictionaryErrorKind::MalformedNumber(err)))
    }

    fn parse_table1<F>(&mut self, key: &str, mut f: F) -> ParseResult
    where
        F: FnMut(&'text str) -> core::result::Result<(), ParseDictionaryErrorKind>,
    {
        let row_count = self
            .take_exactly_one_word()?
            .parse::<usize>()
            .map_err(|err| self.error(ParseDictionaryErrorKind::MalformedNumber(err)))?;

        for row in 1..=row_count {
            self.advance_line();
            if self.is_finished() || self.next_word() != Some(key) {
                return Err(self.error(ParseDictionaryErrorKind::MismatchedRowCount {
                    expected: row_count,
                    actual: row,
                }));
            }

            let mut words = match self.words.take() {
                Some(words) => words,
                None => {
                    return Err(self.error(ParseDictionaryErrorKind::MismatchedArity {
                        expected: 1,
                        actual: 0,
                    }))
                }
            };

            let word = match words.next() {
                Some(word) => word,
                None => {
                    return Err(self.error(ParseDictionaryErrorKind::MismatchedArity {
                        expected: 1,
                        actual: 0,
                    }))
                }
            };

            f(word).map_err(|kind| self.error(kind))?;
        }

        Ok(())
    }

    fn parse_table2<F>(&mut self, key: &str, mut f: F) -> ParseResult
    where
        F: FnMut(&'text str, &'text str) -> core::result::Result<(), ParseDictionaryErrorKind>,
    {
        let row_count = self
            .take_exactly_one_word()?
            .parse::<usize>()
            .map_err(|err| self.error(ParseDictionaryErrorKind::MalformedNumber(err)))?;

        for row in 1..=row_count {
            self.advance_line();
            if self.is_finished() || self.next_word() != Some(key) {
                return Err(self.error(ParseDictionaryErrorKind::MismatchedRowCount {
                    expected: row_count,
                    actual: row,
                }));
            }

            let mut words = match self.words.take() {
                Some(words) => words,
                None => {
                    return Err(self.error(ParseDictionaryErrorKind::MismatchedArity {
                        expected: 2,
                        actual: 0,
                    }))
                }
            };

            let word1 = match words.next() {
                Some(word) => word,
                None => {
                    return Err(self.error(ParseDictionaryErrorKind::MismatchedArity {
                        expected: 2,
                        actual: 0,
                    }))
                }
            };
            let word2 = match words.next() {
                Some(word) => word,
                None => {
                    return Err(self.error(ParseDictionaryErrorKind::MismatchedArity {
                        expected: 2,
                        actual: 1,
                    }))
                }
            };

            f(word1, word2).map_err(|kind| self.error(kind))?;
        }

        Ok(())
    }

    fn parse_affix_table<F>(&mut self, key: &str, flag_type: FlagType, mut f: F) -> ParseResult
    where
        F: FnMut(
            Flag,         // flag
            bool,         // crossproduct
            Option<&str>, // strip
            &str,         // add
            Option<&str>, // condition
            &str,         // flagset
        ) -> core::result::Result<(), ParseDictionaryErrorKind>,
    {
        // Header takes the shapes:
        // PFX flag cross_product row_count
        // SFX flag cross_product row_count
        let mut words = match self.words.take() {
            Some(words) => words,
            None => {
                return Err(self.error(ParseDictionaryErrorKind::MismatchedArity {
                    expected: 3,
                    actual: 0,
                }))
            }
        };

        let flag_str = match words.next() {
            Some(word) => word,
            None => {
                return Err(self.error(ParseDictionaryErrorKind::MismatchedArity {
                    expected: 3,
                    actual: 0,
                }))
            }
        };
        let flag = parse_flag_from_str(flag_type, flag_str)
            .map_err(|err| self.error(ParseDictionaryErrorKind::MalformedFlag(err)))?;

        let crossproduct = match words.next() {
            Some("Y") => true,
            Some("N") => false,
            Some(_) => return Err(self.error(ParseDictionaryErrorKind::MalformedAffix)),
            None => {
                return Err(self.error(ParseDictionaryErrorKind::MismatchedArity {
                    expected: 3,
                    actual: 1,
                }))
            }
        };

        let row_count = match words.next() {
            Some(word) => word
                .parse::<usize>()
                .map_err(|err| self.error(ParseDictionaryErrorKind::MalformedNumber(err)))?,
            None => {
                return Err(self.error(ParseDictionaryErrorKind::MismatchedArity {
                    expected: 3,
                    actual: 2,
                }))
            }
        };

        for row in 1..=row_count {
            // Each row takes the shape:
            // PFX flag stripping prefix [condition [morphological_fields...]]
            // SFX flag stripping suffix [condition [morphological_fields...]]
            self.advance_line();
            if self.is_finished() || self.next_word() != Some(key) {
                return Err(self.error(ParseDictionaryErrorKind::MismatchedRowCount {
                    expected: row_count,
                    actual: row,
                }));
            }

            let mut words = match self.words.take() {
                Some(words) => words,
                None => {
                    return Err(self.error(ParseDictionaryErrorKind::MismatchedArity {
                        expected: 3,
                        actual: 0,
                    }))
                }
            };

            if words.next() != Some(flag_str) {
                return Err(self.error(ParseDictionaryErrorKind::MalformedAffix));
            }

            let strip = match words.next() {
                Some("0") => None,
                Some(word) => Some(word),
                None => {
                    return Err(self.error(ParseDictionaryErrorKind::MismatchedArity {
                        expected: 3,
                        actual: 1,
                    }))
                }
            };

            // the add needs to be split into flagset.
            let (add, flagset) = match words.next() {
                Some(word) => split_word_and_flagset_naive(word),
                None => {
                    return Err(self.error(ParseDictionaryErrorKind::MismatchedArity {
                        expected: 3,
                        actual: 2,
                    }))
                }
            };

            // Like the strip part, add can be nullified with `0`. This is necessary because of
            // the way Nuspell & Hunspell parse "/FLAGS".
            let add = if add == "0" { "" } else { add };

            // "." is the empty condition - it always matches. We'll use an Option for this
            // fast lane instead.
            let condition = words.next().filter(|&cond| cond != ".");

            // NOTE: we don't check the remaining words on the line because morphological fields
            // are allowed after the condition and we don't currently parse those.

            f(flag, crossproduct, strip, add, condition, flagset)
                .map_err(|kind| self.error(kind))?
        }

        Ok(())
    }

    fn error(&mut self, kind: ParseDictionaryErrorKind) -> ParseDictionaryError {
        ParseDictionaryError {
            kind,
            source: self.source,
            line_number: self
                .lines
                .peek()
                // 1-indexed.
                .map(|(line_number, _line)| line_number + 1),
        }
    }
}

fn try_flag_from_u16(val: u16) -> core::result::Result<Flag, ParseFlagError> {
    Flag::new(val).ok_or(ParseFlagError::ZeroFlag)
}

fn try_flag_from_u32(val: u32) -> core::result::Result<Flag, ParseFlagError> {
    if val > u16::MAX as u32 {
        return Err(ParseFlagError::FlagAbove65535);
    }
    try_flag_from_u16(val as u16)
}

fn try_flag_from_char(ch: char) -> core::result::Result<Flag, ParseFlagError> {
    try_flag_from_u32(ch as u32)
}

// See Hunspell's `HashMgr::decode_flag`.
fn parse_flag_from_str(
    flag_type: FlagType,
    input: &str,
) -> core::result::Result<Flag, ParseFlagError> {
    use ParseFlagError::*;
    assert!(!input.is_empty());

    let u16 = match flag_type {
        FlagType::Short => {
            // NOTE: Hunspell and Nuspell do not validate that the flag is ASCII and will accept
            // (and work correctly, for the most part) non-ASCII flags.
            // Also see <https://github.com/helix-editor/spellbook/issues/8>.
            input.as_bytes()[0] as u16
        }
        FlagType::Long => {
            // Same here: the first two bytes are taken and everything else is ignored.
            let bytes = input.as_bytes();
            if bytes.len() < 2 {
                return Err(MissingSecondChar);
            }
            u16::from_ne_bytes([bytes[0], bytes[1]])
        }
        FlagType::Numeric => input.parse::<u16>().map_err(ParseIntError)?,
        FlagType::Utf8 => {
            // A u16 is not large enough to fit any Unicode scalar. Nuspell rejects scalars with
            // codepoint values above `u16::MAX` but Hunspell accepts them. Hunspell converts the
            // input string into UTF-16 and then takes the first u16.
            input
                .encode_utf16()
                .next()
                .expect("asserted to be non-empty above")
        }
    };
    try_flag_from_u16(u16)
}

// See Hunspell's `HashMgr::decode_flags`.
fn parse_flags_from_str(
    flag_type: FlagType,
    input: &str,
) -> core::result::Result<FlagSet, ParseFlagError> {
    use ParseFlagError::*;
    // See above notes about Unicode handling in `parse_flag_from_str`. Handling bytes here may
    // cause the flags to handle Unicode poorly leading to "collisions" between two distinct
    // Unicode characters.

    if input.is_empty() {
        return Ok(FlagSet::default());
    }

    match flag_type {
        FlagType::Short => {
            let flagset = input
                .bytes()
                .map(|b| try_flag_from_u16(b as u16))
                .collect::<core::result::Result<Vec<Flag>, _>>()?;
            Ok(flagset.into())
        }
        FlagType::Long => {
            let bytes = input.as_bytes();
            let mut len = bytes.len();
            if (len & 1) == 1 {
                return Err(MissingSecondChar);
            }
            len >>= 1;
            let mut flags = Vec::with_capacity(len);
            for i in 0..len {
                let u16 = u16::from_ne_bytes([bytes[i << 1], bytes[(i << 1) | 1]]);
                flags.push(try_flag_from_u16(u16)?);
            }
            Ok(flags.into())
        }
        FlagType::Numeric => {
            let mut flags = Vec::new();
            let mut start = 0;
            while let Some(idx) = input[start..].find(',').map(|i| i + start) {
                if input.as_bytes().get(idx + 1).copied() == Some(b',') {
                    return Err(DuplicateComma);
                }
                let flag = input[start..idx]
                    .parse::<u16>()
                    .map_err(ParseIntError)
                    .and_then(try_flag_from_u16)?;
                flags.push(flag);
                start = idx + 1;
            }
            let flag = input[start..]
                .parse::<u16>()
                .map_err(ParseIntError)
                .and_then(try_flag_from_u16)?;
            flags.push(flag);

            Ok(flags.into())
        }
        FlagType::Utf8 => {
            // Using the UTF-16 encoding looks funny here... Nuspell rejects Unicode flags that
            // take more than 16 bits to represent, but Hunspell silently accepts them (though it
            // might lead to weird behavior down the line.)
            let flags = input
                .encode_utf16()
                .map(try_flag_from_u16)
                .collect::<core::result::Result<Vec<Flag>, _>>()?;
            Ok(flags.into())
        }
    }
}

/// Input is assumed to be a single word, i.e. not containing whitespace.
/// This only splits on the slash, it doesn't handle escaping.
// NOTE: in pratice no dictionary uses escaping for affix continuation flags.
fn split_word_and_flagset_naive(input: &str) -> (&str, &str) {
    input.split_once('/').unwrap_or((input, ""))
}

/// Attempt to look up the flagset as an alias.
fn decode_flagset(
    input: &str,
    flag_type: FlagType,
    aliases: &[FlagSet],
) -> core::result::Result<FlagSet, ParseFlagError> {
    // Fast lane for numeric flag-types and empty aliases.
    if matches!(flag_type, FlagType::Numeric) || aliases.is_empty() {
        return parse_flags_from_str(flag_type, input);
    }

    if let Some(index) = input
        .parse::<u16>()
        .ok()
        .map(|i| i as usize)
        .filter(|&i| i > 0 && i <= aliases.len())
    {
        // NOTE: the aliases are 1-indexed.
        Ok(aliases[index - 1].clone())
    } else {
        parse_flags_from_str(flag_type, input)
    }
}

pub(crate) fn parse_dic_line(
    input: &str,
    flag_type: FlagType,
    aliases: &[FlagSet],
    ignore: &[char],
) -> core::result::Result<(Stem, FlagSet), ParseFlagError> {
    fn ignore_chars(s: &str, ignore: &[char]) -> Stem {
        if ignore.is_empty() {
            s.into()
        } else {
            s.chars()
                .filter(|ch| !ignore.contains(ch))
                .collect::<String>()
                .into()
        }
    }

    let Some((idx, byte)) = input
        .find(['/', '\\', ' ', '\t'])
        // Note that the pattern characters above are all ASCII so we can byte index.
        .map(|idx| (idx, input.as_bytes()[idx]))
    else {
        // A fast-lane for words without flagsets.
        let word = ignore_chars(input, ignore);
        return Ok((word, FlagSet::default()));
    };

    // Note: ignore '/' at the start of the line. Not sure why this is correct but it's in the
    // test corpus.
    if byte == b'/' && idx != 0 {
        // Another fast-lane for a common case: a word with a flagset that doesn't use
        // backslash escaping or contain a space.
        let word = ignore_chars(&input[..idx], ignore);
        // Ignore morphological fields for now.
        let flagset_str = input[idx + 1..]
            .split_whitespace()
            .next()
            .unwrap_or(&input[idx + 1..]);
        let flagset = decode_flagset(flagset_str, flag_type, aliases)?;
        return Ok((word, flagset));
    }

    // Fastlane to treat '\t' as the EOL.
    if byte == b'\t' {
        let word = ignore_chars(&input[..idx], ignore);
        return Ok((word, FlagSet::default()));
    }

    // Worst-case scenario the word contains a space or uses backslash to escape the separator
    // '/'. Construct the word gradually.

    // This is always an overallocation but should be more precise than naive string growth.
    let mut word = String::with_capacity(input.len());
    word.push_str(&input[..idx]);

    let input = &input[idx..];
    let mut chars = input.char_indices();
    let mut escaped = false;
    for (idx, ch) in chars.by_ref() {
        match ch {
            '/' if escaped => {
                // Replace the backslash with the slash.
                debug_assert!(!word.is_empty());
                // Note: guaranteed to not underflow because we must have at least one character
                // pushed in order for `escaped` to be true.
                let last_idx = word.len() - 1;
                debug_assert_eq!(word.as_bytes()[last_idx], b'\\');
                // SAFETY: both characters have a UTF-8 length of 1, so overwriting the byte
                // cannot result in invalid UTF-8.
                unsafe {
                    word.as_bytes_mut()[last_idx] = b'/';
                }
            }
            // Note: '/' is allowed at the beginning of a dictionary word.
            '/' if idx != 0 => break,
            '\t' => {
                // Treat tabs as the end-of-line.
                let word = ignore_chars(&word, ignore);
                return Ok((word, FlagSet::default()));
            }
            ' ' => {
                // Detect and ignore morphological fields. Scan ahead three characters into the
                // next non-whitespace part of the input. If it's a lowercase char, a lowercase
                // char and then ':' it's the marker of a morphological field.
                let separates_morphological_field = input[idx + 1..]
                    .find(|ch: char| !ch.is_whitespace())
                    .map(|match_idx| match_idx + idx + 1)
                    .is_some_and(|non_whitespace_idx| {
                        let mut next_non_whitespace = input[non_whitespace_idx..].chars();

                        next_non_whitespace
                            .next()
                            .is_some_and(|ch| ch.is_lowercase())
                            && next_non_whitespace
                                .next()
                                .is_some_and(|ch| ch.is_lowercase())
                            && next_non_whitespace.next() == Some(':')
                    });

                if separates_morphological_field {
                    let word = ignore_chars(&word, ignore);
                    return Ok((word, FlagSet::default()));
                } else {
                    word.push(ch);
                }
            }
            _ => word.push(ch),
        }
        escaped = ch == '\\';
    }

    let flagset_str: String = chars
        .map(|(_idx, ch)| ch)
        .take_while(|ch| !ch.is_whitespace())
        .collect();
    let flagset = decode_flagset(&flagset_str, flag_type, aliases)?;
    let word = ignore_chars(&word, ignore);
    Ok((word, flagset))
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ParseCompoundRuleError {
    ParseFlagError(ParseFlagError),
    InvalidFormat,
}

impl core::fmt::Display for ParseCompoundRuleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ParseFlagError(err) => write!(f, "failed to parse flag: {}", err),
            Self::InvalidFormat => f.write_str("invalid compound rule format"),
        }
    }
}

impl From<ParseFlagError> for ParseCompoundRuleError {
    fn from(err: ParseFlagError) -> Self {
        Self::ParseFlagError(err)
    }
}

impl From<ParseCompoundRuleError> for ParseDictionaryErrorKind {
    fn from(err: ParseCompoundRuleError) -> Self {
        Self::MalformedCompoundRule(err)
    }
}

pub(crate) fn parse_compound_rule(
    input: &str,
    flag_type: FlagType,
) -> core::result::Result<CompoundRule, ParseCompoundRuleError> {
    use super::CompoundRuleElement;

    let rough_capacity = if matches!(flag_type, FlagType::Long) {
        input.len() / 2
    } else {
        input.len()
    };
    let mut rule = Vec::with_capacity(rough_capacity);

    match flag_type {
        FlagType::Short => {
            let mut chars = input.chars().peekable();

            loop {
                let flag = match chars.next() {
                    Some(ch) if !ch.is_ascii() => return Err(ParseFlagError::NonAscii(ch).into()),
                    // All ASCII can fit into a u16 (and a u8).
                    Some(ch) if ch != '?' && ch != '*' => try_flag_from_u16(ch as u16)?,
                    None => break,
                    _ => return Err(ParseCompoundRuleError::InvalidFormat),
                };

                let modifier = match chars.peek() {
                    Some('?') => {
                        chars.next();
                        Some(CompoundRuleModifier::ZeroOrOne)
                    }
                    Some('*') => {
                        chars.next();
                        Some(CompoundRuleModifier::ZeroOrMore)
                    }
                    _ => None,
                };

                rule.push(CompoundRuleElement { flag, modifier })
            }
        }
        FlagType::Utf8 => {
            let mut chars = input.chars().peekable();

            loop {
                let flag = match chars.next() {
                    Some(ch) if ch != '?' && ch != '*' => try_flag_from_char(ch)?,
                    None => break,
                    _ => return Err(ParseCompoundRuleError::InvalidFormat),
                };

                let modifier = match chars.peek() {
                    Some('?') => {
                        chars.next();
                        Some(CompoundRuleModifier::ZeroOrOne)
                    }
                    Some('*') => {
                        chars.next();
                        Some(CompoundRuleModifier::ZeroOrMore)
                    }
                    _ => None,
                };

                rule.push(CompoundRuleElement { flag, modifier })
            }
        }
        FlagType::Long => {
            let mut chars = input.chars().peekable();

            loop {
                let flag = match chars.next() {
                    Some('(') => {
                        let c1 = match chars.next() {
                            Some(ch) if !ch.is_ascii() => {
                                return Err(ParseFlagError::NonAscii(ch).into())
                            }
                            Some(ch) if ch != ')' => ch,
                            _ => return Err(ParseCompoundRuleError::InvalidFormat),
                        };
                        let c2 = match chars.next() {
                            Some(ch) if !ch.is_ascii() => {
                                return Err(ParseFlagError::NonAscii(ch).into())
                            }
                            Some(ch) if ch != ')' => ch,
                            _ => return Err(ParseCompoundRuleError::InvalidFormat),
                        };

                        if chars.next() != Some(')') {
                            return Err(ParseCompoundRuleError::InvalidFormat);
                        }

                        try_flag_from_u16(u16::from_ne_bytes([c1 as u8, c2 as u8]))?
                    }
                    Some(_) => return Err(ParseCompoundRuleError::InvalidFormat),
                    None => break,
                };

                let modifier = match chars.peek() {
                    Some('?') => {
                        chars.next();
                        Some(CompoundRuleModifier::ZeroOrOne)
                    }
                    Some('*') => {
                        chars.next();
                        Some(CompoundRuleModifier::ZeroOrMore)
                    }
                    _ => None,
                };

                rule.push(CompoundRuleElement { flag, modifier })
            }
        }
        FlagType::Numeric => {
            // Most dictionaries will not exceed 3 digit numeric flags.
            let mut number = String::with_capacity(3);
            let mut chars = input.chars().peekable();

            loop {
                let flag = match chars.next() {
                    Some('(') => {
                        loop {
                            match chars.next() {
                                Some(ch) if ch.is_ascii_digit() => number.push(ch),
                                Some(')') if !number.is_empty() => break,
                                _ => return Err(ParseCompoundRuleError::InvalidFormat),
                            }
                        }

                        let n = number
                            .parse::<u16>()
                            .map_err(ParseFlagError::ParseIntError)?;
                        number.clear();

                        try_flag_from_u16(n)?
                    }
                    Some(_) => return Err(ParseCompoundRuleError::InvalidFormat),
                    None => break,
                };

                let modifier = match chars.peek() {
                    Some('?') => {
                        chars.next();
                        Some(CompoundRuleModifier::ZeroOrOne)
                    }
                    Some('*') => {
                        chars.next();
                        Some(CompoundRuleModifier::ZeroOrMore)
                    }
                    _ => None,
                };

                rule.push(CompoundRuleElement { flag, modifier })
            }
        }
    }

    Ok(rule.into())
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct ParseDictionaryError {
    pub kind: ParseDictionaryErrorKind,
    pub source: ParseDictionaryErrorSource,
    pub line_number: Option<usize>,
}

impl fmt::Display for ParseDictionaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line_number {
            Some(line) => write!(
                f,
                "failed to parse {} file on line {}: {}",
                self.source, line, self.kind
            ),
            None => write!(f, "failed to parse {} file: {}", self.source, self.kind),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ParseDictionaryErrorSource {
    Dic,
    Aff,
    // Personal, ?
}

impl fmt::Display for ParseDictionaryErrorSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dic => write!(f, ".dic"),
            Self::Aff => write!(f, ".aff"),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ParseDictionaryErrorKind {
    UnknownFlagType(UnknownFlagTypeError),
    MalformedFlag(ParseFlagError),
    MalformedNumber(core::num::ParseIntError),
    UnexpectedNonWhitespace(char),
    MismatchedArity { expected: usize, actual: usize },
    MismatchedRowCount { expected: usize, actual: usize },
    MalformedCompoundRule(ParseCompoundRuleError),
    // MalformedMorphologicalField(String),
    MalformedAffix,
    MalformedCondition(ConditionError),
    WordTooLong,
    Empty,
}

impl From<UnknownFlagTypeError> for ParseDictionaryErrorKind {
    fn from(err: UnknownFlagTypeError) -> Self {
        Self::UnknownFlagType(err)
    }
}

impl From<ParseFlagError> for ParseDictionaryErrorKind {
    fn from(err: ParseFlagError) -> Self {
        Self::MalformedFlag(err)
    }
}

impl From<ConditionError> for ParseDictionaryErrorKind {
    fn from(err: ConditionError) -> Self {
        Self::MalformedCondition(err)
    }
}

impl fmt::Display for ParseDictionaryErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownFlagType(err) => err.fmt(f),
            Self::MalformedFlag(err) => {
                write!(f, "flag is malformed: {}", err)
            }
            Self::MalformedNumber(err) => err.fmt(f),
            Self::UnexpectedNonWhitespace(ch) => {
                write!(f, "unexpected non-whitespace character '{}'", ch)
            }
            Self::MismatchedArity { expected, actual } => {
                write!(f, "expected {} arguments but found {}", expected, actual)
            }
            Self::MismatchedRowCount { expected, actual } => {
                write!(f, "expected {} rows but found {}", expected, actual)
            }
            Self::MalformedCompoundRule(err) => {
                write!(f, "compound rule is malformed: {}", err)
            }
            // Self::MalformedMorphologicalField(s) => {
            //     write!(f, "morphological field '{}' is malformed", s)
            // }
            Self::MalformedAffix => write!(f, "failed to parse affix"),
            Self::MalformedCondition(err) => write!(f, "condition is malformed: {}", err),
            Self::WordTooLong => write!(f, "word is too long (longer than u16::MAX bytes)"),
            Self::Empty => write!(f, "the file is empty"),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct UnknownFlagTypeError(String);

impl FromStr for FlagType {
    type Err = UnknownFlagTypeError;

    fn from_str(s: &str) -> core::result::Result<Self, Self::Err> {
        match s {
            "long" => Ok(Self::Long),
            "num" => Ok(Self::Numeric),
            "UTF-8" => Ok(Self::Utf8),
            _ => Err(UnknownFlagTypeError(s.to_string())),
        }
    }
}

impl fmt::Display for UnknownFlagTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "expected FLAG to be `long`, `num` or `UTF-8` if set, found {}",
            self.0
        )
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum ParseFlagError {
    NonAscii(char),
    MissingSecondChar,
    ParseIntError(core::num::ParseIntError),
    DuplicateComma,
    ZeroFlag,
    FlagAbove65535,
}

impl fmt::Display for ParseFlagError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonAscii(ch) => write!(f, "expected ascii char, found {}", ch),
            Self::MissingSecondChar => f.write_str("expected two chars, found one"),
            Self::ParseIntError(err) => err.fmt(f),
            Self::DuplicateComma => f.write_str("unexpected extra comma"),
            Self::ZeroFlag => f.write_str("flag cannot be zero"),
            Self::FlagAbove65535 => f.write_str("flag's binary representation exceeds 65535"),
        }
    }
}

/// An error arising from validating a [`Condition`].
///
/// Conditions are a subset of regular expressions that include positive and negative character
/// classes and the wildcard character. A condition might fail validation if the character classes
/// are open (for example `foo]` or `foo[bar`) or if the condition has an empty character class,
/// which is not valid (`[]`).
// Hands where I can see 'em, clippy. The only time I ever went down was when a machine was easing
// at the wrong time.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum ConditionError {
    /// The pattern contained an opening `[` character which did not match a closing `]`
    /// character.
    UnopenedCharacterClass,
    /// The pattern contained a closing `]` character which did not match an opening `[`
    /// character.
    UnclosedCharacterClass,
    /// The pattern contained the literal `[]` which is not a valid character class.
    EmptyCharacterClass,
}

impl fmt::Display for ConditionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnopenedCharacterClass => {
                f.write_str("closing bracket has no matching opening bracket")
            }
            Self::UnclosedCharacterClass => {
                f.write_str("opening bracket has no matching closing bracket")
            }
            Self::EmptyCharacterClass => f.write_str("empty bracket expression"),
        }
    }
}

impl FromStr for Condition {
    type Err = ConditionError;

    fn from_str(s: &str) -> core::result::Result<Self, Self::Err> {
        let mut scan = s;
        let mut chars = 0;

        // Loop through the characters. We can't just iterate through the `.chars()` because we'll
        // be jumping ahead with the help of `find`.
        loop {
            // Find a bracket. Brackets signal character classes.
            let bracket_index = match scan.find(['[', ']']) {
                Some(index) => index,
                None => {
                    // If there isn't one, accept the rest of the string.
                    chars += scan.chars().count();
                    break;
                }
            };
            // If there is one, scan ahead to it.
            chars += scan[..bracket_index].chars().count();
            scan = &scan[bracket_index..];
            match scan
                .chars()
                .next()
                .expect("scan can't be empty if the pattern matched")
            {
                ']' => return Err(Self::Err::UnopenedCharacterClass),
                '[' => {
                    scan = &scan[1..];
                    match scan.chars().next() {
                        None => return Err(Self::Err::UnclosedCharacterClass),
                        Some('^') => scan = &scan[1..],
                        _ => (),
                    }

                    match scan.find(']') {
                        None => return Err(Self::Err::UnclosedCharacterClass),
                        Some(0) => return Err(Self::Err::EmptyCharacterClass),
                        Some(bracket_index) => {
                            // Only count the character class as one character.
                            chars += 1;
                            scan = &scan[bracket_index + 1..];
                            continue;
                        }
                    }
                }
                // This is impossible if find `find` found `[` or `]`.
                _ => unreachable!(),
            }
        }

        Ok(Self {
            pattern: s.into(),
            chars,
            // A literal condition has no wildcard or character class syntax.
            // NOTE: we don't need to search for ']' here since we already rejected mismatched
            // brackets above as an Err.
            is_literal: !s.contains(['.', '[']),
        })
    }
}

#[cfg(test)]
mod test {
    use crate::DefaultHashBuilder;

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
    fn naive_word_flagset_split_test() {
        assert_eq!(
            ("word", "flags"),
            split_word_and_flagset_naive("word/flags")
        );
        assert_eq!(("word", ""), split_word_and_flagset_naive("word"));
        assert_eq!(("", ""), split_word_and_flagset_naive(""));
    }

    #[test]
    fn parse_flagset_test() {
        assert_eq!(
            Ok(flag!('a' as u16)),
            parse_flag_from_str(FlagType::Short, "a")
        );
        // Non-ASCII is accepted for short flags too. Only the first byte is taken.
        assert_eq!(Ok(flag!(195)), parse_flag_from_str(FlagType::Short, "À"));
        // As below, this can cause "collisions."
        // Here both characters share the same first byte (`0xC3`).
        assert_eq!(
            parse_flag_from_str(FlagType::Short, "À"),
            parse_flag_from_str(FlagType::Short, "Ã")
        );
        assert_eq!(Ok(flag!(1)), parse_flag_from_str(FlagType::Numeric, "1"));

        // U+1F52D '🔭' is four bytes in UTF8 and two code units in UTF-16. Nuspell rejects flags
        // like this but Hunspell accepts them by discarding the lower code unit.
        let telescope_flag =
            parse_flag_from_str(FlagType::Utf8, "🔭").expect("can parse 🔭 UTF-8 flag");
        // A consequence of this is that flags describing large Unicode scalar values are not
        // precise and two emojis (for example) may "collide" to reuse the same flag value, for
        // example the above telescope U+1F52D and the next scalar, U+1F52E crystal ball.
        let crystal_ball_flag =
            parse_flag_from_str(FlagType::Utf8, "🔮").expect("can parse 🔮 UTF-8 flag");
        assert_eq!(telescope_flag, crystal_ball_flag);

        assert_eq!(
            Ok(flagset![1]),
            parse_flags_from_str(FlagType::Numeric, "1")
        );
        assert_eq!(
            Ok(flagset![1001, 2002]),
            parse_flags_from_str(FlagType::Numeric, "1001,2002")
        );
        assert_eq!(
            Ok(flagset![214, 216, 54321]),
            parse_flags_from_str(FlagType::Numeric, "214,216,54321")
        );
        assert_eq!(
            Err(ParseFlagError::DuplicateComma),
            parse_flags_from_str(FlagType::Numeric, "123,,789")
        );
    }

    #[test]
    fn decode_flagset_test() {
        let aliases = &[flagset![1], flagset![2], flagset![3, 4]];

        // NOTE: 1-indexing.
        assert_eq!(
            flagset![1],
            decode_flagset("1", FlagType::default(), aliases).unwrap()
        );
        assert_eq!(
            flagset![2],
            decode_flagset("2", FlagType::default(), aliases).unwrap()
        );
        assert_eq!(
            flagset![3, 4],
            decode_flagset("3", FlagType::default(), aliases).unwrap()
        );
        assert_eq!(
            flagset!['a' as u16],
            decode_flagset("a", FlagType::default(), aliases).unwrap()
        );

        assert_eq!(
            flagset![1],
            decode_flagset("1", FlagType::Numeric, aliases).unwrap()
        );
        assert_eq!(
            flagset!['1' as u16],
            decode_flagset("1", FlagType::default(), &[]).unwrap()
        );
    }

    #[test]
    fn parse_condition_test() {
        assert_eq!(
            Err(ConditionError::EmptyCharacterClass),
            "[]".parse::<Condition>()
        );
        assert_eq!(
            Err(ConditionError::UnclosedCharacterClass),
            "[foo".parse::<Condition>()
        );
        assert_eq!(
            Err(ConditionError::UnopenedCharacterClass),
            "foo]".parse::<Condition>()
        );
        assert_eq!(
            Ok(Condition {
                pattern: "foo".into(),
                chars: 3,
                is_literal: true
            }),
            "foo".parse()
        );
        assert_eq!(
            Ok(Condition {
                pattern: "foo[bar]".into(),
                chars: 4,
                is_literal: false
            }),
            "foo[bar]".parse()
        );
        assert_eq!(
            Ok(Condition {
                pattern: "[foo]bar".into(),
                chars: 4,
                is_literal: false
            }),
            "[foo]bar".parse()
        );
        assert_eq!(
            Ok(Condition {
                pattern: "foo[bar]baz".into(),
                chars: 7,
                is_literal: false
            }),
            "foo[bar]baz".parse()
        );
    }

    #[test]
    fn parse_compound_rule_test() {
        use super::ParseCompoundRuleError as Error;
        use crate::aff::{CompoundRuleElement as Elem, CompoundRuleModifier::*};

        assert_eq!(
            &[
                Elem {
                    flag: flag!('a'),
                    modifier: None
                },
                Elem {
                    flag: flag!('b'),
                    modifier: Some(ZeroOrOne)
                },
                Elem {
                    flag: flag!('c'),
                    modifier: Some(ZeroOrMore)
                },
                Elem {
                    flag: flag!('d'),
                    modifier: None
                },
            ],
            parse_compound_rule("ab?c*d", FlagType::Short)
                .unwrap()
                .as_ref()
        );

        // Hello, en_GB.aff
        assert_eq!(
            &[
                Elem {
                    flag: flag!('#'),
                    modifier: Some(ZeroOrMore)
                },
                Elem {
                    flag: flag!('0'),
                    modifier: None
                },
                Elem {
                    flag: flag!('{'),
                    modifier: None
                },
            ],
            parse_compound_rule("#*0{", FlagType::Utf8)
                .unwrap()
                .as_ref()
        );

        assert_eq!(
            &[
                Elem {
                    flag: flag!(5),
                    modifier: None
                },
                Elem {
                    flag: flag!(6),
                    modifier: Some(ZeroOrMore)
                },
                Elem {
                    flag: flag!(11),
                    modifier: Some(ZeroOrOne),
                },
                Elem {
                    flag: flag!(99),
                    modifier: None,
                },
            ],
            parse_compound_rule("(5)(6)*(11)?(99)", FlagType::Numeric)
                .unwrap()
                .as_ref()
        );

        assert_eq!(
            &[
                Elem {
                    flag: flag!(10060),
                    modifier: None
                },
                Elem {
                    flag: flag!(10052),
                    modifier: Some(ZeroOrMore)
                },
                Elem {
                    flag: flag!(10056),
                    modifier: Some(ZeroOrOne),
                },
                Elem {
                    flag: flag!(17218),
                    modifier: None,
                },
            ],
            parse_compound_rule("(L')(D')*(H')?(BC)", FlagType::Long)
                .unwrap()
                .as_ref()
        );

        // Can't start with a wildcard
        assert_eq!(
            Err(Error::InvalidFormat),
            parse_compound_rule("*", FlagType::Short)
        );
        assert_eq!(
            Err(Error::InvalidFormat),
            parse_compound_rule("?", FlagType::Short)
        );
    }

    #[test]
    fn basic_prefix_test() {
        let dic = "0";
        // From `en_GB.aff`.
        let aff = r#"
        PFX A Y 2
        PFX A 0 re [^e]
        PFX A 0 re- e
        "#;

        let (_words, aff_data) = parse(aff, dic, DefaultHashBuilder::default()).unwrap();
        assert_eq!(2, aff_data.prefixes.table.len());
        assert_eq!(
            Prefix::new(flag!('A'), true, None, "re", Some("[^e]"), flagset![]).unwrap(),
            aff_data.prefixes.table[0]
        );
        assert_eq!(
            Prefix::new(flag!('A'), true, None, "re-", Some("e"), flagset![]).unwrap(),
            aff_data.prefixes.table[1]
        );
    }

    #[test]
    fn parse_dic_line_test() {
        fn parse(input: &str) -> (Stem, FlagSet) {
            parse_dic_line(input, FlagType::default(), &[], &[]).unwrap()
        }

        assert_eq!(("stem".into(), flagset![]), parse("stem"));
        assert_eq!(("stem".into(), flagset![]), parse("stem/"));
        assert_eq!(("fox-bax".into(), flagset!['!']), parse("fox-bax/!"));
        assert_eq!(
            ("stem".into(), flagset!['F', 'L', 'A', 'G', 'S']),
            parse("stem/FLAGS")
        );
        // nl_NL
        assert_eq!(
            ("Oost/Watergraafsmeer".into(), flagset![]),
            parse("Oost\\/Watergraafsmeer")
        );
        // nn_NO
        assert_eq!(("Håkon".into(), flagset!['J', '\\']), parse("Håkon/J\\"));

        // hu corpus
        assert_eq!(("devon kor".into(), flagset![]), parse("devon kor"));

        // en_GB
        // The '\t' marks the end of the word+flagset.
        assert_eq!(
            ("activewear".into(), flagset!['M']),
            parse("activewear/M	Noun: uncountable")
        );
        // gl_ES
        assert_eq!(
            ("Aguileño".into(), flagset![]),
            parse("Aguileño po:nome is:ngrama_Movimiento_Aguileño_Socialdemócrata")
        );

        assert_eq!(("devon  kor".into(), flagset!['A']), parse("devon  kor/A"));
        assert_eq!(
            ("activewear".into(), flagset!['M']),
            parse("activewear/M po: nome")
        );
        assert_eq!(
            ("activewear".into(), flagset![]),
            parse("activewear\tpo:name")
        );
    }

    #[test]
    fn basic_flag_parsing() {
        let dic = "0";
        let aff = r#"
        FORBIDDENWORD a
        CIRCUMFIX b
        KEEPCASE c
        NEEDAFFIX d
        NOSUGGEST e
        SUBSTANDARD f
        WARN g
        COMPOUNDFLAG h
        COMPOUNDBEGIN i
        COMPOUNDMIDDLE j
        COMPOUNDEND k
        ONLYINCOMPOUND l
        COMPOUNDPERMITFLAG m
        COMPOUNDFORBIDFLAG n
        COMPOUNDROOT o
        FORCEUCASE p
        "#;
        let (_words, aff_data) = parse(aff, dic, DefaultHashBuilder::default()).unwrap();
        assert_eq!(aff_data.options.forbidden_word_flag, flag!('a'));
        assert_eq!(aff_data.options.circumfix_flag, Some(flag!('b')));
        assert_eq!(aff_data.options.keep_case_flag, Some(flag!('c')));
        assert_eq!(aff_data.options.need_affix_flag, Some(flag!('d')));
        assert_eq!(aff_data.options.no_suggest_flag, Some(flag!('e')));
        assert_eq!(aff_data.options.substandard_flag, Some(flag!('f')));
        assert_eq!(aff_data.options.warn_flag, Some(flag!('g')));
        assert_eq!(aff_data.options.compound_flag, Some(flag!('h')));
        assert_eq!(aff_data.options.compound_begin_flag, Some(flag!('i')));
        assert_eq!(aff_data.options.compound_middle_flag, Some(flag!('j')));
        assert_eq!(aff_data.options.compound_end_flag, Some(flag!('k')));
        assert_eq!(aff_data.options.only_in_compound_flag, Some(flag!('l')));
        assert_eq!(aff_data.options.compound_permit_flag, Some(flag!('m')));
        assert_eq!(aff_data.options.compound_forbid_flag, Some(flag!('n')));
        assert_eq!(aff_data.options.compound_root_flag, Some(flag!('o')));
        assert_eq!(
            aff_data.options.compound_force_uppercase_flag,
            Some(flag!('p'))
        );
    }

    #[test]
    fn break_pattern_parsing() {
        let dic = "0";
        let aff = "";
        let (_words, aff_data) = parse(aff, dic, DefaultHashBuilder::default()).unwrap();
        // By default it's `^-`, `-` and `-$`.
        assert_eq!(aff_data.break_table.table.len(), 3);
        // Default break patterns can be removed by setting `BREAK 0`.

        let dic = "0";
        let aff = "BREAK 0";
        let (_words, aff_data) = parse(aff, dic, DefaultHashBuilder::default()).unwrap();
        assert_eq!(aff_data.break_table.table.len(), 0);
    }

    #[test]
    fn similarity_group_parsing() {
        let sg: SimilarityGroup = "uúü".into();
        assert!(sg.strings.is_empty());
        assert_eq!(sg.chars.as_ref(), "uúü");

        let sg: SimilarityGroup = "ß(ss)".into();
        assert_eq!(sg.chars.as_ref(), "ß");
        assert_eq!(sg.strings.as_ref(), &["ss".into()]);

        let sg: SimilarityGroup = "(o)(ö)".into();
        assert_eq!(sg.chars.as_ref(), "oö");
        assert!(sg.strings.is_empty());
    }
}
