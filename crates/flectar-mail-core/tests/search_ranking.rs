//! Search quality: accent-insensitive contact suggestions, the đ/d query
//! variant, OR fallback for multi-term queries, and affinity-personalized
//! ranking.

use flectar_mail_core::db::repo;
use flectar_mail_core::models::Address;
use flectar_mail_core::search;
use rusqlite::Connection;

fn db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    flectar_mail_core::db::migrations::run(&mut conn).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id,email,provider,auth_kind,username,imap_host,imap_port,smtp_host,smtp_port,created_at)
           VALUES (1,'me@example.com','imap','password','me','h',993,'h',465,0);
         INSERT INTO folders (id,account_id,imap_name,role) VALUES (1,1,'INBOX','inbox');",
    )
    .unwrap();
    conn
}

fn addr(name: &str, email: &str) -> Address {
    Address {
        name: Some(name.to_string()),
        email: email.to_string(),
    }
}

fn insert_message(
    conn: &Connection,
    id: i64,
    thread: i64,
    subject: &str,
    from: &Address,
    date: i64,
) {
    conn.execute(
        "INSERT OR IGNORE INTO threads (id,account_id,subject_norm,last_message_at,message_count)
         VALUES (?1,1,?2,?3,1)",
        rusqlite::params![thread, subject.to_lowercase(), date],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (id,account_id,thread_id,folder_id,uid,subject,from_name,from_addr,date,is_read,body_state)
         VALUES (?1,1,?2,1,?1,?3,?4,?5,?6,1,'cached')",
        rusqlite::params![id, thread, subject, from.name, from.email, date],
    )
    .unwrap();
    repo::search::index_message(conn, id).unwrap();
}

#[test]
fn fold_strips_vietnamese_diacritics() {
    assert_eq!(search::fold("Bé Dọn Dẹp"), "be don dep");
    assert_eq!(search::fold("Đơn hàng"), "don hang");
    assert_eq!(search::fold("Café Ñandú"), "cafe nandu");
}

#[test]
fn suggest_matches_unaccented_input_ranked_by_affinity() {
    let conn = db();
    let cleaner = addr("Bé Dọn Dẹp", "hello@begroup.vn");
    let noise = addr("Ben Porter", "ben@example.com");
    // The cleaner writes often; Ben once.
    for i in 0..10 {
        repo::contacts::harvest(&conn, 1, &cleaner, false, 1000 + i).unwrap();
    }
    repo::contacts::harvest(&conn, 1, &noise, false, 2000).unwrap();

    let hits = repo::contacts::suggest(&conn, "be don dep", None, 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].email, "hello@begroup.vn");
    assert_eq!(hits[0].interactions, 10);

    // Single token: both match, the frequent contact first.
    let hits = repo::contacts::suggest(&conn, "be", None, 5).unwrap();
    assert_eq!(hits[0].email, "hello@begroup.vn");
    assert!(hits.iter().any(|c| c.email == "ben@example.com"));
}

#[test]
fn autocomplete_backfill_covers_pre_fold_rows() {
    let conn = db();
    conn.execute(
        "INSERT INTO contacts (email,name,send_count,recv_count,last_interacted)
         VALUES ('x@y.vn','Trần Đức',0,3,1)",
        [],
    )
    .unwrap();
    // Row predates the folded column; suggest can't see it until backfill.
    assert!(
        repo::contacts::suggest(&conn, "tran duc", None, 5)
            .unwrap()
            .is_empty()
    );
    repo::contacts::backfill_folded(&conn).unwrap();
    let hits = repo::contacts::suggest(&conn, "tran duc", None, 5).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].email, "x@y.vn");
}

#[test]
fn unaccented_query_finds_accented_mail_including_leading_d() {
    let conn = db();
    let be = addr("BE GROUP", "hi@begroup.vn");
    insert_message(&conn, 100, 10, "Bé dọn dẹp nhà cuối tuần", &be, 1_000);
    insert_message(&conn, 101, 20, "Đơn hàng của bạn", &be, 2_000);

    let q = search::parse("don dep");
    let hits = repo::search::hybrid(&conn, &q, &[], 10).unwrap();
    assert_eq!(hits[0].id, 10, "vowel diacritics fold at index time");

    let q = search::parse("don hang");
    let hits = repo::search::hybrid(&conn, &q, &[], 10).unwrap();
    assert!(
        hits.iter().any(|t| t.id == 20),
        "leading đ matched via the d-variant expansion"
    );
}

#[test]
fn multi_term_query_falls_back_to_or_when_and_is_empty() {
    let conn = db();
    let be = addr("BE GROUP", "hi@begroup.vn");
    insert_message(&conn, 100, 10, "House cleaning schedule", &be, 1_000);

    // "cleaning zzzqx": no message has both terms - OR fallback still finds it.
    let q = search::parse("cleaning zzzqx");
    let hits = repo::search::hybrid(&conn, &q, &[], 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, 10);
}

#[test]
fn frequent_sender_outranks_equal_match_from_stranger() {
    let conn = db();
    let friend = addr("BE GROUP", "hi@begroup.vn");
    let stranger = addr("Random Shop", "noreply@shop.com");
    // Same subject and date, so bm25 and recency tie; only affinity differs.
    insert_message(&conn, 100, 10, "House cleaning offer", &stranger, 5_000);
    insert_message(&conn, 101, 20, "House cleaning offer", &friend, 5_000);
    for i in 0..20 {
        repo::contacts::harvest(&conn, 1, &friend, i % 2 == 0, 1000 + i).unwrap();
    }
    repo::contacts::harvest(&conn, 1, &stranger, false, 1000).unwrap();

    let q = search::parse("house cleaning");
    let hits = repo::search::hybrid(&conn, &q, &[], 10).unwrap();
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].id, 20, "high-affinity sender ranks first");
}

#[test]
fn sender_name_match_outranks_body_only_match() {
    let conn = db();
    let be = addr("Bé Dọn Dẹp", "hello@begroup.vn");
    let other = addr("Random Shop", "noreply@shop.com");
    // Same date; "be" appears in the stranger's subject but IS the friend's name.
    insert_message(
        &conn,
        100,
        10,
        "Best deals to be had this week",
        &other,
        5_000,
    );
    insert_message(&conn, 101, 20, "Lịch dọn nhà tuần này", &be, 5_000);

    let q = search::parse("be");
    let hits = repo::search::hybrid(&conn, &q, &[], 10).unwrap();
    assert_eq!(
        hits[0].id, 20,
        "query matching the sender's name ranks that sender first"
    );
}

#[test]
fn chronological_search_puts_newer_body_match_before_older_subject_match() {
    let conn = db();
    let sender = addr("Reports", "reports@example.com");
    insert_message(&conn, 100, 10, "Quarterly report", &sender, 1_000);
    insert_message(&conn, 101, 20, "Status update", &sender, 2_000);
    conn.execute(
        "UPDATE messages SET snippet='Quarterly report attached' WHERE id=101",
        [],
    )
    .unwrap();
    repo::search::index_message(&conn, 101).unwrap();

    let q = search::parse("quarterly report");
    let ranked = repo::search::hybrid(&conn, &q, &[], 10).unwrap();
    assert_eq!(ranked[0].id, 10, "subject weighting drives relevance order");

    let timeline = repo::search::chronological(&conn, &q, 10).unwrap();
    assert_eq!(
        timeline.iter().map(|thread| thread.id).collect::<Vec<_>>(),
        [20, 10]
    );
}

#[test]
fn chronological_search_considers_matches_beyond_the_relevance_candidate_cap() {
    let conn = db();
    let sender = addr("Reports", "reports@example.com");

    // More than the relevance branch's 2,000-candidate cap have a strongly
    // weighted subject match. A newer body-only match must still lead the
    // chronological timeline.
    for offset in 0..2_001_i64 {
        let id = 10_000 + offset;
        insert_message(&conn, id, id, "Needle report", &sender, 1_000 + offset);
    }
    insert_message(&conn, 50_000, 50_000, "Status update", &sender, 100_000);
    conn.execute(
        "UPDATE messages SET snippet='Needle details attached' WHERE id=50000",
        [],
    )
    .unwrap();
    repo::search::index_message(&conn, 50_000).unwrap();

    let q = search::parse("needle");
    let timeline = repo::search::chronological(&conn, &q, 1).unwrap();
    assert_eq!(timeline[0].id, 50_000);
}

#[test]
fn exclude_operator_drops_matching_threads() {
    let conn = db();
    let be = addr("BE GROUP", "hi@begroup.vn");
    insert_message(&conn, 100, 10, "Quarterly report final", &be, 2_000);
    insert_message(&conn, 101, 20, "Quarterly report draft", &be, 1_000);

    // Plain query returns both threads.
    let q = search::parse("quarterly report");
    let hits = repo::search::hybrid(&conn, &q, &[], 10).unwrap();
    assert_eq!(hits.len(), 2);

    // "exclude:draft" drops the thread whose subject matches "draft".
    let q = search::parse("quarterly report exclude:draft");
    let hits = repo::search::hybrid(&conn, &q, &[], 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, 10);

    // The Gmail-style "-draft" is equivalent.
    let q = search::parse("quarterly report -draft");
    let hits = repo::search::hybrid(&conn, &q, &[], 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, 10);
}

#[test]
fn exclude_only_query_lists_all_non_matching_mail() {
    let conn = db();
    let be = addr("BE GROUP", "hi@begroup.vn");
    insert_message(&conn, 100, 10, "House cleaning offer", &be, 2_000);
    insert_message(&conn, 101, 20, "Draft agreement", &be, 1_000);

    // No positive terms, only an exclusion: everything except the "draft" thread.
    let q = search::parse("-draft");
    let hits = repo::search::hybrid(&conn, &q, &[], 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, 10);
}

#[test]
fn from_operator_survives_trailing_comma() {
    let conn = db();
    let be = addr("BE GROUP", "hi@begroup.vn");
    insert_message(&conn, 100, 10, "Software proposal", &be, 1_000);

    let q = search::parse("from:hi@begroup.vn, software");
    let hits = repo::search::hybrid(&conn, &q, &[], 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].id, 10);
}
