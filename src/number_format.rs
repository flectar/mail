//! Whole numbers written with the digit grouping of the UI language, as CLDR
//! has it: "4,812" in English, "4.812" in German, "4 812" in Russian.

use std::cell::Cell;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Grouping {
    separator: char,
    /// Numbers with fewer digits stay ungrouped.
    min_digits: usize,
}

const ENGLISH: Grouping = Grouping {
    separator: ',',
    min_digits: 4,
};

thread_local! {
    static GROUPING: Cell<Grouping> = const { Cell::new(ENGLISH) };
}

fn grouping(language: &str) -> Grouping {
    match language {
        // Spanish groups only from five digits on.
        "es" => Grouping {
            separator: '.',
            min_digits: 5,
        },
        "da" | "de" | "it" | "nl" | "tr" => Grouping {
            separator: '.',
            min_digits: 4,
        },
        "fr" | "ru" => Grouping {
            separator: '\u{a0}',
            min_digits: 4,
        },
        _ => ENGLISH,
    }
}

/// Group digits the way `language`, a bundled translation's code, does.
pub(crate) fn set_language(language: &str) {
    GROUPING.with(|current| current.set(grouping(language)));
}

/// A count, such as of messages, with its digits grouped.
pub(crate) fn count(value: u64) -> String {
    let digits = value.to_string();
    let grouping = GROUPING.with(Cell::get);
    if digits.len() < grouping.min_digits {
        return digits;
    }
    let mut text = String::with_capacity(digits.len() + digits.len() / 3 * 2);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            text.push(grouping.separator);
        }
        text.push(digit);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digits_are_grouped_as_the_language_writes_them() {
        set_language("en");
        assert_eq!(count(0), "0");
        assert_eq!(count(999), "999");
        assert_eq!(count(4812), "4,812");
        assert_eq!(count(1_234_567), "1,234,567");
        set_language("de");
        assert_eq!(count(1000), "1.000");
        set_language("es");
        assert_eq!(count(4812), "4812");
        assert_eq!(count(48_120), "48.120");
        set_language("ru");
        assert_eq!(count(4812), "4\u{a0}812");
        set_language("zh_Hans");
        assert_eq!(count(1_000_000), "1,000,000");
    }
}
