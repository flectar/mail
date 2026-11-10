//! Which words of a text to check.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

/// The words in `text` worth checking, as byte ranges.
///
/// Like word processors by default, this leaves out web, email and file addresses, words with
/// digits and words in capitals, which are mostly acronyms. It also leaves out scripts written
/// without spaces between words, such as Chinese, Japanese and Thai, which need a dictionary of
/// whole phrases to split and no Hunspell dictionary covers.
pub fn checkable_words(text: &str) -> Vec<Range<usize>> {
    let addresses = address_ranges(text);
    let mut addresses = addresses.iter().peekable();
    let mut words = Vec::new();
    for (start, word) in text.unicode_word_indices() {
        let range = start..start + word.len();
        while addresses
            .next_if(|address| address.end <= range.start)
            .is_some()
        {}
        if addresses
            .peek()
            .is_some_and(|address| address.start < range.end)
        {
            continue;
        }
        if word.contains('.')
            || word
                .chars()
                .any(|c| c.is_numeric() || is_unspaced_script(c))
            || is_capitalized(word)
        {
            continue;
        }
        words.push(range);
    }
    words
}

/// Two or more letters, none of them lowercase.
fn is_capitalized(word: &str) -> bool {
    word.chars().filter(|c| c.is_alphabetic()).nth(1).is_some()
        && !word.chars().any(char::is_lowercase)
}

fn is_unspaced_script(c: char) -> bool {
    matches!(c,
        '\u{0E00}'..='\u{0EFF}' // Thai, Lao
        | '\u{1000}'..='\u{109F}' // Myanmar
        | '\u{1780}'..='\u{17FF}' // Khmer
        | '\u{3040}'..='\u{30FF}' // Hiragana, Katakana
        | '\u{3100}'..='\u{312F}' // Bopomofo
        | '\u{31F0}'..='\u{31FF}' // Katakana extensions
        | '\u{3400}'..='\u{4DBF}' // CJK extension A
        | '\u{4E00}'..='\u{9FFF}' // CJK unified ideographs
        | '\u{F900}'..='\u{FAFF}' // CJK compatibility ideographs
        | '\u{FF66}'..='\u{FF9F}' // Halfwidth Katakana
        | '\u{20000}'..='\u{3134F}' // CJK extensions B to G
    )
}

/// The byte ranges of web, email and file addresses in `text`, in order.
fn address_ranges(text: &str) -> Vec<Range<usize>> {
    text.split_whitespace()
        .filter_map(|token| {
            let start = token.as_ptr() as usize - text.as_ptr() as usize;
            let core = token.trim_matches(|c: char| {
                matches!(
                    c,
                    '(' | ')'
                        | '<'
                        | '>'
                        | '['
                        | ']'
                        | '"'
                        | '\''
                        | ','
                        | ';'
                        | '!'
                        | '?'
                        | '“'
                        | '”'
                        | '‘'
                        | '’'
                )
            });
            is_address(core).then_some(start..start + token.len())
        })
        .collect()
}

fn is_address(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("www.") || lower.starts_with("mailto:") {
        return true;
    }
    if let Some((user, domain)) = token.split_once('@') {
        return !user.is_empty() && domain.trim_end_matches('.').contains('.');
    }
    // Unix and Windows paths.
    token.starts_with('/') && token.len() > 1
        || token.starts_with("~/")
        || token.get(1..3) == Some(":\\")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checked(text: &str) -> Vec<&str> {
        checkable_words(text)
            .into_iter()
            .map(|range| &text[range])
            .collect()
    }

    #[test]
    fn prose_words_are_checked() {
        assert_eq!(
            checked("Hi Jordan, don't forget the café’s receipt. I'll send it!"),
            [
                "Hi",
                "Jordan",
                "don't",
                "forget",
                "the",
                "café’s",
                "receipt",
                "I'll",
                "send",
                "it"
            ]
        );
    }

    #[test]
    fn addresses_numbers_capitals_and_unspaced_scripts_are_left_out() {
        assert_eq!(
            checked(
                "See https://files.example/q3 and www.example.com or (mail jordan.lee@example.com), \
                 the report at /home/alex/Q3.pdf or C:\\Reports, the Q3 numbers from 2026, \
                 the NASA and ASAP notes, flectar.com, e.g. this, and 東京で会いましょう."
            ),
            [
                "See", "and", "or", "mail", "the", "report", "at", "or", "the", "numbers", "from",
                "the", "and", "notes", "this", "and"
            ]
        );
    }

    #[test]
    fn single_capitals_are_words() {
        assert_eq!(checked("I saw A and B."), ["I", "saw", "A", "and", "B"]);
    }
}
