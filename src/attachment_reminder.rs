//! The words that say a message carries a file (#236), to ask before it goes
//! out with none, as Thunderbird, Evolution and Roundcube do. Only what the
//! user wrote counts: quoted lines, the signature and links are left out.

use crate::ui_message::UiMessage;

/// Words that suggest a message has a file attached, as a comma-separated
/// list that each language translates. English always counts too, as people
/// write in English whatever their app's language. A word matches when no
/// letter follows it, so a list names each form, and an entry starting with a
/// dot matches file names with that extension.
pub(crate) fn keywords() -> UiMessage {
    UiMessage::plain(
        "attach, attached, attaching, attachment, attachments, enclosed, enclosure, CV, résumé, cover letter, .pdf, .doc, .docx, .xls, .xlsx, .ppt, .pptx, .odt, .ods, .odp, .rtf, .zip",
    )
}

/// The first keyword of `lists` that `text` mentions, as the list writes it.
pub(crate) fn mentioned(text: &str, lists: &[&str]) -> Option<String> {
    let text = text
        .split_whitespace()
        .filter(|word| !is_link(word))
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
    lists
        .iter()
        .flat_map(|list| list.split(','))
        .map(str::trim)
        .filter(|keyword| !keyword.is_empty())
        .find(|keyword| {
            let keyword = keyword
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            found(&text, &keyword)
        })
        .map(str::to_owned)
}

/// What the user wrote of a message: `subject` when it counts, and `body`
/// without quoted lines, without the `signature` the composer put in, and
/// without anything past a signature separator.
pub(crate) fn own_text(subject: Option<&str>, body: &str, signature: Option<&str>) -> String {
    let body = match signature
        .map(str::trim)
        .filter(|signature| !signature.is_empty())
    {
        Some(signature) => body.replacen(signature, "", 1),
        None => body.to_owned(),
    };
    let mut text = subject.unwrap_or_default().to_owned();
    for line in body.lines() {
        if matches!(line.trim_end(), "--" | "-- ") {
            break;
        }
        if line.trim_start().starts_with('>') {
            continue;
        }
        text.push('\n');
        text.push_str(line);
    }
    text
}

fn found(text: &str, keyword: &str) -> bool {
    if keyword.starts_with('.') {
        // A file name with that extension, such as "report.pdf".
        return text.split(' ').any(|word| {
            let word = word.trim_end_matches(|c: char| !c.is_alphanumeric());
            word.len() > keyword.len() && word.ends_with(keyword)
        });
    }
    // Letters may come before the keyword, as in a compound word such as
    // German "Dateianhang", but not after it. Chinese and Japanese words
    // have no spaces to tell where they end.
    let unbounded = keyword.chars().any(is_cjk);
    text.match_indices(keyword).any(|(start, matched)| {
        unbounded
            || !text[start + matched.len()..]
                .chars()
                .next()
                .is_some_and(char::is_alphabetic)
    })
}

fn is_link(word: &str) -> bool {
    let word = word.to_ascii_lowercase();
    ["http://", "https://", "ftp://", "www.", "mailto:"]
        .iter()
        .any(|start| word.starts_with(start))
}

fn is_cjk(c: char) -> bool {
    matches!(c,
        '\u{3040}'..='\u{30ff}' | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}'
        | '\u{ac00}'..='\u{d7af}' | '\u{f900}'..='\u{faff}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions_need_the_whole_word_or_a_file_name() {
        let english = [keywords().key()];
        let mentioned = |text| mentioned(text, &english);
        assert_eq!(
            mentioned("I've Attached the plan."),
            Some("attached".into())
        );
        assert_eq!(mentioned("See attachments!"), Some("attachments".into()));
        assert_eq!(
            mentioned("Here's my\n cover\n letter"),
            Some("cover letter".into())
        );
        assert_eq!(mentioned("Read report.PDF tonight"), Some(".pdf".into()));
        // No letter may follow the word.
        assert_eq!(mentioned("an attachmentless note"), None);
        // Nor does a link mention one.
        assert_eq!(mentioned("https://example.com/attachment/1.pdf"), None);
        assert_eq!(
            mentioned("We resume on Monday, as attaching is optional"),
            Some("attaching".into())
        );
        assert_eq!(mentioned("We resume on Monday."), None);
        assert_eq!(mentioned("Just .pdf"), None);
    }

    #[test]
    fn each_language_adds_its_own_words() {
        let lists = [keywords().key(), "Anhang, anbei, 添付"];
        assert_eq!(
            mentioned("Siehe Dateianhang.", &lists),
            Some("Anhang".into())
        );
        assert_eq!(
            mentioned("ファイルを添付しました", &lists),
            Some("添付".into())
        );
        assert_eq!(mentioned("Anhänge", &["Anhang"]), None);
    }

    #[test]
    fn only_what_the_user_wrote_counts() {
        let body = "Here it is.\n> I attached the old one\n\nBest,\nAlex\n-- \nAttached: nothing";
        let text = own_text(None, body, Some("Best,\nAlex"));
        assert!(
            !text.contains("attached") && !text.contains("Attached"),
            "{text}"
        );
        assert!(text.contains("Here it is."));
        let text = own_text(Some("Report attached"), "Hi", None);
        assert_eq!(mentioned(&text, &["attached"]), Some("attached".into()));
    }
}
