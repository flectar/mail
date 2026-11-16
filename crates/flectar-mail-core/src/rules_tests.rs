use super::*;
use crate::db::testutil;

fn condition(field: RuleField, op: RuleOp, value: &str) -> RuleCondition {
    RuleCondition {
        field,
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
        .arrived(&tx, message_id)
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
            unwanted: false
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
