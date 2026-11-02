//! Deterministic end-to-end test of the semantic retrieval plumbing: the
//! embeddings store, the in-memory vector index, and hybrid (bm25 + vector RRF)
//! search with operator filters - using hand-crafted vectors so it needs no
//! model or network.

use flectar_mail_core::db::repo;
use flectar_mail_core::embed::store::VectorIndex;
use flectar_mail_core::search::ParsedQuery;
use rusqlite::Connection;

const MODEL: &str = "test";
const DIM: usize = 4;

fn seed() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    flectar_mail_core::db::migrations::run(&mut conn).unwrap();

    conn.execute_batch(
        "INSERT INTO accounts (id,email,provider,auth_kind,username,incoming_host,incoming_port,smtp_host,smtp_port,created_at,mail_protocol,settings_json)
           VALUES (1,'me@example.com','generic','password','me','h',993,'h',465,0,'imap','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}');
         INSERT INTO folders (id,account_id,path,role) VALUES (1,1,'INBOX','inbox');
         INSERT INTO threads (id,account_id,subject_norm,last_message_at) VALUES (10,1,'apple pie recipe',100),(20,1,'quarterly earnings',200);
         INSERT INTO messages (id,account_id,thread_id,folder_id,uid,subject,from_name,from_addr,date,is_read,body_state,snippet)
           VALUES (100,1,10,1,1,'apple pie recipe','Chef','chef@example.com',100,1,'cached','apple pie'),
                  (101,1,20,1,2,'quarterly earnings','CFO','cfo@example.com',200,1,'cached','q3 earnings');
         INSERT INTO message_bodies (message_id,text_body) VALUES
                  (100,'how to bake an apple pie with cinnamon'),
                  (101,'the company revenue grew 20 percent this quarter');
         UPDATE threads SET snippet=(SELECT snippet FROM messages WHERE thread_id=threads.id ORDER BY date DESC LIMIT 1),
                            message_count=1;",
    )
    .unwrap();

    repo::search::index_message(&conn, 100).unwrap();
    repo::search::index_message(&conn, 101).unwrap();

    // Orthogonal unit vectors: msg100 -> axis 0, msg101 -> axis 1.
    repo::embeddings::store_vectors(&conn, 100, MODEL, DIM, &[vec![1.0, 0.0, 0.0, 0.0]]).unwrap();
    repo::embeddings::store_vectors(&conn, 101, MODEL, DIM, &[vec![0.0, 1.0, 0.0, 0.0]]).unwrap();
    conn
}

fn index(conn: &Connection) -> VectorIndex {
    let mut idx = VectorIndex::new(DIM, MODEL);
    for (message_id, _, vector) in repo::embeddings::load_all(conn, MODEL).unwrap() {
        idx.push(message_id, &vector);
    }
    idx
}

#[test]
fn semantic_branch_recalls_what_fts_misses() {
    let conn = seed();
    let idx = index(&conn);
    assert_eq!(idx.len(), 2);

    // A query vector aligned with msg101 (thread 20).
    let hits = idx.search(&[0.0, 1.0, 0.0, 0.0], 10);
    assert_eq!(hits[0].0, 101);

    // Lexically the query matches nothing ("banana"), but the vector points to
    // thread 20 - hybrid must still surface it.
    let q = ParsedQuery {
        fts: "\"banana\"*".into(),
        text: "banana".into(),
        ..Default::default()
    };
    let out = repo::search::hybrid(&conn, &q, &hits, 20).unwrap();
    // FTS matched nothing, so the ranking is purely semantic: the vector-aligned
    // thread 20 must come first (thread 10 only trails via its 0-similarity hit).
    assert!(!out.is_empty());
    assert_eq!(out[0].id, 20);
}

#[test]
fn hybrid_fuses_lexical_and_semantic() {
    let conn = seed();
    let idx = index(&conn);

    // Lexical matches thread 10 (apple); vector matches thread 20.
    let hits = idx.search(&[0.0, 1.0, 0.0, 0.0], 10);
    let q = ParsedQuery {
        fts: "\"apple\"*".into(),
        text: "apple".into(),
        ..Default::default()
    };
    let out = repo::search::hybrid(&conn, &q, &hits, 20).unwrap();
    let ids: std::collections::HashSet<i64> = out.iter().map(|t| t.id).collect();
    assert_eq!(ids, [10, 20].into_iter().collect());
}

#[test]
fn operator_filters_apply_to_semantic_hits() {
    let conn = seed();
    let idx = index(&conn);
    let hits = idx.search(&[0.0, 1.0, 0.0, 0.0], 10); // -> msg101

    // is:starred excludes msg101 (not starred), so the semantic hit is dropped.
    let q = ParsedQuery {
        is_starred: Some(true),
        ..Default::default()
    };
    let out = repo::search::hybrid(&conn, &q, &hits, 20).unwrap();
    assert!(
        out.is_empty(),
        "operator filter must drop non-starred semantic hit"
    );
}

#[test]
fn store_and_counts_track_embedding_state() {
    let conn = seed();
    // Both messages embedded for MODEL; none pending (store_vectors set 'done').
    let (total, embedded, pending) = repo::embeddings::counts(&conn, MODEL).unwrap();
    assert_eq!(total, 2);
    assert_eq!(embedded, 2);
    assert_eq!(pending, 0);

    // A fresh body store re-flags the message as pending.
    repo::messages::store_body(&conn, 100, Some("new text"), None, None, false, Some("new"))
        .unwrap();
    let (_, _, pending2) = repo::embeddings::counts(&conn, MODEL).unwrap();
    assert_eq!(pending2, 1);
    let ids = repo::embeddings::pending(&conn, 10).unwrap();
    assert_eq!(ids, vec![100]);
}
