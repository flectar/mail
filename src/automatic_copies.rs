//! Automatic Cc and Bcc (#252): the copies mail written from an account or
//! sender identity starts with. The composer adds them to the message's own
//! Cc and Bcc, where the writer sees them and can take them out, and swaps
//! them when another sender is chosen, as Thunderbird and Evolution do.

use crate::compose_controller::format_address;
use flectar_mail_core::models::{Address, AutomaticCopies};
use std::collections::HashSet;

/// The addresses the composer added to a message's Cc and Bcc, in
/// lowercase, so that choosing another sender takes away only those.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Added {
    cc: Vec<String>,
    bcc: Vec<String>,
}

/// The entries of an address field as typed: separated by commas,
/// semicolons or lines outside quoted names.
fn entries(field: &str) -> impl Iterator<Item = &str> {
    crate::mail::recipient_entries(field)
}

/// An entry's address in lowercase, from "Name <address>" or the address.
fn address_of(entry: &str) -> String {
    entry
        .rsplit_once('<')
        .and_then(|(_, rest)| rest.trim_end().strip_suffix('>'))
        .unwrap_or(entry)
        .trim()
        .to_lowercase()
}

fn key(address: &Address) -> String {
    address.email.trim().to_lowercase()
}

/// `field` without its entries for `addresses`. A field with none of them
/// stays as typed.
fn without(field: &str, addresses: &[String]) -> String {
    if !entries(field).any(|entry| addresses.contains(&address_of(entry))) {
        return field.to_owned();
    }
    entries(field)
        .filter(|entry| !addresses.contains(&address_of(entry)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// `field` with `address` after its entries.
fn with(field: String, address: &Address) -> String {
    let typed = field.trim_end().trim_end_matches([',', ';']).trim_end();
    if typed.is_empty() {
        format_address(address)
    } else {
        format!("{typed}, {}", format_address(address))
    }
}

/// A message's Cc and Bcc with `copies`, after taking away the ones `added`
/// for the sender before. An address the message is already to, or has a
/// copy for, isn't added again. Returns the Cc, the Bcc, and what was
/// added.
pub(crate) fn apply(
    to: &str,
    cc: &str,
    bcc: &str,
    added: &Added,
    copies: &AutomaticCopies,
) -> (String, String, Added) {
    let mut cc = without(cc, &added.cc);
    let mut bcc = without(bcc, &added.bcc);
    let mut present = entries(to)
        .chain(entries(&cc))
        .chain(entries(&bcc))
        .map(address_of)
        .collect::<HashSet<_>>();
    let mut now = Added::default();
    for address in &copies.cc {
        if present.insert(key(address)) {
            cc = with(cc, address);
            now.cc.push(key(address));
        }
    }
    for address in &copies.bcc {
        if present.insert(key(address)) {
            bcc = with(bcc, address);
            now.bcc.push(key(address));
        }
    }
    (cc, bcc, now)
}

/// The copies a saved message has already. A draft opens with the ones its
/// sender added before, and choosing another sender takes them away.
pub(crate) fn adopt(cc: &str, bcc: &str, copies: &AutomaticCopies) -> Added {
    let has = |field: &str, address: &Address| {
        entries(field).any(|entry| address_of(entry) == key(address))
    };
    Added {
        cc: copies
            .cc
            .iter()
            .filter(|address| has(cc, address))
            .map(key)
            .collect(),
        bcc: copies
            .bcc
            .iter()
            .filter(|address| has(bcc, address))
            .map(key)
            .collect(),
    }
}

/// The addresses Settings' Cc or Bcc field holds.
pub(crate) fn parse(field: &str) -> Result<Vec<Address>, String> {
    entries(field).map(crate::mail::parse_address).collect()
}

/// Addresses as Settings shows them.
pub(crate) fn text(addresses: &[Address]) -> String {
    addresses
        .iter()
        .map(format_address)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(name: Option<&str>, email: &str) -> Address {
        Address {
            name: name.map(str::to_owned),
            email: email.to_owned(),
        }
    }

    fn copies(cc: &[&str], bcc: &[&str]) -> AutomaticCopies {
        AutomaticCopies {
            cc: cc.iter().map(|email| address(None, email)).collect(),
            bcc: bcc.iter().map(|email| address(None, email)).collect(),
        }
    }

    #[test]
    fn copies_go_after_what_the_message_has() {
        let automatic = AutomaticCopies {
            cc: vec![address(Some("Boss"), "boss@example.com")],
            bcc: vec![address(None, "archive@example.com")],
        };
        let (cc, bcc, added) = apply("ann@example.com", "", "", &Added::default(), &automatic);
        assert_eq!(cc, "Boss <boss@example.com>");
        assert_eq!(bcc, "archive@example.com");

        // A reply to all keeps its copies, and the new ones follow.
        let (cc, bcc, _) = apply(
            "ann@example.com",
            "bob@example.com;\n",
            "",
            &Added::default(),
            &automatic,
        );
        assert_eq!(cc, "bob@example.com, Boss <boss@example.com>");
        assert_eq!(bcc, "archive@example.com");
        assert_eq!(added, adopt(&cc, &bcc, &automatic));
    }

    #[test]
    fn an_address_the_message_has_is_not_added_again() {
        let automatic = copies(
            &["boss@example.com"],
            &["archive@example.com", "ann@example.com"],
        );
        let (cc, bcc, added) = apply(
            "Ann <ANN@example.com>",
            "Boss <Boss@Example.com>",
            "",
            &Added::default(),
            &automatic,
        );
        assert_eq!(cc, "Boss <Boss@Example.com>");
        assert_eq!(bcc, "archive@example.com");
        // Only what was added goes when another sender is chosen.
        let (cc, bcc, _) = apply(
            "Ann <ANN@example.com>",
            &cc,
            &bcc,
            &added,
            &AutomaticCopies::default(),
        );
        assert_eq!((cc.as_str(), bcc.as_str()), ("Boss <Boss@Example.com>", ""));
    }

    #[test]
    fn another_sender_swaps_the_copies_and_keeps_typed_ones() {
        let first = copies(&["boss@example.com"], &["archive@example.com"]);
        let (cc, bcc, added) = apply("", "", "", &Added::default(), &first);
        let cc = format!("{cc}, carol@example.com");
        let second = copies(&[], &["ledger@example.com"]);
        let (cc, bcc, added) = apply("", &cc, &bcc, &added, &second);
        assert_eq!(cc, "carol@example.com");
        assert_eq!(bcc, "ledger@example.com");
        assert_eq!(added, adopt(&cc, &bcc, &second));

        // A field without the sender's copies stays as typed.
        let typed = "dave@example.com;\nerin@example.com";
        let (cc, _, _) = apply(
            "",
            typed,
            "",
            &Added::default(),
            &AutomaticCopies::default(),
        );
        assert_eq!(cc, typed);
    }

    #[test]
    fn a_draft_counts_the_copies_it_has() {
        let automatic = copies(&["boss@example.com"], &["archive@example.com"]);
        let added = adopt("Boss <boss@example.com>", "", &automatic);
        let (cc, bcc, _) = apply(
            "",
            "Boss <boss@example.com>",
            "",
            &added,
            &AutomaticCopies::default(),
        );
        assert_eq!((cc.as_str(), bcc.as_str()), ("", ""));
    }

    #[test]
    fn settings_fields_hold_addresses() {
        let addresses = parse("Boss <boss@example.com>; archive@example.com\n").unwrap();
        assert_eq!(
            addresses,
            [
                address(Some("Boss"), "boss@example.com"),
                address(None, "archive@example.com"),
            ]
        );
        assert_eq!(
            text(&addresses),
            "Boss <boss@example.com>, archive@example.com"
        );
        assert!(parse("not an address").is_err());
        assert_eq!(parse(" , ").unwrap(), []);
    }
}
