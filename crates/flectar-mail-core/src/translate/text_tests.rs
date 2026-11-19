use super::*;

fn texts(pieces: &[Piece]) -> Vec<&str> {
    pieces.iter().map(|piece| piece.text.as_str()).collect()
}

fn upper(pieces: &[Piece]) -> Vec<String> {
    pieces
        .iter()
        .map(|piece| piece.text.to_uppercase())
        .collect()
}

#[test]
fn wrapped_paragraphs_travel_whole() {
    let text = "Thanks for the update on the project. I read the whole report last\n\
        night and I think we are ready to send it to the client tomorrow.\n\
        \n\
        See you,\n\
        Ana";
    let document = TextDocument::parse(text);
    let pieces = document.pieces();
    assert_eq!(
        texts(&pieces),
        [
            "Thanks for the update on the project. I read the whole report last night and I think we are ready to send it to the client tomorrow.",
            "See you,",
            "Ana",
        ]
    );
    assert_eq!(
        document.translated(&pieces, &upper(&pieces)),
        "THANKS FOR THE UPDATE ON THE PROJECT. I READ THE WHOLE REPORT LAST NIGHT AND I THINK WE ARE READY TO SEND IT TO THE CLIENT TOMORROW.\n\nSEE YOU,\nANA"
    );
}

#[test]
fn quotes_lists_and_signatures_keep_their_shape() {
    let text = "Here is the list:\n\
        - milk\n\
        - 2 eggs\n\
        1. first\n\
        \n\
        > On the call we agreed to move the launch to the second week of\n\
        > March, after the review.\n\
        >> Older text\n\
        -- \n\
        Ana Pérez\n\
        +34 600 000 000";
    let document = TextDocument::parse(text);
    let pieces = document.pieces();
    assert_eq!(
        texts(&pieces),
        [
            "Here is the list:",
            "- milk",
            "- 2 eggs",
            "1. first",
            "On the call we agreed to move the launch to the second week of March, after the review.",
            "Older text",
            "Ana Pérez",
        ]
    );
    assert_eq!(
        document.translated(&pieces, &upper(&pieces)),
        "HERE IS THE LIST:\n- MILK\n- 2 EGGS\n1. FIRST\n\n> ON THE CALL WE AGREED TO MOVE THE LAUNCH TO THE SECOND WEEK OF MARCH, AFTER THE REVIEW.\n>> OLDER TEXT\n-- \nANA PÉREZ\n+34 600 000 000"
    );
}

#[test]
fn markup_characters_travel_escaped_and_come_back_as_text() {
    let document = TextDocument::parse("Use <b> & stop");
    let pieces = document.pieces();
    assert_eq!(texts(&pieces), ["Use &lt;b&gt; &amp; stop"]);
    assert_eq!(
        document.translated(&pieces, &["Nutze &lt;b&gt; &amp; halt".to_owned()]),
        "Nutze <b> & halt"
    );
}

#[test]
fn untranslated_pieces_and_crlf_lines_stay() {
    let document = TextDocument::parse("Hello\r\n   indented line\r\nBye");
    let pieces = document.pieces();
    assert_eq!(texts(&pieces), ["Hello", "indented line", "Bye"]);
    let translations = vec!["Hallo".to_owned(), "eingerückt".to_owned(), String::new()];
    assert_eq!(
        document.translated(&pieces, &translations),
        "Hallo\n   eingerückt\nBye"
    );
}
