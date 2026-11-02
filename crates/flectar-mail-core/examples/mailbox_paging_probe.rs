//! Exercise large offline mailbox paging and mutations on a copied fixture.
//! Usage: cargo run -p flectar-mail-core --example mailbox_paging_probe --
//!        /path/to/profile-100000

use flectar_mail_core::{
    db::{Db, repo},
    error::Result,
    models::{ThreadCursor, ThreadPage, View},
};
use serde_json::json;
use std::{path::Path, time::Instant};

async fn page(
    db: &Db,
    account_id: Option<i64>,
    cursor: Option<ThreadCursor>,
) -> Result<ThreadPage> {
    db.read(move |conn| {
        repo::threads::list(
            conn,
            &repo::threads::ListArgs {
                view: View::Inbox,
                tab: None,
                account_id,
                account_ids: None,
                folder_id: None,
                cursor,
                limit: 25,
            },
        )
    })
    .await
}

async fn count(db: &Db, account_id: Option<i64>) -> Result<usize> {
    db.read(move |conn| {
        repo::threads::count(
            conn,
            &repo::threads::ListArgs {
                view: View::Inbox,
                tab: None,
                account_id,
                account_ids: None,
                folder_id: None,
                cursor: None,
                limit: 25,
            },
        )
    })
    .await
}

fn percentile(sorted: &[f64], numerator: usize, denominator: usize) -> f64 {
    sorted[(sorted.len() - 1) * numerator / denominator]
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let source = std::env::args()
        .nth(1)
        .ok_or("pass the directory containing flectar-mail.db")?;
    let temp = tempfile::tempdir()?;
    let path = temp.path().join("flectar-mail.db");
    std::fs::copy(Path::new(&source).join("flectar-mail.db"), &path)?;
    let db = Db::open(&path)?;
    let (original_count, newest_date) = db
        .read(|conn| {
            Ok((
                conn.query_row("SELECT COUNT(*) FROM threads", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                conn.query_row("SELECT MAX(last_message_at) FROM threads", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            ))
        })
        .await?;
    assert_eq!(original_count, 100_000, "probe expects the 100k fixture");
    let initial = page(&db, Some(1), None).await?;
    let old_anchor = initial.next_cursor.expect("first page has a cursor");
    let old_second = page(&db, Some(1), Some(old_anchor)).await?;
    assert_eq!(initial.threads.len(), 25);
    assert_eq!(old_second.threads.len(), 25);
    assert_eq!(initial.threads.last().unwrap().id, old_anchor.thread_id);
    assert_eq!(old_second.threads[0].id, old_anchor.thread_id - 1);
    let initial_badges = db
        .read(|conn| repo::counts::mailbox_badge_counts(conn))
        .await?;
    assert_eq!(initial_badges.len(), 1);
    assert_eq!(initial_badges[0].inbox, 50_000);
    assert_eq!(count(&db, Some(1)).await?, 100_000);
    let deep_cursor = db
        .read(|conn| {
            conn.query_row(
                "SELECT last_message_at, id FROM threads WHERE id=50000",
                [],
                |row| {
                    Ok(ThreadCursor {
                        last_message_at: row.get(0)?,
                        thread_id: row.get(1)?,
                    })
                },
            )
            .map_err(Into::into)
        })
        .await?;
    let cursor_plan = db
        .read(move |conn| {
            repo::threads::list_query_plan(
                conn,
                &repo::threads::ListArgs {
                    view: View::Inbox,
                    tab: None,
                    account_id: Some(1),
                    account_ids: None,
                    folder_id: None,
                    cursor: Some(deep_cursor),
                    limit: 25,
                },
            )
        })
        .await?;
    assert!(
        cursor_plan.iter().any(|line| {
            line.contains("idx_threads_recent") && line.contains("last_message_at<?")
        }),
        "deep cursor should seek in the account index: {cursor_plan:?}"
    );

    let inserted_id = original_count + 1;
    let other_id = original_count + 2;
    db.write(move |conn| {
        let tx = conn.transaction()?;
        tx.execute_batch(
            "INSERT INTO accounts
             (id,email,provider,auth_kind,username,incoming_host,incoming_port,smtp_host,smtp_port,created_at,mail_protocol,settings_json)
             VALUES (2,'second@example.test','generic','password','second','localhost',993,'localhost',587,0,'imap','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}');
             INSERT INTO folders (id,account_id,path,role)
             VALUES (2,2,'INBOX','inbox');",
        )?;
        for (thread_id, account_id, folder_id, date) in [
            (inserted_id, 1, 1, newest_date + 1_000),
            (other_id, 2, 2, newest_date + 2_000),
        ] {
            tx.execute(
                "INSERT INTO threads
                 (id,account_id,subject_norm,last_message_at,message_count,unread_count,snippet)
                 VALUES (?1,?2,'new',?3,1,1,'new unread message')",
                rusqlite::params![thread_id, account_id, date],
            )?;
            tx.execute(
                "INSERT INTO messages
                 (account_id,thread_id,folder_id,uid,message_id,subject,from_addr,date,is_read)
                 VALUES (?1,?2,?3,?2,?4,'new','sender@example.test',?5,0)",
                rusqlite::params![
                    account_id,
                    thread_id,
                    folder_id,
                    format!("probe-new-{thread_id}"),
                    date
                ],
            )?;
        }
        tx.execute(
            "DELETE FROM messages WHERE thread_id=?1",
            rusqlite::params![old_anchor.thread_id],
        )?;
        tx.execute(
            "DELETE FROM threads WHERE id=?1",
            rusqlite::params![old_anchor.thread_id],
        )?;
        tx.commit()?;
        Ok(())
    })
    .await?;

    let after_badges = db
        .read(|conn| repo::counts::mailbox_badge_counts(conn))
        .await?;
    assert_eq!(after_badges.len(), 2);
    assert_eq!(after_badges[0].account_id, 1);
    assert_eq!(after_badges[0].inbox, 50_000);
    assert_eq!(after_badges[1].account_id, 2);
    assert_eq!(after_badges[1].inbox, 1);
    assert_eq!(count(&db, Some(1)).await?, 100_000);
    assert_eq!(count(&db, Some(2)).await?, 1);
    assert_eq!(count(&db, None).await?, 100_001);
    let refreshed = page(&db, Some(1), None).await?;
    assert_eq!(refreshed.threads[0].id, inserted_id);
    assert!(
        !refreshed
            .threads
            .iter()
            .any(|row| row.id == old_anchor.thread_id)
    );
    let after_old_anchor = page(&db, Some(1), Some(old_anchor)).await?;
    assert_eq!(after_old_anchor.threads[0].id, old_second.threads[0].id);
    let switched = page(&db, Some(2), None).await?;
    assert_eq!(switched.threads.len(), 1);
    assert_eq!(switched.threads[0].id, other_id);
    assert!(switched.next_cursor.is_none());

    let mut cursor = None;
    let mut previous_id = i64::MAX;
    let mut visited = 0_usize;
    let mut page_ms = Vec::new();
    loop {
        let started = Instant::now();
        let result = page(&db, Some(1), cursor).await?;
        page_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        for row in &result.threads {
            assert_eq!(row.account_id, 1);
            assert!(row.id < previous_id, "duplicate or reordered thread ID");
            assert_ne!(row.id, old_anchor.thread_id);
            previous_id = row.id;
            visited += 1;
        }
        cursor = result.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(visited, 100_000);
    assert_eq!(previous_id, 1);
    page_ms.sort_by(f64::total_cmp);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "fixture_threads": original_count,
            "initial_unread": initial_badges[0].inbox,
            "after_unread_account_1": after_badges[0].inbox,
            "after_unread_account_2": after_badges[1].inbox,
            "inserted_thread": inserted_id,
            "deleted_anchor": old_anchor.thread_id,
            "account_1_visited": visited,
            "account_2_rows": switched.threads.len(),
            "pages": page_ms.len(),
            "page_ms_p50": percentile(&page_ms, 50, 100),
            "page_ms_p95": percentile(&page_ms, 95, 100),
            "page_ms_max": page_ms.last(),
            "cursor_plan": cursor_plan,
        }))?
    );
    Ok(())
}
