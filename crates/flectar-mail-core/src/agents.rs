//! Mail for AI agents using the local MCP server (#237). The app decides what
//! an agent may see from `AgentAccess` and calls these for the rest: mail as
//! sync stored it, end-to-end encrypted messages recognized without
//! decrypting them, and the activity log.

use crate::Core;
use crate::db::repo;
use crate::db::repo::agent_activity::{NewAgentActivity, Undo};
use crate::error::Result;
use crate::models::{
    ActionKind, ActionParams, ActionTarget, Address, AgentAccess, AgentActivity, MessageDetail,
    MessageSearchCursor, MessageSearchPage, PerformActionArgs, SaveDraftArgs, now_ms,
};
use std::collections::BTreeMap;

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

    /// Saves a draft an agent wrote, remembering it as the agent's until
    /// someone else saves it. Saving over a draft that isn't the agent's, or
    /// that changed since the agent's last save, fails with `NotFound`.
    pub async fn save_agent_draft(&self, args: SaveDraftArgs, agent: String) -> Result<i64> {
        self.save_draft_as(args, Some(agent)).await
    }

    /// Whether `draft_id` is a draft an agent wrote that nobody saved since.
    pub async fn agent_draft_unchanged(&self, draft_id: i64) -> Result<bool> {
        self.db
            .read(move |conn| repo::agent_drafts::unchanged(conn, draft_id))
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
            .read(move |conn| repo::agent_activity::list(conn, limit, now_ms()))
            .await
    }

    /// Takes back what an agent changed in activity entry `activity_id`, as
    /// far as it still can. False when there was nothing left to undo.
    pub async fn undo_agent_activity(&self, activity_id: i64) -> Result<bool> {
        let undo = self
            .db
            .write(move |conn| repo::agent_activity::claim_undo(conn, activity_id, now_ms()))
            .await?;
        if undo.is_empty() {
            return Ok(false);
        }
        let undone = self.take_back(undo).await;
        // The entry stays undone once anything was taken back, so nothing is
        // taken back twice.
        if matches!(undone, Ok(false)) {
            self.db
                .write(move |conn| repo::agent_activity::release_undo(conn, activity_id))
                .await?;
        }
        undone
    }

    /// Takes back queued actions, and puts labels back as they were.
    async fn take_back(&self, undo: Undo) -> Result<bool> {
        let mut undone = !undo.action_ids.is_empty() && self.undo_actions(undo.action_ids).await?;
        let mut labels = BTreeMap::<(i64, bool), Vec<i64>>::new();
        for change in undo.label_changes {
            labels
                .entry((change.label_id, change.added))
                .or_default()
                .push(change.message_id);
        }
        for ((label_id, added), message_ids) in labels {
            self.perform_action(PerformActionArgs {
                kind: if added {
                    ActionKind::RemoveLabel
                } else {
                    ActionKind::AddLabel
                },
                target: ActionTarget::Messages(message_ids),
                params: Some(ActionParams {
                    wake_at: None,
                    target_folder_id: None,
                    label_id: Some(label_id),
                }),
            })
            .await?;
            undone = true;
        }
        Ok(undone)
    }

    /// Which of `message_ids` have label `label_id`.
    pub async fn messages_with_label(
        &self,
        message_ids: Vec<i64>,
        label_id: i64,
    ) -> Result<Vec<i64>> {
        self.db
            .read(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT 1 FROM message_labels WHERE message_id = ?1 AND label_id = ?2",
                )?;
                let mut labelled = Vec::new();
                for message_id in message_ids {
                    if stmt.exists(rusqlite::params![message_id, label_id])? {
                        labelled.push(message_id);
                    }
                }
                Ok(labelled)
            })
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
    use crate::error::CoreError;
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

    #[tokio::test]
    async fn an_agent_changes_its_draft_only_until_the_user_does() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::start_mail_ui(crate::config::Paths::for_tests(temp.path()))
            .await
            .unwrap();
        core.db
            .write(|conn| {
                testutil::seed_account(conn);
                Ok(())
            })
            .await
            .unwrap();
        let draft = |draft_id: Option<i64>, text: &str| SaveDraftArgs {
            draft_id,
            account_id: 1,
            from: None,
            to: vec![Address {
                name: None,
                email: "alex@example.com".into(),
            }],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "Plans".into(),
            body_text: text.into(),
            body_html: None,
            mode: "new".into(),
            in_reply_to_message_id: None,
            in_reply_to_header: None,
            attachments: Vec::new(),
            request_read_receipt: false,
            importance: Default::default(),
        };

        let id = core
            .save_agent_draft(draft(None, "First"), "Test agent".into())
            .await
            .unwrap();
        assert!(core.agent_draft_unchanged(id).await.unwrap());
        let again = core
            .save_agent_draft(draft(Some(id), "Second"), "Test agent".into())
            .await
            .unwrap();
        assert_eq!(again, id, "saved in place");
        assert!(core.agent_draft_unchanged(id).await.unwrap());
        assert_eq!(core.get_draft(id).await.unwrap().body_text, "Second");

        // The user saves it from the composer: from now on it's theirs, even
        // for a save the agent started before.
        core.save_draft(draft(Some(id), "Mine now")).await.unwrap();
        assert!(!core.agent_draft_unchanged(id).await.unwrap());
        let late = core
            .save_agent_draft(draft(Some(id), "Third"), "Test agent".into())
            .await;
        assert!(matches!(late, Err(CoreError::NotFound(_))), "{late:?}");
        assert_eq!(core.get_draft(id).await.unwrap().body_text, "Mine now");
        // A draft the user wrote never was the agent's.
        let own = core.save_draft(draft(None, "My own")).await.unwrap();
        assert!(!core.agent_draft_unchanged(own).await.unwrap());
        let taken = core
            .save_agent_draft(draft(Some(own), "Taken"), "Test agent".into())
            .await;
        assert!(matches!(taken, Err(CoreError::NotFound(_))), "{taken:?}");
        assert_eq!(core.get_draft(own).await.unwrap().body_text, "My own");
    }

    #[tokio::test]
    async fn the_user_undoes_what_an_agent_changed_from_its_log_entry() {
        use crate::models::AgentOutcome;

        let temp = tempfile::tempdir().unwrap();
        let core = Core::start_mail_ui(crate::config::Paths::for_tests(temp.path()))
            .await
            .unwrap();
        let message_id = core
            .db
            .write(|conn| {
                testutil::seed_account(conn);
                conn.execute(
                    "INSERT INTO folders (id, account_id, path, role) VALUES (2, 1, 'Archive', 'archive')",
                    [],
                )?;
                Ok(testutil::seed_message(conn, "alice@test.dev", "Plans", false).1)
            })
            .await
            .unwrap();
        let folder = || async {
            core.db
                .read(move |conn| repo::messages::get_row(conn, message_id))
                .await
                .unwrap()
                .unwrap()
                .folder_id
        };
        let archived = core
            .perform_action(PerformActionArgs {
                kind: ActionKind::Archive,
                target: ActionTarget::Messages(vec![message_id]),
                params: None,
            })
            .await
            .unwrap()
            .action_ids;
        assert_eq!(folder().await, Some(2));
        core.record_agent_activity(NewAgentActivity {
            occurred_at: 1,
            agent: "Test agent".into(),
            tool: "move_messages".into(),
            account_id: Some(1),
            folder_id: Some(2),
            items: 1,
            outcome: AgentOutcome::Done,
            action_ids: archived,
            label_changes: Vec::new(),
        })
        .await
        .unwrap();
        let entry = core.agent_activity(1).await.unwrap().remove(0);
        assert!(entry.undoable);

        assert!(core.undo_agent_activity(entry.id).await.unwrap());
        assert_eq!(folder().await, Some(1), "back in the inbox");
        let entry = core.agent_activity(1).await.unwrap().remove(0);
        assert!(entry.undone && !entry.undoable);
        assert!(
            !core.undo_agent_activity(entry.id).await.unwrap(),
            "undone once"
        );
        assert_eq!(folder().await, Some(1));

        // A label it added comes off again.
        core.db
            .write(|conn| {
                conn.execute(
                    "INSERT INTO labels (id, name, keyword) VALUES (7, 'Later', 'later')",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        core.perform_action(PerformActionArgs {
            kind: ActionKind::AddLabel,
            target: ActionTarget::Messages(vec![message_id]),
            params: Some(ActionParams {
                wake_at: None,
                target_folder_id: None,
                label_id: Some(7),
            }),
        })
        .await
        .unwrap();
        assert_eq!(
            core.messages_with_label(vec![message_id], 7).await.unwrap(),
            [message_id]
        );
        core.record_agent_activity(NewAgentActivity {
            occurred_at: now_ms(),
            agent: "Test agent".into(),
            tool: "label_messages".into(),
            account_id: Some(1),
            folder_id: None,
            items: 1,
            outcome: AgentOutcome::Done,
            action_ids: Vec::new(),
            label_changes: vec![repo::agent_activity::LabelChange {
                message_id,
                label_id: 7,
                added: true,
            }],
        })
        .await
        .unwrap();
        let entry = core.agent_activity(1).await.unwrap().remove(0);
        assert_eq!(entry.tool, "label_messages");
        assert!(core.undo_agent_activity(entry.id).await.unwrap());
        assert!(
            core.messages_with_label(vec![message_id], 7)
                .await
                .unwrap()
                .is_empty()
        );
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
