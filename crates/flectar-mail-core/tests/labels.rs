//! End-to-end test of the labels repo: keyword derivation, per-message
//! membership, thread-summary aggregation, label filtering, and the IMAP
//! keyword reconcile that rounds labels in from the server.

use flectar_mail_core::db::repo;
use flectar_mail_core::models::View;
use rusqlite::Connection;

fn seed() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    flectar_mail_core::db::migrations::run(&mut conn).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id,email,provider,auth_kind,username,imap_host,imap_port,smtp_host,smtp_port,created_at)
           VALUES (1,'me@example.com','imap','password','me','h',993,'h',465,0);
         INSERT INTO folders (id,account_id,imap_name,role) VALUES (1,1,'INBOX','inbox');
         INSERT INTO threads (id,account_id,subject_norm,last_message_at,message_count) VALUES
           (10,1,'alpha',100,1),(20,1,'beta',200,1);
         INSERT INTO messages (id,account_id,thread_id,folder_id,uid,subject,from_addr,date,is_read,body_state)
           VALUES (100,1,10,1,1,'alpha','a@x.com',100,1,'cached'),
                  (101,1,20,1,2,'beta','b@x.com',200,1,'cached');",
    )
    .unwrap();
    conn
}

fn list_ids(conn: &Connection, label_id: Option<i64>) -> Vec<i64> {
    let page = repo::threads::list(
        conn,
        &repo::threads::ListArgs {
            view: View::All,
            tab: label_id.map(repo::threads::TabFilter::ManualLabel),
            account_id: None,
            folder_id: None,
            cursor: None,
            limit: 50,
        },
    )
    .unwrap();
    page.threads.iter().map(|t| t.id).collect()
}

#[test]
fn keyword_sanitizes_invalid_atom_chars() {
    assert_eq!(repo::labels::keyword_for("Follow up"), "Follow_up");
    assert_eq!(repo::labels::keyword_for("Work"), "Work");
    assert_eq!(repo::labels::keyword_for("(weird)"), "weird");
    assert_eq!(repo::labels::keyword_for("  "), "Label");
}

#[test]
fn membership_flows_to_summary_and_filter() {
    let conn = seed();
    let work = repo::labels::save(&conn, None, "Work", "#2563eb", 0, None).unwrap();

    repo::labels::add_to_message(&conn, 100, work.id).unwrap();

    // Thread 10's summary carries the label; thread 20's does not.
    let t10 = repo::threads::get_summary(&conn, 10).unwrap().unwrap();
    let t20 = repo::threads::get_summary(&conn, 20).unwrap().unwrap();
    assert_eq!(t10.labels, vec![work.id]);
    assert!(t20.labels.is_empty());

    assert_eq!(list_ids(&conn, Some(work.id)), vec![10]);
    assert_eq!(list_ids(&conn, None), vec![20, 10]);

    repo::labels::remove_from_message(&conn, 100, work.id).unwrap();
    assert!(
        repo::threads::get_summary(&conn, 10)
            .unwrap()
            .unwrap()
            .labels
            .is_empty()
    );
    assert_eq!(list_ids(&conn, Some(work.id)), Vec::<i64>::new());
}

#[test]
fn reconcile_keywords_rounds_labels_in_and_out() {
    let conn = seed();
    let work = repo::labels::save(&conn, None, "Work", "#2563eb", 0, Some(1)).unwrap();
    let personal = repo::labels::save(&conn, None, "Personal", "#16a34a", 1, Some(1)).unwrap();

    // Server reports the "Work" keyword on message 100 -> membership added.
    let changed = repo::labels::reconcile_keywords(&conn, 100, &["Work".into()]).unwrap();
    assert!(changed);
    assert_eq!(repo::labels::for_thread(&conn, 10).unwrap(), vec![work.id]);

    // An unknown server keyword is ignored (never becomes a label membership).
    let changed =
        repo::labels::reconcile_keywords(&conn, 100, &["Work".into(), "$Junk".into()]).unwrap();
    assert!(!changed);
    assert_eq!(repo::labels::for_thread(&conn, 10).unwrap(), vec![work.id]);

    // Server now reports "Personal" instead of "Work": Work removed, Personal added.
    let changed = repo::labels::reconcile_keywords(&conn, 100, &["Personal".into()]).unwrap();
    assert!(changed);
    assert_eq!(
        repo::labels::for_thread(&conn, 10).unwrap(),
        vec![personal.id]
    );

    repo::labels::delete(&conn, personal.id).unwrap();
    assert!(repo::labels::for_thread(&conn, 10).unwrap().is_empty());
}
