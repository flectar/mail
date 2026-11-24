use super::{
    Record, Service, TYPE_MX, TYPE_SRV, TYPE_TXT, chosen, preferred, read_name, record_in_data,
    records_in_message,
};

fn name(host: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    for label in host.split('.').filter(|label| !label.is_empty()) {
        bytes.push(label.len() as u8);
        bytes.extend_from_slice(label.as_bytes());
    }
    bytes.push(0);
    bytes
}

/// A response to a question of `kind` for example.com, with answers of
/// that kind holding these data. The question's name sits at offset 12, so
/// answers can point back to it.
fn response(code: u8, kind: u16, answers: &[Vec<u8>]) -> Vec<u8> {
    let mut message = vec![0x12, 0x34, 0x81, 0x80 | code, 0, 1];
    message.extend_from_slice(&(answers.len() as u16).to_be_bytes());
    message.extend_from_slice(&[0, 0, 0, 0]);
    message.extend(name("example.com"));
    message.extend_from_slice(&kind.to_be_bytes());
    message.extend_from_slice(&[0, 1]);
    for data in answers {
        // The owner name points at the question's.
        message.extend_from_slice(&[0xc0, 12]);
        message.extend_from_slice(&kind.to_be_bytes());
        message.extend_from_slice(&[0, 1, 0, 0, 0x0e, 0x10]);
        message.extend_from_slice(&(data.len() as u16).to_be_bytes());
        message.extend_from_slice(data);
    }
    message
}

fn mx(preference: u16, exchange: Vec<u8>) -> Vec<u8> {
    let mut data = preference.to_be_bytes().to_vec();
    data.extend(exchange);
    data
}

fn srv(priority: u16, weight: u16, port: u16, target: &str) -> Vec<u8> {
    let mut data = Vec::new();
    for number in [priority, weight, port] {
        data.extend_from_slice(&number.to_be_bytes());
    }
    data.extend(name(target));
    data
}

fn service(priority: u16, weight: u16, target: &str) -> Service {
    Service {
        priority,
        weight,
        port: 443,
        target: target.into(),
    }
}

fn exchanges(records: Vec<Record>) -> Vec<(u16, String)> {
    records
        .into_iter()
        .map(|record| match record {
            Record::Mx { preference, host } => (preference, host),
            other => panic!("not MX: {other:?}"),
        })
        .collect()
}

#[test]
fn responses_give_every_mail_server_and_the_preferred_one_wins() {
    // "mx2" followed by a pointer to "example.com" in the question.
    let compressed = vec![3, b'm', b'x', b'2', 0xc0, 12];
    let message = response(
        0,
        TYPE_MX,
        &[mx(20, compressed), mx(10, name("ASPMX.L.Google.com"))],
    );
    let found = exchanges(records_in_message(&message, TYPE_MX).unwrap());
    assert_eq!(
        found,
        [
            (20, "mx2.example.com".into()),
            (10, "aspmx.l.google.com".into())
        ]
    );
    assert_eq!(preferred(found).as_deref(), Some("aspmx.l.google.com"));
}

#[test]
fn a_domain_that_takes_no_mail_or_answers_with_an_error_has_no_server() {
    // RFC 7505's null MX.
    let message = response(0, TYPE_MX, &[mx(0, vec![0])]);
    assert_eq!(
        preferred(exchanges(records_in_message(&message, TYPE_MX).unwrap())),
        None
    );
    // NXDOMAIN.
    assert_eq!(
        records_in_message(&response(3, TYPE_MX, &[]), TYPE_MX),
        None
    );
    assert_eq!(
        records_in_message(&response(0, TYPE_MX, &[]), TYPE_MX),
        Some(Vec::new())
    );
}

#[test]
fn damaged_messages_are_refused() {
    let message = response(0, TYPE_MX, &[mx(10, name("mx.example.com"))]);
    for cut in [0, 5, 11, 20, message.len() - 3] {
        assert_eq!(
            records_in_message(&message[..cut], TYPE_MX),
            None,
            "cut at {cut}"
        );
    }
    // A query, not a response.
    let mut query = message.clone();
    query[2] &= 0x7f;
    assert_eq!(records_in_message(&query, TYPE_MX), None);
    // A pointer to itself or forwards would loop.
    assert_eq!(read_name(&[0xc0, 0], 0), None);
    assert_eq!(read_name(&[1, b'a', 0xc0, 2], 0), None);
    assert_eq!(read_name(&[1, b'a', 0xc0, 5, 0, 0], 0), None);
    // Non-ASCII labels aren't host names.
    assert_eq!(read_name(&[2, 0xc3, 0xa9, 0], 0), None);
}

#[test]
fn only_answers_of_the_kind_asked_for_count() {
    // A CNAME answer beside the SRV one, as resolvers send when the name
    // is an alias.
    let mut message = response(0, TYPE_SRV, &[srv(0, 1, 443, "dav.example.com")]);
    message[7] = 2;
    message.extend_from_slice(&[0xc0, 12, 0, 5, 0, 1, 0, 0, 0x0e, 0x10]);
    let alias = name("other.example.com");
    message.extend_from_slice(&(alias.len() as u16).to_be_bytes());
    message.extend(alias);
    assert_eq!(
        records_in_message(&message, TYPE_SRV),
        Some(vec![Record::Srv(Service {
            priority: 0,
            weight: 1,
            port: 443,
            target: "dav.example.com".into(),
        })])
    );
    // A record whose data doesn't parse is skipped.
    let message = response(0, TYPE_SRV, &[vec![0, 1, 0]]);
    assert_eq!(records_in_message(&message, TYPE_SRV), Some(Vec::new()));
}

#[test]
fn text_records_keep_each_string() {
    let data = [b"\x0apath=/dav/".as_slice(), b"\x00", b"\x03a=b"].concat();
    let message = response(0, TYPE_TXT, &[data]);
    assert_eq!(
        records_in_message(&message, TYPE_TXT),
        Some(vec![Record::Txt(vec![
            b"path=/dav/".to_vec(),
            Vec::new(),
            b"a=b".to_vec()
        ])])
    );
    // A string longer than the record is refused.
    assert_eq!(record_in_data(TYPE_TXT, b"\x05abc"), None);
}

#[test]
fn record_data_alone_gives_one_record() {
    let data = mx(5, name("mx.example.org"));
    assert_eq!(
        record_in_data(TYPE_MX, &data),
        Some(Record::Mx {
            preference: 5,
            host: "mx.example.org".into()
        })
    );
    let mut longer = data.clone();
    longer.push(0);
    assert_eq!(record_in_data(TYPE_MX, &longer), None, "trailing bytes");
    assert_eq!(record_in_data(TYPE_MX, &[0]), None);
    assert_eq!(
        record_in_data(TYPE_SRV, &srv(10, 5, 8443, "DAV.Example.org")),
        Some(Record::Srv(Service {
            priority: 10,
            weight: 5,
            port: 8443,
            target: "dav.example.org".into(),
        }))
    );
}

#[test]
fn the_lowest_priority_wins_and_weight_shares_out_the_rest() {
    let services = vec![
        service(10, 0, "backup.example.com"),
        service(0, 1, "light.example.com"),
        service(0, 3, "heavy.example.com"),
    ];
    // Weights 1 and 3 share 0..=4: 0 and 1 land on the first, 2 to 4 on
    // the second.
    let pick = |random| chosen(services.clone(), random).unwrap().target;
    assert_eq!(pick(0), "light.example.com");
    assert_eq!(pick(1), "light.example.com");
    assert_eq!(pick(2), "heavy.example.com");
    assert_eq!(pick(4), "heavy.example.com");
    assert_eq!(pick(5), "light.example.com", "wraps around the total");
    // With no weights, the first is used.
    let unweighted = vec![
        service(1, 0, "a.example.com"),
        service(1, 0, "b.example.com"),
    ];
    assert_eq!(chosen(unweighted, 7).unwrap().target, "a.example.com");
}

#[test]
fn a_root_target_means_the_service_isnt_offered() {
    assert_eq!(chosen(vec![service(0, 0, "")], 0), None);
    assert_eq!(chosen(Vec::new(), 0), None);
    // Beside a real server, a root target is just skipped.
    assert_eq!(
        chosen(vec![service(0, 0, ""), service(5, 0, "dav.example.com")], 0)
            .unwrap()
            .target,
        "dav.example.com"
    );
}
