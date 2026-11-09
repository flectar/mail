//! Numbers written the way the UI language writes them, as CLDR has it:
//! "4,812" in English, "4.812" in German, "4 812" in Russian, and prices
//! such as "€4.50" in English and "4,50 €" in Spanish.

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

/// How a language writes an amount of money: CLDR's standard currency
/// pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Money {
    decimal: char,
    /// The currency symbol comes before the number.
    symbol_first: bool,
    /// Between the symbol and the number.
    space: &'static str,
}

const ENGLISH_MONEY: Money = Money {
    decimal: '.',
    symbol_first: true,
    space: "",
};

/// The bundled translations, so the language keeps a `'static` name.
const LANGUAGES: [&str; 11] = [
    "en", "da", "de", "es", "fr", "it", "ja", "nl", "ru", "tr", "zh_Hans",
];

thread_local! {
    static GROUPING: Cell<Grouping> = const { Cell::new(ENGLISH) };
    static LANGUAGE: Cell<&'static str> = const { Cell::new("en") };
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

fn money_format(language: &str) -> Money {
    match language {
        "da" | "de" | "es" | "fr" | "it" | "ru" => Money {
            decimal: ',',
            symbol_first: false,
            space: "\u{a0}",
        },
        "nl" => Money {
            decimal: ',',
            symbol_first: true,
            space: "\u{a0}",
        },
        "tr" => Money {
            decimal: ',',
            symbol_first: true,
            space: "",
        },
        _ => ENGLISH_MONEY,
    }
}

/// The symbol `language` writes for `currency`, an upper-case ISO 4217
/// code, as CLDR has it; `None` where it writes the code.
fn currency_symbol(language: &str, currency: &str) -> Option<&'static str> {
    Some(match (currency, language) {
        ("EUR", _) => "€",
        ("USD", "da" | "it") => return None,
        ("USD", "es" | "nl" | "zh_Hans") => "US$",
        ("USD", "fr") => "$US",
        ("USD", _) => "$",
        ("GBP", "es") => return None,
        ("GBP", "fr") => "£GB",
        ("GBP", _) => "£",
        ("JPY", "es" | "fr" | "it") => return None,
        ("JPY", "da" | "nl" | "zh_Hans") => "JP¥",
        ("JPY", "ja") => "￥",
        ("JPY", _) => "¥",
        _ => return None,
    })
}

/// Digits after the decimal point in an amount Stripe gives in the
/// currency's smallest unit.
fn minor_digits(currency: &str) -> u32 {
    // Stripe's zero-decimal currencies, but for UGX, which it still counts
    // in hundredths, as it does ISK.
    const NONE: [&str; 15] = [
        "BIF", "CLP", "DJF", "GNF", "JPY", "KMF", "KRW", "MGA", "PYG", "RWF", "VND", "VUV", "XAF",
        "XOF", "XPF",
    ];
    const THREE: [&str; 6] = ["BHD", "JOD", "KWD", "LYD", "OMR", "TND"];
    if NONE.contains(&currency) {
        0
    } else if THREE.contains(&currency) {
        3
    } else {
        2
    }
}

/// Write numbers the way `language`, a bundled translation's code, does.
pub(crate) fn set_language(language: &str) {
    GROUPING.with(|current| current.set(grouping(language)));
    let known = LANGUAGES
        .iter()
        .find(|known| **known == language)
        .copied()
        .unwrap_or("en");
    LANGUAGE.with(|current| current.set(known));
}

/// An amount of money the way the UI language writes it: "€4.50" in
/// English, "4,50 €" in Spanish, "€ 4,50" in Dutch. `amount` is in the
/// currency's smallest unit, as Stripe counts it, and `currency` is its ISO
/// 4217 code. A whole amount leaves its decimals out, as price tags do.
pub(crate) fn money(amount: u64, currency: &str) -> String {
    let currency = currency.to_ascii_uppercase();
    let language = LANGUAGE.with(Cell::get);
    let format = money_format(language);
    let digits = minor_digits(&currency);
    let unit = 10u64.pow(digits);
    let mut number = count(amount / unit);
    if !amount.is_multiple_of(unit) {
        number.push(format.decimal);
        number.push_str(&format!(
            "{:0width$}",
            amount % unit,
            width = digits as usize
        ));
    }
    let symbol = currency_symbol(language, &currency).unwrap_or(&currency);
    // A code such as "CHF" keeps a space from the number, as CLDR's
    // currency spacing has it.
    let space = match format.space {
        "" if format.symbol_first && symbol.ends_with(char::is_alphabetic) => "\u{a0}",
        "" if !format.symbol_first && symbol.starts_with(char::is_alphabetic) => "\u{a0}",
        space => space,
    };
    if format.symbol_first {
        format!("{symbol}{space}{number}")
    } else {
        format!("{number}{space}{symbol}")
    }
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

    #[test]
    fn prices_are_written_the_way_the_language_writes_them() {
        set_language("en");
        assert_eq!(money(400, "eur"), "€4");
        assert_eq!(money(450, "EUR"), "€4.50");
        assert_eq!(money(4805, "usd"), "$48.05");
        assert_eq!(money(400, "chf"), "CHF\u{a0}4");
        assert_eq!(money(123_456, "eur"), "€1,234.56");
        set_language("es");
        assert_eq!(money(450, "eur"), "4,50\u{a0}€");
        assert_eq!(money(450, "usd"), "4,50\u{a0}US$");
        assert_eq!(money(400, "gbp"), "4\u{a0}GBP");
        set_language("ru");
        assert_eq!(money(333, "eur"), "3,33\u{a0}€");
        set_language("nl");
        assert_eq!(money(450, "eur"), "€\u{a0}4,50");
        set_language("tr");
        assert_eq!(money(450, "eur"), "€4,50");
        set_language("fr");
        assert_eq!(money(450, "usd"), "4,50\u{a0}$US");
        set_language("ja");
        assert_eq!(money(500, "jpy"), "￥500");
        // Units without decimals, and with three.
        set_language("en");
        assert_eq!(money(500, "jpy"), "¥500");
        assert_eq!(money(4500, "kwd"), "KWD\u{a0}4.500");
        assert_eq!(money(4000, "kwd"), "KWD\u{a0}4");
        // Stripe still counts Ugandan shillings in hundredths.
        assert_eq!(money(500_000, "ugx"), "UGX\u{a0}5,000");
        // A language without a translation of its own writes as English.
        set_language("xx");
        assert_eq!(money(450, "eur"), "€4.50");
    }
}
