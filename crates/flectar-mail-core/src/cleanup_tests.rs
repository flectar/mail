use super::*;
use crate::accounts::credentials::DevelopmentFileCredentialStore;
use crate::config::Paths;
use crate::db::testutil;
use std::sync::Arc;

const DAY: i64 = DAY_MS;

/// Folders 2 and 3 of account 1 are its Trash and Junk.
fn conn() -> Connection {
    let conn = testutil::conn();
    testutil::seed_account(&conn);
    conn.execute(
        "INSERT INTO folders (id, account_id, path, role) VALUES
           (2, 1, 'Trash', 'trash'), (3, 1, 'Junk', 'spam'), (4, 1, 'Labels', NULL)",
        [],
    )
    .unwrap();
    conn
}

fn folder_since(conn: &Connection, message_id: i64) -> Option<i64> {
    conn.query_row(
        "SELECT folder_since FROM messages WHERE id = ?1",
        [message_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn age(conn: &Connection, message_id: i64, days: i64) {
    conn.execute(
        "UPDATE messages SET folder_since = ?2 WHERE id = ?1",
        params![message_id, now_ms() - days * DAY],
    )
    .unwrap();
}

#[test]
fn time_in_a_folder_counts_from_when_mail_went_there() {
    let conn = conn();
    let (_, message) = testutil::seed_message(&conn, "a@example.com", "Old", false);
    let arrived = folder_since(&conn, message).expect("set as it arrives");
    assert!((now_ms() - arrived).abs() < 60_000);
    // Long in the Inbox, then thrown away: it counts from now.
    age(&conn, message, 400);
    conn.execute("UPDATE messages SET folder_id = 2 WHERE id = ?1", [message])
        .unwrap();
    let thirty_days_ago = now_ms() - 30 * DAY;
    assert!(
        old_mail(&conn, roles::TRASH, thirty_days_ago, 10)
            .unwrap()
            .is_empty()
    );
    // Forty days on, it goes.
    age(&conn, message, 40);
    assert_eq!(
        old_mail(&conn, roles::TRASH, thirty_days_ago, 10)
            .unwrap()
            .len(),
        1
    );
    assert!(
        old_mail(&conn, roles::SPAM, thirty_days_ago, 10)
            .unwrap()
            .is_empty()
    );
    // Other changes keep the time.
    conn.execute("UPDATE messages SET is_read = 1 WHERE id = ?1", [message])
        .unwrap();
    assert_eq!(
        old_mail(&conn, roles::TRASH, thirty_days_ago, 10)
            .unwrap()
            .len(),
        1
    );
    // Starred mail stays.
    conn.execute(
        "UPDATE messages SET is_starred = 1 WHERE id = ?1",
        [message],
    )
    .unwrap();
    assert!(
        old_mail(&conn, roles::TRASH, thirty_days_ago, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_folder_a_message_stays_in_keeps_its_time() {
    let conn = conn();
    let (thread, message) = testutil::seed_message(&conn, "a@example.com", "Labeled", false);
    repo::gmail::set_message_folders(&conn, message, &[1, 2]).unwrap();
    let since = |folder: i64| -> Option<i64> {
        conn.query_row(
            "SELECT since FROM message_folders WHERE message_id = ?1 AND folder_id = ?2",
            params![message, folder],
            |row| row.get(0),
        )
        .unwrap()
    };
    assert!(since(2).is_some());
    let long_ago = now_ms() - 40 * DAY;
    conn.execute(
        "UPDATE message_folders SET since = ?2 WHERE message_id = ?1",
        params![message, long_ago],
    )
    .unwrap();
    // A sync that names the same folders and one more changes only that one.
    repo::gmail::set_message_folders(&conn, message, &[1, 2, 4]).unwrap();
    assert_eq!(since(2), Some(long_ago));
    assert!(since(4).unwrap() > long_ago);
    assert_eq!(
        old_mail(&conn, roles::TRASH, now_ms() - 30 * DAY, 10).unwrap(),
        [(message, thread)]
    );
    // Out of Trash and back, it counts from then.
    repo::gmail::set_message_folders(&conn, message, &[1]).unwrap();
    repo::gmail::set_message_folders(&conn, message, &[1, 2]).unwrap();
    assert!(since(2).unwrap() > long_ago);
    assert!(
        old_mail(&conn, roles::TRASH, now_ms() - 30 * DAY, 10)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn trash_and_junk_lose_what_sat_there_longer_than_set() {
    let directory = tempfile::tempdir().unwrap();
    let credentials = Arc::new(DevelopmentFileCredentialStore::new(
        directory.path().join("credentials.json"),
    ));
    let core =
        Core::start_mail_ui_with_credentials(Paths::for_tests(directory.path()), credentials)
            .await
            .unwrap();
    let (old, recent, junk) = core
        .db
        .write(|conn| {
            testutil::seed_account(conn);
            conn.execute(
                "INSERT INTO folders (id, account_id, path, role) VALUES
                   (2, 1, 'Trash', 'trash'), (3, 1, 'Junk', 'spam')",
                [],
            )?;
            let mut ids = Vec::new();
            for (subject, folder, days) in [("Old", 2, 40), ("Recent", 2, 3), ("Junk", 3, 40)] {
                let (_, message) = testutil::seed_message(conn, "a@example.com", subject, false);
                conn.execute(
                    "UPDATE messages SET folder_id = ?2 WHERE id = ?1",
                    params![message, folder],
                )?;
                conn.execute(
                    "UPDATE messages SET folder_since = ?2 WHERE id = ?1",
                    params![message, now_ms() - days * DAY],
                )?;
                ids.push(message);
            }
            Ok((ids[0], ids[1], ids[2]))
        })
        .await
        .unwrap();
    // Nothing set, nothing goes.
    assert_eq!(core.empty_old_mail().await.unwrap(), 0);
    core.set_folder_cleanup(30, 0).await.unwrap();
    let queued = |message: i64| {
        let db = core.db.clone();
        async move {
            db.read(move |conn| {
                Ok(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM pending_actions
                     WHERE message_id = ?1 AND kind = 'delete_permanently')",
                    [message],
                    |row| row.get::<_, bool>(0),
                )?)
            })
            .await
            .unwrap()
        }
    };
    assert!(queued(old).await);
    assert!(!queued(recent).await);
    assert!(!queued(junk).await, "Junk is kept for ever");
    // Asked again, nothing goes twice.
    assert_eq!(core.empty_old_mail().await.unwrap(), 0);
    core.set_folder_cleanup(30, 30).await.unwrap();
    assert!(queued(junk).await);
    let settings = core
        .db
        .read(|conn| repo::settings::get(conn))
        .await
        .unwrap();
    assert_eq!(
        (
            settings.empty_trash_after_days,
            settings.empty_junk_after_days
        ),
        (30, 30)
    );
}
