//! Account-scoped signature validation and plain-text composition.
use crate::{
    Core,
    db::repo,
    error::{CoreError, Result},
    models::{Signature, SignatureDefaults},
};

/// A signature's HTML, pictures included, can be this large.
pub const MAX_SIGNATURE_BYTES: usize = 256 * 1024;
pub(crate) const MAX_SIGNATURES_PER_ACCOUNT: usize = 20;

pub fn plain_text(signature: &Signature) -> String {
    let mut text = mail_parser::decoders::html::html_to_text(&signature.html);
    text.truncate(text.trim_end().len());
    let start = text.len() - text.trim_start().len();
    if start > 0 {
        text.drain(..start);
    }
    // The decoder reserves the HTML input length, which can greatly exceed
    // the retained text after markup removal and whitespace trimming.
    text.shrink_to_fit();
    text
}

pub fn text_html(text: &str) -> String {
    let extra: usize = text
        .bytes()
        .map(|byte| match byte {
            b'&' => 4,
            b'<' | b'>' | b'\n' => 3,
            _ => 0,
        })
        .sum();
    let mut html = String::with_capacity(text.len() + extra + 11);
    html.push_str("<div>");
    for ch in text.chars() {
        match ch {
            '&' => html.push_str("&amp;"),
            '<' => html.push_str("&lt;"),
            '>' => html.push_str("&gt;"),
            '\n' => html.push_str("<br>"),
            _ => html.push(ch),
        }
    }
    html.push_str("</div>");
    html
}

/// The formatting the composer writes, and pictures the signature holds itself: no active
/// HTML, pictures from the web or other remote requests in signature settings.
pub(crate) fn sanitize_html(html: &str) -> String {
    static SANITIZER: std::sync::LazyLock<ammonia::Builder<'static>> =
        std::sync::LazyLock::new(|| {
            let mut builder = ammonia::Builder::default();
            builder
                .add_generic_attributes(["style", "align", "dir"])
                .add_url_schemes(["data"])
                .attribute_filter(|element, attribute, value| match (element, attribute) {
                    ("img", "src") => is_embedded_picture(value).then(|| value.into()),
                    (_, "href") if has_data_scheme(value) => None,
                    _ => Some(value.into()),
                })
                .link_rel(Some("noopener noreferrer"));
            builder
        });
    SANITIZER.clean(html).to_string()
}

/// Whether `src` holds a PNG, JPEG, GIF or WebP picture itself, as the composer writes them.
fn is_embedded_picture(src: &str) -> bool {
    let src = src.trim();
    ["png", "jpeg", "gif", "webp"].iter().any(|kind| {
        let prefix = format!("data:image/{kind};base64,");
        src.get(..prefix.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(&prefix))
    })
}

fn has_data_scheme(url: &str) -> bool {
    url.trim()
        .get(..5)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"))
}

/// Where a signature goes in `body`: above the quotation, or at the end.
pub fn insertion_point(body: &str) -> usize {
    quotation_start(body).unwrap_or(body.len())
}

/// Insert above the quotation, preserving the standard `-- ` delimiter.
pub fn insert(body: &str, signature: &str) -> String {
    let signature = signature.trim();
    if signature.is_empty() {
        return body.to_owned();
    }
    let at = insertion_point(body);
    let mut result = String::with_capacity(body.len() + signature.len() + 6);
    result.push_str(&body[..at]);
    result.push_str("\n\n-- \n");
    result.push_str(signature);
    result.push_str(&body[at..]);
    result
}

/// Where a reply or forward quotation begins: the paragraph that opens with
/// a forwarded message's dashed rule, or whose first line introduces quoted
/// lines, written with `>` or as the composer's quote block. The structure,
/// unlike the wording, is the same in every language the header was written
/// in.
fn quotation_start(body: &str) -> Option<usize> {
    let quoted = |line: &str| line.starts_with('>') || line.starts_with("│ ");
    (0..body.len()).find(|&at| {
        if !body.as_bytes()[at..].starts_with(b"\n\n") {
            return false;
        }
        let paragraph = &body[at + 2..];
        let mut lines = paragraph.lines();
        paragraph.starts_with("---------- ")
            || (lines.next().is_some_and(|line| !quoted(line)) && lines.next().is_some_and(quoted))
    })
}

impl Core {
    pub async fn save_signature(&self, mut signature: Signature) -> Result<Signature> {
        signature.name = signature.name.trim().to_owned();
        if signature.name.is_empty()
            || signature.name.len() > 100
            || signature.html.len() > MAX_SIGNATURE_BYTES
        {
            return Err(CoreError::Other(
                "Use a signature name of 1–100 bytes and a body under 256 KiB".into(),
            ));
        }
        signature.html = sanitize_html(&signature.html);
        if signature.id.is_empty() {
            signature.id = format!("sig-{:032x}", rand::random::<u128>());
        }
        self.db
            .write(move |conn| {
                if repo::accounts::get(conn, signature.account_id)?.is_none() {
                    return Err(CoreError::NotFound("account".into()));
                }
                let mut settings = repo::settings::get(conn)?;
                if let Some(existing) = settings
                    .signature_list
                    .iter_mut()
                    .find(|s| s.id == signature.id)
                {
                    if existing.account_id != signature.account_id {
                        return Err(CoreError::Other(
                            "Signature belongs to another account".into(),
                        ));
                    }
                    *existing = signature.clone();
                } else {
                    if settings
                        .signature_list
                        .iter()
                        .filter(|s| s.account_id == signature.account_id)
                        .count()
                        >= MAX_SIGNATURES_PER_ACCOUNT
                    {
                        return Err(CoreError::Other(
                            "An account can have up to 20 signatures".into(),
                        ));
                    }
                    settings.signature_list.push(signature.clone());
                }
                repo::settings::set(conn, &settings)?;
                Ok(signature)
            })
            .await
    }

    pub async fn delete_signature(&self, account_id: i64, id: String) -> Result<()> {
        self.db
            .write(move |conn| {
                let mut settings = repo::settings::get(conn)?;
                settings
                    .signature_list
                    .retain(|s| !(s.account_id == account_id && s.id == id));
                if let Some(defaults) = settings.signature_defaults.get_mut(&account_id.to_string())
                {
                    if defaults.new_id.as_ref() == Some(&id) {
                        defaults.new_id = None;
                    }
                    if defaults.reply_id.as_ref() == Some(&id) {
                        defaults.reply_id = None;
                    }
                }
                repo::settings::set(conn, &settings)
            })
            .await
    }

    pub async fn set_signature_defaults(
        &self,
        account_id: i64,
        defaults: SignatureDefaults,
    ) -> Result<()> {
        self.db
            .write(move |conn| {
                if repo::accounts::get(conn, account_id)?.is_none() {
                    return Err(CoreError::NotFound("account".into()));
                }
                let mut settings = repo::settings::get(conn)?;
                for id in [&defaults.new_id, &defaults.reply_id].into_iter().flatten() {
                    if !settings
                        .signature_list
                        .iter()
                        .any(|s| s.account_id == account_id && &s.id == id)
                    {
                        return Err(CoreError::Other(
                            "Choose a signature belonging to this account".into(),
                        ));
                    }
                }
                settings
                    .signature_defaults
                    .insert(account_id.to_string(), defaults);
                repo::settings::set(conn, &settings)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inserts_before_history_and_escapes_text() {
        assert_eq!(
            insert("Reply\n\nOn Monday, Pat wrote:\n> Hello", "Me"),
            "Reply\n\n-- \nMe\n\nOn Monday, Pat wrote:\n> Hello"
        );
        assert_eq!(
            insert("Hola\n\nEl lunes, Pat escribió:\n> Hola", "Yo"),
            "Hola\n\n-- \nYo\n\nEl lunes, Pat escribió:\n> Hola"
        );
        assert_eq!(
            insert(
                "\n\n---------- Пересылаемое сообщение ----------\nОт: Pat",
                "Я"
            ),
            "\n\n-- \nЯ\n\n---------- Пересылаемое сообщение ----------\nОт: Pat"
        );
        assert_eq!(
            insert("Hi\n\n\nPat wrote:\n> Hello", "Me"),
            "Hi\n\n\n-- \nMe\n\nPat wrote:\n> Hello"
        );
        // A quote moved into the editor is a quote block under its line.
        assert_eq!(
            insert("Reply\n\nPat wrote:\n│ Hello\n│ \n│ Bye", "Me"),
            "Reply\n\n-- \nMe\n\nPat wrote:\n│ Hello\n│ \n│ Bye"
        );
        assert_eq!(
            insert("On Monday we met.\n\nOnward, then.", "Me"),
            "On Monday we met.\n\nOnward, then.\n\n-- \nMe"
        );
        let s = Signature {
            id: "s".into(),
            account_id: 1,
            name: "Work".into(),
            html: text_html("Zoë <me@example.org>\nEngineer"),
        };
        assert_eq!(plain_text(&s), "Zoë <me@example.org>\nEngineer");
        assert_eq!(insert("Body", ""), "Body");
    }

    #[test]
    fn signatures_keep_their_formatting_and_their_own_pictures() {
        let logo = "data:image/png;base64,iVBORw0KGgo=";
        let html = format!(
            "<div align=\"center\" style=\"text-align:center\"><span style=\"color:#0b5394;\
             font-size:x-large\">Zoë</span><br><a href=\"https://example.org\">example.org</a>\
             <br><img src=\"{logo}\" alt=\"Logo\" width=\"120\" height=\"40\" \
             style=\"width:120px;max-width:100%;height:auto\"></div>"
        );
        let clean = sanitize_html(&html);
        for kept in [
            "align=\"center\"",
            "style=\"text-align:center\"",
            "style=\"color:#0b5394;font-size:x-large\"",
            "href=\"https://example.org\"",
            "src=\"data:image/png;base64,iVBORw0KGgo=\"",
            "alt=\"Logo\"",
            "style=\"width:120px;max-width:100%;height:auto\"",
        ] {
            assert!(clean.contains(kept), "{kept} in {clean}");
        }

        // Pictures from the web, other embedded data, scripts and data links go.
        let clean = sanitize_html(
            "<img src=\"https://tracker.example/p.gif\" alt=\"web\">\
             <img src=\"data:image/svg+xml;base64,PHN2Zy8+\" alt=\"svg\">\
             <a href=\"data:text/html,hi\">data</a><a href=\"javascript:alert(1)\">js</a>\
             <script>alert(1)</script><span onclick=\"x()\">Me</span>",
        );
        for gone in [
            "tracker.example",
            "svg+xml",
            "data:text",
            "javascript",
            "<script",
            "onclick",
        ] {
            assert!(!clean.contains(gone), "{gone} in {clean}");
        }
        assert!(clean.contains("Me"), "{clean}");
    }
}
