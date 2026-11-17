use super::*;
use crate::db::testutil;

fn condition(field: RuleField, op: RuleOp, value: &str) -> RuleCondition {
    RuleCondition {
        field,
        header: String::new(),
        op,
        value: value.into(),
        negate: false,
    }
}

fn not(condition: RuleCondition) -> RuleCondition {
    RuleCondition {
        negate: true,
        ..condition
    }
}

fn rule(conditions: Vec<RuleCondition>, actions: Vec<RuleAction>) -> MailRule {
    MailRule {
        id: 0,
        account_id: Some(1),
        name: "Rule".into(),
        enabled: true,
        match_all: true,
        conditions,
        actions,
        stop: false,
    }
}

fn address(name: Option<&str>, email: &str) -> Address {
    Address {
        name: name.map(str::to_owned),
        email: email.into(),
    }
}

fn facts() -> Facts {
    Facts {
        subject: "  Your Weekly Digest ".into(),
        from: vec![address(Some("Example News"), "News@Example.com")],
        to: vec![address(None, "me@test.dev")],
        cc: vec![address(Some("Alex Doe"), "alex@team.dev")],
        list_id: Some("digest.example.com".into()),
        size: Some(300 * 1024),
        ..Facts::default()
    }
}

fn holds(condition: RuleCondition) -> Option<bool> {
    Test::new(&condition).holds(&facts())
}

#[test]
fn conditions_compare_text_without_case_and_addresses_by_email_or_name() {
    use RuleField::*;
    use RuleOp::*;
    assert_eq!(holds(condition(From, Contains, "example.COM")), Some(true));
    assert_eq!(holds(condition(From, Is, "news@example.com")), Some(true));
    assert_eq!(holds(condition(From, Is, "Example News")), Some(true));
    assert_eq!(holds(condition(From, StartsWith, "example")), Some(true));
    assert_eq!(
        holds(condition(From, EndsWith, "@example.org")),
        Some(false)
    );
    assert_eq!(
        holds(not(condition(From, Contains, "example"))),
        Some(false)
    );
    assert_eq!(holds(condition(To, Is, "me@test.dev")), Some(true));
    assert_eq!(holds(condition(Cc, Is, "me@test.dev")), Some(false));
    assert_eq!(holds(condition(ToOrCc, EndsWith, "@team.dev")), Some(true));
    // The subject's own spacing doesn't count.
    assert_eq!(
        holds(condition(Subject, Is, "your weekly digest")),
        Some(true)
    );
    assert_eq!(
        holds(condition(Subject, Matches, r"weekly\s+dig")),
        Some(true)
    );
    assert_eq!(holds(condition(Subject, Matches, r"^daily")), Some(false));
    assert_eq!(holds(condition(MailingList, Exists, "")), Some(true));
    assert_eq!(
        holds(condition(MailingList, Is, "digest.example.com")),
        Some(true)
    );
    assert_eq!(holds(condition(Size, Over, "299")), Some(true));
    assert_eq!(holds(condition(Size, Under, "300")), Some(false));
    // A size that isn't one can't be told.
    assert_eq!(holds(condition(Size, Over, "big")), None);

    let plain = Facts::default();
    let no_list = Test::new(&not(condition(MailingList, Exists, "")));
    assert_eq!(no_list.holds(&plain), Some(true));
    let no_sender = Test::new(&not(condition(From, Contains, "x")));
    assert_eq!(no_sender.holds(&plain), Some(true));
}

#[test]
fn a_rule_matches_on_all_or_any_of_its_conditions() {
    use RuleField::*;
    use RuleOp::*;
    let compiled = |match_all, conditions| {
        Compiled::new(MailRule {
            match_all,
            ..rule(conditions, vec![RuleAction::Star])
        })
    };
    let yes = condition(From, Contains, "news");
    let no = condition(Subject, Contains, "invoice");
    let unknown = condition(Size, Over, "big");
    let facts = facts();
    assert_eq!(
        compiled(true, vec![yes.clone()]).matches(&facts),
        Some(true)
    );
    assert_eq!(
        compiled(true, vec![yes.clone(), no.clone()]).matches(&facts),
        Some(false)
    );
    assert_eq!(
        compiled(false, vec![no.clone(), yes.clone()]).matches(&facts),
        Some(true)
    );
    assert_eq!(
        compiled(false, vec![no.clone()]).matches(&facts),
        Some(false)
    );
    // What isn't known yet decides only when the rest doesn't.
    assert_eq!(
        compiled(true, vec![unknown.clone(), no.clone()]).matches(&facts),
        Some(false)
    );
    assert_eq!(
        compiled(true, vec![unknown.clone(), yes.clone()]).matches(&facts),
        None
    );
    assert_eq!(
        compiled(false, vec![unknown.clone(), yes]).matches(&facts),
        Some(true)
    );
    assert_eq!(compiled(false, vec![unknown, no]).matches(&facts), None);
}

/// Account 1 with Inbox 1, Archive 2, Spam 3, Trash 4 and a folder 5, and
/// a message from `from` in the Inbox.
fn mailbox(from: &str, subject: &str) -> (rusqlite::Connection, i64) {
    let conn = testutil::conn();
    testutil::seed_account(&conn);
    for (id, path, role) in [
        (2, "Archive", Some("archive")),
        (3, "Junk", Some("spam")),
        (4, "Trash", Some("trash")),
        (5, "Newsletters", None),
    ] {
        conn.execute(
            "INSERT INTO folders (id, account_id, path, role) VALUES (?1, 1, ?2, ?3)",
            params![id, path, role],
        )
        .unwrap();
    }
    let (_, message_id) = testutil::seed_message(&conn, from, subject, false);
    (conn, message_id)
}

fn sort(conn: &mut rusqlite::Connection, rules: &[MailRule], message_id: i64) -> Sorted {
    for rule in rules {
        repo::rules::save(conn, rule, 1).unwrap();
    }
    let tx = conn.transaction().unwrap();
    let sorted = Sorter::load(&tx, 1)
        .unwrap()
        .arrived(&tx, message_id, Arrival::default())
        .unwrap();
    tx.commit().unwrap();
    sorted
}

fn state(conn: &rusqlite::Connection, message_id: i64) -> (i64, bool, bool, Vec<String>) {
    let (folder, read, starred) = conn
        .query_row(
            "SELECT folder_id, is_read, is_starred FROM messages WHERE id = ?1",
            [message_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    let mut stmt = conn
        .prepare("SELECT kind FROM pending_actions WHERE message_id = ?1 ORDER BY id")
        .unwrap();
    let kinds = stmt
        .query_map([message_id], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap();
    (folder, read, starred, kinds)
}

#[test]
fn a_rule_marks_then_files_new_inbox_mail_and_ends_the_rules() {
    let (mut conn, message_id) = mailbox("news@example.com", "Weekly digest");
    let sorted = sort(
        &mut conn,
        &[
            rule(
                vec![condition(RuleField::From, RuleOp::EndsWith, "@example.com")],
                // Filing away comes last whatever the order written.
                vec![RuleAction::Move { folder: 5 }, RuleAction::MarkRead],
            ),
            rule(
                vec![condition(RuleField::Subject, RuleOp::Contains, "digest")],
                vec![RuleAction::Star],
            ),
        ],
        message_id,
    );
    assert_eq!(
        sorted,
        Sorted {
            filed: true,
            read: true,
            unwanted: false,
            waiting: false,
        }
    );
    assert!(sorted.silent());
    assert_eq!(
        state(&conn, message_id),
        (5, true, false, vec!["mark_read".into(), "move".into()])
    );
}

#[test]
fn stop_ends_the_rules_and_an_action_that_cant_be_taken_is_skipped() {
    let (mut conn, message_id) = mailbox("alex@team.dev", "Lunch?");
    // The first rule's folder is gone since the rule was saved.
    conn.execute("DELETE FROM folders WHERE id = 5", [])
        .unwrap();
    let sorted = sort(
        &mut conn,
        &[
            rule(
                vec![condition(RuleField::From, RuleOp::Contains, "alex")],
                vec![RuleAction::Move { folder: 5 }],
            ),
            MailRule {
                stop: true,
                ..rule(
                    vec![condition(RuleField::Subject, RuleOp::Contains, "lunch")],
                    vec![RuleAction::Star],
                )
            },
            rule(
                vec![condition(RuleField::Subject, RuleOp::Contains, "lunch")],
                vec![RuleAction::MarkRead],
            ),
        ],
        message_id,
    );
    assert_eq!(sorted, Sorted::default());
    let (folder, read, starred, kinds) = state(&conn, message_id);
    assert_eq!((folder, read, starred), (1, false, true));
    assert_eq!(kinds, ["star"]);

    let (mut conn, message_id) = mailbox("alex@team.dev", "Lunch?");
    let sorted = sort(
        &mut conn,
        &[rule(
            vec![condition(RuleField::From, RuleOp::Contains, "alex")],
            vec![RuleAction::Trash],
        )],
        message_id,
    );
    assert!(sorted.filed && sorted.unwanted);
    assert_eq!(state(&conn, message_id).0, 4);
}

#[test]
fn rules_leave_mail_outside_the_inbox_and_the_users_own_alone() {
    let star_all = rule(
        vec![condition(RuleField::Subject, RuleOp::Contains, "a")],
        vec![RuleAction::Star],
    );
    let (mut conn, message_id) = mailbox("news@example.com", "Archived");
    conn.execute(
        "UPDATE messages SET folder_id = 2 WHERE id = ?1",
        [message_id],
    )
    .unwrap();
    assert_eq!(
        sort(&mut conn, std::slice::from_ref(&star_all), message_id),
        Sorted::default()
    );
    assert!(!state(&conn, message_id).2);

    let (mut conn, message_id) = mailbox("me@test.dev", "A note to self");
    conn.execute(
        "UPDATE messages SET is_outgoing = 1 WHERE id = ?1",
        [message_id],
    )
    .unwrap();
    sort(&mut conn, std::slice::from_ref(&star_all), message_id);
    assert!(!state(&conn, message_id).2);

    // A rule that is off, or for another account, doesn't run.
    let (mut conn, message_id) = mailbox("news@example.com", "A digest");
    sort(
        &mut conn,
        &[
            MailRule {
                enabled: false,
                ..star_all.clone()
            },
            MailRule {
                account_id: None,
                actions: vec![RuleAction::MarkRead],
                ..star_all
            },
        ],
        message_id,
    );
    let (_, read, starred, _) = state(&conn, message_id);
    assert!(read && !starred);
}

#[test]
fn a_rule_is_checked_when_it_is_saved() {
    let (conn, _) = mailbox("news@example.com", "Digest");
    let from = condition(RuleField::From, RuleOp::Contains, "news");
    let fine = rule(vec![from.clone()], vec![RuleAction::Move { folder: 5 }]);
    assert!(check(&conn, &fine).is_ok());
    let rejected = |rule: MailRule| check(&conn, &rule).is_err();
    assert!(rejected(MailRule {
        name: " ".into(),
        ..fine.clone()
    }));
    assert!(rejected(MailRule {
        conditions: Vec::new(),
        ..fine.clone()
    }));
    assert!(rejected(MailRule {
        actions: Vec::new(),
        ..fine.clone()
    }));
    // Folders are one account's.
    assert!(rejected(MailRule {
        account_id: None,
        ..fine.clone()
    }));
    // An IMAP account keeps a message in one folder.
    assert!(rejected(rule(
        vec![from.clone()],
        vec![RuleAction::Copy { folder: 5 }]
    )));
    assert!(rejected(rule(
        vec![from.clone()],
        vec![RuleAction::Archive, RuleAction::Trash]
    )));
    assert!(rejected(rule(
        vec![condition(RuleField::Subject, RuleOp::Matches, "(unclosed")],
        vec![RuleAction::Star]
    )));
    assert!(rejected(rule(
        vec![condition(RuleField::Size, RuleOp::Over, "1 MB")],
        vec![RuleAction::Star]
    )));
    assert!(rejected(rule(
        vec![condition(RuleField::Subject, RuleOp::Over, "10")],
        vec![RuleAction::Star]
    )));
    assert!(rejected(rule(
        vec![condition(RuleField::Subject, RuleOp::Contains, "  ")],
        vec![RuleAction::Star]
    )));
    assert!(!rejected(rule(
        vec![condition(RuleField::MailingList, RuleOp::Exists, "")],
        vec![RuleAction::Archive]
    )));
}

fn header(name: &str, op: RuleOp, value: &str) -> RuleCondition {
    RuleCondition {
        header: name.into(),
        ..condition(RuleField::Header, op, value)
    }
}

#[test]
fn headers_the_body_and_attachments_count_once_they_are_known() {
    use RuleOp::*;
    let unknown = Facts::default();
    let known = Facts {
        headers: Some(vec![
            ("X-Spam-Flag".into(), "YES".into()),
            ("Received".into(), "from a".into()),
            ("received".into(), "from b".into()),
        ]),
        body: Some(body_text("Click to UNSUBSCRIBE here")),
        attachments: Some(false),
        ..Facts::default()
    };
    let check = |condition: RuleCondition| {
        let test = Test::new(&condition);
        (test.holds(&unknown), test.holds(&known))
    };
    assert_eq!(check(header("x-spam-flag", Is, "yes")), (None, Some(true)));
    assert_eq!(check(header("X-Spam-Flag", Exists, "")), (None, Some(true)));
    assert_eq!(
        check(not(header("X-Missing", Exists, ""))),
        (None, Some(true))
    );
    // Any of a header's copies counts.
    assert_eq!(
        check(header("Received", EndsWith, " b")),
        (None, Some(true))
    );
    assert_eq!(
        check(condition(RuleField::Body, Contains, "Unsubscribe")),
        (None, Some(true))
    );
    assert_eq!(
        check(condition(RuleField::Body, Matches, r"click\s+to")),
        (None, Some(true))
    );
    assert_eq!(
        check(condition(RuleField::Attachments, Exists, "")),
        (None, Some(false))
    );
    assert_eq!(
        check(not(condition(RuleField::Attachments, Exists, ""))),
        (None, Some(true))
    );
}

#[test]
fn header_names_and_the_new_fields_are_checked() {
    assert!(is_valid_header_name("X-Spam-Flag"));
    assert!(!is_valid_header_name(""));
    assert!(!is_valid_header_name("X Spam"));
    assert!(!is_valid_header_name("X-Spam:"));
    assert!(!is_valid_header_name(&"x".repeat(65)));
    assert!(op_fits(RuleField::Body, RuleOp::Contains));
    assert!(!op_fits(RuleField::Body, RuleOp::StartsWith));
    assert!(op_fits(RuleField::Attachments, RuleOp::Exists));
    assert!(!op_fits(RuleField::Attachments, RuleOp::Is));
    assert!(op_fits(RuleField::Header, RuleOp::Exists));

    let (conn, _) = mailbox("news@example.com", "Digest");
    let rejected = |condition: RuleCondition| {
        check(&conn, &rule(vec![condition], vec![RuleAction::Star])).is_err()
    };
    assert!(rejected(header("X Spam", RuleOp::Exists, "")));
    assert!(rejected(header("", RuleOp::Exists, "")));
    assert!(!rejected(header("X-Spam-Flag", RuleOp::Is, "YES")));
}

fn waiting_json(conn: &rusqlite::Connection, message_id: i64) -> Option<String> {
    conn.query_row(
        "SELECT rules_waiting FROM messages WHERE id = ?1",
        [message_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn notified(conn: &rusqlite::Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM notification_outbox", [], |row| {
        row.get(0)
    })
    .unwrap()
}

#[test]
fn a_rule_on_the_body_waits_for_it_then_goes_on() {
    let (mut conn, message_id) = mailbox("news@example.com", "Weekly digest");
    for rule in [
        rule(
            vec![condition(RuleField::Subject, RuleOp::Contains, "digest")],
            vec![RuleAction::Star],
        ),
        rule(
            vec![condition(RuleField::Body, RuleOp::Contains, "unsubscribe")],
            vec![RuleAction::Move { folder: 5 }],
        ),
        rule(
            vec![condition(RuleField::From, RuleOp::Contains, "news")],
            vec![RuleAction::MarkRead],
        ),
    ] {
        repo::rules::save(&conn, &rule, 1).unwrap();
    }
    let tx = conn.transaction().unwrap();
    let sorted = Sorter::load(&tx, 1)
        .unwrap()
        .arrived(&tx, message_id, Arrival::default())
        .unwrap();
    // The first rule ran; the second needs the body.
    assert!(sorted.waiting && !sorted.silent());
    hold(&tx, message_id, true).unwrap();
    tx.commit().unwrap();
    let (folder, read, starred, _) = state(&conn, message_id);
    assert_eq!((folder, read, starred), (1, false, true));
    assert!(waiting_json(&conn, message_id).is_some());

    repo::messages::store_body(
        &conn,
        message_id,
        None,
        Some("<p>To stop these, <a href=x>Unsubscribe</a></p>"),
        None,
        false,
        None,
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    let resumed = resume(&tx, message_id).unwrap().unwrap();
    tx.commit().unwrap();
    // Filed away, so the third rule doesn't run and nothing notifies.
    assert!(!resumed.announced);
    let (folder, read, _, _) = state(&conn, message_id);
    assert_eq!((folder, read), (5, false));
    assert_eq!(notified(&conn), 0);
    assert_eq!(waiting_json(&conn, message_id), None);

    // Done once: resuming again does nothing.
    let tx = conn.transaction().unwrap();
    assert_eq!(resume(&tx, message_id).unwrap(), None);
}

#[test]
fn the_held_notification_shows_when_the_rules_leave_the_mail_be() {
    let (mut conn, message_id) = mailbox("news@example.com", "Weekly digest");
    repo::rules::save(
        &conn,
        &rule(
            vec![condition(RuleField::Body, RuleOp::Contains, "unsubscribe")],
            vec![RuleAction::Trash],
        ),
        1,
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    let sorted = Sorter::load(&tx, 1)
        .unwrap()
        .arrived(&tx, message_id, Arrival::default())
        .unwrap();
    assert!(sorted.waiting);
    hold(&tx, message_id, false).unwrap();
    tx.commit().unwrap();
    repo::messages::store_body(&conn, message_id, Some("Hello"), None, None, false, None).unwrap();
    let tx = conn.transaction().unwrap();
    let resumed = resume(&tx, message_id).unwrap().unwrap();
    tx.commit().unwrap();
    assert!(resumed.announced);
    assert_eq!(notified(&conn), 1);
    assert_eq!(state(&conn, message_id).0, 1);
}

#[test]
fn rules_that_wait_too_long_settle_without_the_body() {
    let (mut conn, message_id) = mailbox("news@example.com", "Weekly digest");
    repo::rules::save(
        &conn,
        &rule(
            vec![not(condition(RuleField::Attachments, RuleOp::Exists, ""))],
            vec![RuleAction::Archive],
        ),
        1,
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    let sorted = Sorter::load(&tx, 1)
        .unwrap()
        .arrived(&tx, message_id, Arrival::default())
        .unwrap();
    assert!(sorted.waiting);
    hold(&tx, message_id, true).unwrap();
    tx.commit().unwrap();
    // Not yet too long.
    assert!(overdue(&conn, 1, 0).unwrap().is_empty());
    let overdue = overdue(&conn, 1, now_ms() + 1).unwrap();
    assert_eq!(overdue, [message_id]);
    let tx = conn.transaction().unwrap();
    let resumed = give_up(&tx, message_id).unwrap().unwrap();
    tx.commit().unwrap();
    // What can't be told doesn't match: it stays, and notifies.
    assert!(resumed.announced);
    assert_eq!(state(&conn, message_id).0, 1);

    // With the attachments known at arrival, as on JMAP, nothing waits.
    let (mut conn, message_id) = mailbox("news@example.com", "Weekly digest");
    repo::rules::save(
        &conn,
        &rule(
            vec![not(condition(RuleField::Attachments, RuleOp::Exists, ""))],
            vec![RuleAction::Archive],
        ),
        1,
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    let sorted = Sorter::load(&tx, 1)
        .unwrap()
        .arrived(
            &tx,
            message_id,
            Arrival {
                attachments_known: true,
                ..Arrival::default()
            },
        )
        .unwrap();
    tx.commit().unwrap();
    assert!(sorted.filed && !sorted.waiting);
    assert_eq!(waiting_json(&conn, message_id), None);
}

#[test]
fn a_rule_counts_and_runs_on_the_inbox_mail_it_matches_now() {
    let (mut conn, digest) = mailbox("news@example.com", "Weekly digest");
    let (_, note) = testutil::seed_message(&conn, "alex@team.dev", "Lunch?", false);
    let (_, archived) = testutil::seed_message(&conn, "news@example.com", "Old digest", false);
    conn.execute(
        "UPDATE messages SET folder_id = 2 WHERE id = ?1",
        [archived],
    )
    .unwrap();
    let from_news = rule(
        vec![condition(RuleField::From, RuleOp::EndsWith, "@example.com")],
        vec![RuleAction::MarkRead, RuleAction::Move { folder: 5 }],
    );
    // Only Inbox mail counts.
    let found = inbox_matches(&conn, &from_news).unwrap();
    assert_eq!(found.message_ids, [digest]);
    assert_eq!(found.unknown, 0);

    // A rule on the body can't tell for mail whose body isn't here.
    let on_body = rule(
        vec![condition(RuleField::Body, RuleOp::Contains, "lunch")],
        vec![RuleAction::Star],
    );
    assert_eq!(
        inbox_matches(&conn, &on_body).unwrap(),
        InboxMatches {
            message_ids: Vec::new(),
            unknown: 2
        }
    );
    repo::messages::store_body(&conn, note, Some("Lunch at noon?"), None, None, false, None)
        .unwrap();
    let found = inbox_matches(&conn, &on_body).unwrap();
    assert_eq!((found.message_ids, found.unknown), (vec![note], 1));

    let tx = conn.transaction().unwrap();
    let queued = run_on(&tx, &from_news, &[digest, archived]).unwrap();
    tx.commit().unwrap();
    // Marked, then moved; mail that wasn't in the Inbox is left alone.
    assert_eq!(queued.len(), 2);
    assert_eq!(
        state(&conn, digest),
        (5, true, false, vec!["mark_read".into(), "move".into()])
    );
    assert_eq!(state(&conn, archived).0, 2);
}

#[test]
fn a_rule_made_from_a_message_is_for_its_list_or_its_sender() {
    let (conn, message_id) = mailbox("news@example.com", "Weekly digest");
    let draft = draft_for(&conn, message_id).unwrap();
    assert_eq!(draft.id, 0);
    assert_eq!(draft.account_id, Some(1));
    assert_eq!(draft.name, "news@example.com");
    assert_eq!(
        draft.conditions,
        [condition(RuleField::From, RuleOp::Is, "news@example.com")]
    );
    conn.execute(
        "UPDATE messages SET list_id = 'digest.example.com', from_name = 'Example News'
         WHERE id = ?1",
        [message_id],
    )
    .unwrap();
    let draft = draft_for(&conn, message_id).unwrap();
    assert_eq!(draft.name, "Example News");
    assert_eq!(
        draft.conditions,
        [condition(
            RuleField::MailingList,
            RuleOp::Is,
            "digest.example.com"
        )]
    );
}

fn tabs(conn: &rusqlite::Connection, message_id: i64) -> (Option<String>, Option<String>) {
    conn.query_row(
        "SELECT t.routed_tab, t.pinned_tab FROM threads t
         JOIN messages m ON m.thread_id = t.id WHERE m.id = ?1",
        [message_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

#[test]
fn a_rule_shows_mail_in_a_category_where_it_stays_until_undone() {
    let (mut conn, message) = mailbox("deals@shop.example", "50% off");
    let news: i64 = conn
        .query_row(
            "SELECT id FROM labels WHERE keyword = 'FlectarMailAutoNews'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let news_tab = format!("label:{news}");
    let show_in_news = rule(
        vec![condition(
            RuleField::From,
            RuleOp::EndsWith,
            "@shop.example",
        )],
        vec![RuleAction::ShowIn {
            tab: news_tab.clone(),
        }],
    );
    // Only a tab the Inbox has.
    for tab in ["important", "other", news_tab.as_str()] {
        let fine = MailRule {
            actions: vec![RuleAction::ShowIn { tab: tab.into() }],
            ..show_in_news.clone()
        };
        assert!(check(&conn, &fine).is_ok(), "{tab}");
    }
    for tab in ["label:9999", "split:1", "inbox"] {
        let wrong = MailRule {
            actions: vec![RuleAction::ShowIn { tab: tab.into() }],
            ..show_in_news.clone()
        };
        assert!(check(&conn, &wrong).is_err(), "{tab}");
    }
    let twice = MailRule {
        actions: vec![
            RuleAction::ShowIn {
                tab: "important".into(),
            },
            RuleAction::ShowIn {
                tab: "other".into(),
            },
        ],
        ..show_in_news.clone()
    };
    assert!(check(&conn, &twice).is_err());

    sort(&mut conn, &[show_in_news], message);
    assert_eq!(
        tabs(&conn, message),
        (Some(news_tab.clone()), Some(news_tab.clone()))
    );
    // A later sort keeps it there, the AI's too.
    let thread: i64 = conn
        .query_row(
            "SELECT thread_id FROM messages WHERE id = ?1",
            [message],
            |row| row.get(0),
        )
        .unwrap();
    crate::route::route_thread_deterministic(&conn, true, thread).unwrap();
    assert_eq!(tabs(&conn, message).0, Some(news_tab));
    // Undo puts it back where it was, for routing to choose again.
    let action = repo::actions::get(
        &conn,
        conn.query_row(
            "SELECT id FROM pending_actions WHERE kind = 'show_in'",
            [],
            |row| row.get(0),
        )
        .unwrap(),
    )
    .unwrap()
    .unwrap();
    crate::revert_action(&mut conn, &action).unwrap();
    assert_eq!(tabs(&conn, message), (None, None));
}
