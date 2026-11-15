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
        &script::generate(&away(), &[], Some("filters")),
        true,
    );
    save_to(&keeper, &away(), None).await.unwrap();
    assert!(!keeper.script(script::NAME).unwrap().contains("include"));

    // Turned off with nothing of theirs left, nothing runs.
    keeper.put(
        script::NAME,
        &script::generate(&away(), &[], Some("filters")),
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
