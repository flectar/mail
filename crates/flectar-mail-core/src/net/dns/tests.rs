use super::{exchange_in_record, exchanges_in_message, preferred, read_name};

fn name(host: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    for label in host.split('.').filter(|label| !label.is_empty()) {
        bytes.push(label.len() as u8);
        bytes.extend_from_slice(label.as_bytes());
    }
    bytes.push(0);
    bytes
}

/// A response to an MX question for example.com, with these answers. The
/// question's name sits at offset 12, so answers can point back to it.
fn response(code: u8, answers: &[(u16, Vec<u8>)]) -> Vec<u8> {
    let mut message = vec![0x12, 0x34, 0x81, 0x80 | code, 0, 1];
    message.extend_from_slice(&(answers.len() as u16).to_be_bytes());
    message.extend_from_slice(&[0, 0, 0, 0]);
    message.extend(name("example.com"));
    message.extend_from_slice(&[0, 15, 0, 1]);
    for (preference, exchange) in answers {
        // The owner name points at the question's.
        message.extend_from_slice(&[0xc0, 12, 0, 15, 0, 1, 0, 0, 0x0e, 0x10]);
        message.extend_from_slice(&((exchange.len() + 2) as u16).to_be_bytes());
        message.extend_from_slice(&preference.to_be_bytes());
        message.extend_from_slice(exchange);
    }
    message
}

#[test]
fn responses_give_every_mail_server_and_the_preferred_one_wins() {
    // "mx2" followed by a pointer to "example.com" in the question.
    let compressed = vec![3, b'm', b'x', b'2', 0xc0, 12];
    let message = response(0, &[(20, compressed), (10, name("ASPMX.L.Google.com"))]);
    let exchanges = exchanges_in_message(&message).unwrap();
    assert_eq!(
        exchanges,
        [
            (20, "mx2.example.com".into()),
            (10, "aspmx.l.google.com".into())
        ]
    );
    assert_eq!(preferred(exchanges).as_deref(), Some("aspmx.l.google.com"));
}

#[test]
fn a_domain_that_takes_no_mail_or_answers_with_an_error_has_no_server() {
    // RFC 7505's null MX.
    let message = response(0, &[(0, vec![0])]);
    assert_eq!(preferred(exchanges_in_message(&message).unwrap()), None);
    // NXDOMAIN.
    assert_eq!(exchanges_in_message(&response(3, &[])), None);
    assert_eq!(
        preferred(exchanges_in_message(&response(0, &[])).unwrap()),
        None
    );
}

#[test]
fn damaged_messages_are_refused() {
    let message = response(0, &[(10, name("mx.example.com"))]);
    for cut in [0, 5, 11, 20, message.len() - 3] {
        assert_eq!(exchanges_in_message(&message[..cut]), None, "cut at {cut}");
    }
    // A query, not a response.
    let mut query = message.clone();
    query[2] &= 0x7f;
    assert_eq!(exchanges_in_message(&query), None);
    // A pointer to itself or forwards would loop.
    assert_eq!(read_name(&[0xc0, 0], 0), None);
    assert_eq!(read_name(&[1, b'a', 0xc0, 2], 0), None);
    assert_eq!(read_name(&[1, b'a', 0xc0, 5, 0, 0], 0), None);
    // Non-ASCII labels aren't host names.
    assert_eq!(read_name(&[2, 0xc3, 0xa9, 0], 0), None);
}

#[test]
fn record_data_alone_gives_one_server() {
    let mut data = vec![0, 5];
    data.extend(name("mx.example.org"));
    assert_eq!(
        exchange_in_record(&data),
        Some((5, "mx.example.org".into()))
    );
    data.push(0);
    assert_eq!(exchange_in_record(&data), None, "trailing bytes");
    assert_eq!(exchange_in_record(&[0]), None);
}
