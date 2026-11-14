//! Marking everything a mail list shows as read (#234).
//!
//! A list that is whole mailboxes, such as an Inbox, a folder or a Gmail
//! label, is marked here and on the server, where one queued action marks
//! the mailbox through what the last check knew of it. That reaches mail
//! older than this device keeps, as Thunderbird, Evolution and K-9 Mail do,
//! and leaves alone mail that arrived since. A list that shows part of a
//! mailbox, such as a split inbox half or Starred, marks the messages this
//! device keeps that it shows, as ordinary read actions.

use crate::db::repo::{self, threads::ListArgs, threads::TabFilter};
use crate::error::Result;
use crate::models::View;
use rusqlite::{Connection, OptionalExtension, params};

/// What marking a list read does.
pub(crate) enum Outcome {
    /// The list was whole mailboxes, marked here and queued for their
    /// servers.
    Whole {
        threads: Vec<i64>,
        accounts: Vec<i64>,
        action_ids: Vec<i64>,
    },
    /// The list shows part of a mailbox: the unread messages this device
    /// keeps that it shows, to mark as ordinary read actions.
    Part(Vec<i64>),
}

/// A whole mailbox: a folder, or a Gmail label shown as a label.
struct Mailbox {
    account_id: i64,
    protocol: String,
    folder_id: Option<i64>,
    /// The Gmail label id and the local label it shows as.
    gmail_label: Option<(String, i64)>,
}

/// Mark what `args` lists as read at `now`, or say which messages to mark.
pub(crate) fn apply(conn: &mut Connection, args: &ListArgs, now: i64) -> Result<Outcome> {
    let tx = conn.transaction()?;
    let Some(mailboxes) = whole_mailboxes(&tx, args)? else {
        return Ok(Outcome::Part(repo::threads::unread_message_ids(&tx, args)?));
    };
    let mut threads = Vec::new();
    let mut accounts = Vec::new();
    let mut action_ids = Vec::new();
    for mailbox in mailboxes {
        for thread_id in mark_here(&tx, &mailbox)? {
            if !threads.contains(&thread_id) {
                threads.push(thread_id);
            }
        }
        if let Some(folder_id) = mailbox.folder_id {
            // Its unread mail that only the server has is read now too.
            tx.execute(
                "UPDATE folders SET server_only_unseen = 0
                 WHERE id = ?1 AND server_only_unseen IS NOT NULL",
                params![folder_id],
            )?;
        }
        if let Some(payload) = server_payload(&tx, &mailbox, now)? {
            action_ids.push(repo::actions::enqueue(
                &tx,
                mailbox.account_id,
                "mark_folder_read",
                None,
                None,
                &payload,
                None,
            )?);
            if !accounts.contains(&mailbox.account_id) {
                accounts.push(mailbox.account_id);
            }
        }
    }
    for &thread_id in &threads {
        repo::threads::recompute(&tx, thread_id)?;
    }
    tx.commit()?;
    Ok(Outcome::Whole {
        threads,
        accounts,
        action_ids,
    })
}

/// The whole mailboxes a list is, or `None` when it shows part of one.
fn whole_mailboxes(conn: &Connection, args: &ListArgs) -> Result<Option<Vec<Mailbox>>> {
    let in_scope = |account_id: i64| {
        args.account_id.is_none_or(|only| only == account_id)
            && args
                .account_ids
                .as_ref()
                .is_none_or(|accounts| accounts.contains(&account_id))
    };
    let role = match args.view {
        View::Inbox => Some(crate::models::roles::INBOX),
        View::Sent => Some(crate::models::roles::SENT),
        View::Drafts => Some(crate::models::roles::DRAFTS),
        View::Trash => Some(crate::models::roles::TRASH),
        View::Spam => Some(crate::models::roles::SPAM),
        // Starred and Snoozed are flags, Archive in a Gmail account is mail
        // without the Inbox label: parts of mailboxes.
        View::Starred | View::Snoozed | View::Done | View::All => None,
    };
    let folders: Vec<(i64, i64)> = match (&args.tab, args.folder_id, role) {
        (None, Some(folder_id), _) => conn
            .query_row(
                "SELECT id, account_id FROM folders WHERE id = ?1",
                params![folder_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .into_iter()
            .collect(),
        (None, None, Some(role)) => {
            let mut stmt = conn.prepare("SELECT id, account_id FROM folders WHERE role = ?1")?;
            stmt.query_map(params![role], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?
        }
        (Some(TabFilter::ManualLabel(label_id)), None, None) if args.view == View::All => {
            return gmail_label(conn, *label_id).map(|mailbox| {
                mailbox.map(|mailbox| {
                    if in_scope(mailbox.account_id) {
                        vec![mailbox]
                    } else {
                        Vec::new()
                    }
                })
            });
        }
        _ => return Ok(None),
    };
    let mut mailboxes = Vec::new();
    for (folder_id, account_id) in folders {
        if !in_scope(account_id) {
            continue;
        }
        let protocol = protocol_of(conn, account_id)?;
        let gmail_label = if protocol == "gmail" {
            conn.query_row(
                "SELECT provider_id FROM gmail_labels WHERE account_id = ?1 AND folder_id = ?2",
                params![account_id, folder_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|provider_id| (provider_id, 0))
        } else {
            None
        };
        mailboxes.push(Mailbox {
            account_id,
            protocol,
            folder_id: Some(folder_id),
            gmail_label,
        });
    }
    Ok(Some(mailboxes))
}

/// A label owned by a Gmail account is a Gmail label, a whole mailbox on
/// the server. Other labels are this device's own, kept on parts of
/// mailboxes.
fn gmail_label(conn: &Connection, label_id: i64) -> Result<Option<Mailbox>> {
    Ok(conn
        .query_row(
            "SELECT gl.account_id, gl.provider_id
             FROM gmail_labels gl JOIN accounts a ON a.id = gl.account_id
             WHERE gl.local_label_id = ?1 AND a.mail_protocol = 'gmail'",
            params![label_id],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
        .map(|(account_id, provider_id)| Mailbox {
            account_id,
            protocol: "gmail".into(),
            folder_id: None,
            gmail_label: Some((provider_id, label_id)),
        }))
}

fn protocol_of(conn: &Connection, account_id: i64) -> Result<String> {
    Ok(conn.query_row(
        "SELECT mail_protocol FROM accounts WHERE id = ?1",
        params![account_id],
        |row| row.get(0),
    )?)
}

/// Mark the mailbox's unread messages this device keeps; their threads.
fn mark_here(conn: &Connection, mailbox: &Mailbox) -> Result<Vec<i64>> {
    let rows: Vec<(i64, Option<i64>)> = match (mailbox.folder_id, &mailbox.gmail_label) {
        (Some(folder_id), _) => {
            let mut stmt = conn.prepare(
                "SELECT m.id, m.thread_id FROM messages m
                 WHERE m.is_read = 0 AND m.account_id = ?1
                   AND (m.folder_id = ?2
                        OR EXISTS (SELECT 1 FROM message_folders mf
                                   WHERE mf.message_id = m.id AND mf.folder_id = ?2))",
            )?;
            stmt.query_map(params![mailbox.account_id, folder_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        }
        (None, Some((_, label_id))) => {
            let mut stmt = conn.prepare(
                "SELECT m.id, m.thread_id FROM messages m
                 WHERE m.is_read = 0 AND m.account_id = ?1
                   AND EXISTS (SELECT 1 FROM message_labels ml
                               WHERE ml.message_id = m.id AND ml.label_id = ?2)",
            )?;
            stmt.query_map(params![mailbox.account_id, label_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        }
        (None, None) => Vec::new(),
    };
    let mut threads = Vec::new();
    for (message_id, thread_id) in rows {
        conn.execute(
            "UPDATE messages SET is_read = 1 WHERE id = ?1",
            params![message_id],
        )?;
        if let Some(thread_id) = thread_id
            && !threads.contains(&thread_id)
        {
            threads.push(thread_id);
        }
    }
    Ok(threads)
}

/// What the server is asked to mark, or `None` where there is no server or
/// nothing it knows of yet.
fn server_payload(
    conn: &Connection,
    mailbox: &Mailbox,
    now: i64,
) -> Result<Option<serde_json::Value>> {
    match mailbox.protocol.as_str() {
        "imap" => {
            let Some(folder_id) = mailbox.folder_id else {
                return Ok(None);
            };
            let Some(folder) = repo::folders::get(conn, folder_id)? else {
                return Ok(None);
            };
            // UIDNEXT less one is the newest message the last check knew of;
            // mail that came after it stays unread.
            let known: Option<i64> = conn.query_row(
                "SELECT MAX(uid) FROM messages WHERE folder_id = ?1",
                params![folder_id],
                |row| row.get(0),
            )?;
            let through = folder
                .uidnext
                .map(|next| next - 1)
                .into_iter()
                .chain(known)
                .max()
                .filter(|through| *through > 0);
            Ok(through.map(|through| {
                serde_json::json!({
                    "folderId": folder_id,
                    "throughUid": through,
                    "uidValidity": folder.uidvalidity,
                })
            }))
        }
        "jmap" => Ok(mailbox
            .folder_id
            .map(|folder_id| serde_json::json!({ "folderId": folder_id, "before": now }))),
        "gmail" => Ok(mailbox.gmail_label.as_ref().map(|(label, _)| {
            serde_json::json!({
                "folderId": mailbox.folder_id,
                "labelId": label,
                "before": now,
            })
        })),
        // POP3 keeps no read state on the server, and local mail has none.
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    fn args(view: View) -> ListArgs {
        ListArgs {
            view,
            tab: None,
            account_id: None,
            account_ids: None,
            folder_id: None,
            cursor: None,
            limit: 50,
            order: crate::models::ListOrder::NewestFirst,
        }
    }

    /// Account 1 (IMAP) has Inbox 1, last checked at UIDNEXT 41, with two
    /// unread messages and a read one, and Work (folder 2) with one unread.
    /// Account 2 (POP3) has Inbox 3 with one unread; account 3 (Gmail) has
    /// one unread message with its label "Plans" (label 90, Gmail Label_7).
    fn seed(conn: &Connection) {
        testutil::seed_account(conn);
        conn.execute_batch(
            "INSERT INTO accounts (id, email, provider, auth_kind, username, incoming_host,
                 incoming_port, smtp_host, smtp_port, created_at, mail_protocol, settings_json)
             SELECT 2, 'pop@test.dev', provider, auth_kind, username, incoming_host,
                 incoming_port, smtp_host, smtp_port, created_at, 'pop3', settings_json
             FROM accounts WHERE id = 1;
             INSERT INTO accounts (id, email, provider, auth_kind, username, incoming_host,
                 incoming_port, smtp_host, smtp_port, created_at, mail_protocol, settings_json)
             SELECT 3, 'g@test.dev', 'gmail', 'oauth2', username, incoming_host,
                 incoming_port, smtp_host, smtp_port, created_at, 'gmail', settings_json
             FROM accounts WHERE id = 1;
             UPDATE folders SET uidvalidity = 7, uidnext = 41, server_only_unseen = 5 WHERE id = 1;
             INSERT INTO folders (id, account_id, path, role) VALUES
               (2, 1, 'Work', NULL), (3, 2, 'INBOX', 'inbox'), (4, 3, 'INBOX', 'inbox');
             INSERT INTO labels (id, name, keyword, scope, owner_account_id, origin)
               VALUES (90, 'Plans', 'Plans', 'account', 3, 'provider');
             INSERT INTO gmail_labels (account_id, provider_id, name, kind, local_label_id)
               VALUES (3, 'Label_7', 'Plans', 'user', 90);
             INSERT INTO gmail_labels (account_id, provider_id, name, kind, folder_id)
               VALUES (3, 'INBOX', 'INBOX', 'system', 4);
             INSERT INTO threads (id, account_id, subject_norm, unread_count, last_message_at) VALUES
               (10, 1, 'a', 2, 1000), (11, 1, 'b', 1, 1000), (12, 2, 'c', 1, 1000),
               (13, 3, 'd', 1, 1000);
             INSERT INTO messages (id, thread_id, account_id, folder_id, uid, subject, date, is_read, is_starred)
               VALUES (100, 10, 1, 1, 10, 'a', 1000, 0, 0),
                      (101, 10, 1, 1, 20, 'a', 1001, 0, 1),
                      (102, 10, 1, 1, 30, 'a', 1002, 1, 0),
                      (103, 11, 1, 2, 5, 'b', 1003, 0, 1),
                      (104, 12, 2, 3, NULL, 'c', 1004, 0, 0),
                      (105, 13, 3, 4, NULL, 'd', 1005, 0, 0);
             INSERT INTO message_labels (message_id, label_id) VALUES (105, 90);",
        )
        .unwrap();
    }

    fn unread(conn: &Connection) -> Vec<i64> {
        let mut stmt = conn
            .prepare("SELECT id FROM messages WHERE is_read = 0 ORDER BY id")
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap()
    }

    fn queued(conn: &Connection) -> Vec<(i64, serde_json::Value)> {
        let mut stmt = conn
            .prepare(
                "SELECT account_id, payload FROM pending_actions
                 WHERE kind = 'mark_folder_read' ORDER BY id",
            )
            .unwrap();
        stmt.query_map([], |row| {
            Ok((
                row.get(0)?,
                serde_json::from_str(&row.get::<_, String>(1)?).unwrap(),
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
    }

    #[test]
    fn a_whole_inbox_is_marked_here_and_on_the_server_through_the_last_check() {
        let mut conn = testutil::conn();
        seed(&conn);
        let mut only_imap = args(View::Inbox);
        only_imap.account_id = Some(1);
        let Outcome::Whole {
            threads, accounts, ..
        } = apply(&mut conn, &only_imap, 5000).unwrap()
        else {
            panic!("an Inbox is a whole mailbox");
        };
        assert_eq!((threads, accounts), (vec![10], vec![1]));
        assert_eq!(unread(&conn), vec![103, 104, 105]);
        assert_eq!(
            queued(&conn),
            vec![(
                1,
                serde_json::json!({ "folderId": 1, "throughUid": 40, "uidValidity": 7 })
            )]
        );
        let (server_only, thread_unread): (Option<i64>, i64) = conn
            .query_row(
                "SELECT (SELECT server_only_unseen FROM folders WHERE id = 1),
                        (SELECT unread_count FROM threads WHERE id = 10)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!((server_only, thread_unread), (Some(0), 0));
        // Until the server has it, a check keeps them read here.
        assert!(repo::actions::has_pending_for_message(&conn, 100).unwrap());
        assert!(!repo::actions::has_pending_for_message(&conn, 103).unwrap());
    }

    #[test]
    fn every_inbox_of_the_unified_inbox_and_pop3_only_here() {
        let mut conn = testutil::conn();
        seed(&conn);
        let Outcome::Whole { accounts, .. } = apply(&mut conn, &args(View::Inbox), 5000).unwrap()
        else {
            panic!("the unified Inbox is whole mailboxes");
        };
        assert_eq!(unread(&conn), vec![103]);
        // POP3 keeps no read state on the server.
        assert_eq!(accounts, vec![1, 3]);
        let queued = queued(&conn);
        assert_eq!(queued.len(), 2, "{queued:?}");
        assert_eq!(
            queued[1],
            (
                3,
                serde_json::json!({ "folderId": 4, "labelId": "INBOX", "before": 5000 })
            )
        );
    }

    #[test]
    fn a_folder_and_a_gmail_label_are_whole_mailboxes() {
        let mut conn = testutil::conn();
        seed(&conn);
        let mut work = args(View::All);
        work.folder_id = Some(2);
        assert!(matches!(
            apply(&mut conn, &work, 5000).unwrap(),
            Outcome::Whole { .. }
        ));
        assert_eq!(unread(&conn), vec![100, 101, 104, 105]);

        let mut plans = args(View::All);
        plans.tab = Some(TabFilter::ManualLabel(90));
        assert!(matches!(
            apply(&mut conn, &plans, 5000).unwrap(),
            Outcome::Whole { .. }
        ));
        assert_eq!(unread(&conn), vec![100, 101, 104]);
        assert_eq!(
            queued(&conn).last().unwrap(),
            &(
                3,
                serde_json::json!({ "folderId": null, "labelId": "Label_7", "before": 5000 })
            )
        );
    }

    #[test]
    fn part_of_a_mailbox_marks_only_the_mail_it_shows() {
        let mut conn = testutil::conn();
        seed(&conn);
        let Outcome::Part(mut ids) = apply(&mut conn, &args(View::Starred), 5000).unwrap() else {
            panic!("Starred is part of mailboxes");
        };
        ids.sort_unstable();
        assert_eq!(ids, vec![101, 103]);
        // Nothing changed yet: they go as ordinary read actions.
        assert_eq!(unread(&conn), vec![100, 101, 103, 104, 105]);
        assert!(queued(&conn).is_empty());
    }
}
