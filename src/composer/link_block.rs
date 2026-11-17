//! The block of links a message carries after what's written, in HTML and plain text, and
//! what a draft keeps in it to reopen as the files.

use flectar_mail_core::links::SharedLink;

/// The attribute on the block, and the one each link in a draft keeps its details in.
const BLOCK_MARK: &str = "data-flectar-links";
const LINK_MARK: &str = "data-flectar-link";

/// The words a block of links reads with, in the app's language.
pub(super) struct BlockWords {
    /// "2 files shared by link".
    pub(super) header: String,
    /// "password protected".
    pub(super) password: String,
}

/// What a draft keeps of a link, to reopen as the file: all but its password, which the
/// sender passes on another way.
#[derive(serde::Serialize, serde::Deserialize)]
struct KeptLink {
    service_id: i64,
    url: String,
    name: String,
    size: u64,
    expires_on: Option<String>,
    share_id: String,
    folder: String,
}

/// The links as a block for after what's written, in HTML, with `until` saying until when a
/// link works from its `YYYY-MM-DD` date. A draft's keeps each link's details, which a sent
/// message leaves out.
pub(super) fn block_html(
    links: &[&SharedLink],
    words: &BlockWords,
    until: impl Fn(&str) -> String,
    draft: bool,
) -> String {
    // Only a draft's block is marked: a reply quoting a sent one keeps it as it is.
    let mark = if draft {
        format!(" {BLOCK_MARK}=\"1\"")
    } else {
        String::new()
    };
    let mut html = format!(
        "<div{mark} style=\"margin:16px 0 8px;padding:10px 14px;border:1px solid #d0d7de;\
         border-radius:8px\"><div style=\"font-weight:600;margin-bottom:4px\">{}</div>\
         <ul style=\"margin:0;padding-left:20px\">",
        crate::email_document::escape(&words.header)
    );
    for link in links {
        html.push_str("<li");
        if draft {
            let kept = KeptLink {
                service_id: link.service_id,
                url: link.url.clone(),
                name: link.name.clone(),
                size: link.size,
                expires_on: link.expires_on.clone(),
                share_id: link.share_id.clone(),
                folder: link.folder.clone(),
            };
            let json = serde_json::to_string(&kept).unwrap_or_default();
            html.push_str(&format!(
                " {LINK_MARK}=\"{}\"",
                crate::email_document::escape(&json)
            ));
        }
        html.push_str(&format!(
            "><a href=\"{}\">{}</a> <span style=\"color:#57606a\">({})</span></li>",
            crate::email_document::escape(&link.url),
            crate::email_document::escape(&link.name),
            crate::email_document::escape(&details(link, words, &until))
        ));
    }
    html.push_str("</ul></div>");
    html
}

/// The links as a block for after what's written, in plain text.
pub(super) fn block_text(
    links: &[&SharedLink],
    words: &BlockWords,
    until: impl Fn(&str) -> String,
) -> String {
    let mut text = format!("{}:\n", words.header);
    for link in links {
        text.push_str(&format!(
            "  * {} ({}): {}\n",
            link.name,
            details(link, words, &until),
            link.url
        ));
    }
    text
}

/// A link's size, until when it works, and whether it asks for a password.
fn details(link: &SharedLink, words: &BlockWords, until: &impl Fn(&str) -> String) -> String {
    let mut details = vec![crate::compose_controller::format_file_size(link.size)];
    if let Some(date) = &link.expires_on {
        details.push(until(date));
    }
    if link.password.is_some() {
        details.push(words.password.clone());
    }
    details.join(", ")
}

/// `html` without the block of links a draft keeps, and the links it held.
pub(super) fn take_block(html: &str) -> (String, Vec<SharedLink>) {
    let Some(mark) = html.find(BLOCK_MARK) else {
        return (html.to_owned(), Vec::new());
    };
    let Some(start) = html[..mark].rfind("<div") else {
        return (html.to_owned(), Vec::new());
    };
    // The block holds one nested div, so it ends at the second closing div after it.
    let Some(end) = html[start..]
        .match_indices("</div>")
        .nth(1)
        .map(|(at, close)| start + at + close.len())
    else {
        return (html.to_owned(), Vec::new());
    };
    let block = &html[start..end];
    let mut links = Vec::new();
    let mut rest = block;
    let opening = format!("{LINK_MARK}=\"");
    while let Some(at) = rest.find(&opening) {
        let after = &rest[at + opening.len()..];
        let Some(close) = after.find('"') else {
            break;
        };
        let json = unescape(&after[..close]);
        if let Ok(kept) = serde_json::from_str::<KeptLink>(&json) {
            links.push(SharedLink {
                service_id: kept.service_id,
                url: kept.url,
                name: kept.name,
                size: kept.size,
                expires_on: kept.expires_on,
                password: None,
                share_id: kept.share_id,
                folder: kept.folder,
            });
        }
        rest = &after[close..];
    }
    let mut kept = String::with_capacity(html.len() - block.len());
    kept.push_str(&html[..start]);
    kept.push_str(&html[end..]);
    (kept, links)
}

fn unescape(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(name: &str, expires_on: Option<&str>, password: Option<&str>) -> SharedLink {
        SharedLink {
            service_id: 1,
            url: format!("https://cloud.example.com/s/{name}?a=1&b=2"),
            name: name.to_owned(),
            size: 3 * 1024 * 1024 * 1024 / 2,
            expires_on: expires_on.map(str::to_owned),
            password: password.map(str::to_owned),
            share_id: "7".to_owned(),
            folder: "Mail attachments/abc".to_owned(),
        }
    }

    fn words() -> BlockWords {
        BlockWords {
            header: "2 files shared by link".to_owned(),
            password: "password protected".to_owned(),
        }
    }

    #[test]
    fn links_go_in_a_block_that_a_draft_reopens_as_the_files() {
        let report = link("Q3 <report>.zip", Some("2026-12-14"), Some("secret"));
        let video = link("video.mp4", None, None);
        let until = |date: &str| format!("available until [{date}]");
        let sent = block_html(&[&report, &video], &words(), until, false);
        assert!(sent.contains(
            "<a href=\"https://cloud.example.com/s/Q3 &lt;report&gt;.zip?a=1&amp;b=2\">Q3 &lt;report&gt;.zip</a>"
        ));
        assert!(sent.contains("(1.5 GB, available until [2026-12-14], password protected)"));
        assert!(sent.contains("(1.5 GB)"));
        // A sent message keeps no mark and no details, and never the password.
        assert!(!sent.contains("data-flectar"));
        assert!(!sent.contains("secret"));
        assert_eq!(take_block(&sent).1.len(), 0);
        assert_eq!(
            block_text(&[&report, &video], &words(), until),
            "2 files shared by link:\n  * Q3 <report>.zip (1.5 GB, available until [2026-12-14], \
             password protected): https://cloud.example.com/s/Q3 <report>.zip?a=1&b=2\n  * \
             video.mp4 (1.5 GB): https://cloud.example.com/s/video.mp4?a=1&b=2\n"
        );

        // A draft's block reopens as the files, without the password, and leaves what's
        // around it as it was.
        let draft = block_html(&[&report, &video], &words(), until, true);
        assert!(!draft.contains("secret"));
        let html = format!("<p>Hi</p>{draft}<blockquote>quoted</blockquote>");
        let (rest, links) = take_block(&html);
        assert_eq!(rest, "<p>Hi</p><blockquote>quoted</blockquote>");
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].name, "Q3 <report>.zip");
        assert_eq!(links[0].url, report.url);
        assert_eq!(links[0].expires_on.as_deref(), Some("2026-12-14"));
        assert_eq!(links[0].password, None);
        assert_eq!(links[0].share_id, "7");
        assert_eq!(links[1].expires_on, None);
        // HTML without a block stays as it is.
        assert_eq!(
            take_block("<p>Hi</p>"),
            ("<p>Hi</p>".to_owned(), Vec::new())
        );
    }
}
