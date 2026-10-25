//! Account migration from Thunderbird for Android/K-9 and Thunderbird desktop:
//! reading real export layouts, planning, and persisting through Core.

use flectar_mail_core::Core;
use flectar_mail_core::config::Paths;
use flectar_mail_core::migration::{
    self, AccountPlan, FindingCode, ImportInventory, OtherEntryKind, PlanStatus, ServerProtocol,
    SignIn, Source, SourceError, SourceKind,
};
use flectar_mail_core::models::{
    ConnectionSecurity, KeepOnServer, MailProtocol, Provider, SenderIdentitySource,
};
use std::io::Write;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/migration")
        .join(name)
}

fn accounts(source: Source) -> ImportInventory {
    match source {
        Source::Accounts(inventory) => inventory,
        Source::FlectarBackup(_) => panic!("expected accounts"),
    }
}

fn codes(plan: &AccountPlan) -> Vec<FindingCode> {
    plan.findings.iter().map(|finding| finding.code).collect()
}

/// A Thunderbird profile folder with the files `prefs.js` refers to.
fn thunderbird_profile() -> tempfile::TempDir {
    let profile = tempfile::tempdir().unwrap();
    std::fs::copy(fixture("prefs.js"), profile.path().join("prefs.js")).unwrap();
    std::fs::create_dir(profile.path().join("signatures")).unwrap();
    std::fs::write(
        profile.path().join("signatures/lists.txt"),
        "Ana\nList moderator",
    )
    .unwrap();
    std::fs::write(
        profile.path().join("folderTree.json"),
        r##"{"colors":{"imap://ana@imap.example.org":"#AA3355","imap://ana@imap.example.org/INBOX":"#000000"}}"##,
    )
    .unwrap();
    // user.js overrides prefs.js, as at every Thunderbird start.
    std::fs::write(
        profile.path().join("user.js"),
        "user_pref(\"mail.identity.id1.fullName\", \"Ana Ruiz García\");\n",
    )
    .unwrap();
    profile
}

async fn core() -> (tempfile::TempDir, Core) {
    let directory = tempfile::tempdir().unwrap();
    let core = Core::start(Paths::for_tests(directory.path()))
        .await
        .unwrap();
    (directory, core)
}

#[test]
fn k9_export_maps_supported_accounts_and_explains_the_rest() {
    let inventory = accounts(migration::open(&fixture("k9-settings.k9s")).unwrap());
    assert_eq!(inventory.source, SourceKind::K9Settings);
    assert_eq!(inventory.source_version.as_deref(), Some("112"));
    let plans = migration::plan(&inventory, &[]);
    assert_eq!(plans.len(), 3);

    let work = &plans[0];
    assert_eq!(work.label.as_deref(), Some("Work"));
    assert_eq!(work.status, PlanStatus::Ready);
    assert_eq!(work.sign_in, SignIn::Password);
    assert_eq!(codes(work), [FindingCode::PasswordMechanismChanged]);
    let draft = work.draft.as_ref().unwrap();
    let config = &draft.config;
    assert_eq!(config.email, "jane@example.com");
    assert_eq!(config.display_name.as_deref(), Some("Jane Doe"));
    assert_eq!(config.username, "jane@example.com");
    assert_eq!(
        (config.incoming_host.as_str(), config.incoming_port),
        ("imap.example.com", 993)
    );
    // An omitted port is the protocol default for the configured security.
    assert_eq!(
        (config.smtp_host.as_str(), config.smtp_port),
        ("smtp.example.com", 587)
    );
    assert_eq!(
        config.settings.connection.incoming_security,
        ConnectionSecurity::Tls
    );
    assert_eq!(
        config.settings.connection.smtp_security,
        ConnectionSecurity::Starttls
    );
    assert_eq!(draft.aliases.len(), 1);
    assert_eq!(draft.aliases[0].email, "support@example.com");
    assert_eq!(
        draft.aliases[0].reply_to.as_deref(),
        Some("help@example.com")
    );
    // Only the enabled signature is kept; text becomes editor HTML.
    assert_eq!(draft.signatures.len(), 1);
    assert_eq!(
        draft.signatures[0].html,
        "<div>Jane &amp; Co.<br>Example Ltd</div>"
    );
    assert!(draft.signatures[0].default);
    assert_eq!(draft.color.as_deref(), Some("#2196F3"));

    let gmail = &plans[1];
    assert_eq!(gmail.status, PlanStatus::Ready);
    assert_eq!(gmail.sign_in, SignIn::Google);
    assert_eq!(gmail.email, "jane.doe@gmail.com");
    assert_eq!(codes(gmail), [FindingCode::AliasesManagedByProvider]);
    let gmail = &gmail.draft.as_ref().unwrap().config;
    assert_eq!(gmail.provider, Provider::Gmail);
    assert_eq!(gmail.incoming_host, "imap.gmail.com");

    // A POP3 account keeps downloading, and mail deleted here leaves the
    // server as mail deleted in K-9 did.
    let pop = &plans[2];
    assert_eq!(pop.status, PlanStatus::Ready);
    assert_eq!(pop.email, "old@example.net");
    assert_eq!(
        codes(pop),
        [
            FindingCode::UnencryptedOutgoing,
            FindingCode::RemovesServerMail
        ]
    );
    let pop = &pop.draft.as_ref().unwrap().config;
    assert_eq!(pop.mail_protocol, MailProtocol::Pop3);
    assert_eq!(
        (pop.incoming_host.as_str(), pop.incoming_port),
        ("pop.example.net", 995)
    );
    assert_eq!(pop.settings.keep_on_server, KeepOnServer::UntilDeleted);
}

#[test]
fn k9_passwords_and_document_types_are_never_used() {
    let bytes = std::fs::read(fixture("k9-settings.k9s")).unwrap();
    let inventory = migration::k9::parse(&bytes).unwrap();
    assert!(!format!("{inventory:?}").contains("must-never-be-read"));

    let hostile = br#"<?xml version="1.0"?>
<!DOCTYPE k9settings [<!ENTITY x SYSTEM "file:///etc/passwd">]>
<k9settings format="1" version="112"><accounts><account uuid="a"><name>&x;</name></account></accounts></k9settings>"#;
    assert!(matches!(
        migration::k9::parse(hostile),
        Err(SourceError::Malformed { .. })
    ));
    assert!(matches!(
        migration::k9::parse(br#"<k9settings format="2" version="112"/>"#),
        Err(SourceError::UnsupportedVersion(version)) if version == "2"
    ));
}

#[test]
fn k9_legacy_exports_use_the_defaults_k9_applies() {
    let inventory = migration::k9::parse(
        br#"<k9settings format="1" version="60"><accounts><account uuid="a">
<incoming-server type="IMAP"><host>imap.example.com</host>
<authentication-type>LOGIN</authentication-type><username>me</username></incoming-server>
<outgoing-server type="SMTP"><host>smtp.example.com</host><port>465</port>
<connection-security>SSL_TLS_OPTIONAL</connection-security>
<authentication-type>AUTOMATIC</authentication-type><username>me</username></outgoing-server>
<settings><value key="chipColor">-14575885</value></settings>
<identities><identity><email>me@example.com</email>
<settings><value key="signature">Legacy</value></settings></identity></identities>
</account></accounts></k9settings>"#,
    )
    .unwrap();
    let plans = migration::plan(&inventory, &[]);
    let draft = plans[0].draft.as_ref().expect("legacy account imports");
    // Before content version 92 a missing security mode meant TLS.
    assert_eq!(
        draft.config.settings.connection.incoming_security,
        ConnectionSecurity::Tls
    );
    assert_eq!(draft.config.incoming_port, 993);
    // Before version 68 signatures were on unless switched off.
    assert_eq!(draft.signatures[0].html, "<div>Legacy</div>");
    assert_eq!(draft.color.as_deref(), Some("#2196F3"));
}

#[test]
fn thunderbird_profile_reads_accounts_identities_signatures_and_colors() {
    let profile = thunderbird_profile();
    let inventory = accounts(migration::open(profile.path()).unwrap());
    assert_eq!(inventory.source, SourceKind::ThunderbirdProfile);
    assert_eq!(
        inventory
            .other_entries
            .iter()
            .map(|entry| entry.kind)
            .collect::<Vec<_>>(),
        [
            OtherEntryKind::Newsgroups,
            OtherEntryKind::Feeds,
            OtherEntryKind::LocalFolders
        ]
    );
    let plans = migration::plan(&inventory, &[]);
    assert_eq!(plans.len(), 2);

    let example = &plans[0];
    assert_eq!(example.label.as_deref(), Some("Ana — Example"));
    assert_eq!(example.status, PlanStatus::Ready);
    // The third identity sends through another server.
    assert_eq!(codes(example), [FindingCode::AliasesSkipped]);
    let draft = example.draft.as_ref().unwrap();
    assert_eq!(draft.config.email, "ana@example.org");
    assert_eq!(
        draft.config.display_name.as_deref(),
        Some("Ana Ruiz García")
    );
    assert_eq!(draft.config.username, "ana");
    assert_eq!(draft.config.incoming_port, 993);
    assert_eq!(draft.config.smtp_port, 587);
    assert_eq!(
        draft
            .aliases
            .iter()
            .map(|alias| alias.email.as_str())
            .collect::<Vec<_>>(),
        ["lists@example.org"]
    );
    assert_eq!(draft.signatures.len(), 2);
    assert_eq!(draft.signatures[1].name, "lists@example.org");
    assert_eq!(draft.signatures[1].html, "<div>Ana<br>List moderator</div>");
    assert_eq!(draft.color.as_deref(), Some("#AA3355"));

    let outlook = &plans[1];
    assert_eq!(outlook.status, PlanStatus::Ready);
    assert_eq!(outlook.sign_in, SignIn::Microsoft);
    assert!(outlook.findings.is_empty(), "{:?}", outlook.findings);
}

#[test]
fn thunderbird_reads_a_lone_prefs_file_without_touching_other_files() {
    let inventory = accounts(migration::open(&fixture("prefs.js")).unwrap());
    let plans = migration::plan(&inventory, &[]);
    let example = plans[0].draft.as_ref().unwrap();
    // Without the profile folder the attached signature file is unavailable.
    assert_eq!(example.signatures.len(), 1);
    assert!(
        plans[0]
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::SignatureFileNotImported)
    );
    assert_eq!(example.color, None);
}

#[test]
fn thunderbird_signature_paths_cannot_leave_the_profile() {
    let profile = thunderbird_profile();
    let outside = tempfile::tempdir().unwrap();
    let secret = outside.path().join("secret.txt");
    std::fs::write(&secret, "secret").unwrap();
    // A link inside the profile that leads outside it.
    #[cfg(unix)]
    let linked = {
        std::os::unix::fs::symlink(&secret, profile.path().join("signatures/linked.txt")).unwrap();
        "[ProfD]signatures/linked.txt"
    };
    #[cfg(not(unix))]
    let linked = "[ProfD]../secret.txt";
    let mut prefs = std::fs::read_to_string(profile.path().join("prefs.js")).unwrap();
    prefs.push_str(&format!(
        "user_pref(\"mail.identity.id1.attach_signature\", true);\n\
         user_pref(\"mail.identity.id1.sig_file\", {:?});\n\
         user_pref(\"mail.identity.id2.sig_file-rel\", {linked:?});\n",
        secret.display().to_string()
    ));
    std::fs::write(profile.path().join("prefs.js"), prefs).unwrap();

    let inventory = accounts(migration::open(profile.path()).unwrap());
    assert!(!format!("{inventory:?}").contains("secret\""));
    let plans = migration::plan(&inventory, &[]);
    assert!(plans[0].draft.as_ref().unwrap().signatures.is_empty());
    assert!(
        plans[0]
            .findings
            .iter()
            .any(|finding| finding.code == FindingCode::SignatureFileNotImported)
    );
}

#[test]
fn thunderbird_profile_export_archives_are_read_in_place() {
    let profile = thunderbird_profile();
    let directory = tempfile::tempdir().unwrap();
    let archive_path = directory.path().join("thunderbird_profile_backup.zip");
    let mut archive = zip::ZipWriter::new(std::fs::File::create(&archive_path).unwrap());
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for name in ["prefs.js", "folderTree.json", "signatures/lists.txt"] {
        archive.start_file(name, options).unwrap();
        archive
            .write_all(&std::fs::read(profile.path().join(name)).unwrap())
            .unwrap();
    }
    // Mail stores in the archive are never opened.
    archive
        .start_file("ImapMail/imap.example.org/INBOX", options)
        .unwrap();
    archive.write_all(&[0xFF; 1024]).unwrap();
    archive.finish().unwrap();

    let inventory = accounts(migration::open(&archive_path).unwrap());
    let plans = migration::plan(&inventory, &[]);
    let draft = plans[0].draft.as_ref().unwrap();
    assert_eq!(draft.signatures.len(), 2);
    assert_eq!(draft.color.as_deref(), Some("#AA3355"));
}

#[test]
fn flectar_backups_and_unknown_files_are_recognized() {
    let directory = tempfile::tempdir().unwrap();
    let backup = directory.path().join("backup");
    std::fs::write(
        &backup,
        r#"{"format":"flectar-mail-account-backup","version":1,"accounts":[]}"#,
    )
    .unwrap();
    assert!(matches!(
        migration::open(&backup).unwrap(),
        Source::FlectarBackup(bytes) if bytes.starts_with(b"{")
    ));
    let other = directory.path().join("notes.txt");
    std::fs::write(&other, "hello").unwrap();
    assert_eq!(migration::open(&other), Err(SourceError::UnknownFormat));
    let empty = directory.path().join("prefs.js");
    std::fs::write(&empty, "user_pref(\"browser.startup.page\", 3);").unwrap();
    assert_eq!(migration::open(&empty), Err(SourceError::NoAccounts));
}

#[tokio::test]
async fn import_creates_paused_accounts_with_identities_signatures_and_colors() {
    let (_directory, core) = core().await;
    let inventory = accounts(migration::open(&fixture("k9-settings.k9s")).unwrap());
    let drafts = migration::plan(&inventory, &[])
        .into_iter()
        .filter_map(|plan| plan.draft)
        .collect::<Vec<_>>();
    let report = core.import_accounts(drafts.clone()).await.unwrap();
    assert_eq!(
        report
            .imported
            .iter()
            .map(|account| (account.email.as_str(), account.sign_in))
            .collect::<Vec<_>>(),
        [
            ("jane@example.com", SignIn::Password),
            ("jane.doe@gmail.com", SignIn::Google),
            ("old@example.net", SignIn::Password)
        ]
    );
    assert!(report.already_present.is_empty() && report.failed.is_empty());
    // An imported POP3 account starts with its folders on this device.
    let pop3 = report.imported[2].account_id;
    assert_eq!(core.list_folders(Some(pop3)).await.unwrap().len(), 6);

    let listed = core.list_accounts().await.unwrap();
    assert!(
        listed
            .iter()
            .all(|account| account.sync_state == "needs_reauth")
    );
    let work = report.imported[0].account_id;
    let identities = core.list_sender_identities(work).await.unwrap();
    let alias = identities
        .iter()
        .find(|identity| identity.email == "support@example.com")
        .unwrap();
    assert_eq!(alias.source, SenderIdentitySource::Manual);
    assert_eq!(alias.reply_to_email.as_deref(), Some("help@example.com"));

    let settings = core.get_settings().await.unwrap();
    let signature = settings
        .signature_list
        .iter()
        .find(|signature| signature.account_id == work)
        .unwrap();
    let defaults = &settings.signature_defaults[&work.to_string()];
    assert_eq!(defaults.new_id.as_ref(), Some(&signature.id));
    assert_eq!(defaults.reply_id.as_ref(), Some(&signature.id));
    assert_eq!(settings.account_colors[&work.to_string()], "#2196F3");

    // Repeating the import changes nothing and says so.
    let again = core.import_accounts(drafts).await.unwrap();
    assert!(again.imported.is_empty());
    assert_eq!(again.already_present.len(), 3);
    let plans = migration::plan(
        &inventory,
        &listed
            .iter()
            .map(|account| account.email.clone())
            .collect::<Vec<_>>(),
    );
    assert_eq!(plans[0].status, PlanStatus::AlreadyAdded);
}

#[tokio::test]
async fn another_gmail_spelling_imports_as_the_same_account() {
    let (_directory, core) = core().await;
    let inventory = accounts(migration::open(&fixture("k9-settings.k9s")).unwrap());
    let gmail = migration::plan(&inventory, &[])
        .into_iter()
        .filter_map(|plan| plan.draft)
        .find(|draft| draft.config.email == "jane.doe@gmail.com")
        .unwrap();
    let mut respelled = gmail.clone();
    respelled.config.email = "JaneDoe@googlemail.com".into();
    respelled.config.username = respelled.config.email.clone();

    let report = core
        .import_accounts(vec![gmail, respelled.clone()])
        .await
        .unwrap();
    assert_eq!(report.imported.len(), 1);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(
        report.failed[0].error,
        "the address appears twice in this import"
    );

    let again = core.import_accounts(vec![respelled]).await.unwrap();
    assert!(again.imported.is_empty());
    assert_eq!(again.already_present, ["JaneDoe@googlemail.com"]);
}

#[tokio::test]
async fn one_invalid_draft_does_not_block_or_corrupt_the_others() {
    let (_directory, core) = core().await;
    let profile = thunderbird_profile();
    let inventory = accounts(migration::open(profile.path()).unwrap());
    let mut drafts = migration::plan(&inventory, &[])
        .into_iter()
        .filter_map(|plan| plan.draft)
        .collect::<Vec<_>>();
    drafts[0].config.incoming_port = 0;
    drafts[0].color = Some("not a color".into());

    let report = core.import_accounts(drafts).await.unwrap();
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].email, "ana@example.org");
    assert_eq!(report.imported.len(), 1);
    assert_eq!(report.imported[0].email, "ana@outlook.com");
    let accounts = core.list_accounts().await.unwrap();
    assert_eq!(accounts.len(), 1);
    let settings = core.get_settings().await.unwrap();
    assert!(settings.signature_list.is_empty());
    assert!(settings.account_colors.is_empty());
}

#[tokio::test]
async fn imported_signatures_are_sanitized_like_edited_ones() {
    let (_directory, core) = core().await;
    let profile = thunderbird_profile();
    let inventory = accounts(migration::open(profile.path()).unwrap());
    let drafts = migration::plan(&inventory, &[])
        .into_iter()
        .filter_map(|plan| plan.draft)
        .collect::<Vec<_>>();
    core.import_accounts(drafts).await.unwrap();
    let settings = core.get_settings().await.unwrap();
    let html = &settings.signature_list[0].html;
    assert!(html.contains("<b>Ana</b>"));
    assert!(!html.contains("script"));
}

#[test]
fn thunderbird_profile_edge_cases_follow_thunderbird() {
    let profile = tempfile::tempdir().unwrap();
    std::fs::write(
        profile.path().join("prefs.js"),
        // A byte order mark, no default outgoing server, a disabled identity,
        // an identity naming a deleted server, and a bad account color.
        "\u{feff}user_pref(\"mail.accountmanager.accounts\", \"account1,account2\");
user_pref(\"mail.account.account1.identities\", \"id0,id1\");
user_pref(\"mail.account.account1.server\", \"server1\");
user_pref(\"mail.account.account2.server\", \"server9\");
user_pref(\"mail.identity.id0.useremail\", \"retired@example.org\");
user_pref(\"mail.identity.id0.valid\", false);
user_pref(\"mail.identity.id1.useremail\", \"ana@example.org\");
user_pref(\"mail.identity.id1.smtpServer\", \"smtp7\");
user_pref(\"mail.identity.id1.attach_signature\", true);
user_pref(\"mail.identity.id1.sig_file-rel\", \"[ProfD]./sig.txt\");
user_pref(\"mail.server.server1.hostname\", \"imap.example.org\");
user_pref(\"mail.server.server1.socketType\", 3);
user_pref(\"mail.server.server1.type\", \"imap\");
user_pref(\"mail.server.server1.userName\", \"ana\");
user_pref(\"mail.smtpserver.smtp1.hostname\", \"smtp.example.org\");
user_pref(\"mail.smtpserver.smtp1.try_ssl\", 3);
user_pref(\"mail.smtpserver.smtp1.username\", \"ana\");
user_pref(\"mail.smtpservers\", \"smtp1\");
",
    )
    .unwrap();
    // Windows editors save signatures as UTF-16 or the ANSI code page.
    let mut utf16 = vec![0xFF, 0xFE];
    utf16.extend("Ana · Example".encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(profile.path().join("sig.txt"), utf16).unwrap();
    std::fs::write(
        profile.path().join("folderTree.json"),
        r#"{"colors":{"imap://ana@imap.example.org":"red"}}"#,
    )
    .unwrap();

    let inventory = accounts(migration::open(profile.path()).unwrap());
    // account2 points at a server that does not exist and is skipped.
    assert_eq!(inventory.accounts.len(), 1);
    let plans = migration::plan(&inventory, &[]);
    assert_eq!(
        plans[0].status,
        PlanStatus::Ready,
        "{:?}",
        plans[0].findings
    );
    let draft = plans[0].draft.as_ref().unwrap();
    assert_eq!(draft.config.email, "ana@example.org");
    assert_eq!(
        (draft.config.smtp_host.as_str(), draft.config.smtp_port),
        ("smtp.example.org", 465)
    );
    assert!(draft.aliases.is_empty());
    assert_eq!(draft.signatures[0].html, "<div>Ana · Example</div>");
    assert_eq!(draft.color, None);

    std::fs::write(profile.path().join("sig.txt"), b"Caf\xe9").unwrap();
    let inventory = accounts(migration::open(profile.path()).unwrap());
    let plans = migration::plan(&inventory, &[]);
    assert_eq!(
        plans[0].draft.as_ref().unwrap().signatures[0].html,
        "<div>Café</div>"
    );
}

#[test]
fn thunderbird_pop3_accounts_keep_their_rules_for_mail_on_the_server() {
    let profile = tempfile::tempdir().unwrap();
    let mut prefs = String::from(
        "user_pref(\"mail.accountmanager.accounts\", \"account1,account2,account3,account4,account5\");
user_pref(\"mail.server.default.leave_on_server\", true);
user_pref(\"mail.smtpserver.smtp1.hostname\", \"smtp.example.org\");
user_pref(\"mail.smtpserver.smtp1.try_ssl\", 3);
user_pref(\"mail.smtpserver.smtp1.username\", \"me\");
user_pref(\"mail.smtpservers\", \"smtp1\");
",
    );
    // Thunderbird's account wizard, the profile's default, a day count no
    // choice matches, removal right after download, and Gmail over POP3.
    let servers = [
        (
            "wizard@example.org",
            "pop.example.org",
            "me",
            &[
                "delete_by_age_from_server\", true",
                "num_days_to_leave_on_server\", 14",
                "delete_mail_left_on_server\", true",
            ][..],
        ),
        ("default@example.org", "pop.example.org", "me", &[][..]),
        (
            "ten@example.org",
            "pop.example.org",
            "me",
            &[
                "delete_by_age_from_server\", true",
                "num_days_to_leave_on_server\", 10",
            ][..],
        ),
        (
            "once@example.org",
            "pop.example.org",
            "me",
            &["leave_on_server\", false"][..],
        ),
        ("me@gmail.com", "pop.gmail.com", "me@gmail.com", &[][..]),
    ];
    for (index, (email, host, user, extra)) in servers.iter().enumerate() {
        let n = index + 1;
        prefs.push_str(&format!(
            "user_pref(\"mail.account.account{n}.identities\", \"id{n}\");
user_pref(\"mail.account.account{n}.server\", \"server{n}\");
user_pref(\"mail.identity.id{n}.useremail\", \"{email}\");
user_pref(\"mail.server.server{n}.hostname\", \"{host}\");
user_pref(\"mail.server.server{n}.socketType\", 3);
user_pref(\"mail.server.server{n}.type\", \"pop3\");
user_pref(\"mail.server.server{n}.userName\", \"{user}\");
"
        ));
        for setting in *extra {
            prefs.push_str(&format!("user_pref(\"mail.server.server{n}.{setting});\n"));
        }
    }
    std::fs::write(profile.path().join("prefs.js"), prefs).unwrap();

    let plans = migration::plan(&accounts(migration::open(profile.path()).unwrap()), &[]);
    let imported = |index: usize| {
        let plan = &plans[index];
        assert_eq!(plan.status, PlanStatus::Ready, "{:?}", plan.findings);
        let config = plan.draft.as_ref().unwrap().config.clone();
        (codes(plan), config)
    };
    let removes = FindingCode::RemovesServerMail;
    let (findings, wizard) = imported(0);
    assert_eq!(findings, [removes]);
    assert_eq!(plans[0].findings[0].detail, "fourteenDays");
    assert_eq!(wizard.mail_protocol, MailProtocol::Pop3);
    assert_eq!(wizard.incoming_port, 995);
    assert_eq!(wizard.settings.keep_on_server, KeepOnServer::FourteenDays);
    let (findings, default) = imported(1);
    assert!(findings.is_empty());
    assert_eq!(default.settings.keep_on_server, KeepOnServer::Always);
    let (findings, ten) = imported(2);
    assert_eq!(findings, [removes, FindingCode::ServerMailRuleApproximated]);
    assert_eq!(ten.settings.keep_on_server, KeepOnServer::FourteenDays);
    let (findings, once) = imported(3);
    assert_eq!(findings, [removes]);
    assert_eq!(once.settings.keep_on_server, KeepOnServer::UntilDownloaded);
    let (findings, gmail) = imported(4);
    assert_eq!(
        findings,
        [
            FindingCode::ProviderSyncReplacesPop3,
            FindingCode::GoogleSignInReplacesPassword,
            FindingCode::OutgoingServerReplaced
        ]
    );
    assert_eq!(plans[4].findings[0].detail, "Gmail");
    assert_eq!(gmail.provider, Provider::Gmail);
    assert_eq!(gmail.settings.keep_on_server, KeepOnServer::Always);
}

#[test]
fn thunderbird_exports_tolerate_an_enclosing_folder() {
    let profile = thunderbird_profile();
    let directory = tempfile::tempdir().unwrap();
    let archive_path = directory.path().join("export.zip");
    let mut archive = zip::ZipWriter::new(std::fs::File::create(&archive_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    for name in ["prefs.js", "signatures/lists.txt"] {
        archive
            .start_file(format!("x7k2.default/{name}"), options)
            .unwrap();
        archive
            .write_all(&std::fs::read(profile.path().join(name)).unwrap())
            .unwrap();
    }
    archive.finish().unwrap();
    let inventory = accounts(migration::open(&archive_path).unwrap());
    let plans = migration::plan(&inventory, &[]);
    assert_eq!(plans[0].draft.as_ref().unwrap().signatures.len(), 2);
}

#[test]
fn a_thunderbird_data_folder_without_a_default_profile_asks_for_one() {
    let root = tempfile::tempdir().unwrap();
    for name in ["a.one", "b.two"] {
        std::fs::create_dir(root.path().join(name)).unwrap();
        std::fs::copy(fixture("prefs.js"), root.path().join(name).join("prefs.js")).unwrap();
    }
    std::fs::write(
        root.path().join("profiles.ini"),
        "[Profile0]\nName=one\nIsRelative=1\nPath=a.one\n\n[Profile1]\nName=two\nIsRelative=1\nPath=b.two\n",
    )
    .unwrap();
    assert_eq!(
        migration::open(root.path()),
        Err(SourceError::MultipleProfiles)
    );
}

#[test]
fn k9_documents_beyond_the_format_limits_are_rejected() {
    let deep = format!(
        "<k9settings format=\"1\">{}{}</k9settings>",
        "<a>".repeat(40),
        "</a>".repeat(40)
    );
    let wide = format!(
        "<k9settings format=\"1\" {}/>",
        (0..20)
            .map(|index| format!("a{index}=\"\""))
            .collect::<Vec<_>>()
            .join(" ")
    );
    let many = format!(
        "<k9settings format=\"1\"><accounts>{}</accounts></k9settings>",
        "<account uuid=\"a\"/>".repeat(101)
    );
    for document in [deep, wide, many] {
        assert!(matches!(
            migration::k9::parse(document.as_bytes()),
            Err(SourceError::Malformed { .. })
        ));
    }
}

#[tokio::test]
async fn import_rejects_aliases_editors_would_reject_and_batch_duplicates() {
    let (_directory, core) = core().await;
    let inventory = accounts(migration::open(&fixture("k9-settings.k9s")).unwrap());
    let plans = migration::plan(&inventory, &[]);
    let work = plans[0].draft.clone().unwrap();
    let mut shouted = work.clone();
    shouted.config.email = work.config.email.to_uppercase();
    let mut wildcard = work.clone();
    wildcard.config.email = "other@example.com".into();
    wildcard.aliases[0].email = "*@example.com".into();
    let mut echo = work.clone();
    echo.config.email = "third@example.com".into();
    echo.aliases[0].email = "Third@Example.com".into();
    let mut gmail = plans[1].draft.clone().unwrap();
    gmail.aliases.push(migration::AliasDraft {
        email: "alias@example.com".into(),
        display_name: None,
        reply_to: None,
    });

    let report = core
        .import_accounts(vec![work, shouted, wildcard, echo, gmail])
        .await
        .unwrap();
    assert_eq!(report.imported.len(), 1);
    assert_eq!(
        report
            .failed
            .iter()
            .map(|failed| failed.email.as_str())
            .collect::<Vec<_>>(),
        [
            "JANE@EXAMPLE.COM",
            "other@example.com",
            "third@example.com",
            "jane.doe@gmail.com"
        ]
    );
    assert_eq!(core.list_accounts().await.unwrap().len(), 1);
}

#[test]
fn thunderbird_references_cannot_multiply_a_small_profile() {
    // One identity listed thousands of times is one identity.
    let prefs = format!(
        "user_pref(\"mail.accountmanager.accounts\", \"a\");\n\
         user_pref(\"mail.account.a.server\", \"s\");\n\
         user_pref(\"mail.account.a.identities\", \"{}\");\n\
         user_pref(\"mail.identity.i.useremail\", \"ana@example.org\");\n\
         user_pref(\"mail.identity.i.htmlSigText\", \"{}\");\n\
         user_pref(\"mail.server.s.type\", \"imap\");\n\
         user_pref(\"mail.server.s.hostname\", \"imap.example.org\");\n",
        vec!["i"; 5_000].join(","),
        "x".repeat(1024 * 1024)
    );
    let inventory = migration::thunderbird::parse_prefs(prefs.as_bytes()).unwrap();
    assert_eq!(inventory.accounts[0].identities.len(), 1);

    // Accounts or feeds sharing a server cannot copy its huge name past the
    // limit.
    let keys = (0..40).map(|index| format!("a{index}")).collect::<Vec<_>>();
    for kind in ["imap", "rss"] {
        let mut prefs = format!(
            "user_pref(\"mail.accountmanager.accounts\", \"{}\");\n\
             user_pref(\"mail.server.s.type\", \"{kind}\");\n\
             user_pref(\"mail.server.s.hostname\", \"imap.example.org\");\n\
             user_pref(\"mail.server.s.name\", \"{}\");\n",
            keys.join(","),
            "n".repeat(1024 * 1024)
        );
        for key in &keys {
            prefs.push_str(&format!(
                "user_pref(\"mail.account.{key}.server\", \"s\");\n"
            ));
        }
        assert_eq!(
            migration::thunderbird::parse_prefs(prefs.as_bytes()),
            Err(SourceError::TooLarge),
            "{kind}"
        );
    }
}

#[test]
fn thunderbird_outgoing_servers_follow_thunderbird() {
    let account = "user_pref(\"mail.accountmanager.accounts\", \"a\");\n\
                   user_pref(\"mail.account.a.server\", \"s\");\n\
                   user_pref(\"mail.account.a.identities\", \"i\");\n\
                   user_pref(\"mail.identity.i.useremail\", \"ana@contoso.com\");\n";
    // A default that names an unlisted server leaves identities without
    // one, as in Thunderbird.
    let unlisted = format!(
        "{account}user_pref(\"mail.server.s.type\", \"imap\");\n\
         user_pref(\"mail.server.s.hostname\", \"imap.contoso.com\");\n\
         user_pref(\"mail.server.s.socketType\", 3);\n\
         user_pref(\"mail.server.s.userName\", \"ana\");\n\
         user_pref(\"mail.smtp.defaultserver\", \"smtp9\");\n\
         user_pref(\"mail.smtpserver.smtp1.hostname\", \"smtp.contoso.com\");\n\
         user_pref(\"mail.smtpserver.smtp9.hostname\", \"smtp.old.example\");\n\
         user_pref(\"mail.smtpservers\", \"smtp1\");\n"
    );
    let inventory = migration::thunderbird::parse_prefs(unlisted.as_bytes()).unwrap();
    assert_eq!(inventory.accounts[0].outgoing, None);
    assert!(
        codes(&migration::plan(&inventory, &[])[0]).contains(&FindingCode::MissingOutgoingServer)
    );

    // Exchange outgoing servers keep their settings in their own branch.
    let exchange = format!(
        "{account}user_pref(\"mail.server.s.type\", \"ews\");\n\
         user_pref(\"mail.server.s.hostname\", \"outlook.office365.com\");\n\
         user_pref(\"mail.server.s.userName\", \"ana@contoso.com\");\n\
         user_pref(\"mail.identity.i.smtpServer\", \"ews1\");\n\
         user_pref(\"mail.smtpserver.ews1.type\", \"ews\");\n\
         user_pref(\"mail.outgoingserver.ews1.ews_url\", \"https://outlook.office365.com/EWS/Exchange.asmx\");\n\
         user_pref(\"mail.smtpserver.smtp1.hostname\", \"smtp.gmail.com\");\n\
         user_pref(\"mail.smtpservers\", \"smtp1,ews1\");\n"
    );
    let inventory = migration::thunderbird::parse_prefs(exchange.as_bytes()).unwrap();
    let outgoing = inventory.accounts[0].outgoing.as_ref().unwrap();
    assert_eq!(
        (outgoing.protocol, outgoing.host.as_str()),
        (ServerProtocol::Ews, "outlook.office365.com")
    );
    let plan = &migration::plan(&inventory, &[])[0];
    assert_eq!(plan.sign_in, SignIn::Microsoft);
    assert!(!codes(plan).contains(&FindingCode::OutgoingServerReplaced));
}

#[test]
fn folders_that_are_not_readable_profiles_say_why() {
    let folder = tempfile::tempdir().unwrap();
    assert_eq!(
        migration::open(folder.path()),
        Err(SourceError::NotAProfile)
    );
    let prefs = std::fs::File::create(folder.path().join("prefs.js")).unwrap();
    prefs.set_len(migration::MAX_SOURCE_BYTES + 1).unwrap();
    assert_eq!(migration::open(folder.path()), Err(SourceError::TooLarge));
}

#[test]
fn thunderbird_exports_with_windows_separators_or_several_profiles() {
    let profile = thunderbird_profile();
    let directory = tempfile::tempdir().unwrap();
    let write = |path: &Path, folders: &[&str], separator: &str| {
        let mut archive = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for folder in folders {
            for name in ["prefs.js", "signatures/lists.txt"] {
                archive
                    .start_file(
                        format!("{folder}{separator}{}", name.replace('/', separator)),
                        zip::write::SimpleFileOptions::default(),
                    )
                    .unwrap();
                archive
                    .write_all(&std::fs::read(profile.path().join(name)).unwrap())
                    .unwrap();
            }
        }
        archive.finish().unwrap();
    };
    // PowerShell's Compress-Archive writes `\` separators.
    let windows = directory.path().join("windows.zip");
    write(&windows, &["x7k2.default"], "\\");
    let plans = migration::plan(&accounts(migration::open(&windows).unwrap()), &[]);
    assert_eq!(plans[0].draft.as_ref().unwrap().signatures.len(), 2);

    let several = directory.path().join("several.zip");
    write(&several, &["a.one", "b.two"], "/");
    assert_eq!(
        migration::open(&several),
        Err(SourceError::MultipleProfiles)
    );
}

#[test]
fn thunderbird_signature_files_are_read_as_thunderbird_reads_them() {
    let profile = thunderbird_profile();
    let mut prefs = std::fs::read_to_string(profile.path().join("prefs.js")).unwrap();
    prefs.push_str(
        "user_pref(\"mail.identity.id1.attach_signature\", true);\n\
         user_pref(\"mail.identity.id1.sig_file-rel\", \"[ProfD]sig.d/work\");\n\
         user_pref(\"mail.identity.id3.attach_signature\", true);\n\
         user_pref(\"mail.identity.id3.sig_file-rel\", \"[ProfD]signatures/ru.html\");\n",
    );
    std::fs::write(profile.path().join("prefs.js"), prefs).unwrap();
    // A file without an extension in a folder that has one is plain text.
    std::fs::create_dir(profile.path().join("sig.d")).unwrap();
    std::fs::write(profile.path().join("sig.d/work"), "Plain").unwrap();
    // A legacy HTML file declares its encoding.
    let mut html = b"<meta charset=\"windows-1251\"><p>".to_vec();
    html.extend(encoding_rs::WINDOWS_1251.encode("Привет").0.iter());
    html.extend(b"</p>");
    std::fs::write(profile.path().join("signatures/ru.html"), html).unwrap();

    let inventory = accounts(migration::open(profile.path()).unwrap());
    let plans = migration::plan(&inventory, &[]);
    let signatures = &plans[0].draft.as_ref().unwrap().signatures;
    assert!(
        signatures
            .iter()
            .any(|signature| signature.html == "<div>Plain</div>"),
        "{signatures:?}"
    );
    assert!(
        signatures
            .iter()
            .any(|signature| signature.html.contains("Привет")),
        "{signatures:?}"
    );
}

#[test]
fn thunderbird_colors_follow_the_original_host_of_a_renamed_server() {
    let profile = tempfile::tempdir().unwrap();
    std::fs::write(
        profile.path().join("prefs.js"),
        "user_pref(\"mail.accountmanager.accounts\", \"account1\");
user_pref(\"mail.account.account1.identities\", \"id1\");
user_pref(\"mail.account.account1.server\", \"server1\");
user_pref(\"mail.identity.id1.useremail\", \"ana@example.org\");
user_pref(\"mail.server.server1.hostname\", \"old.example.org\");
user_pref(\"mail.server.server1.realhostname\", \"imap.example.org\");
user_pref(\"mail.server.server1.type\", \"imap\");
user_pref(\"mail.server.server1.userName\", \"ana\");
",
    )
    .unwrap();
    std::fs::write(
        profile.path().join("folderTree.json"),
        r##"{"open":{"all":true},"colors":{"imap://ana@OLD.example.org/":"#123456"}}"##,
    )
    .unwrap();
    let inventory = accounts(migration::open(profile.path()).unwrap());
    let account = &inventory.accounts[0];
    assert_eq!(account.incoming.as_ref().unwrap().host, "imap.example.org");
    assert_eq!(account.color.as_deref(), Some("#123456"));
}

#[tokio::test]
async fn import_keeps_an_account_whose_color_is_unusable() {
    let (_directory, core) = core().await;
    let inventory = accounts(migration::open(&fixture("k9-settings.k9s")).unwrap());
    let mut draft = migration::plan(&inventory, &[])[0].draft.clone().unwrap();
    draft.color = Some("red".into());
    let report = core.import_accounts(vec![draft]).await.unwrap();
    assert_eq!(report.imported.len(), 1);
    assert!(core.get_settings().await.unwrap().account_colors.is_empty());
}
