//! Mail for AI agents using the local MCP server (#237). The app decides what
//! an agent may see from `AgentAccess` and calls these for the rest: mail as
//! sync stored it, end-to-end encrypted messages recognized without
//! decrypting them, and the activity log.

use crate::Core;
use crate::db::repo;
use crate::db::repo::agent_activity::NewAgentActivity;
use crate::error::Result;
use crate::models::{
    Address, AgentAccess, AgentActivity, MessageDetail, MessageSearchCursor, MessageSearchPage,
};

impl Core {
    /// Messages matching `query` in any of `folder_ids`, newest first.
    pub async fn search_messages_in_folders(
        &self,
        query: String,
        folder_ids: Vec<i64>,
        cursor: Option<MessageSearchCursor>,
        limit: i64,
    ) -> Result<MessageSearchPage> {
        let mut parsed = crate::search::parse(&query);
        parsed.folder_ids = Some(folder_ids);
        self.search_messages_parsed(parsed, cursor, limit).await
    }

    /// The folders `message_id` is in, as mail lists show it.
    pub async fn message_folder_ids(&self, message_id: i64) -> Result<Vec<i64>> {
        self.db
            .read(move |conn| repo::messages::folder_ids(conn, message_id))
            .await
    }

    /// A message as sync stored it, without the pictures `get_body` puts in
    /// its HTML. A body that isn't downloaded yet starts downloading.
    pub async fn stored_message(&self, message_id: i64) -> Result<MessageDetail> {
        let message = self
            .db
            .read(move |conn| repo::messages::detail(conn, message_id))
            .await?;
        if message.body_state == "none" {
            self.request_body(message.account_id, message_id).await;
        }
        Ok(message)
    }

    /// A conversation's messages as sync stored them, oldest first.
    pub async fn stored_thread_messages(&self, thread_id: i64) -> Result<Vec<MessageDetail>> {
        self.db
            .read(move |conn| repo::messages::list_for_thread(conn, thread_id))
            .await
    }

    /// Contacts matching `query` that `account_ids` know, or every account
    /// when `None`: saved contacts, and people from the mail when the user lets
    /// suggestions learn. Unlike `list_contacts`, the accounts always narrow it,
    /// whatever the suggestion settings say.
    pub async fn contacts_for_accounts(
        &self,
        query: String,
        account_ids: Option<Vec<i64>>,
        limit: i64,
    ) -> Result<Vec<Address>> {
        self.db
            .read(move |conn| {
                let learned = repo::settings::get(conn)?.suggest_learned_contacts;
                let limit = limit.clamp(1, 50);
                let Some(account_ids) = account_ids else {
                    return repo::contacts::autocomplete(conn, &query, None, learned, limit);
                };
                let mut found: Vec<Address> = Vec::new();
                for account_id in account_ids {
                    let matches = repo::contacts::autocomplete(
                        conn,
                        &query,
                        Some(account_id),
                        learned,
                        limit,
                    )?;
                    for address in matches {
                        if !found
                            .iter()
                            .any(|known| known.email.eq_ignore_ascii_case(&address.email))
                        {
                            found.push(address);
                        }
                    }
                }
                found.truncate(usize::try_from(limit).unwrap_or_default());
                Ok(found)
            })
            .await
    }

    /// Changes what agents may do, read and written on the writer thread so
    /// no other preference is overwritten with a stale copy.
    pub async fn update_agent_access(
        &self,
        change: impl FnOnce(&mut AgentAccess) + Send + 'static,
    ) -> Result<AgentAccess> {
        self.db
            .write(move |conn| {
                let mut settings = repo::settings::get(conn)?;
                change(&mut settings.agent_access);
                repo::settings::set(conn, &settings)?;
                Ok(settings.agent_access)
            })
            .await
    }

    pub async fn record_agent_activity(&self, entry: NewAgentActivity) -> Result<()> {
        self.db
            .write(move |conn| repo::agent_activity::record(conn, &entry))
            .await
    }

    /// The newest `limit` activity log entries, newest first.
    pub async fn agent_activity(&self, limit: i64) -> Result<Vec<AgentActivity>> {
        self.db
            .read(move |conn| repo::agent_activity::list(conn, limit))
            .await
    }

    pub async fn clear_agent_activity(&self) -> Result<()> {
        self.db
            .write(move |conn| repo::agent_activity::clear(conn))
            .await
    }
}

/// Whether a message is end-to-end encrypted, with OpenPGP or S/MIME, judged
/// from what sync stored and without decrypting anything.
pub fn is_end_to_end_encrypted(message: &MessageDetail) -> bool {
    const ARMOR: &str = "-----BEGIN PGP MESSAGE-----";
    let armored = |body: &Option<String>| body.as_deref().is_some_and(|body| body.contains(ARMOR));
    armored(&message.text_body)
        || armored(&message.html_body)
        || message.attachments.iter().any(|attachment| {
            let name = attachment.filename.as_deref().unwrap_or_default();
            let mime = attachment
                .mime_type
                .as_deref()
                .unwrap_or_default()
                .to_ascii_lowercase();
            name.eq_ignore_ascii_case("encrypted.asc")
                || name.eq_ignore_ascii_case("smime.p7m")
                || mime == "application/pgp-encrypted"
                || mime.starts_with("application/pkcs7-mime")
                || mime.starts_with("application/x-pkcs7-mime")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;
    use crate::models::AttachmentMeta;

    fn attachment(filename: &str, mime_type: &str) -> AttachmentMeta {
        AttachmentMeta {
            id: 1,
            filename: Some(filename.into()),
            mime_type: Some(mime_type.into()),
            size: Some(10),
            is_inline: false,
        }
    }

    #[test]
    fn a_message_is_in_its_folder_and_for_gmail_its_labels() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (2, 1, 'Work', NULL)",
            [],
        )
        .unwrap();
        let (_, message_id) = testutil::seed_message(&conn, "a@example.com", "Plans", false);
        conn.execute(
            "INSERT INTO message_folders (message_id, folder_id) VALUES (?1, 2)",
            [message_id],
        )
        .unwrap();
        // Outside Gmail a message is in its own folder only, as lists show it.
        assert_eq!(repo::messages::folder_ids(&conn, message_id).unwrap(), [1]);
        conn.execute(
            "UPDATE accounts SET provider = 'gmail', mail_protocol = 'gmail' WHERE id = 1",
            [],
        )
        .unwrap();
        let mut folders = repo::messages::folder_ids(&conn, message_id).unwrap();
        folders.sort();
        assert_eq!(folders, [1, 2]);
    }

    #[test]
    fn encrypted_mail_is_recognized_without_decrypting_it() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let (_, message_id) = testutil::seed_message(&conn, "a@example.com", "Plans", false);
        let mut message = repo::messages::detail(&conn, message_id).unwrap();
        assert!(!is_end_to_end_encrypted(&message));

        message.text_body = Some("Hi\n-----BEGIN PGP MESSAGE-----\nhQEM...".into());
        assert!(is_end_to_end_encrypted(&message));
        message.text_body = Some("Signed, not secret.".into());
        assert!(!is_end_to_end_encrypted(&message));

        for (filename, mime_type) in [
            ("Encrypted.asc", "application/octet-stream"),
            ("", "application/pgp-encrypted"),
            ("smime.p7m", "application/pkcs7-mime"),
            ("", "application/x-pkcs7-mime; smime-type=enveloped-data"),
        ] {
            message.attachments = vec![attachment(filename, mime_type)];
            assert!(is_end_to_end_encrypted(&message), "{filename} {mime_type}");
        }
        message.attachments = vec![attachment("signature.asc", "application/pgp-signature")];
        assert!(!is_end_to_end_encrypted(&message));
    }
}
