use super::*;
use crate::translate::Slot;
use crate::translate::test_server::{ok, server};

fn pieces(texts: &[&str]) -> Vec<Piece> {
    texts
        .iter()
        .enumerate()
        .map(|(index, text)| Piece {
            text: (*text).to_owned(),
            tagged: text.contains('<'),
            slot: Slot::Run(index),
        })
        .collect()
}

#[test]
fn free_keys_go_to_the_free_api() {
    let Service::DeepL { base, .. } = Service::deepl("abc:fx".into()) else {
        unreachable!()
    };
    assert_eq!(base, "https://api-free.deepl.com");
    let Service::DeepL { base, .. } = Service::deepl("abc".into()) else {
        unreachable!()
    };
    assert_eq!(base, "https://api.deepl.com");
}

#[tokio::test]
async fn deepl_gets_the_key_in_its_header_the_subject_as_context_and_tags_as_xml() {
    let (origin, requests) = server(|_, request| {
        let texts = request.json()["text"].as_array().unwrap().clone();
        ok(json!({ "translations": texts.iter().map(|text| json!({
            "detected_source_language": "DE",
            "text": text.as_str().unwrap().to_uppercase(),
        })).collect::<Vec<_>>() }))
    })
    .await;
    let service = Service::DeepL {
        key: "secret".into(),
        base: origin,
    };
    let many: Vec<String> = (0..60).map(|n| format!("Satz <g1>{n}</g1>")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let out = service
        .translate(&pieces(&many), "en-GB", "English", "Rechnung")
        .await
        .unwrap();
    assert_eq!(out.texts[59], "SATZ <G1>59</G1>");
    assert_eq!(out.source.as_deref(), Some("de"));
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2, "50 texts a request at most");
    let first = &requests[0];
    assert_eq!(first.method, "POST");
    assert_eq!(first.target, "/v2/translate");
    assert_eq!(first.headers["authorization"], "DeepL-Auth-Key secret");
    let body = first.json();
    assert_eq!(body["text"].as_array().unwrap().len(), 50);
    assert_eq!(body["target_lang"], "en-GB");
    assert_eq!(body["tag_handling"], "xml");
    assert_eq!(body["tag_handling_version"], "v2");
    assert_eq!(body["context"], "Rechnung");
    assert!(body.get("source_lang").is_none());
}

#[tokio::test]
async fn deepl_errors_say_what_happened() {
    for (status, body, error) in [
        (
            403,
            json!({"message": "Forbidden"}),
            TranslationError::KeyRefused,
        ),
        (
            456,
            json!({"message": "Quota exceeded"}),
            TranslationError::QuotaUsed,
        ),
        (
            400,
            json!({"message": "Value for 'target_lang' not supported."}),
            TranslationError::Service("Value for 'target_lang' not supported.".into()),
        ),
        (
            400,
            json!({"error": {"message": "Bad request"}}),
            TranslationError::Service("Bad request".into()),
        ),
    ] {
        let (origin, _) = server(move |_, _| (status, Vec::new(), body.to_string())).await;
        let service = Service::DeepL {
            key: "k".into(),
            base: origin,
        };
        let got = service
            .translate(&pieces(&["Hallo"]), "en", "English", "")
            .await;
        assert_eq!(got.unwrap_err(), error);
    }
}

#[tokio::test]
async fn busy_services_are_asked_again() {
    let (origin, requests) = server(|n, _| match n {
        0 => (429, vec![("Retry-After", "0".to_owned())], "{}".to_owned()),
        1 => (503, vec![("Retry-After", "0".to_owned())], "{}".to_owned()),
        _ => ok(json!({ "translations": [{ "text": "Hello" }] })),
    })
    .await;
    let service = Service::DeepL {
        key: "k".into(),
        base: origin,
    };
    let out = service
        .translate(&pieces(&["Hallo"]), "en", "English", "")
        .await
        .unwrap();
    assert_eq!(out.texts, ["Hello"]);
    assert_eq!(requests.lock().unwrap().len(), 3);

    let (origin, requests) =
        server(|_, _| (429, vec![("Retry-After", "0".to_owned())], "{}".to_owned())).await;
    let service = Service::DeepL {
        key: "k".into(),
        base: origin,
    };
    let got = service
        .translate(&pieces(&["Hallo"]), "en", "English", "")
        .await;
    assert_eq!(got.unwrap_err(), TranslationError::TooMany);
    assert_eq!(requests.lock().unwrap().len(), 1 + RETRIES as usize);
}

#[tokio::test]
async fn deepl_languages_come_from_the_v3_list_or_else_v2() {
    let (origin, _) = server(|_, request| {
        assert_eq!(request.target, "/v3/languages?resource=translate_text");
        ok(json!([
            { "lang": "de", "name": "German", "usable_as_source": true, "usable_as_target": true },
            { "lang": "en-GB", "name": "English (British)", "usable_as_target": true },
            { "lang": "xx", "name": "Source only", "usable_as_target": false },
        ]))
    })
    .await;
    let service = Service::DeepL {
        key: "k".into(),
        base: origin,
    };
    let targets = service.targets().await.unwrap();
    let codes: Vec<&str> = targets.iter().map(|target| target.code.as_str()).collect();
    assert_eq!(codes, ["de", "en-GB"]);

    let (origin, _) = server(|_, request| {
        if request.target.starts_with("/v3/") {
            (404, Vec::new(), "{}".to_owned())
        } else {
            ok(json!([{ "language": "PT-BR", "name": "Portuguese (Brazilian)" }]))
        }
    })
    .await;
    let service = Service::DeepL {
        key: "k".into(),
        base: origin,
    };
    assert_eq!(service.targets().await.unwrap()[0].code, "PT-BR");
}

#[tokio::test]
async fn libretranslate_gets_plain_text_with_the_key_in_the_body() {
    let (origin, requests) = server(|_, request| {
        if request.target == "/frontend/settings" {
            return ok(json!({ "charLimit": 30, "keyRequired": true }));
        }
        let q = request.json()["q"].as_array().unwrap().clone();
        ok(json!({
            "translatedText": q.iter().map(|text| text.as_str().unwrap().to_uppercase()).collect::<Vec<_>>(),
            "detectedLanguage": q.iter().map(|_| json!({"confidence": 90, "language": "fr"})).collect::<Vec<_>>(),
        }))
    })
    .await;
    let service = Service::LibreTranslate {
        url: origin,
        key: "abc".into(),
    };
    assert!(!service.keeps_tags());
    let long = "Une phrase assez longue. Et une deuxième phrase ici.";
    let out = service
        .translate(&pieces(&["Tom &amp; Jerry", long]), "en", "English", "")
        .await
        .unwrap();
    assert_eq!(
        out.texts,
        [
            "TOM &amp; JERRY",
            "UNE PHRASE ASSEZ LONGUE. ET UNE DEUXIÈME PHRASE ICI."
        ]
    );
    assert_eq!(out.source.as_deref(), Some("fr"));
    let requests = requests.lock().unwrap();
    let body = requests[1].json();
    assert_eq!(body["source"], "auto");
    assert_eq!(body["target"], "en");
    assert_eq!(body["format"], "text");
    assert_eq!(body["api_key"], "abc");
    assert_eq!(
        body["q"],
        json!([
            "Tom & Jerry",
            "Une phrase assez longue.",
            "Et une deuxième phrase ici."
        ])
    );
}

#[tokio::test]
async fn libretranslate_asking_for_a_key_is_a_refused_key() {
    let (origin, _) = server(|_, request| {
        if request.target == "/translate" {
            (
                400,
                Vec::new(),
                json!({"error": "Visit https://portal.libretranslate.com to get an API key"})
                    .to_string(),
            )
        } else {
            (404, Vec::new(), "{}".to_owned())
        }
    })
    .await;
    let service = Service::LibreTranslate {
        url: origin,
        key: String::new(),
    };
    let got = service
        .translate(&pieces(&["Bonjour"]), "en", "English", "")
        .await;
    assert_eq!(got.unwrap_err(), TranslationError::KeyRefused);
}

#[tokio::test]
async fn a_model_gets_json_with_ids_and_its_missing_answers_are_asked_again() {
    let (origin, requests) = server(|n, request| {
        let body = request.json();
        let user: Value =
            serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        let texts = user["texts"].as_array().unwrap();
        // The first answer leaves out the second text, and thinks aloud first.
        let answer: Vec<Value> = texts
            .iter()
            .filter(|item| n > 0 || item["id"] != 2)
            .map(|item| json!({ "id": item["id"], "text": item["text"].as_str().unwrap().to_uppercase() }))
            .collect();
        let content = format!("<think>easy</think>{}", json!({ "texts": answer }));
        ok(json!({ "choices": [{ "message": { "content": content } }] }))
    })
    .await;
    let service = Service::Model {
        url: origin,
        model: "small".into(),
        key: "token".into(),
    };
    let out = service
        .translate(
            &pieces(&["Hallo <g1>du</g1>", "Zwei", "Drei"]),
            "en",
            "English",
            "Betreff",
        )
        .await
        .unwrap();
    assert_eq!(out.texts, ["HALLO <G1>DU</G1>", "ZWEI", "DREI"]);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].target, "/chat/completions");
    assert_eq!(requests[0].headers["authorization"], "Bearer token");
    let first = requests[0].json();
    assert_eq!(first["model"], "small");
    assert_eq!(first["response_format"]["type"], "json_object");
    assert!(
        first["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("into English")
    );
    let user: Value =
        serde_json::from_str(first["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(user["subject"], "Betreff");
    // Only the missing text is asked for again.
    let again: Value = serde_json::from_str(
        requests[1].json()["messages"][1]["content"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(again["texts"], json!([{ "id": 1, "text": "Zwei" }]));
}

#[tokio::test]
async fn a_model_server_without_json_mode_is_asked_without_it() {
    let (origin, requests) = server(|_, request| {
        let body = request.json();
        if body.get("response_format").is_some() {
            return (400, Vec::new(), json!({"error": {"message": "response_format is not supported"}}).to_string());
        }
        ok(json!({ "choices": [{ "message": { "content": "{\"texts\":[{\"id\":\"1\",\"text\":\"Hi\"}]}" } }] }))
    })
    .await;
    let service = Service::Model {
        url: origin,
        model: "m".into(),
        key: String::new(),
    };
    let out = service
        .translate(&pieces(&["Hallo"]), "en", "English", "")
        .await
        .unwrap();
    assert_eq!(out.texts, ["Hi"]);
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(!requests[1].headers.contains_key("authorization"));
}

#[test]
fn only_https_or_this_device_over_http() {
    assert!(checked_url("https://translate.example.com").is_ok());
    assert!(checked_url("http://localhost:5000").is_ok());
    assert!(checked_url("http://127.0.0.1:11434/v1").is_ok());
    assert!(checked_url("http://[::1]:1234/v1").is_ok());
    assert_eq!(
        checked_url("http://192.168.1.20:5000").unwrap_err(),
        TranslationError::BadAddress
    );
    assert_eq!(
        checked_url("https://user:pw@example.com").unwrap_err(),
        TranslationError::BadAddress
    );
    assert_eq!(
        checked_url("ftp://example.com").unwrap_err(),
        TranslationError::BadAddress
    );
    assert_eq!(
        checked_url("not a url").unwrap_err(),
        TranslationError::BadAddress
    );
}

#[test]
fn long_texts_are_cut_at_sentence_ends() {
    assert_eq!(
        cut(
            "First sentence here. Second one is longer than that. Third.",
            25
        ),
        [
            "First sentence here.",
            "Second one is longer",
            "than that. Third."
        ]
    );
    assert_eq!(cut("short", 25), ["short"]);
}

#[test]
fn too_long_a_piece_is_refused() {
    let big = "a".repeat(DEEPL_BYTES + 1);
    let all = pieces(&[&big]);
    assert_eq!(
        batches(&[0], &all, DEEPL_TEXTS, DEEPL_BYTES).unwrap_err(),
        TranslationError::TooLong
    );
}
