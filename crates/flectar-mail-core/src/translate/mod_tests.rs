use super::*;
use crate::accounts::credentials::DevelopmentFileCredentialStore;
use crate::config::Paths;
use crate::translate::test_server::{ok, server};
use serde_json::{Value, json};
use std::sync::Arc;

const GERMAN: &str = "Hallo Anna, vielen Dank für deine Nachricht. Ich habe den Bericht gestern Abend gelesen und finde, dass wir ihn morgen an den Kunden schicken können.";
const ENGLISH: &str = "Hi Anna, thanks a lot for your message. I read the report last night and I think we can send it to the client tomorrow morning.";

fn english_reader() -> ReaderLanguages {
    ReaderLanguages {
        reading: vec!["en-US".into(), "es-ES".into()],
        fallback_target: "en-US".into(),
    }
}

fn set_up(service: &str) -> TranslationSettings {
    TranslationSettings {
        service: service.into(),
        ..TranslationSettings::default()
    }
}

#[test]
fn the_bar_is_offered_for_languages_the_user_doesnt_read() {
    let languages = english_reader();
    let settings = set_up(DEEPL);
    let offer = offer_for(Some("de"), &settings, &languages);
    assert_eq!(
        offer,
        TranslationOffer {
            language: Some("de".into()),
            offer: true,
            automatic: false
        }
    );
    // English and Spanish are read.
    assert!(!offer_for(Some("en"), &settings, &languages).offer);
    assert!(!offer_for(Some("es"), &settings, &languages).offer);
    // Nothing found, nothing offered.
    assert_eq!(
        offer_for(None, &settings, &languages),
        TranslationOffer::default()
    );
    // No service, no bar: the language is still told.
    let none = offer_for(Some("de"), &TranslationSettings::default(), &languages);
    assert!(!none.offer && !none.automatic);
    assert_eq!(none.language.as_deref(), Some("de"));
    // The offer can be turned off.
    let quiet = TranslationSettings {
        offer: false,
        ..set_up(DEEPL)
    };
    assert!(!offer_for(Some("de"), &quiet, &languages).offer);
}

#[test]
fn always_and_never_decide_per_language() {
    let languages = english_reader();
    let always = TranslationSettings {
        always: vec!["de".into()],
        ..set_up(MODEL)
    };
    assert!(offer_for(Some("de"), &always, &languages).automatic);
    assert!(!offer_for(Some("fr"), &always, &languages).automatic);
    let never = TranslationSettings {
        never: vec!["de".into()],
        ..set_up(MODEL)
    };
    let offer = offer_for(Some("de"), &never, &languages);
    assert!(!offer.offer && !offer.automatic);
    assert!(offer_for(Some("fr"), &never, &languages).offer);
    // Norwegian's codes count as one language.
    let norwegian = TranslationSettings {
        never: vec!["no".into()],
        ..set_up(MODEL)
    };
    assert!(!offer_for(Some("nb"), &norwegian, &languages).offer);
}

#[test]
fn the_target_is_the_chosen_language_else_the_app_s() {
    let languages = english_reader();
    assert_eq!(wanted_target(&set_up(DEEPL), &languages), "en-US");
    let chosen = TranslationSettings {
        target: "pt-BR".into(),
        ..set_up(DEEPL)
    };
    assert_eq!(wanted_target(&chosen, &languages), "pt-BR");
    // A German speaker with a German target isn't offered German mail.
    let german = TranslationSettings {
        target: "de".into(),
        ..set_up(DEEPL)
    };
    assert!(!offer_for(Some("de"), &german, &languages).offer);
}

#[test]
fn each_service_gets_its_own_code_for_a_language() {
    let deepl = Service::deepl("k".into());
    let libre = Service::LibreTranslate {
        url: "https://lt.example".into(),
        key: String::new(),
    };
    let model = Service::Model {
        url: "http://localhost:11434/v1".into(),
        model: "m".into(),
        key: String::new(),
    };
    for (wanted, deepl_code, libre_code) in [
        ("en", "en-US", "en"),
        ("en-GB", "en-GB", "en"),
        ("en_AU", "en-GB", "en"),
        ("pt", "pt-BR", "pt"),
        ("pt-PT", "pt-PT", "pt"),
        ("pt-BR", "pt-BR", "pt-BR"),
        ("zh-CN", "zh-Hans", "zh-Hans"),
        ("zh_Hant", "zh-Hant", "zh-Hant"),
        ("zh-TW", "zh-Hant", "zh-Hant"),
        ("de-AT", "de", "de"),
        ("nn", "nb", "nb"),
    ] {
        assert_eq!(service_target(&deepl, wanted, &[]), deepl_code, "{wanted}");
        assert_eq!(service_target(&libre, wanted, &[]), libre_code, "{wanted}");
    }
    assert_eq!(service_target(&model, "pt-BR", &[]), "pt-BR");
    // The service's own list wins: a variant it has, else the language.
    let targets = [
        Target {
            code: "de".into(),
            name: "German".into(),
        },
        Target {
            code: "de-CH".into(),
            name: "German (Swiss)".into(),
        },
        Target {
            code: "es-419".into(),
            name: "Spanish (Latin American)".into(),
        },
    ];
    assert_eq!(service_target(&deepl, "de-ch", &targets), "de-CH");
    assert_eq!(service_target(&deepl, "de-AT", &targets), "de");
    assert_eq!(service_target(&deepl, "es-MX", &targets), "es-419");
}

/// A core with account 1 and a message, id returned, with these bodies.
async fn core_with_message(
    directory: &tempfile::TempDir,
    text: Option<&str>,
    html: Option<&str>,
) -> (Core, i64) {
    let credentials = Arc::new(DevelopmentFileCredentialStore::new(
        directory.path().join("credentials.json"),
    ));
    let core =
        Core::start_mail_ui_with_credentials(Paths::for_tests(directory.path()), credentials)
            .await
            .unwrap();
    let (text, html) = (text.map(str::to_owned), html.map(str::to_owned));
    let message_id = core
        .db
        .write(move |conn| {
            crate::db::testutil::seed_account(conn);
            let (_, message_id) =
                crate::db::testutil::seed_message(conn, "anna@example.de", "Bericht", false);
            conn.execute(
                "INSERT INTO message_bodies (message_id, text_body, html_body) VALUES (?1, ?2, ?3)",
                params![message_id, text, html],
            )?;
            Ok(message_id)
        })
        .await
        .unwrap();
    (core, message_id)
}

/// A model server that writes back every text in upper case.
async fn shouting_model() -> (
    String,
    Arc<std::sync::Mutex<Vec<crate::translate::test_server::Request>>>,
) {
    server(|_, request| {
        let body = request.json();
        let user: Value =
            serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        let texts: Vec<Value> = user["texts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| json!({ "id": item["id"], "text": shout(item["text"].as_str().unwrap()) }))
            .collect();
        ok(json!({ "choices": [{ "message": { "content": json!({ "texts": texts }).to_string() } }] }))
    })
    .await
}

/// Upper case outside tags, as a translation that keeps them.
fn shout(text: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in text.chars() {
        tag = (tag || c == '<') && c != '>';
        if tag || c == '>' {
            out.push(c);
        } else {
            out.extend(c.to_uppercase());
        }
    }
    out
}

/// Translation by the AI server at `url`, which the app's AI features share.
async fn use_model(core: &Core, url: &str) {
    core.set_ai_settings(crate::models::AiSettings {
        url: url.into(),
        model: "small".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    core.set_translation_settings(set_up(MODEL)).await.unwrap();
    core.set_ai_key("token".into()).await.unwrap();
}

#[tokio::test]
async fn a_message_is_translated_once_and_then_kept() {
    let directory = tempfile::tempdir().unwrap();
    let html = format!(
        "<html><head><title>x</title></head><body><p>{GERMAN} <a href=\"https://example.com/\">Bericht</a></p><img src=\"cid:logo\" alt=\"Unser Logo\"></body></html>"
    );
    let (core, message_id) = core_with_message(&directory, Some(GERMAN), Some(&html)).await;
    let (url, requests) = shouting_model().await;
    use_model(&core, &url).await;

    let offer = core
        .translation_offer(message_id, english_reader())
        .await
        .unwrap();
    assert_eq!(offer.language.as_deref(), Some("de"));
    assert!(offer.offer);

    let translation = core
        .translate_message(message_id, english_reader())
        .await
        .unwrap();
    let out = translation.html.unwrap();
    assert!(out.contains("HALLO ANNA"), "{out}");
    assert!(
        out.contains("<a href=\"https://example.com/\" rel=\"noopener noreferrer\">BERICHT</a>"),
        "{out}"
    );
    assert!(out.contains("alt=\"UNSER LOGO\""), "{out}");
    assert_eq!(translation.source.as_deref(), Some("de"));
    assert_eq!(translation.target, "en-US");
    assert!(!translation.right_to_left);
    let asked = requests.lock().unwrap().len();
    assert!(asked > 0);
    // The prompt names the language in English.
    let first = requests.lock().unwrap()[0].json();
    assert!(
        first["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("into American English")
    );

    // Opened again, it comes from the device.
    let again = core
        .translate_message(message_id, english_reader())
        .await
        .unwrap();
    assert_eq!(again.html.as_deref(), Some(out.as_str()));
    assert_eq!(requests.lock().unwrap().len(), asked);

    // It goes with its message.
    core.db
        .write(move |conn| {
            conn.execute("DELETE FROM messages WHERE id = ?1", params![message_id])?;
            Ok(())
        })
        .await
        .unwrap();
    let left: i64 = core
        .db
        .read(|conn| {
            Ok(
                conn.query_row("SELECT COUNT(*) FROM message_translations", [], |row| {
                    row.get(0)
                })?,
            )
        })
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[tokio::test]
async fn a_text_message_is_translated_as_text() {
    let directory = tempfile::tempdir().unwrap();
    let text = format!("{GERMAN}\n\n> Alte Nachricht\n-- \nAnna");
    let (core, message_id) = core_with_message(&directory, Some(&text), None).await;
    let (url, _) = shouting_model().await;
    use_model(&core, &url).await;
    let translation = core
        .translate_message(message_id, english_reader())
        .await
        .unwrap();
    assert_eq!(translation.html, None);
    assert_eq!(
        translation.text.as_deref(),
        Some(format!("{}\n\n> ALTE NACHRICHT\n-- \nANNA", GERMAN.to_uppercase()).as_str())
    );
}

#[tokio::test]
async fn mail_in_the_target_language_isn_t_sent() {
    let directory = tempfile::tempdir().unwrap();
    let (core, message_id) = core_with_message(&directory, Some(ENGLISH), None).await;
    let (url, requests) = shouting_model().await;
    use_model(&core, &url).await;
    let error = core
        .translate_message(message_id, english_reader())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        CoreError::Translation(TranslationError::SameLanguage)
    ));
    assert!(requests.lock().unwrap().is_empty());

    // A German document forwarded under an English note is still German.
    let forwarded = format!(
        "{ENGLISH}\n\n---------- Forwarded message ---------\n{GERMAN}\n{GERMAN}\n{GERMAN}"
    );
    let other = tempfile::tempdir().unwrap();
    let (core, message_id) = core_with_message(&other, Some(&forwarded), None).await;
    use_model(&core, &url).await;
    assert!(
        core.translate_message(message_id, english_reader())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn without_a_service_nothing_is_offered_or_sent() {
    let directory = tempfile::tempdir().unwrap();
    let (core, message_id) = core_with_message(&directory, Some(GERMAN), None).await;
    let offer = core
        .translation_offer(message_id, english_reader())
        .await
        .unwrap();
    assert!(!offer.offer);
    // A model needs the AI server's model as well as its address.
    core.set_ai_settings(crate::models::AiSettings {
        url: "http://localhost:1/v1".into(),
        ..Default::default()
    })
    .await
    .unwrap();
    for settings in [
        set_up(""),
        set_up(DEEPL),
        set_up(LIBRETRANSLATE),
        set_up(MODEL),
    ] {
        core.set_translation_settings(settings).await.unwrap();
        let error = core
            .translate_message(message_id, english_reader())
            .await
            .unwrap_err();
        assert!(
            matches!(error, CoreError::Translation(TranslationError::NotSetUp)),
            "{error:?}"
        );
    }
}

#[tokio::test]
async fn languages_go_on_one_list_at_most_and_keys_are_kept_per_service() {
    let directory = tempfile::tempdir().unwrap();
    let (core, _) = core_with_message(&directory, None, None).await;
    core.set_translation_settings(TranslationSettings {
        always: vec!["de-DE".into(), "de".into(), "fr".into()],
        never: vec!["FR".into(), "nn-NO".into()],
        ..set_up(DEEPL)
    })
    .await
    .unwrap();
    let saved = core.translation_settings().await.unwrap();
    assert_eq!(saved.always, ["de"]);
    assert_eq!(saved.never, ["fr", "nb"]);

    core.set_language_choice("fr".into(), LanguageChoice::Always)
        .await
        .unwrap();
    core.set_language_choice("de".into(), LanguageChoice::Ask)
        .await
        .unwrap();
    let saved = core.translation_settings().await.unwrap();
    assert_eq!(saved.always, ["fr"]);
    assert_eq!(saved.never, ["nb"]);

    assert!(!core.translation_key_saved(DEEPL.into()).await.unwrap());
    core.set_translation_key(DEEPL.into(), " key:fx ".into())
        .await
        .unwrap();
    core.set_translation_key(MODEL.into(), "token".into())
        .await
        .unwrap();
    assert!(core.translation_key_saved(DEEPL.into()).await.unwrap());
    core.set_translation_key(DEEPL.into(), String::new())
        .await
        .unwrap();
    assert!(!core.translation_key_saved(DEEPL.into()).await.unwrap());
    assert!(core.translation_key_saved(MODEL.into()).await.unwrap());
    assert!(
        core.set_translation_key("other".into(), "x".into())
            .await
            .is_err()
    );
}

#[test]
fn old_translations_are_pruned_when_one_is_kept() {
    let conn = crate::db::testutil::conn();
    crate::db::testutil::seed_account(&conn);
    let (_, old) = crate::db::testutil::seed_message(&conn, "a@example.com", "Old", false);
    let (_, new) = crate::db::testutil::seed_message(&conn, "b@example.com", "New", false);
    let now = 100 * KEPT_FOR_MS;
    keep(
        &conn,
        old,
        "en",
        "deepl",
        Some("de"),
        Some("<p>x</p>"),
        None,
        now - KEPT_FOR_MS - 1,
    )
    .unwrap();
    keep(&conn, new, "en", "deepl", None, None, Some("y"), now).unwrap();
    let left: Vec<i64> = conn
        .prepare("SELECT message_id FROM message_translations")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(left, [new]);
}
