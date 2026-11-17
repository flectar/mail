use super::*;

const EVERYTHING: [&str; 6] = [
    "fileinto",
    "imap4flags",
    "copy",
    "body",
    "mailboxid",
    "include",
];

fn compile_on(rules: &[Rule], extensions: &[&str], implementation: &str) -> Compiled {
    let offers = |extension: &str| extensions.contains(&extension);
    compile(
        rules,
        &Engine {
            offers: &offers,
            implementation,
        },
    )
}

fn compiled(rules: &[Rule]) -> Compiled {
    compile_on(rules, &EVERYTHING, "Keeper")
}

fn condition(field: RuleField, op: RuleOp, value: &str) -> RuleCondition {
    RuleCondition {
        field,
        header: String::new(),
        op,
        value: value.into(),
        negate: false,
    }
}

fn rule(id: i64, conditions: Vec<RuleCondition>, actions: Vec<Action>) -> Rule {
    Rule {
        id,
        name: format!("Rule {id}"),
        match_all: true,
        conditions,
        actions,
        stop: false,
    }
}

fn folder(path: &str) -> Folder {
    Folder {
        path: path.into(),
        jmap_id: None,
    }
}

fn subject(value: &str) -> RuleCondition {
    condition(RuleField::Subject, RuleOp::Contains, value)
}

#[test]
fn rules_become_ifs_in_order_with_their_names() {
    let mut lists = rule(
        1,
        vec![condition(RuleField::MailingList, RuleOp::Exists, "")],
        vec![Action::Move {
            folder: folder("Lists"),
        }],
    );
    lists.name = "News\nletters".into();
    let boss = Rule {
        match_all: false,
        stop: true,
        ..rule(
            2,
            vec![
                condition(RuleField::From, RuleOp::Is, "boss@example.com"),
                RuleCondition {
                    negate: true,
                    ..condition(RuleField::Size, RuleOp::Under, "10")
                },
            ],
            vec![
                Action::Star,
                Action::Label {
                    keyword: "Work".into(),
                },
            ],
        )
    };
    let compiled = compiled(&[lists, boss]);
    assert_eq!(compiled.on_server, [1, 2]);
    assert!(compiled.here.is_empty());
    let section = compiled.section.unwrap();
    assert_eq!(
        section.text,
        "# Rules\r\n\
         # Rule: News letters\r\n\
         if exists \"List-Id\" {\r\n  fileinto \"Lists\";\r\n  stop;\r\n}\r\n\
         # Rule: Rule 2\r\n\
         if anyof (address :all :is \"from\" \"boss@example.com\",\r\n          \
         not size :under 10K) {\r\n  addflag [\"\\\\Flagged\", \"Work\"];\r\n  stop;\r\n}\r\n\
         # End of rules\r\n"
    );
    assert_eq!(section.needs, ["fileinto", "imap4flags"]);
}

#[test]
fn marks_come_before_copies_and_copies_before_filing() {
    let both = rule(
        1,
        vec![subject("x")],
        vec![
            Action::Move {
                folder: folder("Done"),
            },
            Action::Copy {
                folder: Folder {
                    path: "Keep".into(),
                    jmap_id: Some("M7".into()),
                },
            },
            Action::MarkRead,
        ],
    );
    let section = compiled(&[both]).section.unwrap();
    assert!(section.text.contains(
        "  addflag \"\\\\Seen\";\r\n  fileinto :copy :mailboxid \"M7\" \"INBOX\";\r\n  \
         fileinto \"Done\";\r\n  stop;\r\n"
    ));
    assert_eq!(
        section.needs,
        ["mailboxid", "imap4flags", "fileinto", "copy"]
    );
}

#[test]
fn the_first_rule_the_server_cant_run_keeps_it_and_the_rest_in_the_app() {
    let pattern = rule(
        2,
        vec![condition(RuleField::Subject, RuleOp::Matches, "^re:")],
        vec![Action::Star],
    );
    let rules = [
        rule(1, vec![subject("a")], vec![Action::Star]),
        pattern,
        rule(3, vec![subject("c")], vec![Action::Star]),
    ];
    let compiled = compiled(&rules);
    assert_eq!(compiled.on_server, [1]);
    assert_eq!(compiled.here, [(2, Why::Pattern), (3, Why::After)]);
    assert!(
        compiled
            .section
            .unwrap()
            .text
            .contains("# Rule: Rule 1\r\n")
    );

    // With none to run, there's no section.
    let compiled = compile_on(&rules[1..], &EVERYTHING, "Keeper");
    assert!(compiled.section.is_none());
    assert!(compiled.on_server.is_empty());
}

#[test]
fn what_sieve_cant_tell_as_the_app_does_stays_in_the_app() {
    let why = |condition: RuleCondition| {
        compiled(&[rule(1, vec![condition], vec![Action::Star])])
            .here
            .first()
            .map(|(_, why)| *why)
    };
    use RuleField::*;
    use RuleOp::*;
    assert_eq!(
        why(condition(Attachments, Exists, "")),
        Some(Why::Attachments)
    );
    assert_eq!(why(condition(Body, Matches, "x+")), Some(Why::Pattern));
    // A name can't be told from an address, except for text inside one.
    assert_eq!(why(condition(From, Is, "Bob Smith")), Some(Why::Name));
    assert_eq!(why(condition(From, StartsWith, "bob")), Some(Why::Name));
    assert_eq!(why(condition(To, Contains, "Smith, Bob")), Some(Why::Name));
    assert_eq!(why(condition(From, Contains, "Smith")), None);
    assert_eq!(why(condition(From, EndsWith, "@example.com")), None);
    assert_eq!(why(condition(Size, Over, "big")), Some(Why::Value));
    assert_eq!(why(condition(Size, Over, " 30 ")), None);
}

#[test]
fn text_beyond_ascii_needs_a_server_that_ignores_its_case() {
    let rules = [rule(1, vec![subject("Été")], vec![Action::Star])];
    assert_eq!(
        compile_on(&rules, &EVERYTHING, "Keeper").here,
        [(1, Why::Case)]
    );
    let listed = [&EVERYTHING[..], &["comparator-i;unicode-casemap"]].concat();
    let section = compile_on(&rules, &listed, "Keeper").section.unwrap();
    assert!(
        section.text.contains(
            "if header :comparator \"i;unicode-casemap\" :contains \"subject\" \"Été\" {"
        )
    );
    assert_eq!(
        section.needs,
        ["comparator-i;unicode-casemap", "imap4flags"]
    );
    // Pigeonhole runs that comparator without listing it.
    let section = compile_on(&rules, &EVERYTHING, "Dovecot Pigeonhole")
        .section
        .unwrap();
    assert!(section.needs.contains(&UNICODE_CASEMAP.to_owned()));
    // Stalwart's default comparator lowercases all text, as the app does.
    let section = compile_on(&rules, &EVERYTHING, "Stalwart ManageSieve")
        .section
        .unwrap();
    assert!(
        section
            .text
            .contains("if header :contains \"subject\" \"Été\" {")
    );
    assert_eq!(section.needs, ["imap4flags"]);
}

#[test]
fn sizes_beyond_what_a_server_reads_stay_beyond_any_message() {
    let rules = [rule(
        1,
        vec![condition(RuleField::Size, RuleOp::Over, "3000000000")],
        vec![Action::Star],
    )];
    let section = compiled(&rules).section.unwrap();
    assert!(section.text.contains("if size :over 2147483647K {"));
}

#[test]
fn wildcards_and_quotes_are_taken_as_they_are() {
    let rules = [rule(
        1,
        vec![condition(
            RuleField::Subject,
            RuleOp::StartsWith,
            "a*b?\\\"c",
        )],
        vec![Action::Star],
    )];
    let section = compiled(&rules).section.unwrap();
    assert!(
        section
            .text
            .contains("header :matches \"subject\" \"a\\\\*b\\\\?\\\\\\\\\\\"c*\""),
        "{}",
        section.text
    );
}

#[test]
fn folders_go_by_the_name_the_server_reads() {
    let move_to = |path: &str| {
        vec![rule(
            1,
            vec![subject("x")],
            vec![Action::Move {
                folder: folder(path),
            }],
        )]
    };
    // IMAP gives names in modified UTF-7; Sieve reads UTF-8.
    let section = compiled(&move_to("Envoy&AOk-/Q&-A")).section.unwrap();
    assert!(section.text.contains("fileinto \"Envoyé/Q&A\";"));
    // Cyrus reads modified UTF-7 unless set up otherwise, so only names
    // that read the same both ways go there.
    let cyrus = "Cyrus timsieved v3.12.0";
    assert_eq!(
        compile_on(&move_to("Envoy&AOk-"), &EVERYTHING, cyrus).here,
        [(1, Why::Folder)]
    );
    assert!(
        compile_on(&move_to("INBOX.Lists"), &EVERYTHING, cyrus)
            .section
            .is_some()
    );
    // A folder that is gone has no name.
    assert_eq!(compiled(&move_to("")).here, [(1, Why::Folder)]);
    // A JMAP folder goes by its id, which needs the server to take one.
    let jmap = [rule(
        1,
        vec![subject("x")],
        vec![Action::Move {
            folder: Folder {
                path: "Lists".into(),
                jmap_id: Some("M1".into()),
            },
        }],
    )];
    assert_eq!(
        compile_on(&jmap, &["fileinto", "imap4flags"], "Keeper").here,
        [(1, Why::Folder)]
    );
}

#[test]
fn labels_need_ascii_keywords() {
    let label = |keyword: &str| {
        compiled(&[rule(
            1,
            vec![subject("x")],
            vec![Action::Label {
                keyword: keyword.into(),
            }],
        )])
        .here
    };
    assert_eq!(label("Déjà"), [(1, Why::Label)]);
    assert_eq!(label(""), [(1, Why::Label)]);
    assert!(label("Follow_up").is_empty());
}

#[test]
fn a_missing_extension_keeps_the_rule_in_the_app() {
    let rules = [rule(1, vec![subject("x")], vec![Action::MarkRead])];
    assert_eq!(
        compile_on(&rules, &["fileinto"], "Keeper").here,
        [(1, Why::Extension("imap4flags"))]
    );
    let body = [rule(
        1,
        vec![condition(RuleField::Body, RuleOp::Contains, "x")],
        vec![Action::Star],
    )];
    assert_eq!(
        compile_on(&body, &["imap4flags"], "Keeper").here,
        [(1, Why::Extension("body"))]
    );
}

#[test]
fn the_digest_tells_rules_apart_by_what_they_do() {
    let rules = [rule(
        1,
        vec![subject("x")],
        vec![Action::Move {
            folder: folder("A"),
        }],
    )];
    let digest = |rules: &[Rule]| compiled(rules).section.unwrap().digest;
    // The same rules on another device, under other ids.
    let mut elsewhere = rules.clone();
    elsewhere[0].id = 9;
    assert_eq!(digest(&rules), digest(&elsewhere));
    // A renamed folder, rule or anything else they do tells them apart.
    let mut renamed = rules.clone();
    renamed[0].actions = vec![Action::Move {
        folder: folder("B"),
    }];
    assert_ne!(digest(&rules), digest(&renamed));
    let mut named = rules.clone();
    named[0].name = "Other".into();
    assert_ne!(digest(&rules), digest(&named));
    assert_eq!(digest(&rules).len(), 16);
}
