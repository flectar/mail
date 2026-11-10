//! Spelling languages by name. Dictionaries are named by code, such as "de_CH_frami", and
//! listed by their language's own name for itself, "Deutsch (Schweiz, frami)", as language
//! menus list them so each reader finds their own.

// Only Linux lists its languages so far; other platforms check none.
#[cfg_attr(not(linux_desktop), allow(dead_code))]
mod data {
    include!("names_data.rs");
}
use data::{ALIASES, LANGUAGES, REGIONS};

/// The language of a dictionary, as CLDR codes it: "de" for "de_CH_frami".
pub(super) fn language_of(id: &str) -> &str {
    let language = id.split(['_', '-']).next().unwrap_or_default();
    ALIASES
        .iter()
        .find(|(dictionary, _)| dictionary.eq_ignore_ascii_case(language))
        .map_or(language, |(_, cldr)| cldr)
}

/// The region of a dictionary: "CH" for "de_CH_frami", "419" for "es_419".
pub(super) fn region_of(id: &str) -> Option<&str> {
    id.split(['_', '-']).skip(1).find(|part| is_region(part))
}

fn is_region(part: &str) -> bool {
    (part.len() == 2 && part.bytes().all(|b| b.is_ascii_uppercase()))
        || (part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit()))
}

/// The region a dictionary for `language` means when it names none.
pub(super) fn main_region(language: &str) -> Option<&'static str> {
    LANGUAGES
        .iter()
        .find(|(code, ..)| *code == language)
        .and_then(|(.., region)| *region)
}

/// A dictionary's name for people: "Deutsch (Schweiz)" for "de_CH", with the rest of its code
/// after the region, "Deutsch (Schweiz, frami)". Codes CLDR has no name for stay as they are.
#[cfg_attr(not(linux_desktop), allow(dead_code))]
pub(super) fn display_name(id: &str) -> String {
    let language = language_of(id);
    let Some((_, name, _)) = LANGUAGES.iter().find(|(code, ..)| *code == language) else {
        return id.to_owned();
    };
    let details: Vec<&str> = id
        .split(['_', '-'])
        .skip(1)
        .map(|part| {
            REGIONS
                .iter()
                .find(|(code, region, _)| *code == language && *region == part)
                .map_or(part, |(.., region_name)| region_name)
        })
        .collect();
    if details.is_empty() {
        (*name).to_owned()
    } else {
        format!("{name} ({})", details.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictionaries_are_named_in_their_own_language() {
        assert_eq!(display_name("en_US"), "English (United States)");
        assert_eq!(display_name("de_CH_frami"), "Deutsch (Schweiz, frami)");
        assert_eq!(display_name("es_ES"), "Español (España)");
        assert_eq!(display_name("ru_RU"), "Русский (Россия)");
        assert_eq!(display_name("ca_valencia"), "Català (valencia)");
        assert_eq!(display_name("eo"), "Esperanto");
        assert_eq!(display_name("gug"), "Avañe’ẽ");
        assert_eq!(display_name("es_419"), "Español (Latinoamérica)");
        assert_eq!(display_name("xx_YY"), "xx_YY");
    }

    #[test]
    fn codes_split_into_language_and_region() {
        assert_eq!(language_of("de_CH_frami"), "de");
        assert_eq!(language_of("kmr_Latn"), "ku");
        assert_eq!(region_of("de_CH_frami"), Some("CH"));
        assert_eq!(region_of("es_419"), Some("419"));
        assert_eq!(region_of("ca_valencia"), None);
        assert_eq!(main_region("en"), Some("US"));
        assert_eq!(main_region("eo"), None);
    }
}
