//! Languages named in the app's language (#240), from CLDR, as written in a
//! sentence: "This message is in German", "Translate into Spanish".

mod data {
    include!("names_data.rs");
}

/// The names in the app's language, else in English.
fn names_in(app_language: &str) -> &'static [(&'static str, &'static str)] {
    let wanted = app_language.replace('-', "_");
    data::NAMES
        .iter()
        .find(|(code, _)| code.eq_ignore_ascii_case(&wanted))
        .or_else(|| data::NAMES.iter().find(|(code, _)| *code == "en"))
        .map_or(&[], |(_, names)| names)
}

/// A language tag's name in the app's language, as written in a sentence.
/// A variant CLDR names, such as "pt-BR", has its own name, else it's named
/// as its language. `fallback` is for a tag CLDR has no name for, such as a
/// service's own name for it.
pub(crate) fn name(app_language: &str, tag: &str, fallback: &str) -> String {
    let names = names_in(app_language);
    let tag = tag.trim().replace('_', "-");
    let lookup = |wanted: &str| {
        names
            .iter()
            .find(|(code, _)| code.eq_ignore_ascii_case(wanted))
            .map(|(_, name)| (*name).to_owned())
    };
    let mut parts = tag.split('-');
    let primary = parts.next().unwrap_or_default().to_ascii_lowercase();
    let primary = match primary.as_str() {
        // Norwegian's codes are one language, named by its written standard.
        "no" | "nn" => "nb".to_owned(),
        "iw" => "he".to_owned(),
        "in" => "id".to_owned(),
        _ => primary,
    };
    // Chinese is named by its script: a Traditional region writes Hant.
    let chinese = |rest: &[&str]| {
        let traditional = rest.iter().any(|part| {
            ["hant", "tw", "hk", "mo"]
                .iter()
                .any(|value| part.eq_ignore_ascii_case(value))
        });
        if traditional { "zh-Hant" } else { "zh-Hans" }
    };
    let rest: Vec<&str> = parts.collect();
    let variant = if primary == "zh" && !rest.is_empty() {
        lookup(chinese(&rest))
    } else if rest.is_empty() {
        None
    } else {
        lookup(&tag)
    };
    variant.or_else(|| lookup(&primary)).unwrap_or_else(|| {
        if fallback.trim().is_empty() {
            tag.clone()
        } else {
            fallback.trim().to_owned()
        }
    })
}

/// A name as a menu or list shows it, capitalized, as CLDR's menus do.
pub(crate) fn menu_name(app_language: &str, tag: &str, fallback: &str) -> String {
    let name = name(app_language, tag, fallback);
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_in_the_app_s_language() {
        assert_eq!(name("en", "de", ""), "German");
        assert_eq!(name("es", "de", ""), "alemán");
        assert_eq!(menu_name("es", "de", ""), "Alemán");
        assert_eq!(name("de", "fr", ""), "Französisch");
        assert_eq!(name("zh_Hans", "en", ""), "英语");
    }

    #[test]
    fn variants_have_their_own_names_where_cldr_has_one() {
        assert_eq!(name("en", "pt-BR", ""), "Brazilian Portuguese");
        assert_eq!(name("en", "EN-GB", ""), "British English");
        assert_eq!(name("en", "zh-TW", ""), "Traditional Chinese");
        assert_eq!(name("en", "zh_Hans_CN", ""), "Simplified Chinese");
        // A variant without its own name is named as its language.
        assert_eq!(name("en", "de-AT", ""), "German");
        assert_eq!(name("en", "nn", ""), "Norwegian Bokmål");
    }

    #[test]
    fn unknown_languages_fall_back() {
        // An app language without names uses English ones.
        assert_eq!(name("xx", "de", ""), "German");
        assert_eq!(name("en", "qaa", "Service name"), "Service name");
        assert_eq!(name("en", "qaa", ""), "qaa");
    }
}
