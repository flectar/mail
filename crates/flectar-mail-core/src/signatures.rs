//! Account-scoped signature validation and plain-text composition.
use crate::{
    Core,
    db::repo,
    error::{CoreError, Result},
    models::{Signature, SignatureDefaults},
};

pub const MAX_SIGNATURE_BYTES: usize = 32 * 1024;
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

/// No active HTML, images or remote requests in signature settings.
pub(crate) fn sanitize_html(html: &str) -> String {
    ammonia::Builder::default()
        .rm_tags(&["img"])
        .clean(html)
        .to_string()
}

/// Insert above the quotation, preserving the standard `-- ` delimiter.
pub fn insert(body: &str, signature: &str) -> String {
    let signature = signature.trim();
    if signature.is_empty() {
        return body.to_owned();
    }
    let at = quotation_start(body).unwrap_or(body.len());
    let mut result = String::with_capacity(body.len() + signature.len() + 6);
    result.push_str(&body[..at]);
    result.push_str("\n\n-- \n");
    result.push_str(signature);
    result.push_str(&body[at..]);
    result
}

/// Where a reply or forward quotation begins: the paragraph that opens with
/// a forwarded message's dashed rule, or whose first line introduces `>`
/// quoted lines. The structure, unlike the wording, is the same in every
/// language the header was written in.
fn quotation_start(body: &str) -> Option<usize> {
    (0..body.len()).find(|&at| {
        if !body.as_bytes()[at..].starts_with(b"\n\n") {
            return false;
        }
        let paragraph = &body[at + 2..];
        let mut lines = paragraph.lines();
        paragraph.starts_with("---------- ")
            || (lines.next().is_some_and(|line| !line.starts_with('>'))
                && lines.next().is_some_and(|line| line.starts_with('>')))
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
                "Use a signature name of 1–100 bytes and a body under 32 KiB".into(),
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
}
