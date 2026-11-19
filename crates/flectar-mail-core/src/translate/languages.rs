//! Language names in English, for asking a model to translate and for the
//! languages a model can translate into (#240). The app shows names in the
//! user's own language.

use super::detect;

/// Languages whatlang doesn't know that services translate, with their
/// English names.
const MORE: &[(&str, &str)] = &[
    ("bs", "Bosnian"),
    ("eu", "Basque"),
    ("ga", "Irish"),
    ("gl", "Galician"),
    ("ha", "Hausa"),
    ("ht", "Haitian Creole"),
    ("ig", "Igbo"),
    ("is", "Icelandic"),
    ("kk", "Kazakh"),
    ("ky", "Kyrgyz"),
    ("lb", "Luxembourgish"),
    ("mg", "Malagasy"),
    ("mi", "Maori"),
    ("mn", "Mongolian"),
    ("ms", "Malay"),
    ("mt", "Maltese"),
    ("ps", "Pashto"),
    ("so", "Somali"),
    ("sq", "Albanian"),
    ("sw", "Swahili"),
    ("tg", "Tajik"),
    ("xh", "Xhosa"),
    ("yo", "Yoruba"),
];

/// The English name of a language tag's primary language.
fn primary_name(primary: &str) -> Option<String> {
    let fixed = match primary {
        // whatlang names the Norwegian it knows "Bokmal", and Chinese by
        // its spoken Mandarin.
        "nb" => Some("Norwegian"),
        "zh" => Some("Chinese"),
        "fa" => Some("Persian"),
        _ => None,
    };
    if let Some(name) = fixed {
        return Some(name.to_owned());
    }
    whatlang::Lang::all()
        .iter()
        .find(|lang| detect::tag_of(**lang) == Some(primary))
        .map(|lang| lang.eng_name().to_owned())
        .or_else(|| {
            MORE.iter()
                .find(|(code, _)| *code == primary)
                .map(|(_, name)| (*name).to_owned())
        })
}

/// The English name of a language tag, with its variant where it has one
/// a model knows, as in "Brazilian Portuguese".
pub(crate) fn english_name(tag: &str) -> String {
    let lower = tag.trim().replace('_', "-").to_ascii_lowercase();
    let variant = match lower.as_str() {
        "en-gb" => Some("British English"),
        "en-us" => Some("American English"),
        "pt-br" => Some("Brazilian Portuguese"),
        "pt-pt" => Some("European Portuguese"),
        "es-419" => Some("Latin American Spanish"),
        "fr-ca" => Some("Canadian French"),
        "de-ch" => Some("Swiss Standard German"),
        tag if tag.starts_with("zh-hant") || tag == "zh-tw" || tag == "zh-hk" => {
            Some("Traditional Chinese")
        }
        tag if tag.starts_with("zh-hans") || tag == "zh-cn" => Some("Simplified Chinese"),
        _ => None,
    };
    if let Some(name) = variant {
        return name.to_owned();
    }
    primary_name(&detect::primary(tag)).unwrap_or_else(|| tag.trim().to_owned())
}

/// The languages a model is offered to translate into: every language the
/// detector knows and the others above, by English name.
pub(crate) fn model_targets() -> Vec<(String, String)> {
    let mut targets: Vec<(String, String)> = whatlang::Lang::all()
        .iter()
        .filter_map(|lang| detect::tag_of(*lang))
        // Latin and Esperanto mail is rare, and a model's grasp of them less sure.
        .filter(|tag| !matches!(*tag, "la" | "eo" | "zh"))
        .map(|tag| (tag.to_owned(), english_name(tag)))
        .chain(
            MORE.iter()
                .map(|(code, name)| ((*code).to_owned(), (*name).to_owned())),
        )
        .chain([
            ("zh-Hans".to_owned(), english_name("zh-Hans")),
            ("zh-Hant".to_owned(), english_name("zh-Hant")),
        ])
        .collect();
    targets.sort_by(|a, b| a.1.cmp(&b.1));
    targets.dedup_by(|a, b| a.0 == b.0);
    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_english_with_their_variant() {
        assert_eq!(english_name("de"), "German");
        assert_eq!(english_name("pt-BR"), "Brazilian Portuguese");
        assert_eq!(english_name("zh_Hant_TW"), "Traditional Chinese");
        assert_eq!(english_name("nn-NO"), "Norwegian");
        assert_eq!(english_name("eu"), "Basque");
        assert_eq!(english_name("fa-IR"), "Persian");
        assert_eq!(english_name("qq"), "qq");
    }

    #[test]
    fn a_model_is_offered_every_language_once() {
        let targets = model_targets();
        let codes: Vec<&str> = targets.iter().map(|(code, _)| code.as_str()).collect();
        assert!(codes.contains(&"de") && codes.contains(&"zh-Hant") && codes.contains(&"sw"));
        assert!(!codes.contains(&"zh"));
        let mut unique = codes.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), codes.len());
    }
}
