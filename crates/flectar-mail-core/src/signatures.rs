//! Account-scoped signature validation and plain-text composition.
use crate::{
    Core,
    db::repo,
    error::{CoreError, Result},
    models::{Signature, SignatureDefaults},
};

pub const MAX_SIGNATURE_BYTES: usize = 32 * 1024;

pub fn plain_text(signature: &Signature) -> String {
    mail_parser::decoders::html::html_to_text(&signature.html)
        .trim()
        .to_owned()
}

pub fn text_html(text: &str) -> String {
    format!(
        "<div>{}</div>",
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('\n', "<br>")
    )
}

/// Insert above the quotation, preserving the standard `-- ` delimiter.
pub fn insert(body: &str, signature: &str) -> String {
    if signature.trim().is_empty() {
        return body.to_owned();
    }
    let at = body
        .find("\n\nOn ")
        .or_else(|| body.find("\n\n---------- Forwarded message"))
        .unwrap_or(body.len());
    format!(
        "{}\n\n-- \n{}{}",
        &body[..at],
        signature.trim(),
        &body[at..]
    )
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
        // No active HTML, images or remote requests in signature settings.
        signature.html = ammonia::Builder::default()
            .rm_tags(&["img"])
            .clean(&signature.html)
            .to_string();
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
                        >= 20
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
