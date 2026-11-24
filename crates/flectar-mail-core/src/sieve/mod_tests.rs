use super::test_server::{Keeper, server_at};
use super::*;
use crate::models::{AuthKind, MailConnectionSettings, MailProtocol, Provider};

const EVERYTHING: &str = "fileinto vacation date relational include";
const FILTERS: &str = "require \"fileinto\";\r\nif header :contains \"Subject\" \"[list]\" {\r\n  fileinto \"Lists\";\r\n}\r\n";

fn away() -> AutoReply {
    AutoReply {
        enabled: true,
        subject: "Away".into(),
        text: "Back on Monday.".into(),
        starts_at: Some(1_794_787_200_000),
        ends_at: Some(1_795_219_199_999),
        contacts_only: false,
        domain_only: false,
    }
}

fn off() -> AutoReply {
    AutoReply {
        enabled: false,
        ..away()
    }
}

async fn save_to(
    keeper: &Keeper,
    reply: &AutoReply,
    allowed: Option<AutoReplyQuestion>,
) -> Result<AutoReplySaved> {
    save(
        &server_at(keeper.port),
        "me@example.test",
        "secret",
        reply,
        &["me@example.test".into()],
        allowed.as_ref(),
    )
    .await
}

/// The account's automatic replies as the app's script on the server holds
/// them, as another device reads them, signing in only to a server that can
/// send them.
async fn read(server: &Server<'_>, user: &str, password: &str) -> Result<Option<AutoReply>> {
    let Some(mut session) = open(server, user, password, &["vacation"]).await? else {
        return Ok(None);
    };
    let reply = super::read_from(&mut session).await;
    let _ = session.logout().await;
    reply.map(Some)
}

async fn read_from(keeper: &Keeper) -> Option<AutoReply> {
    read(&server_at(keeper.port), "me@example.test", "secret")
        .await
        .unwrap()
}

#[tokio::test]
async fn replies_go_in_the_apps_script_when_nothing_runs() {
    let keeper = Keeper::start(EVERYTHING).await;
    assert_eq!(
        save_to(&keeper, &away(), None).await.unwrap(),
        AutoReplySaved::Done(away())
    );
    assert_eq!(
        keeper.changes(),
        ["PUTSCRIPT \"flectar-mail\"", "SETACTIVE \"flectar-mail\""]
    );
    assert_eq!(keeper.active().as_deref(), Some(script::NAME));
    let ours = keeper.script(script::NAME).unwrap();
    assert!(ours.contains(":addresses [\"me@example.test\"] :subject \"Away\""));
    // Another device reads the same replies back.
    assert_eq!(read_from(&keeper).await, Some(away()));

    // Turned off, the app's script keeps them but steps aside, leaving the
    // server as it was.
    save_to(&keeper, &off(), None).await.unwrap();
    assert!(
        !keeper
            .script(script::NAME)
            .unwrap()
            .contains("vacation :days")
    );
    assert_eq!(keeper.active(), None);
    assert_eq!(read_from(&keeper).await, Some(off()));
}

#[tokio::test]
async fn turning_off_with_nothing_running_activates_nothing() {
    let keeper = Keeper::start(EVERYTHING).await;
    save_to(&keeper, &off(), None).await.unwrap();
    assert_eq!(keeper.changes(), ["PUTSCRIPT \"flectar-mail\""]);
    assert_eq!(keeper.active(), None);
    assert_eq!(read_from(&keeper).await, Some(off()));
}

#[tokio::test]
async fn the_users_own_script_runs_after_the_replies_once_allowed() {
    let keeper = Keeper::start(EVERYTHING).await;
    keeper.put("filters", FILTERS, true);
    let question = AutoReplyQuestion::RunBefore("filters".into());
    assert_eq!(
        save_to(&keeper, &away(), None).await.unwrap(),
        AutoReplySaved::Ask(question.clone())
    );
    assert!(
        keeper.changes().is_empty(),
        "nothing changes before the user says so"
    );

    assert_eq!(
        save_to(&keeper, &away(), Some(question)).await.unwrap(),
        AutoReplySaved::Done(away())
    );
    assert_eq!(keeper.active().as_deref(), Some(script::NAME));
    assert_eq!(keeper.script("filters").as_deref(), Some(FILTERS));
    let ours = keeper.script(script::NAME).unwrap();
    assert!(ours.ends_with("include :personal :optional \"filters\";\r\n"));
    assert_eq!(read_from(&keeper).await, Some(away()));

    // Saving again asks nothing.
    let later = AutoReply {
        text: "Back on Tuesday.".into(),
        ..away()
    };
    assert_eq!(
        save_to(&keeper, &later, None).await.unwrap(),
        AutoReplySaved::Done(later)
    );
    assert!(keeper.script(script::NAME).unwrap().contains("include"));

    // Turned off, their script runs alone again, and turning replies on
    // asks again.
    save_to(&keeper, &off(), None).await.unwrap();
    assert_eq!(keeper.active().as_deref(), Some("filters"));
    assert!(!keeper.script(script::NAME).unwrap().contains("include"));
    assert_eq!(keeper.script("filters").as_deref(), Some(FILTERS));
    assert_eq!(read_from(&keeper).await, Some(off()));
    assert_eq!(
        save_to(&keeper, &away(), None).await.unwrap(),
        AutoReplySaved::Ask(AutoReplyQuestion::RunBefore("filters".into()))
    );
}

#[tokio::test]
async fn an_answer_for_another_script_asks_again() {
    let keeper = Keeper::start(EVERYTHING).await;
    keeper.put("sorting", FILTERS, true);
    let stale = AutoReplyQuestion::RunBefore("filters".into());
    assert_eq!(
        save_to(&keeper, &away(), Some(stale)).await.unwrap(),
        AutoReplySaved::Ask(AutoReplyQuestion::RunBefore("sorting".into()))
    );
    assert!(keeper.changes().is_empty());
}

#[tokio::test]
async fn a_script_that_replies_on_its_own_is_never_combined() {
    let keeper = Keeper::start(EVERYTHING).await;
    keeper.put(
        "main",
        "require \"include\";\r\ninclude :personal \"away\";\r\n",
        true,
    );
    keeper.put(
        "away",
        "require [\"vacation\"];\r\nvacation \"Gone fishing\";\r\n",
        false,
    );
    let refused = save_to(
        &keeper,
        &away(),
        Some(AutoReplyQuestion::RunBefore("main".into())),
    )
    .await;
    assert!(
        matches!(&refused, Err(CoreError::Other(text)) if text.contains("sends automatic replies of its own")),
        "{refused:?}"
    );
    assert!(keeper.changes().is_empty());
}

#[tokio::test]
async fn without_include_the_users_script_keeps_running_alone() {
    let keeper = Keeper::start("fileinto vacation date relational").await;
    keeper.put("filters", FILTERS, true);
    let refused = save_to(
        &keeper,
        &away(),
        Some(AutoReplyQuestion::RunBefore("filters".into())),
    )
    .await;
    assert!(
        matches!(&refused, Err(CoreError::Other(text)) if text.contains("can't run automatic replies beside it")),
        "{refused:?}"
    );
    assert!(keeper.changes().is_empty());
    assert_eq!(keeper.active().as_deref(), Some("filters"));
}

#[tokio::test]
async fn a_script_that_runs_the_apps_stays_active() {
    let keeper = Keeper::start(EVERYTHING).await;
    keeper.put(
        "main",
        "require \"include\";\r\ninclude :personal \"flectar-mail\";\r\ninclude :personal \"filters\";\r\n",
        true,
    );
    keeper.put("filters", FILTERS, false);
    assert_eq!(
        save_to(&keeper, &away(), None).await.unwrap(),
        AutoReplySaved::Done(away())
    );
    assert_eq!(keeper.changes(), ["PUTSCRIPT \"flectar-mail\""]);
    assert_eq!(keeper.active().as_deref(), Some("main"));
    assert!(!keeper.script(script::NAME).unwrap().contains("include"));
    assert_eq!(read_from(&keeper).await, Some(away()));
}

#[tokio::test]
async fn replies_read_as_off_once_another_script_runs_instead() {
    let keeper = Keeper::start(EVERYTHING).await;
    save_to(&keeper, &away(), None).await.unwrap();
    // Webmail switched to a script of its own.
    keeper.put("roundcube", FILTERS, true);
    assert_eq!(read_from(&keeper).await, Some(off()));
}

#[tokio::test]
async fn changes_made_elsewhere_are_replaced_only_when_allowed() {
    let keeper = Keeper::start(EVERYTHING).await;
    save_to(&keeper, &away(), None).await.unwrap();
    let edited = keeper
        .script(script::NAME)
        .unwrap()
        .replace("Back on Monday.", "Back on Tuesday.");
    keeper.put(script::NAME, &edited, true);
    let before = keeper.changes().len();
    assert_eq!(
        save_to(&keeper, &away(), None).await.unwrap(),
        AutoReplySaved::Ask(AutoReplyQuestion::ReplaceChanges)
    );
    assert_eq!(keeper.changes().len(), before);
    assert_eq!(
        save_to(&keeper, &away(), Some(AutoReplyQuestion::ReplaceChanges))
            .await
            .unwrap(),
        AutoReplySaved::Done(away())
    );
    assert!(!keeper.script(script::NAME).unwrap().contains("Tuesday"));
}

#[tokio::test]
async fn a_deleted_script_of_theirs_is_no_longer_run() {
    let keeper = Keeper::start(EVERYTHING).await;
    keeper.put(
        script::NAME,
        &script::generate(&away(), &[], None, Some("filters")),
        true,
    );
    save_to(&keeper, &away(), None).await.unwrap();
    assert!(!keeper.script(script::NAME).unwrap().contains("include"));

    // Turned off with nothing of theirs left, nothing runs.
    keeper.put(
        script::NAME,
        &script::generate(&away(), &[], None, Some("filters")),
        true,
    );
    save_to(&keeper, &off(), None).await.unwrap();
    assert_eq!(keeper.active(), None);
}

#[tokio::test]
async fn days_need_a_server_that_knows_dates() {
    let keeper = Keeper::start("vacation").await;
    let refused = save_to(&keeper, &away(), None).await;
    assert!(
        matches!(&refused, Err(CoreError::Other(text)) if text.contains("certain days")),
        "{refused:?}"
    );
    let always = AutoReply {
        starts_at: None,
        ends_at: None,
        ..away()
    };
    assert_eq!(
        save_to(&keeper, &always, None).await.unwrap(),
        AutoReplySaved::Done(always)
    );
}

#[tokio::test]
async fn servers_without_sieve_or_vacation_have_no_replies() {
    let keeper = Keeper::start("fileinto").await;
    assert_eq!(read_from(&keeper).await, None);
    // The password stayed home.
    assert_eq!(keeper.kept.lock().unwrap().commands, ["LOGOUT"]);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    assert_eq!(
        read(&server_at(port), "me@example.test", "secret")
            .await
            .unwrap(),
        None
    );
}

#[test]
fn the_server_is_the_imap_host_secured_as_its_mail_is() {
    let mut config = AccountConfig {
        id: 1,
        email: "me@example.test".into(),
        display_name: None,
        name: None,
        avatar_url: None,
        provider: Provider::Generic,
        auth_kind: AuthKind::Password,
        mail_protocol: MailProtocol::Imap,
        username: "me".into(),
        jmap_url: String::new(),
        jmap_account_id: None,
        incoming_host: "imap.example.test".into(),
        incoming_port: 993,
        smtp_host: String::new(),
        smtp_port: 465,
        settings: crate::db::testutil::account_settings(),
    };
    config.settings.connection =
        MailConnectionSettings::new(ConnectionSecurity::Tls, ConnectionSecurity::Tls);
    let found = server(&config);
    assert_eq!((found.host, found.port), ("imap.example.test", 4190));
    assert_eq!(found.security, ConnectionSecurity::Starttls);
    config.settings.connection.incoming_security = ConnectionSecurity::None;
    assert_eq!(server(&config).security, ConnectionSecurity::None);
}

fn section(digest: &str) -> Section {
    Section {
        text: "# Rules\r\n# Rule: Lists\r\nif exists \"List-Id\" {\r\n  fileinto \"Lists\";\r\n  \
               stop;\r\n}\r\n# End of rules\r\n"
            .into(),
        needs: vec!["fileinto".into()],
        digest: digest.into(),
    }
}

async fn put(
    keeper: &Keeper,
    rules: Option<&Section>,
    written: Option<&str>,
    replace: bool,
    allowed: Option<Question>,
) -> Mirrored {
    let mut session = open(
        &server_at(keeper.port),
        "me@example.test",
        "secret",
        &USEFUL,
    )
    .await
    .unwrap()
    .expect("a server that sorts mail");
    let mirrored = put_rules(
        &mut session,
        rules,
        written,
        &["me@example.test".into()],
        replace,
        allowed.as_ref(),
    )
    .await
    .unwrap();
    let _ = session.logout().await;
    mirrored
}

#[tokio::test]
async fn rules_go_in_the_apps_script_beside_the_replies() {
    let keeper = Keeper::start(EVERYTHING).await;
    save_to(&keeper, &away(), None).await.unwrap();
    assert_eq!(
        put(&keeper, Some(&section("a1")), None, false, None).await,
        Mirrored::Done
    );
    let ours = keeper.script(script::NAME).unwrap();
    let reply = ours.find("vacation :days").unwrap();
    let rules = ours.find("# Rules\r\n").unwrap();
    assert!(
        reply < rules,
        "the reply goes out whatever the rules file away"
    );
    assert!(ours.contains("require [\"vacation\", \"date\", \"relational\", \"fileinto\"];"));
    assert!(ours.contains("\"rules\":{\"digest\":\"a1\",\"needs\":[\"fileinto\"]}"));
    assert_eq!(read_from(&keeper).await, Some(away()));
    assert_eq!(
        script::read(&ours).rules,
        Some(section("a1")),
        "another device finds them as written"
    );

    // Saving the replies again keeps the rules, and turning them off keeps
    // the script running for the rules.
    save_to(&keeper, &off(), None).await.unwrap();
    let ours = keeper.script(script::NAME).unwrap();
    assert_eq!(script::read(&ours).rules, Some(section("a1")));
    assert!(!ours.contains("vacation :days"));
    assert_eq!(keeper.active().as_deref(), Some(script::NAME));

    // The same rules again change nothing.
    let changes = keeper.changes().len();
    assert_eq!(
        put(&keeper, Some(&section("a1")), Some("a1"), false, None).await,
        Mirrored::Done
    );
    assert_eq!(keeper.changes().len(), changes);

    // With no rules and no replies left, the script steps aside.
    assert_eq!(
        put(&keeper, None, Some("a1"), false, None).await,
        Mirrored::Done
    );
    assert_eq!(
        script::read(&keeper.script(script::NAME).unwrap()).rules,
        None
    );
    assert_eq!(keeper.active(), None);
}

#[tokio::test]
async fn rules_run_before_the_users_own_script_once_allowed() {
    let keeper = Keeper::start(EVERYTHING).await;
    keeper.put("filters", FILTERS, true);
    let question = Question::RunBefore("filters".into());
    assert_eq!(
        put(&keeper, Some(&section("a1")), None, false, None).await,
        Mirrored::Ask(question.clone())
    );
    assert!(keeper.changes().is_empty());
    assert_eq!(
        put(&keeper, Some(&section("a1")), None, false, Some(question)).await,
        Mirrored::Done
    );
    assert_eq!(keeper.active().as_deref(), Some(script::NAME));
    assert_eq!(keeper.script("filters").as_deref(), Some(FILTERS));
    let ours = keeper.script(script::NAME).unwrap();
    assert!(ours.ends_with("# End of rules\r\ninclude :personal :optional \"filters\";\r\n"));

    // Without rules, theirs runs again as before.
    assert_eq!(
        put(&keeper, None, Some("a1"), false, None).await,
        Mirrored::Done
    );
    assert_eq!(keeper.active().as_deref(), Some("filters"));
}

#[tokio::test]
async fn rules_another_device_put_there_stay_until_replaced() {
    let keeper = Keeper::start(EVERYTHING).await;
    assert_eq!(
        put(&keeper, Some(&section("a1")), None, false, None).await,
        Mirrored::Done
    );
    // This device wrote other rules last, or none.
    let changes = keeper.changes().len();
    for written in [Some("b0"), None] {
        assert_eq!(
            put(&keeper, Some(&section("b1")), written, false, None).await,
            Mirrored::Others
        );
        assert_eq!(
            put(&keeper, None, written, false, None).await,
            Mirrored::Others
        );
    }
    assert_eq!(keeper.changes().len(), changes);
    assert_eq!(
        put(&keeper, Some(&section("b1")), Some("b0"), true, None).await,
        Mirrored::Done
    );
    let ours = keeper.script(script::NAME).unwrap();
    assert_eq!(script::read(&ours).rules, Some(section("b1")));
}

#[tokio::test]
async fn rules_ask_before_replacing_changes_made_to_the_script() {
    let keeper = Keeper::start(EVERYTHING).await;
    put(&keeper, Some(&section("a1")), None, false, None).await;
    let edited = keeper
        .script(script::NAME)
        .unwrap()
        .replace("Lists", "Listas");
    keeper.put(script::NAME, &edited, true);
    assert_eq!(
        put(&keeper, Some(&section("a2")), Some("a1"), false, None).await,
        Mirrored::Ask(Question::ReplaceChanges)
    );
    assert_eq!(
        put(
            &keeper,
            Some(&section("a2")),
            Some("a1"),
            false,
            Some(Question::ReplaceChanges)
        )
        .await,
        Mirrored::Done
    );
    assert!(!script::read(&keeper.script(script::NAME).unwrap()).edited);
}

#[tokio::test]
async fn a_server_that_only_sorts_mail_takes_rules_and_no_replies() {
    let keeper = Keeper::start("fileinto imap4flags").await;
    assert_eq!(read_from(&keeper).await, None);
    assert_eq!(
        put(&keeper, Some(&section("a1")), None, false, None).await,
        Mirrored::Done
    );
    assert_eq!(keeper.active().as_deref(), Some(script::NAME));
    // A server with neither never gets the password.
    let keeper = Keeper::start("envelope").await;
    let none = open(
        &server_at(keeper.port),
        "me@example.test",
        "secret",
        &USEFUL,
    )
    .await
    .unwrap();
    assert!(none.is_none());
}
