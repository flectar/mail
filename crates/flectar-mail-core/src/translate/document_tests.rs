use super::*;

fn texts(pieces: &[Piece]) -> Vec<&str> {
    pieces.iter().map(|piece| piece.text.as_str()).collect()
}

/// Translates by upper-casing, which keeps tags as they are.
fn shout(pieces: &[Piece]) -> Vec<String> {
    pieces
        .iter()
        .map(|piece| {
            let mut out = String::new();
            let mut in_tag = false;
            for c in piece.text.chars() {
                match c {
                    '<' => in_tag = true,
                    '>' => in_tag = false,
                    _ => {}
                }
                if in_tag || c == '>' {
                    out.push(c);
                } else {
                    out.extend(c.to_uppercase());
                }
            }
            out
        })
        .collect()
}

#[test]
fn writes_back_what_it_reads() {
    let html = "<html dir=\"ltr\"><head><style>p > a { color: red }</style><title>A &amp; B</title></head>\
        <body class=\"x\"><p>One &amp; <b>two</b> &lt;three&gt;&nbsp;four<br>five</p>\
        <img src=\"cid:a\" alt=\"Logo &quot;big&quot;\"><table><tr><td>Cell</td></tr></table></body></html>";
    let document = Document::parse(html).unwrap();
    assert_eq!(write(&document.tree), html);
}

#[test]
fn a_paragraph_is_one_piece_with_its_links_as_tags() {
    let html = "<p>Read <a href=\"https://example.com/\">the <b>whole</b> story</a> today.</p>";
    let pieces = Document::parse(html).unwrap().pieces(true);
    assert_eq!(
        texts(&pieces),
        ["Read <g1>the <g2>whole</g2> story</g1> today."]
    );
    assert!(pieces[0].tagged);
}

#[test]
fn blocks_and_the_text_between_them_are_pieces_of_their_own() {
    let html =
        "<div>Hello <i>there</i><p>Inside</p>after <br> it<ul><li>One</li><li>Two</li></ul></div>";
    let pieces = Document::parse(html).unwrap().pieces(true);
    assert_eq!(
        texts(&pieces),
        [
            "Hello <g1>there</g1>",
            "Inside",
            "after <x1/> it",
            "One",
            "Two"
        ]
    );
}

#[test]
fn code_hidden_parts_and_notranslate_stay_out() {
    let html = "<head><title>Subject</title><style>.a{}</style></head><body>\
        <p>Run <code>cargo build</code> now</p><pre><code>fn main() {}</code></pre>\
        <div class=\"notranslate\">Brand Name</div><p>12:30 - 14:00</p></body>";
    let pieces = Document::parse(html).unwrap().pieces(true);
    assert_eq!(texts(&pieces), ["Run <x1/> now"]);
}

#[test]
fn image_descriptions_and_link_titles_are_pieces() {
    let html = "<p><a href=\"https://example.com/\" title=\"Open the shop\"><img src=\"cid:x\" alt=\"Summer sale\"></a> Shop now</p>\
        <img src=\"cid:y\" alt=\"\"><img src=\"cid:z\" alt=\"2024\">";
    let pieces = Document::parse(html).unwrap().pieces(true);
    assert_eq!(
        texts(&pieces),
        ["Open the shop", "Summer sale", "<g1><x2/></g1> Shop now"]
    );
}

#[test]
fn the_translation_keeps_the_original_elements_and_attributes() {
    let html = "<p style=\"color:red\">Read <a href=\"https://example.com/\" class=\"btn\">the <b>whole</b> story</a>.</p>";
    let document = Document::parse(html).unwrap();
    let pieces = document.pieces(true);
    // A translation that moves the link to the front, as German might.
    let translations = vec!["<g1>Die <g2>ganze</g2> Geschichte</g1> lesen.".to_owned()];
    assert!(document.misfits(&pieces, &translations).is_empty());
    assert_eq!(
        document.translated(&pieces, &translations, "ltr"),
        "<p style=\"color:red\"><a href=\"https://example.com/\" class=\"btn\">Die <b>ganze</b> Geschichte</a> lesen.</p>"
    );
}

#[test]
fn markup_in_a_translation_is_only_text() {
    let html = "<p>Hello <b>friend</b></p>";
    let document = Document::parse(html).unwrap();
    let pieces = document.pieces(true);
    let translations =
        vec!["Hallo <g1>Freund</g1><img src=x onerror=alert(1)> &lt;script&gt;".to_owned()];
    let out = document.translated(&pieces, &translations, "ltr");
    assert_eq!(
        out,
        "<p>Hallo <b>Freund</b>&lt;img src=x onerror=alert(1)&gt; &lt;script&gt;</p>"
    );
}

#[test]
fn tags_that_dont_match_fall_back_to_text_nodes() {
    let html = "<p>Read <a href=\"https://example.com/\">the story</a> today.</p><p>Plain</p>";
    let document = Document::parse(html).unwrap();
    let pieces = document.pieces(true);
    for wrong in [
        "Lies die Geschichte heute.",                   // tag dropped
        "Lies <g1>die</g1> <g1>Geschichte</g1> heute.", // tag twice
        "Lies <x1/> heute.",                            // wrong kind
        "Lies <g1>die Geschichte heute.",               // never closed
        "Lies <g2>die Geschichte</g2> heute.",          // unknown tag
    ] {
        let translations = vec![wrong.to_owned(), "Schlicht".to_owned()];
        assert_eq!(document.misfits(&pieces, &translations), [0], "{wrong}");
    }
    let fallback = document.fallback_pieces(&[0]);
    assert_eq!(texts(&fallback), ["Read", "the story", "today."]);
    let mut all = pieces.clone();
    all.extend(fallback);
    let translations: Vec<String> = [
        "Lies die Geschichte heute.",
        "Schlicht",
        "Lies",
        "die Geschichte",
        "heute.",
    ]
    .map(str::to_owned)
    .to_vec();
    assert_eq!(
        document.translated(&all, &translations, "ltr"),
        "<p>Lies <a href=\"https://example.com/\">die Geschichte</a> heute.</p><p>Schlicht</p>"
    );
}

#[test]
fn without_tags_every_text_node_is_a_piece() {
    let html = "<p>Read <a href=\"https://example.com/\">the story</a> today.</p>";
    let document = Document::parse(html).unwrap();
    let pieces = document.pieces(false);
    assert_eq!(texts(&pieces), ["Read", "the story", "today."]);
    let out = document.translated(&pieces, &shout(&pieces), "ltr");
    assert_eq!(
        out,
        "<p>READ <a href=\"https://example.com/\">THE STORY</a> TODAY.</p>"
    );
}

#[test]
fn spaces_around_a_run_survive() {
    let html = "<td> <span>Price</span>: <b>Free</b> </td><td>a</td>";
    let document = Document::parse(html).unwrap();
    let pieces = document.pieces(true);
    assert_eq!(texts(&pieces), ["<g1>Price</g1>: <g2>Free</g2>", "a"]);
    let out = document.translated(&pieces, &shout(&pieces), "ltr");
    assert_eq!(out, "<td> <span>PRICE</span>: <b>FREE</b> </td><td>A</td>");
}

#[test]
fn preformatted_text_keeps_its_lines() {
    let html = "<pre>First line\nsecond <a href=\"https://example.com/\">link</a>\n\nlast</pre>";
    let document = Document::parse(html).unwrap();
    let pieces = document.pieces(true);
    assert_eq!(
        texts(&pieces),
        ["First line<x1/>second <g2>link</g2><x3/><x4/>last"]
    );
    let out = document.translated(&pieces, &shout(&pieces), "ltr");
    assert_eq!(
        out,
        "<pre>FIRST LINE\nSECOND <a href=\"https://example.com/\">LINK</a>\n\nLAST</pre>"
    );
}

#[test]
fn descriptions_are_translated_in_place() {
    let html = "<p><img src=\"cid:x\" alt=\"Summer sale\"> Shop now</p>";
    let document = Document::parse(html).unwrap();
    let pieces = document.pieces(true);
    let out = document.translated(&pieces, &shout(&pieces), "ltr");
    assert_eq!(
        out,
        "<p><img src=\"cid:x\" alt=\"SUMMER SALE\"> SHOP NOW</p>"
    );
}

#[test]
fn entities_from_a_service_are_read_and_bare_ampersands_kept() {
    let html = "<p>Tom <b>and</b> Jerry</p>";
    let document = Document::parse(html).unwrap();
    let pieces = document.pieces(true);
    let translations = vec!["Tom &amp; <g1>&#x26;</g1> Jerry & Co &copy;".to_owned()];
    assert_eq!(
        document.translated(&pieces, &translations, "ltr"),
        "<p>Tom &amp; <b>&amp;</b> Jerry &amp; Co &amp;copy;</p>"
    );
}

#[test]
fn spaces_inside_tags_from_a_service_are_allowed() {
    let html = "<p>Hello <b>you</b><br>there</p>";
    let document = Document::parse(html).unwrap();
    let pieces = document.pieces(true);
    assert_eq!(texts(&pieces), ["Hello <g1>you</g1><x2/>there"]);
    let translations = vec!["Hallo <g1 >du</g1 ><x2 />da".to_owned()];
    assert!(document.misfits(&pieces, &translations).is_empty());
    assert_eq!(
        document.translated(&pieces, &translations, "ltr"),
        "<p>Hallo <b>du</b><br>da</p>"
    );
}

#[test]
fn right_to_left_translations_set_the_direction() {
    let html = "<html><head></head><body><p dir=\"ltr\">Hello</p><p>World</p></body></html>";
    let document = Document::parse(html).unwrap();
    let pieces = document.pieces(true);
    let translations = vec!["שלום".to_owned(), "עולם".to_owned()];
    assert_eq!(
        document.translated(&pieces, &translations, "rtl"),
        "<html><head></head><body dir=\"rtl\"><p dir=\"rtl\">שלום</p><p>עולם</p></body></html>"
    );
}

#[test]
fn an_empty_translation_leaves_the_text_as_it_was() {
    let html = "<p>Hello</p><p>World</p>";
    let document = Document::parse(html).unwrap();
    let pieces = document.pieces(true);
    let translations = vec![String::new(), "Welt".to_owned()];
    assert_eq!(
        document.translated(&pieces, &translations, "ltr"),
        "<p>Hello</p><p>Welt</p>"
    );
}

#[test]
fn deep_or_huge_documents_are_refused() {
    let deep = "<div>".repeat(MAX_DEPTH + 10);
    assert!(Document::parse(&deep).is_none());
    let huge = "<b>x</b>".repeat(MAX_NODES);
    assert!(Document::parse(&huge).is_none());
}

#[test]
fn stray_end_tags_and_unclosed_elements_are_tolerated() {
    let html = "</div><p>One <b>two</p><p>three</p>";
    let document = Document::parse(html).unwrap();
    let pieces = document.pieces(true);
    // The tokenizer doesn't fix nesting the way a browser does; the open
    // <b> holds the rest, and the text is still there to translate.
    assert!(!pieces.is_empty());
    let out = document.translated(&pieces, &shout(&pieces), "ltr");
    assert!(
        out.contains("ONE") && out.contains("TWO") && out.contains("THREE"),
        "{out}"
    );
}
