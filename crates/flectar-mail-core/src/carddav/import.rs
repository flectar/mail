//! Contacts from vCard files brought from elsewhere: another address book,
//! an old phone, or the export of Google Contacts. Unlike what a CardDAV
//! server serves, such a file holds many cards, may be vCard 2.1, and its
//! cards need no UID or even a name. Each card becomes a vCard 3.0 or 4.0
//! document of the kind saved contacts keep, with every property it had.

use std::borrow::Cow;
use std::collections::HashSet;

use super::vcard::{self, Card};
use crate::models::ContactFileProblem;

/// Google Contacts keeps at most 25,000 contacts, of up to 128 KB each
/// without their photos.
pub const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_CARDS: usize = 25_000;

#[derive(Debug, Default)]
pub struct ImportedCards {
    pub cards: Vec<Card>,
    /// Cards with nothing to call the contact by, or that are malformed.
    pub unreadable: usize,
}

/// The cards of a vCard file, in their order.
pub fn read(bytes: &[u8]) -> Result<ImportedCards, ContactFileProblem> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(ContactFileProblem::TooLarge);
    }
    let text = decode(bytes);
    let cards = split(&text);
    if cards.len() > MAX_CARDS {
        return Err(ContactFileProblem::TooManyContacts);
    }
    let mut imported = ImportedCards::default();
    for lines in cards {
        match normalize(&lines) {
            Some(card) => imported.cards.push(card),
            None => imported.unreadable += 1,
        }
    }
    Ok(imported)
}

/// The file's text. vCard 4.0 is always UTF-8. Older files may be UTF-16
/// with a byte order mark, or in a legacy charset, most often Windows-1252.
fn decode(bytes: &[u8]) -> Cow<'_, str> {
    if let Some((encoding, bom)) = encoding_rs::Encoding::for_bom(bytes) {
        return Cow::Owned(
            encoding
                .decode_without_bom_handling(&bytes[bom..])
                .0
                .into_owned(),
        );
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Cow::Borrowed(text),
        Err(_) => {
            encoding_rs::WINDOWS_1252
                .decode_without_bom_handling(bytes)
                .0
        }
    }
}

/// The unfolded lines of each card, without its BEGIN and END. Cards nested
/// in another, as old AGENT properties hold them, stay out.
fn split(text: &str) -> Vec<Vec<String>> {
    let mut cards = Vec::new();
    let mut current = Vec::new();
    let mut depth = 0usize;
    for line in vcard::unfold(text) {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case("BEGIN:VCARD") {
            depth += 1;
            if depth == 1 {
                current.clear();
            }
        } else if trimmed.eq_ignore_ascii_case("END:VCARD") {
            if depth == 1 {
                cards.push(std::mem::take(&mut current));
            }
            depth = depth.saturating_sub(1);
        } else if depth == 1 && !trimmed.is_empty() {
            current.push(line.into_owned());
        }
    }
    cards
}

/// The UIDs and addresses of the cards read so far, to tell a card that
/// repeats an earlier one of the same file.
#[derive(Default)]
pub struct EarlierCards {
    uids: HashSet<String>,
    emails: HashSet<String>,
}

impl EarlierCards {
    pub fn repeats(&mut self, card: &Card) -> bool {
        let uid = vcard::uid(card.document());
        let emails = card
            .record()
            .details
            .emails
            .iter()
            .map(|email| email.value.trim().to_lowercase())
            .filter(|email| !email.is_empty())
            .collect::<Vec<_>>();
        let repeated = uid.as_ref().is_some_and(|uid| self.uids.contains(uid))
            || emails.iter().any(|email| self.emails.contains(email));
        self.uids.extend(uid);
        self.emails.extend(emails);
        repeated
    }
}

/// A card given a new UID, as a copy of a contact already saved needs: an
/// address book names each card by its UID.
pub fn with_new_uid(card: &Card) -> Option<Card> {
    let uid = vcard::new_uid();
    let lines = vcard::unfold(card.document())
        .into_iter()
        .map(|line| {
            let is_uid = line
                .split_once(':')
                .is_some_and(|(head, _)| head.trim().eq_ignore_ascii_case("UID"));
            if is_uid {
                Cow::Owned(format!("UID:{uid}"))
            } else {
                Cow::Owned(line.into_owned())
            }
        })
        .collect();
    Card::parse(vcard::render(lines)).ok()
}

/// One card as a document saved contacts can keep, or `None` when nothing
/// in it names the contact.
fn normalize(lines: &[String]) -> Option<Card> {
    let version = lines
        .iter()
        .find_map(|line| {
            let (head, value) = line.split_once(':')?;
            head.trim()
                .eq_ignore_ascii_case("VERSION")
                .then(|| value.trim())
        })
        .filter(|version| *version == "4.0")
        .unwrap_or("3.0");
    let mut output: Vec<Cow<'static, str>> =
        vec!["BEGIN:VCARD".into(), format!("VERSION:{version}").into()];
    let mut names = Names::default();
    for line in lines {
        let Some((head, raw)) = line.split_once(':') else {
            continue;
        };
        let mut parameters = head.split(';');
        let Some(property) = parameters
            .next()
            .map(str::trim)
            .filter(|property| !property.is_empty())
        else {
            continue;
        };
        // Grouped properties, as Google's item1.EMAIL, keep their group.
        let bare = property.rsplit('.').next().unwrap_or(property);
        if ["VERSION", "BEGIN", "END"]
            .iter()
            .any(|skipped| bare.eq_ignore_ascii_case(skipped))
        {
            continue;
        }
        // The document is UTF-8, so vCard 2.1's quoted-printable values are
        // decoded, with line breaks escaped as later versions write them.
        let mut value: Cow<'_, str> = if vcard::contains_ascii_case(head, "QUOTED-PRINTABLE") {
            let decoded = vcard::decoded_value(head, raw);
            Cow::Owned(
                decoded
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .replace('\n', "\\n"),
            )
        } else {
            Cow::Borrowed(raw)
        };
        let mut kept = Vec::new();
        let mut omits_year = false;
        for parameter in parameters.map(str::trim).filter(|p| !p.is_empty()) {
            let (key, setting) = match parameter.split_once('=') {
                Some((key, setting)) => (key.trim(), Some(setting.trim().trim_matches('"'))),
                None => (parameter, None),
            };
            let encoding = match setting {
                Some(setting) if key.eq_ignore_ascii_case("ENCODING") => Some(setting),
                None => Some(key),
                Some(_) => None,
            };
            match encoding {
                Some(encoding)
                    if ["QUOTED-PRINTABLE", "8BIT", "7BIT"]
                        .iter()
                        .any(|dropped| encoding.eq_ignore_ascii_case(dropped)) =>
                {
                    continue;
                }
                Some(encoding)
                    if encoding.eq_ignore_ascii_case("BASE64")
                        || encoding.eq_ignore_ascii_case("B") =>
                {
                    kept.push("ENCODING=b".to_owned());
                    continue;
                }
                _ => {}
            }
            if setting.is_some() && key.eq_ignore_ascii_case("CHARSET") {
                continue;
            }
            if setting.is_some() && key.eq_ignore_ascii_case("X-APPLE-OMIT-YEAR") {
                omits_year = true;
                continue;
            }
            // vCard 2.1 names types bare, as in TEL;CELL.
            kept.push(match setting {
                Some(_) => parameter.to_owned(),
                None => format!("TYPE={parameter}"),
            });
        }
        if omits_year && bare.eq_ignore_ascii_case("BDAY") {
            // A birthday without a year, which Apple writes with a stand-in
            // year, as vCard 4.0 writes it: --MM-DD.
            if let Some(day) = value.trim().get(4..).filter(|day| day.starts_with('-')) {
                value = Cow::Owned(format!("-{day}"));
            }
        }
        if !names.note(bare, value.trim()) {
            continue;
        }
        let head = if kept.is_empty() {
            property.to_owned()
        } else {
            format!("{property};{}", kept.join(";"))
        };
        output.push(format!("{head}:{value}").into());
    }
    let fallback = if names.formatted {
        None
    } else {
        Some(names.fallback()?)
    };
    if let Some(name) = fallback {
        output.push(format!("FN:{}", vcard::escape(&name)).into());
    }
    // vCard 3.0 requires N, and some servers refuse a card without it.
    if !names.structured && version == "3.0" {
        output.push("N:;;;;".into());
    }
    if !names.uid {
        output.push(format!("UID:{}", vcard::new_uid()).into());
    }
    output.push("END:VCARD".into());
    let document = vcard::render(output);
    vcard::ensure_size(&document).ok()?;
    Card::parse(document).ok()
}

/// What a card offers to call its contact by.
#[derive(Default)]
struct Names {
    uid: bool,
    formatted: bool,
    structured: bool,
    from_structured: Option<String>,
    organization: Option<String>,
    email: Option<String>,
    phone: Option<String>,
}

impl Names {
    /// Note a property, and tell whether to keep it: empty names and UIDs
    /// are left out.
    fn note(&mut self, property: &str, value: &str) -> bool {
        let text =
            || Some(vcard::unescape(value).trim().to_owned()).filter(|text| !text.is_empty());
        if property.eq_ignore_ascii_case("UID") {
            if value.is_empty() || self.uid {
                return false;
            }
            self.uid = true;
        } else if property.eq_ignore_ascii_case("FN") {
            if text().is_none() || self.formatted {
                return false;
            }
            self.formatted = true;
        } else if property.eq_ignore_ascii_case("N") {
            if self.structured {
                return false;
            }
            self.structured = true;
            let parts = vcard::split_escaped(value, ';');
            let name = [
                parts.get(3),
                parts.get(1),
                parts.get(2),
                parts.first(),
                parts.get(4),
            ]
            .into_iter()
            .flatten()
            .map(|part| vcard::unescape(part).trim().to_owned())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
            if !name.is_empty() {
                self.from_structured = Some(name);
            }
        } else if property.eq_ignore_ascii_case("ORG") {
            if self.organization.is_none() {
                self.organization = vcard::split_escaped(value, ';')
                    .first()
                    .map(|part| vcard::unescape(part).trim().to_owned())
                    .filter(|part| !part.is_empty());
            }
        } else if property.eq_ignore_ascii_case("EMAIL") {
            if self.email.is_none() {
                self.email = text();
            }
        } else if property.eq_ignore_ascii_case("TEL") && self.phone.is_none() {
            self.phone = text();
        }
        true
    }

    /// A name for a card without one: its structured name, else its
    /// organization, address or phone number.
    fn fallback(&self) -> Option<String> {
        [
            &self.from_structured,
            &self.organization,
            &self.email,
            &self.phone,
        ]
        .into_iter()
        .flatten()
        .next()
        .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_text(text: &str) -> ImportedCards {
        read(text.as_bytes()).unwrap()
    }

    #[test]
    fn a_file_holds_many_cards_with_any_line_endings() {
        let file = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada Lovelace\r\nEMAIL;TYPE=INTERNET:ada@example.com\r\nEND:VCARD\r\n\
                    BEGIN:VCARD\nVERSION:3.0\nFN:Grace Hopper\nTEL;TYPE=CELL:+1 555 0100\nEND:VCARD\n\
                    BEGIN:VCARD\r\r\nVERSION:3.0\r\r\nFN:Alan Turing\r\r\nEND:VCARD\r\r\n";
        let imported = read_text(file);
        assert_eq!(imported.unreadable, 0);
        let names = imported
            .cards
            .iter()
            .map(|card| card.record().name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["Ada Lovelace", "Grace Hopper", "Alan Turing"]);
        assert_eq!(imported.cards[0].record().email, "ada@example.com");
        assert_eq!(imported.cards[1].record().phone, "+1 555 0100");
        // Each gets a UID of its own.
        let uids = imported
            .cards
            .iter()
            .map(|card| vcard::uid(card.document()).unwrap())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(uids.len(), 3);
    }

    #[test]
    fn vcard_2_1_cards_from_old_phones_are_read() {
        let file = "BEGIN:VCARD\r\n\
                    VERSION:2.1\r\n\
                    N;CHARSET=ISO-8859-1;ENCODING=QUOTED-PRINTABLE:M=FCller;J=FCrgen;;;\r\n\
                    TEL;CELL;PREF:+49 30 1234\r\n\
                    TEL;HOME;VOICE:+49 30 5678\r\n\
                    NOTE;ENCODING=QUOTED-PRINTABLE:First line=0D=0ASecond =\r\n\
                    line\r\n\
                    END:VCARD\r\n";
        let imported = read_text(file);
        let card = &imported.cards[0];
        let record = card.record();
        assert_eq!(record.name, "Jürgen Müller");
        assert_eq!(record.phone, "+49 30 1234");
        assert_eq!(record.details.phones[0].label, "CELL");
        assert_eq!(record.details.phones[1].label, "HOME,VOICE");
        assert_eq!(record.notes, "First line\nSecond line");
        assert!(card.document().contains("VERSION:3.0"));
        assert!(!card.document().contains("QUOTED-PRINTABLE"));
        assert!(!card.document().contains("CHARSET"));
    }

    #[test]
    fn a_card_without_a_name_is_called_by_what_it_has() {
        let file = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Lovelace;Ada;;Lady;\r\nEND:VCARD\r\n\
                    BEGIN:VCARD\r\nVERSION:3.0\r\nFN:\r\nORG:Analytical Engines;Research\r\nEND:VCARD\r\n\
                    BEGIN:VCARD\r\nVERSION:3.0\r\nEMAIL:grace@example.com\r\nEND:VCARD\r\n\
                    BEGIN:VCARD\r\nVERSION:3.0\r\nTEL:+1 555 0100\r\nEND:VCARD\r\n\
                    BEGIN:VCARD\r\nVERSION:3.0\r\nNOTE:Nothing to call them by\r\nEND:VCARD\r\n";
        let imported = read_text(file);
        let names = imported
            .cards
            .iter()
            .map(|card| card.record().name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "Lady Ada Lovelace",
                "Analytical Engines",
                "grace@example.com",
                "+1 555 0100"
            ]
        );
        assert_eq!(imported.unreadable, 1);
        // vCard 3.0 requires N.
        assert!(imported.cards[2].document().contains("\r\nN:;;;;\r\n"));
    }

    #[test]
    fn google_contacts_exports_keep_their_groups_labels_and_photos() {
        let file = "BEGIN:VCARD\r\n\
                    VERSION:3.0\r\n\
                    FN:Ada Lovelace\r\n\
                    N:Lovelace;Ada;;;\r\n\
                    item1.EMAIL;TYPE=INTERNET:ada@example.com\r\n\
                    item1.X-ABLabel:Work\r\n\
                    PHOTO;ENCODING=b;TYPE=PNG:AAAB\r\n\
                    PHOTO:https://lh3.googleusercontent.com/contacts/photo\r\n\
                    CATEGORIES:myContacts\r\n\
                    END:VCARD\r\n";
        let card = &read_text(file).cards[0];
        assert_eq!(card.record().email, "ada@example.com");
        assert!(card.document().contains("item1.X-ABLabel:Work"));
        // Embedded photos are kept; linked ones stay links nothing fetches.
        assert_eq!(vcard::photo(card.document()), Some(vec![0, 0, 1]));
        assert!(card.document().contains("lh3.googleusercontent.com"));
    }

    #[test]
    fn birthdays_without_a_year_and_legacy_charsets() {
        let file = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada\r\nBDAY;X-APPLE-OMIT-YEAR=1604:1604-12-10\r\nEND:VCARD\r\n";
        assert_eq!(read_text(file).cards[0].record().birthday, "--12-10");

        // Windows-1252, as older address books export.
        let mut file = b"BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ren".to_vec();
        file.push(0xe9);
        file.extend_from_slice(b"e\r\nEND:VCARD\r\n");
        assert_eq!(read(&file).unwrap().cards[0].record().name, "Renée");

        // UTF-16 with a byte order mark.
        let text = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:Zoë\r\nEND:VCARD\r\n";
        let mut file = vec![0xff, 0xfe];
        file.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        let card = &read(&file).unwrap().cards[0];
        assert_eq!(card.record().name, "Zoë");
        assert!(card.document().contains("VERSION:4.0"));
    }

    #[test]
    fn cards_keep_their_own_uid_once() {
        let file = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:urn:uuid:ada\r\nUID:urn:uuid:other\r\nFN:Ada\r\nEND:VCARD\r\n";
        let card = &read_text(file).cards[0];
        assert_eq!(vcard::uid(card.document()).as_deref(), Some("urn:uuid:ada"));
        assert_eq!(card.document().matches("UID:").count(), 1);
    }

    #[test]
    fn nested_cards_and_text_around_cards_are_left_out() {
        let file = "Exported contacts\r\nBEGIN:VCARD\r\nVERSION:2.1\r\nFN:Ada\r\n\
                    AGENT:\r\nBEGIN:VCARD\r\nFN:Assistant\r\nEND:VCARD\r\nEND:VCARD\r\ntrailing\r\n";
        let imported = read_text(file);
        assert_eq!(imported.cards.len(), 1);
        assert_eq!(imported.cards[0].record().name, "Ada");
        assert!(!imported.cards[0].document().contains("Assistant"));
    }

    #[test]
    fn oversized_files_are_refused() {
        let file = vec![b' '; MAX_FILE_BYTES + 1];
        assert_eq!(read(&file).unwrap_err(), ContactFileProblem::TooLarge);
    }

    #[test]
    fn a_copy_gets_a_uid_of_its_own() {
        let file = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:urn:uuid:ada\r\nFN:Ada\r\nEND:VCARD\r\n";
        let card = &read_text(file).cards[0];
        let copy = with_new_uid(card).unwrap();
        assert_ne!(vcard::uid(copy.document()), vcard::uid(card.document()));
        assert_eq!(copy.record().name, "Ada");
    }
}
