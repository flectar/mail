use super::*;
use crate::accounts::credentials::DevelopmentFileCredentialStore;
use crate::config::Paths;
use crate::models::{MailRule, RuleCondition, RuleField, RuleOp};
use crate::sieve::script;
use crate::sieve::test_server::{Keeper, server_at};

/// A core with a generic IMAP account, id 1, with Lists and Trash folders.
async fn core(directory: &tempfile::TempDir) -> Core {
    let credentials = Arc::new(DevelopmentFileCredentialStore::new(
        directory.path().join("credentials.json"),
    ));
    let core =
        Core::start_mail_ui_with_credentials(Paths::for_tests(directory.path()), credentials)
            .await
            .unwrap();
    core.db
        .write(|conn| {
            crate::db::testutil::seed_account(conn);
            conn.execute(
                "INSERT INTO folders (id, account_id, path, role) VALUES
                   (2, 1, 'Lists', NULL), (3, 1, 'Trash', 'trash'),
                   (4, 1, 'Envoy&AOk-', NULL)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    core
}

fn rule(
    account_id: Option<i64>,
    name: &str,
    condition: RuleCondition,
    actions: Vec<RuleAction>,
) -> MailRule {
    MailRule {
        id: 0,
        account_id,
        name: name.into(),
        enabled: true,
        match_all: true,
        conditions: vec![condition],
        actions,
        stop: false,
    }
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

/// Stores `rules` without telling the server, and gives their ids.
async fn store(core: &Core, rules: Vec<MailRule>) -> Vec<i64> {
    core.db
        .write(move |conn| {
            rules
                .iter()
                .map(|rule| Ok(repo::rules::save(conn, rule, 1)?.id))
                .collect()
        })
        .await
        .unwrap()
}

async fn mirror_to(
    core: &Core,
    keeper: &Keeper,
    answer: Option<&ServerRulesAnswer>,
) -> ServerRules {
    let mut session = sieve::open(&server_at(keeper.port), "me", "secret", &sieve::USEFUL)
        .await
        .unwrap()
        .unwrap();
    let checked = mirror(&core.sync_ctx(), 1, &mut session, answer)
        .await
        .unwrap();
    let _ = session.logout().await;
    checked
}

#[tokio::test]
async fn the_rules_the_server_runs_as_the_app_does_go_there() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    let ids = store(
        &core,
        vec![
            rule(
                Some(1),
                "Lists",
                condition(RuleField::MailingList, RuleOp::Exists, ""),
                vec![RuleAction::Move { folder: 2 }],
            ),
            // For every account, into each one's Trash.
            rule(
                None,
                "Spam",
                condition(RuleField::Subject, RuleOp::Contains, "lottery"),
                vec![RuleAction::Trash],
            ),
            rule(
                Some(1),
                "Pattern",
                condition(RuleField::Subject, RuleOp::Matches, "^re:"),
                vec![RuleAction::Star],
            ),
            rule(
                Some(1),
                "Later",
                condition(RuleField::Subject, RuleOp::Contains, "x"),
                vec![RuleAction::MarkRead],
            ),
        ],
    )
    .await;
    let keeper = Keeper::start("fileinto imap4flags include vacation").await;
    let checked = mirror_to(&core, &keeper, None).await;
    assert_eq!(checked.state, ServerRulesState::Runs);
    assert_eq!(checked.on_server, ids[..2]);
    assert_eq!(
        checked.here,
        [
            RuleHere {
                rule_id: ids[2],
                why: RuleHereWhy::Pattern
            },
            RuleHere {
                rule_id: ids[3],
                why: RuleHereWhy::After
            },
        ]
    );
    assert_eq!(keeper.active().as_deref(), Some(script::NAME));
    let text = keeper.script(script::NAME).unwrap();
    assert!(text.contains("# Rule: Lists\r\nif exists \"List-Id\" {\r\n  fileinto \"Lists\";"));
    assert!(text.contains(
        "# Rule: Spam\r\nif header :contains \"subject\" \"lottery\" {\r\n  fileinto \"Trash\";"
    ));
    assert!(!text.contains("Pattern"));
    // Kept, with the digest of what this device wrote.
    assert_eq!(core.server_rules().await.unwrap(), vec![checked.clone()]);
    let written = core
        .db
        .read(|conn| repo::server_rules::written(conn, 1))
        .await
        .unwrap();
    assert_eq!(written, script::read(&text).rules.map(|rules| rules.digest));

    // Nothing changed, nothing to write.
    let changes = keeper.changes().len();
    mirror_to(&core, &keeper, None).await;
    assert_eq!(keeper.changes().len(), changes);

    // A folder renamed, and the rule goes again.
    core.db
        .write(|conn| {
            conn.execute("UPDATE mail_rules SET actions_json = '[{\"kind\":\"move\",\"folder\":4}]' WHERE name = 'Lists'", [])?;
            Ok(())
        })
        .await
        .unwrap();
    mirror_to(&core, &keeper, None).await;
    assert!(
        keeper
            .script(script::NAME)
            .unwrap()
            .contains("fileinto \"Envoyé\";")
    );
}

#[tokio::test]
async fn rules_another_device_put_on_the_server_stay_until_the_user_replaces_them() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    let keeper = Keeper::start("fileinto imap4flags include vacation").await;
    // Another device's rules.
    let theirs = crate::sieve::rules::Section {
        text: "# Rules\r\n# Rule: Theirs\r\nif true {\r\n  stop;\r\n}\r\n# End of rules\r\n".into(),
        needs: vec![],
        digest: "other".into(),
    };
    keeper.put(
        script::NAME,
        &script::generate(&Default::default(), &[], Some(&theirs), None),
        true,
    );
    store(
        &core,
        vec![rule(
            Some(1),
            "Mine",
            condition(RuleField::Subject, RuleOp::Contains, "x"),
            vec![RuleAction::Star],
        )],
    )
    .await;
    let checked = mirror_to(&core, &keeper, None).await;
    assert_eq!(checked.state, ServerRulesState::Others);
    assert!(checked.on_server.is_empty());
    assert!(keeper.changes().is_empty());

    let checked = mirror_to(&core, &keeper, Some(&ServerRulesAnswer::ReplaceOthers)).await;
    assert_eq!(checked.state, ServerRulesState::Runs);
    assert!(
        keeper
            .script(script::NAME)
            .unwrap()
            .contains("# Rule: Mine\r\n")
    );
    // Now this device's own, so its later changes go without asking.
    store(
        &core,
        vec![rule(
            Some(1),
            "More",
            condition(RuleField::Subject, RuleOp::Contains, "y"),
            vec![RuleAction::MarkRead],
        )],
    )
    .await;
    let checked = mirror_to(&core, &keeper, None).await;
    assert_eq!(checked.state, ServerRulesState::Runs);
    assert_eq!(checked.on_server.len(), 2);
}

#[tokio::test]
async fn rules_wait_to_run_before_the_users_own_script() {
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    let keeper = Keeper::start("fileinto imap4flags include vacation").await;
    keeper.put("roundcube", "require \"fileinto\";\r\n", true);
    store(
        &core,
        vec![rule(
            Some(1),
            "Mine",
            condition(RuleField::Subject, RuleOp::Contains, "x"),
            vec![RuleAction::Star],
        )],
    )
    .await;
    let checked = mirror_to(&core, &keeper, None).await;
    assert_eq!(
        checked.state,
        ServerRulesState::RunBefore("roundcube".into())
    );
    assert_eq!(keeper.active().as_deref(), Some("roundcube"));
    let answer = ServerRulesAnswer::RunBefore("roundcube".into());
    let checked = mirror_to(&core, &keeper, Some(&answer)).await;
    assert_eq!(checked.state, ServerRulesState::Runs);
    assert_eq!(keeper.active().as_deref(), Some(script::NAME));
}
