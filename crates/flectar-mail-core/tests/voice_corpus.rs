//! Deterministic test of the "draft in my voice" corpus queries: only the
//! user's own sent, non-draft messages with bodies are surfaced. No model.

use flectar_mail_core::db::repo;
use rusqlite::Connection;

fn seed() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
    flectar_mail_core::db::migrations::run(&mut conn).unwrap();
    conn.execute_batch(
        "INSERT INTO accounts (id,email,provider,auth_kind,username,incoming_host,incoming_port,smtp_host,smtp_port,created_at,mail_protocol,settings_json)
           VALUES (1,'me@example.com','generic','password','me','h',993,'h',465,0,'imap','{\"connection\":{\"incomingSecurity\":\"tls\",\"smtpSecurity\":\"tls\"}}');
         INSERT INTO folders (id,account_id,path,role) VALUES (1,1,'INBOX','inbox'),(2,1,'Sent','sent'),(3,1,'Drafts','drafts');
         INSERT INTO threads (id,account_id,subject_norm,last_message_at) VALUES (30,1,'lunch',300);
         INSERT INTO messages (id,account_id,thread_id,folder_id,uid,subject,from_addr,date,is_read,is_draft,body_state)
           VALUES (200,1,30,1,1,'lunch?','alice@x.com',100,1,0,'cached'),        -- incoming
                  (201,1,30,2,1,'Re: lunch','me@example.com',200,1,0,'cached'),   -- my sent reply
                  (202,1,30,3,NULL,'Re: lunch','me@example.com',300,1,1,'cached');-- a draft (excluded)
         INSERT INTO message_bodies (message_id,text_body) VALUES
                  (200,'want to grab lunch tuesday?'),
                  (201,'Sure - Tuesday at noon works. Cheers, Me'),
                  (202,'half written draft');",
    )
    .unwrap();
    conn
}

#[test]
fn list_sent_bodies_returns_only_sent_nondraft() {
    let conn = seed();
    let rows = repo::messages::list_sent_bodies(&conn, None, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, 201);
    assert!(rows[0].2.contains("Tuesday at noon"));

    // Account scoping works too.
    assert_eq!(
        repo::messages::list_sent_bodies(&conn, Some(1), 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        repo::messages::list_sent_bodies(&conn, Some(999), 10)
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn filter_sent_keeps_only_self_authored_sent() {
    let conn = seed();
    // 200 = incoming, 201 = sent, 202 = draft, 999 = missing.
    let kept = repo::messages::filter_sent(&conn, &[200, 201, 202, 999]).unwrap();
    assert_eq!(kept, vec![201]);
    assert!(repo::messages::filter_sent(&conn, &[]).unwrap().is_empty());
}
