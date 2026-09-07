//! Performance validation at scale: 100k messages / ~30k threads.
//! Gated: FLECTAR_MAIL_TEST_PERF=1 (seeding takes ~10-30s).

use flectar_mail_core::db::{Db, repo};
use flectar_mail_core::models::{ThreadCursor, View};
use std::time::Instant;

#[tokio::test(flavor = "multi_thread")]
async fn hundred_k_messages_stay_instant() {
    if std::env::var("FLECTAR_MAIL_TEST_PERF").as_deref() != Ok("1") {
        eprintln!("skipping (set FLECTAR_MAIL_TEST_PERF=1 to run)");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let db = Db::open(&tmp.path().join("perf.db")).unwrap();

    const MESSAGES: i64 = 100_000;
    const THREADS: i64 = 30_000;

    let seed_start = Instant::now();
    db.write(move |conn| {
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO accounts (id, email, display_name, provider, auth_kind, username,
                                   imap_host, imap_port, smtp_host, smtp_port, created_at)
             VALUES (1, 'perf@example.com', NULL, 'imap', 'password', 'perf',
                     'localhost', 993, 'localhost', 587, 0)",
            [],
        )?;
        tx.execute(
            "INSERT INTO folders (id, account_id, imap_name, role) VALUES (1, 1, 'INBOX', 'inbox')",
            [],
        )?;

        let words = [
            "meeting", "invoice", "deploy", "quarterly", "flight", "review", "budget",
            "onboarding", "design", "roadmap", "offsite", "launch", "metrics", "hiring",
        ];
        let now: i64 = 1_760_000_000_000;

        {
            let mut ins_thread = tx.prepare(
                "INSERT INTO threads (id, account_id, subject_norm, last_message_at) VALUES (?1, 1, ?2, 0)",
            )?;
            for t in 1..=THREADS {
                let subj = format!(
                    "{} {} update {t}",
                    words[(t as usize * 7) % words.len()],
                    words[(t as usize * 13) % words.len()]
                );
                ins_thread.execute(rusqlite::params![t, subj])?;
            }
        }
        {
            let mut ins_msg = tx.prepare(
                "INSERT INTO messages (id, account_id, thread_id, folder_id, uid, message_id,
                     subject, from_name, from_addr, date, is_read, is_automated, snippet, body_state)
                 VALUES (?1, 1, ?2, 1, ?1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 'cached')",
            )?;
            let mut ins_body = tx.prepare(
                "INSERT INTO message_bodies (message_id, text_body) VALUES (?1, ?2)",
            )?;
            for m in 1..=MESSAGES {
                let t = (m % THREADS) + 1;
                let w1 = words[(m as usize * 3) % words.len()];
                let w2 = words[(m as usize * 11) % words.len()];
                let subject = format!("{w1} {w2} update {t}");
                let body = format!(
                    "Hi team, quick note about the {w1} and the {w2}. \
                     Numbers attached, let me know what you think. ({m})"
                );
                ins_msg.execute(rusqlite::params![
                    m,
                    t,
                    format!("<perf{m}@example.com>"),
                    subject,
                    format!("Sender {}", m % 500),
                    format!("sender{}@example.com", m % 500),
                    now - (MESSAGES - m) * 60_000, // one per minute going back
                    (m % 3 != 0) as i64,
                    (m % 5 == 0) as i64,
                    format!("quick note about the {w1} and the {w2}"),
                ])?;
                ins_body.execute(rusqlite::params![m, body])?;
            }
        }
        // FTS index in one statement.
        tx.execute(
            "INSERT INTO messages_fts (rowid, subject, from_text, to_text, body)
             SELECT m.id, m.subject, COALESCE(m.from_name,'') || ' ' || COALESCE(m.from_addr,''),
                    '', COALESCE(b.text_body, '')
             FROM messages m LEFT JOIN message_bodies b ON b.message_id = m.id",
            [],
        )?;
        // Thread aggregates in bulk.
        tx.execute(
            "UPDATE threads SET
                last_message_at = (SELECT MAX(date) FROM messages WHERE thread_id = threads.id),
                message_count   = (SELECT COUNT(*) FROM messages WHERE thread_id = threads.id),
                unread_count    = (SELECT COUNT(*) FROM messages WHERE thread_id = threads.id AND is_read = 0),
                snippet         = (SELECT snippet FROM messages WHERE thread_id = threads.id ORDER BY date DESC LIMIT 1)",
            [],
        )?;
        tx.commit()?;
        Ok(())
    })
    .await
    .unwrap();
    eprintln!(
        "seeded {MESSAGES} messages / {THREADS} threads in {:?}",
        seed_start.elapsed()
    );

    // Warm-up (first query pays page-cache costs).
    db.read(|conn| {
        repo::threads::list(
            conn,
            &repo::threads::ListArgs {
                view: View::Inbox,
                tab: None,
                account_id: None,
                folder_id: None,
                cursor: None,
                limit: 50,
            },
        )
    })
    .await
    .unwrap();

    // 1. Inbox page.
    let t0 = Instant::now();
    let page = db
        .read(|conn| {
            repo::threads::list(
                conn,
                &repo::threads::ListArgs {
                    view: View::Inbox,
                    tab: None,
                    account_id: None,
                    folder_id: None,
                    cursor: None,
                    limit: 50,
                },
            )
        })
        .await
        .unwrap();
    let list_ms = t0.elapsed().as_millis();
    assert_eq!(page.threads.len(), 50);

    // 2. Exact sidebar total without materializing every thread summary.
    let t0 = Instant::now();
    let inbox_count = db
        .read(|conn| {
            repo::threads::count(
                conn,
                &repo::threads::ListArgs {
                    view: View::Inbox,
                    tab: None,
                    account_id: None,
                    folder_id: None,
                    cursor: None,
                    limit: 1,
                },
            )
        })
        .await
        .unwrap();
    let count_ms = t0.elapsed().as_millis();
    assert_eq!(inbox_count, THREADS as usize);

    // All native sidebar badges are refreshed together after mutations.
    let t0 = Instant::now();
    let badges = db
        .read(|conn| repo::counts::mailbox_badge_counts(conn))
        .await
        .unwrap();
    let badge_ms = t0.elapsed().as_millis();
    assert_eq!(badges.len(), 1);
    assert_eq!(badges[0].inbox, THREADS / 3);

    // 3. Paginated deep fetch (cursor into the middle).
    let t0 = Instant::now();
    let deep = db
        .read({
            let cursor = page.threads.last().unwrap().last_message_at;
            move |conn| {
                repo::threads::list(
                    conn,
                    &repo::threads::ListArgs {
                        view: View::Inbox,
                        tab: None,
                        account_id: None,
                        folder_id: None,
                        cursor: Some(ThreadCursor {
                            last_message_at: cursor - 500_000_000,
                            thread_id: i64::MAX,
                        }),
                        limit: 50,
                    },
                )
            }
        })
        .await
        .unwrap();
    let deep_ms = t0.elapsed().as_millis();
    assert_eq!(deep.threads.len(), 50);

    // 4. FTS search across all 100k bodies.
    let t0 = Instant::now();
    let hits = db
        .read(|conn| {
            let q = flectar_mail_core::search::parse("quarterly numbers");
            repo::search::search(conn, &q, 50)
        })
        .await
        .unwrap();
    let search_ms = t0.elapsed().as_millis();
    assert!(!hits.is_empty(), "search should hit");

    // 5. Chronological FTS across the complete result set, matching the mail
    // list UI path. "update" appears in every subject, so this guards the
    // worst-case cost of ordering all matching threads by newest message.
    let t0 = Instant::now();
    let chronological = db
        .read(|conn| {
            let q = flectar_mail_core::search::parse("update");
            repo::search::chronological(conn, &q, 50)
        })
        .await
        .unwrap();
    let chronological_ms = t0.elapsed().as_millis();
    assert_eq!(chronological.len(), 50);

    // 6. Search with operator filter.
    let t0 = Instant::now();
    let hits2 = db
        .read(|conn| {
            let q = flectar_mail_core::search::parse("from:sender42 meeting");
            repo::search::search(conn, &q, 50)
        })
        .await
        .unwrap();
    let op_ms = t0.elapsed().as_millis();
    let _ = hits2;

    // 7. Full hybrid pipeline (lexical + OR fallback + RRF + personal boosts
    //    + hydration) - the path the search screen hits per keystroke.
    let t0 = Instant::now();
    let hits3 = db
        .read(|conn| {
            let q = flectar_mail_core::search::parse("quarterly numbers");
            let lex = repo::search::lexical_thread_ids(conn, &q, repo::search::candidate_cap(60))?;
            repo::search::fuse(conn, &q, lex, &[], 60)
        })
        .await
        .unwrap();
    let hybrid_ms = t0.elapsed().as_millis();
    assert!(!hits3.is_empty());

    eprintln!(
        "perf: inbox page {list_ms}ms · inbox count {count_ms}ms · live badges {badge_ms}ms · deep page {deep_ms}ms · fts {search_ms}ms · chronological fts {chronological_ms}ms · fts+operator {op_ms}ms · hybrid {hybrid_ms}ms"
    );
    assert!(list_ms < 50, "inbox page took {list_ms}ms (budget 50ms)");
    assert!(
        count_ms < 100,
        "inbox count took {count_ms}ms (budget 100ms)"
    );
    assert!(
        badge_ms < 150,
        "live sidebar badges took {badge_ms}ms (budget 150ms)"
    );
    assert!(deep_ms < 50, "deep page took {deep_ms}ms (budget 50ms)");
    assert!(search_ms < 100, "search took {search_ms}ms (budget 100ms)");
    assert!(
        chronological_ms < 150,
        "chronological search took {chronological_ms}ms (budget 150ms)"
    );
    assert!(op_ms < 150, "operator search took {op_ms}ms (budget 150ms)");
    assert!(
        hybrid_ms < 150,
        "hybrid search took {hybrid_ms}ms (budget 150ms)"
    );
}
