use super::*;

const GERMAN: &str = "Hallo Anna, vielen Dank für deine Nachricht. Ich habe den Bericht gestern Abend gelesen und finde, dass wir ihn morgen an den Kunden schicken können.";
const ENGLISH: &str = "Hi Anna, thanks a lot for your message. I read the report last night and I think we can send it to the client tomorrow morning.";
const FRENCH: &str = "Bonjour à tous, la réunion de demain est déplacée à quinze heures dans la grande salle du deuxième étage, merci de prévenir vos équipes.";

#[test]
fn a_reply_is_in_the_language_of_its_own_words_not_its_quote() {
    let long_quote = format!("> {ENGLISH}\n").repeat(6);
    let text = format!(
        "{GERMAN}\n\nOn Tue, 3 Nov 2026 at 10:00, Anna <anna@example.com> wrote:\n{long_quote}"
    );
    assert_eq!(language(Some(&text), None), Some("de"));
    let quoted_first = format!("{long_quote}\n{GERMAN}");
    assert_eq!(language(Some(&quoted_first), None), Some("de"));
}

#[test]
fn quotes_in_html_are_left_out() {
    let html = format!(
        "<html><head><title>{ENGLISH}</title></head><body><div dir=\"ltr\">{GERMAN}</div>\
         <div class=\"gmail_quote\"><blockquote>{ENGLISH} {ENGLISH} {ENGLISH}</blockquote></div></body></html>"
    );
    assert_eq!(language(None, Some(&html)), Some("de"));
    let outlook = format!(
        "<div>{FRENCH}</div><div id=\"divRplyFwdMsg\"><b>From:</b> Anna</div><div>{ENGLISH} {ENGLISH} {ENGLISH}</div>"
    );
    assert_eq!(language(None, Some(&outlook)), Some("fr"));
}

#[test]
fn links_addresses_and_signatures_dont_count() {
    let text = format!(
        "{FRENCH}\nhttps://example.com/the/english/words/in/this/link/should/not/count/at/all\nanna.smith@example.com\n-- \nAnna Smith\nHead of Sales, Example Company Limited, London"
    );
    assert_eq!(language(Some(&text), None), Some("fr"));
}

#[test]
fn outlook_header_blocks_end_the_sender_s_words() {
    let text = format!(
        "{GERMAN}\n\nFrom: Anna Smith\nSent: Tuesday, November 3, 2026 10:00\nTo: Ben\nSubject: Report\n\n{ENGLISH} {ENGLISH} {ENGLISH}"
    );
    assert_eq!(language(Some(&text), None), Some("de"));
}

#[test]
fn short_or_uncertain_text_gets_no_language() {
    assert_eq!(language(Some("Thanks!"), None), None);
    assert_eq!(language(Some("ok 👍"), None), None);
    assert_eq!(language(Some(""), Some("<p>Danke</p>")), None);
    assert_eq!(language(None, None), None);
}

#[test]
fn the_text_part_comes_first_and_html_fills_in() {
    assert_eq!(
        language(Some(ENGLISH), Some(&format!("<p>{GERMAN}</p>"))),
        Some("en")
    );
    assert_eq!(
        language(Some("   "), Some(&format!("<p>{GERMAN}</p>"))),
        Some("de")
    );
}

#[test]
fn long_messages_are_sampled() {
    let text = format!("{FRENCH} ").repeat(200);
    assert!(text.len() > SAMPLE);
    assert_eq!(language(Some(&text), None), Some("fr"));
}

#[test]
fn other_scripts_are_found() {
    assert_eq!(
        language(
            Some(
                "Привет! Спасибо за письмо, я прочитал отчёт вчера вечером и думаю, что его можно отправить клиенту."
            ),
            None
        ),
        Some("ru")
    );
    assert_eq!(
        language(
            Some(
                "こんにちは。昨日の夜に報告書を読みました。明日の朝にお客様に送っても大丈夫だと思います。"
            ),
            None
        ),
        Some("ja")
    );
}

#[test]
fn primary_tags_are_compared_as_one_language() {
    assert_eq!(primary("nn-NO"), "nb");
    assert_eq!(primary("no"), "nb");
    assert_eq!(primary("pt_BR.UTF-8"), "pt");
    assert_eq!(primary("zh-Hant-TW"), "zh");
    assert_eq!(primary("iw"), "he");
    assert!(right_to_left("ar-EG"));
    assert!(!right_to_left("en"));
}
