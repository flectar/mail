//! Mail for AI agents using the local MCP server (#237). The app decides what
//! an agent may see from `AgentAccess` and calls these for the rest: mail as
//! sync stored it, end-to-end encrypted messages recognized without
//! decrypting them, and the activity log.

use crate::Core;
use crate::db::repo;
use crate::db::repo::agent_activity::{NewAgentActivity, Undo};
use crate::error::Result;
use crate::models::{
    ActionKind, ActionParams, ActionTarget, Address, AgentAccess, AgentActivity, MessageCursor,
    MessageDetail, MessagePage, MessageSearchCursor, MessageSearchPage, PerformActionArgs,
    SaveDraftArgs, now_ms,
};
use crate::search::{Filter, ParsedQuery};
use std::collections::BTreeMap;

impl Core {
    /// Messages matching `query` in any of `folder_ids`, newest first, that
    /// an agent may read when it may read the folders `readable`. See
    /// [`Self::agent_folder_ids`].
    pub async fn search_messages_in_folders(
        &self,
        query: String,
        folder_ids: Vec<i64>,
        readable: Vec<i64>,
        cursor: Option<MessageSearchCursor>,
        limit: i64,
    ) -> Result<MessageSearchPage> {
        let mut parsed = crate::search::parse(&query);
        parsed.folder_ids = Some(folder_ids);
        parsed.agent_readable = Some(readable);
        self.search_messages_parsed(parsed, cursor, limit).await
    }

    /// The messages in `folder_id`, newest first, that an agent may read
    /// when it may read the folders `readable`, and only the unread ones with
    /// `unread_only`. See [`Self::agent_folder_ids`].
    pub async fn agent_folder_messages(
        &self,
        folder_id: i64,
        readable: Vec<i64>,
        unread_only: bool,
        cursor: Option<MessageCursor>,
        limit: i64,
    ) -> Result<MessagePage> {
        let query = ParsedQuery {
            filters: if unread_only {
                vec![Filter::Unread]
            } else {
                Vec::new()
            },
            folder_ids: Some(vec![folder_id]),
            agent_readable: Some(readable),
            ..ParsedQuery::default()
        };
        let limit = limit.clamp(1, 100);
        self.db
            .read(move |conn| repo::search::scoped_message_page(conn, &query, cursor, limit))
            .await
    }

    /// The folders that decide whether an agent may read `message_id`: its
    /// folder and every other folder or label it's in, except Gmail's All
    /// Mail, which every Gmail message is in. An agent may read the message
    /// only when it may read each of them, so a folder or label hidden from
    /// agents, as Spam and Trash are until the user shows them, wins over any
    /// other the message is in.
    pub async fn agent_folder_ids(&self, message_id: i64) -> Result<Vec<i64>> {
        self.db
            .read(move |conn| repo::messages::agent_folder_ids(conn, message_id))
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
    fn every_folder_a_message_is_in_decides_for_agents_but_gmail_all_mail() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        conn.execute_batch(
            "INSERT INTO folders (id, account_id, path, role) VALUES
                (2, 1, 'Work', NULL), (3, 1, 'All Mail', 'all');",
        )
        .unwrap();
        let (_, message_id) = testutil::seed_message(&conn, "a@example.com", "Plans", false);
        conn.execute(
            "INSERT INTO message_folders (message_id, folder_id) VALUES (?1, 2), (?1, 3)",
            [message_id],
        )
        .unwrap();
        let folders = || repo::messages::agent_folder_ids(&conn, message_id).unwrap();
        // Each folder a message is in counts, as a JMAP message is in each of
        // its mailboxes, and a server's own All Mail is a folder like any
        // other.
        assert_eq!(folders(), [1, 2, 3]);
        // Gmail's All Mail has every message, Spam and Trash too, so it
        // decides nothing; labels do.
        conn.execute(
            "UPDATE accounts SET provider = 'gmail', mail_protocol = 'gmail' WHERE id = 1",
            [],
        )
        .unwrap();
        assert_eq!(folders(), [1, 2]);
        // A message only in All Mail is in no folder an agent could read it in.
        conn.execute(
            "UPDATE messages SET folder_id = 3 WHERE id = ?1",
            [message_id],
        )
        .unwrap();
        conn.execute(
            "DELETE FROM message_folders WHERE message_id = ?1 AND folder_id = 2",
            [message_id],
        )
        .unwrap();
        assert!(folders().is_empty());
    }

    /// A Gmail account as sync stores it: Inbox, the synthetic Archive and
    /// All Mail, Spam, Trash and a user label, with every message in All Mail.
    /// Returns the ids of a message in the Inbox, one in Spam, one in the Inbox
    /// under the label, one archived, and one sync placed only in All Mail.
    fn seed_gmail(conn: &rusqlite::Connection) -> [i64; 5] {
        testutil::seed_account(conn);
        conn.execute_batch(
            "UPDATE accounts SET provider = 'gmail', mail_protocol = 'gmail' WHERE id = 1;
             INSERT INTO folders (id, account_id, path, role) VALUES
                (2, 1, '[Gmail]/Archive', 'archive'),
                (3, 1, '[Gmail]/All Mail', 'all'),
                (4, 1, 'SPAM', 'spam'),
                (5, 1, 'TRASH', 'trash'),
                (6, 1, 'Taxes', NULL);",
        )
        .unwrap();
        // Each message, oldest first: its folder, its labels besides All Mail,
        // and whether it's read.
        let shapes: [(i64, &[i64], bool); 5] = [
            (1, &[1], true),
            (4, &[4], false),
            (1, &[1, 6], true),
            (2, &[2], false),
            (3, &[], true),
        ];
        let mut ids = [0; 5];
        for (index, (folder, labels, read)) in shapes.into_iter().enumerate() {
            let (_, id) = testutil::seed_message(conn, "a@example.com", "quarterly plans", false);
            conn.execute(
                "UPDATE messages SET folder_id = ?2, date = ?3, is_read = ?4 WHERE id = ?1",
                rusqlite::params![id, folder, 1000 + index as i64, read],
            )
            .unwrap();
            for label in labels.iter().chain(&[3]) {
                conn.execute(
                    "INSERT INTO message_folders (message_id, folder_id) VALUES (?1, ?2)",
                    [id, *label],
                )
                .unwrap();
            }
            repo::search::index_message(conn, id).unwrap();
            ids[index] = id;
        }
        ids
    }

    /// The folders an agent may read under `access`, as the app works them
    /// out: selectable folders `AgentAccess::shows_folder` keeps.
    async fn readable(core: &Core, access: &AgentAccess) -> Vec<i64> {
        core.list_folders(None)
            .await
            .unwrap()
            .iter()
            .filter(|folder| folder.selectable && access.shows_folder(folder))
            .map(|folder| folder.id)
            .collect()
    }

    #[tokio::test]
    async fn on_gmail_spam_and_hidden_labels_stay_hidden_from_agents_everywhere() {
        let temp = tempfile::tempdir().unwrap();
        let core = Core::start_mail_ui(crate::config::Paths::for_tests(temp.path()))
            .await
            .unwrap();
        let [inbox, spam, labeled, archived, all_only] =
            core.db.write(|conn| Ok(seed_gmail(conn))).await.unwrap();
        let mut access = AgentAccess {
            enabled: true,
            ..AgentAccess::default()
        };
        let sorted = |mut ids: Vec<i64>| {
            ids.sort_unstable();
            ids
        };
        // Every way an agent comes to mail agrees with the rule for one
        // message: each folder that decides it is one the agent may read.
        let check = async |access: &AgentAccess, expected: &[i64]| {
            let readable = readable(&core, access).await;
            for id in [inbox, spam, labeled, archived, all_only] {
                let folders = core.agent_folder_ids(id).await.unwrap();
                let shown = !folders.is_empty() && folders.iter().all(|id| readable.contains(id));
                assert_eq!(shown, expected.contains(&id), "message {id} in {folders:?}");
            }
            let found = core
                .search_messages_in_folders(
                    "quarterly".into(),
                    readable.clone(),
                    readable.clone(),
                    None,
                    50,
                )
                .await
                .unwrap();
            assert_eq!(
                sorted(found.messages.iter().map(|message| message.id).collect()),
                expected
            );
            // All Mail lists only what agents may read, as does a search in it.
            if readable.contains(&3) {
                let listed = core
                    .agent_folder_messages(3, readable.clone(), false, None, 50)
                    .await
                    .unwrap();
                assert_eq!(
                    sorted(listed.messages.iter().map(|message| message.id).collect()),
                    expected
                );
                let found = core
                    .search_messages_in_folders(
                        "quarterly".into(),
                        vec![3],
                        readable.clone(),
                        None,
                        50,
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    sorted(found.messages.iter().map(|message| message.id).collect()),
                    expected
                );
            }
        };

        // Spam and Trash stay out until the user shows them, though every
        // message is in All Mail, and so does mail sync left only there.
        check(&access, &[inbox, labeled, archived]).await;
        // Hiding a label hides its mail, though it's in the Inbox too.
        access.hidden_folders = vec![6];
        check(&access, &[inbox, archived]).await;
        let inbox_list = core
            .agent_folder_messages(1, readable(&core, &access).await, false, None, 50)
            .await
            .unwrap();
        assert_eq!(
            inbox_list
                .messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            [inbox]
        );
        // An agent that names Spam in its search finds nothing there.
        let readable_now = readable(&core, &access).await;
        let named = core
            .search_messages_in_folders(
                "in:spam quarterly".into(),
                readable_now.clone(),
                readable_now,
                None,
                50,
            )
            .await
            .unwrap();
        assert!(named.messages.is_empty());
        // Hiding the synthetic Archive hides archived mail, while hiding All
        // Mail only takes that folder away: the Inbox still shows.
        access.hidden_folders = vec![2, 3];
        check(&access, &[inbox, labeled]).await;
        // Once the user shows Spam, its mail is there for agents.
        access.hidden_folders.clear();
        access.shown_folders = vec![4];
        check(&access, &[inbox, spam, labeled, archived]).await;

        // Unread only, newest first, a page at a time.
        access.shown_folders.clear();
        let readable_now = readable(&core, &access).await;
        let first = core
            .agent_folder_messages(3, readable_now.clone(), false, None, 2)
            .await
            .unwrap();
        assert_eq!(
            first
                .messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            [archived, labeled]
        );
        let rest = core
            .agent_folder_messages(3, readable_now.clone(), false, first.next_cursor, 2)
            .await
            .unwrap();
        assert_eq!(
            rest.messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            [inbox]
        );
        assert!(rest.next_cursor.is_none());
        // The spam is unread too, but stays hidden.
        let unread = core
            .agent_folder_messages(3, readable_now, true, None, 50)
            .await
            .unwrap();
        assert_eq!(
            unread
                .messages
                .iter()
                .map(|message| message.id)
                .collect::<Vec<_>>(),
            [archived]
        );
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
