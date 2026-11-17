//! Read receipts that came back for sent mail (RFC 8098).

use crate::error::Result;
use crate::mime::DispositionReport;
use crate::models::{Address, Disposition, MessageDetail, ReturnedReceipt};
use rusqlite::{Connection, OptionalExtension, params};

/// Keep what a receipt reports, and return the threads holding the message
/// it's about, which show it. "displayed" stands once reported, at its
/// earliest; any other report gives way to a later one, since a recipient
/// may undelete a message and read it after all.
pub fn record(
    conn: &Connection,
    account_id: i64,
    report: &DispositionReport,
    received_at: i64,
) -> Result<Vec<i64>> {
    conn.execute(
        "INSERT INTO returned_receipts (
             original_message_id, recipient, account_id, recipient_name,
             disposition, reported_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (original_message_id, recipient, account_id) DO UPDATE SET
             recipient_name = COALESCE(excluded.recipient_name, returned_receipts.recipient_name),
             disposition = excluded.disposition,
             reported_at = excluded.reported_at
         WHERE CASE
             WHEN excluded.disposition = 'displayed'
                 THEN returned_receipts.disposition <> 'displayed'
                     OR excluded.reported_at < returned_receipts.reported_at
             ELSE returned_receipts.disposition <> 'displayed'
                 AND excluded.reported_at >= returned_receipts.reported_at
         END",
        params![
            report.original_message_id,
            report.recipient.email,
            account_id,
            report.recipient.name,
            report.disposition.as_str(),
            report.reported_at.unwrap_or(received_at),
        ],
    )?;
    // Any account's copy: a receipt can reach another account than the one
    // that sent the message, such as one that collects an alias's mail.
    let mut statement = conn.prepare(
        "SELECT DISTINCT thread_id FROM messages
         WHERE account_id IN (SELECT id FROM accounts) AND message_id = ?1
           AND thread_id IS NOT NULL",
    )?;
    Ok(statement
        .query_map([&report.original_message_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Fill in the receipts that came back for sent messages. Each recipient
/// appears once, named as the message addressed them where it did.
pub(crate) fn attach(conn: &Connection, details: &mut [MessageDetail]) -> Result<()> {
    let mut statement = conn.prepare_cached(
        "SELECT r.recipient, r.recipient_name, r.disposition, r.reported_at
         FROM messages m
         JOIN returned_receipts r ON r.original_message_id = m.message_id
         WHERE m.id = ?1
         ORDER BY r.reported_at, r.recipient",
    )?;
    for detail in details
        .iter_mut()
        .filter(|detail| detail.is_outgoing && !detail.is_draft)
    {
        let mut receipts = Vec::<ReturnedReceipt>::new();
        let rows = statement.query_map([detail.id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (email, name, disposition, reported_at) = row?;
            let Some(disposition) = Disposition::parse(&disposition) else {
                continue;
            };
            // Two accounts can hold a report for the same recipient.
            if let Some(seen) = receipts
                .iter_mut()
                .find(|seen| seen.recipient.email.eq_ignore_ascii_case(&email))
            {
                let later = seen.disposition != Disposition::Displayed
                    && (disposition == Disposition::Displayed || reported_at >= seen.reported_at);
                if later {
                    seen.disposition = disposition;
                    seen.reported_at = reported_at;
                }
                continue;
            }
            receipts.push(ReturnedReceipt {
                recipient: Address { name, email },
                disposition,
                reported_at,
            });
        }
        if receipts.is_empty() {
            continue;
        }
        let bcc = bcc_of(conn, detail.id)?;
        for receipt in &mut receipts {
            if let Some(name) = detail
                .to
                .iter()
                .chain(&detail.cc)
                .chain(&bcc)
                .find(|address| address.email.eq_ignore_ascii_case(&receipt.recipient.email))
                .and_then(|address| address.name.clone())
                .filter(|name| !name.trim().is_empty())
            {
                receipt.recipient.name = Some(name);
            }
        }
        receipts.sort_by(|left, right| {
            (left.reported_at, &left.recipient.email)
                .cmp(&(right.reported_at, &right.recipient.email))
        });
        detail.returned_receipts = receipts;
    }
    Ok(())
}

fn bcc_of(conn: &Connection, message_id: i64) -> Result<Vec<Address>> {
    Ok(conn
        .query_row(
            "SELECT bcc_json FROM messages WHERE id = ?1",
            [message_id],
            |row| super::parse_addrs(&row.get::<_, String>(0)?, 0),
        )
        .optional()?
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{repo::messages, testutil};

    fn report(
        email: &str,
        name: Option<&str>,
        disposition: Disposition,
        at: i64,
    ) -> DispositionReport {
        DispositionReport {
            original_message_id: "plan@example.org".into(),
            recipient: Address {
                name: name.map(str::to_owned),
                email: email.into(),
            },
            disposition,
            reported_at: Some(at),
        }
    }

    /// Message 100 went to Ana, and to Ben in Bcc. Message 101 is a copy of
    /// it that came back in, in another thread.
    fn seed(conn: &Connection) {
        testutil::seed_account(conn);
        conn.execute_batch(
            r#"INSERT INTO threads (id, account_id, subject_norm, unread_count, last_message_at)
                   VALUES (10, 1, 'plan', 0, 1000), (11, 1, 'plan', 0, 1000);
               INSERT INTO messages (id, thread_id, account_id, folder_id, message_id, subject,
                   from_addr, to_json, bcc_json, date, is_outgoing)
                   VALUES (100, 10, 1, 1, 'plan@example.org', 'Plan', 'me@test.dev',
                           '[{"name":"Ana Ruiz","email":"ana@example.com"}]',
                           '[{"name":"Ben Ortiz","email":"ben@example.com"}]', 1000, 1),
                          (101, 11, 1, 1, 'plan@example.org', 'Plan', 'me@test.dev',
                           '[]', '[]', 1000, 0);"#,
        )
        .unwrap();
    }

    fn shown(
        conn: &Connection,
        message_id: i64,
    ) -> Vec<(Option<String>, String, Disposition, i64)> {
        messages::detail(conn, message_id)
            .unwrap()
            .returned_receipts
            .into_iter()
            .map(|receipt| {
                (
                    receipt.recipient.name,
                    receipt.recipient.email,
                    receipt.disposition,
                    receipt.reported_at,
                )
            })
            .collect()
    }

    #[test]
    fn receipts_show_on_the_sent_message_named_as_it_addressed_them() {
        let conn = testutil::conn();
        seed(&conn);
        let mut threads = record(
            &conn,
            1,
            &report("ana@example.com", None, Disposition::Displayed, 2000),
            0,
        )
        .unwrap();
        threads.sort_unstable();
        assert_eq!(threads, vec![10, 11]);
        record(
            &conn,
            1,
            &report("BEN@example.com", Some("B"), Disposition::Deleted, 3000),
            0,
        )
        .unwrap();
        record(
            &conn,
            1,
            &report(
                "carl@example.net",
                Some("Carl"),
                Disposition::Processed,
                1500,
            ),
            0,
        )
        .unwrap();
        let expected = vec![
            (
                Some("Carl".into()),
                "carl@example.net".into(),
                Disposition::Processed,
                1500,
            ),
            (
                Some("Ana Ruiz".into()),
                "ana@example.com".into(),
                Disposition::Displayed,
                2000,
            ),
            (
                Some("Ben Ortiz".into()),
                "BEN@example.com".into(),
                Disposition::Deleted,
                3000,
            ),
        ];
        assert_eq!(shown(&conn, 100), expected);
        let outline = messages::list_outline_for_thread(&conn, 10).unwrap();
        assert_eq!(outline[0].returned_receipts.len(), 3);
        assert_eq!(
            messages::list_for_thread(&conn, 10).unwrap()[0]
                .returned_receipts
                .len(),
            3
        );
        // Only the message one sent shows them.
        assert!(shown(&conn, 101).is_empty());
    }

    #[test]
    fn displayed_stands_and_other_reports_give_way_to_later_ones() {
        let conn = testutil::conn();
        seed(&conn);
        let state = |conn: &Connection, email: &str| {
            shown(conn, 100)
                .into_iter()
                .find(|(_, shown, _, _)| shown == email)
                .map(|(_, _, disposition, at)| (disposition, at))
                .unwrap()
        };
        let ana = "ana@example.com";
        record(&conn, 1, &report(ana, None, Disposition::Deleted, 1000), 0).unwrap();
        record(
            &conn,
            1,
            &report(ana, None, Disposition::Displayed, 3000),
            0,
        )
        .unwrap();
        assert_eq!(state(&conn, ana), (Disposition::Displayed, 3000));
        record(
            &conn,
            1,
            &report(ana, None, Disposition::Displayed, 2000),
            0,
        )
        .unwrap();
        record(&conn, 1, &report(ana, None, Disposition::Deleted, 4000), 0).unwrap();
        assert_eq!(state(&conn, ana), (Disposition::Displayed, 2000));

        let ben = "ben@example.com";
        record(
            &conn,
            1,
            &report(ben, None, Disposition::Processed, 1000),
            0,
        )
        .unwrap();
        record(&conn, 1, &report(ben, None, Disposition::Deleted, 2000), 0).unwrap();
        record(
            &conn,
            1,
            &report(ben, None, Disposition::Dispatched, 500),
            0,
        )
        .unwrap();
        assert_eq!(state(&conn, ben), (Disposition::Deleted, 2000));
        // Without a Date, the receipt's arrival stands in.
        let mut undated = report("carl@example.net", None, Disposition::Displayed, 0);
        undated.reported_at = None;
        record(&conn, 1, &undated, 5000).unwrap();
        assert_eq!(
            state(&conn, "carl@example.net"),
            (Disposition::Displayed, 5000)
        );
    }

    #[test]
    fn receipts_from_two_accounts_show_once_and_leave_with_their_account() {
        let conn = testutil::conn();
        seed(&conn);
        conn.execute(
            "INSERT INTO accounts (id, email, provider, auth_kind, username, incoming_host,
             incoming_port, smtp_host, smtp_port, created_at, mail_protocol, settings_json)
             SELECT 2, 'alias@test.dev', provider, auth_kind, username, incoming_host,
             incoming_port, smtp_host, smtp_port, created_at, mail_protocol, settings_json
             FROM accounts WHERE id = 1",
            [],
        )
        .unwrap();
        let ana = "ana@example.com";
        record(&conn, 1, &report(ana, None, Disposition::Deleted, 1000), 0).unwrap();
        record(
            &conn,
            2,
            &report(ana, None, Disposition::Displayed, 2000),
            0,
        )
        .unwrap();
        assert_eq!(
            shown(&conn, 100),
            vec![(
                Some("Ana Ruiz".into()),
                ana.into(),
                Disposition::Displayed,
                2000
            )]
        );
        conn.execute("DELETE FROM accounts WHERE id = 2", [])
            .unwrap();
        assert_eq!(
            shown(&conn, 100),
            vec![(
                Some("Ana Ruiz".into()),
                ana.into(),
                Disposition::Deleted,
                1000
            )]
        );
    }

    #[test]
    fn a_receipt_waits_for_the_message_it_reports_on() {
        let conn = testutil::conn();
        testutil::seed_account(&conn);
        let ana = report("ana@example.com", Some("Ana"), Disposition::Displayed, 2000);
        assert!(record(&conn, 1, &ana, 0).unwrap().is_empty());
        seed_sent_later(&conn);
        assert_eq!(
            shown(&conn, 100),
            vec![(
                Some("Ana Ruiz".into()),
                "ana@example.com".into(),
                Disposition::Displayed,
                2000
            )]
        );
    }

    fn seed_sent_later(conn: &Connection) {
        conn.execute_batch(
            r#"INSERT INTO threads (id, account_id, subject_norm, unread_count, last_message_at)
                   VALUES (10, 1, 'plan', 0, 1000);
               INSERT INTO messages (id, thread_id, account_id, folder_id, message_id, subject,
                   from_addr, to_json, date, is_outgoing)
                   VALUES (100, 10, 1, 1, 'plan@example.org', 'Plan', 'me@test.dev',
                           '[{"name":"Ana Ruiz","email":"ana@example.com"}]', 1000, 1);"#,
        )
        .unwrap();
    }
}
