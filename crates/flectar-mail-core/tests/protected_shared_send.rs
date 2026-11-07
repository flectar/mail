//! A signed message scheduled from an IMAP account travels to the user's
//! other devices already signed, so any of them sends it without the key.
//! Needs GnuPG and the sync server that `FLECTAR_MAIL_SYNC_SERVER` names (see
//! device_sync_e2e.rs); skipped without either. Its own test binary, because
//! it points GnuPG at a test keyring through the process environment.

use flectar_mail_core::{
    Core,
    accounts::credentials::{CredentialStore, Slot},
    config::Paths,
    error::{CoreError, Result},
    mail_security::MailSecurity,
    models::{
        AccountSettings, Address, AuthKind, ConnectionSecurity, MailConnectionSettings,
        MailProtocol, PortableAccountConfig, Provider, QueueSendArgs, SaveDraftArgs, now_ms, roles,
    },
};
use std::collections::{BTreeMap, HashMap};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
struct MemoryCredentials(Mutex<HashMap<(i64, Slot), String>>);

impl CredentialStore for MemoryCredentials {
    fn store(&self, account_id: i64, slot: Slot, secret: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert((account_id, slot), secret.into());
        Ok(())
    }

    fn load(&self, account_id: i64, slot: Slot) -> Result<String> {
        self.0
            .lock()
            .unwrap()
            .get(&(account_id, slot))
            .cloned()
            .ok_or_else(|| CoreError::Auth("no stored credential".into()))
    }

    fn delete(&self, account_id: i64, slot: Slot) -> Result<()> {
        self.0.lock().unwrap().remove(&(account_id, slot));
        Ok(())
    }

    fn delete_all(&self, account_id: i64) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .retain(|(owner, _), _| *owner != account_id);
        Ok(())
    }
}

struct Server {
    child: Child,
    url: String,
    _dir: tempfile::TempDir,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn start_server(binary: &str) -> Server {
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let dir = tempfile::tempdir().unwrap();
    let url = format!("http://127.0.0.1:{port}");
    let child = Command::new(binary)
        .env("FLECTAR_MAIL_SYNC_PUBLIC_URL", &url)
        .env("FLECTAR_MAIL_SYNC_LISTEN", format!("127.0.0.1:{port}"))
        .env("FLECTAR_MAIL_SYNC_DATABASE", dir.path().join("sync.db"))
        .env("RUST_LOG", "warn")
        .spawn()
        .unwrap();
    let server = Server {
        child,
        url,
        _dir: dir,
    };
    for _ in 0..100 {
        if reqwest::get(format!("{}/healthz", server.url))
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            return server;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the sync server did not start");
}

async fn start_core(dir: &std::path::Path) -> Core {
    Core::start_mail_ui_with_credentials(
        Paths::for_tests(dir),
        Arc::new(MemoryCredentials::default()),
    )
    .await
    .unwrap()
}

fn imap_account(email: &str) -> PortableAccountConfig {
    PortableAccountConfig {
        email: email.into(),
        display_name: Some("Work".into()),
        provider: Provider::Generic,
        auth_kind: AuthKind::Password,
        mail_protocol: MailProtocol::Imap,
        username: email.into(),
        jmap_url: String::new(),
        incoming_host: "imap.example.com".into(),
        incoming_port: 993,
        smtp_host: "smtp.example.com".into(),
        smtp_port: 465,
        settings: AccountSettings::new(MailConnectionSettings::new(
            ConnectionSecurity::Tls,
            ConnectionSecurity::Tls,
        )),
    }
}

/// A signing key for `user` in a new keyring at `home`; returns its
/// fingerprint.
fn signing_key(home: &std::path::Path, user: &str) -> String {
    let gpg = |args: &[&str]| {
        let output = Command::new("gpg")
            .arg("--batch")
            .arg("--homedir")
            .arg(home)
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    gpg(&[
        "--pinentry-mode",
        "loopback",
        "--passphrase",
        "",
        "--quick-gen-key",
        user,
        "ed25519",
        "sign",
        "1d",
    ]);
    gpg(&["--with-colons", "--list-secret-keys"])
        .lines()
        .find_map(|line| line.strip_prefix("fpr:"))
        .and_then(|rest| rest.split(':').find(|field| field.len() == 40))
        .unwrap()
        .to_owned()
}

#[test]
fn a_signed_imap_message_reaches_the_other_devices_signed() {
    let Ok(binary) = std::env::var("FLECTAR_MAIL_SYNC_SERVER") else {
        eprintln!("skipped: set FLECTAR_MAIL_SYNC_SERVER to a flectar-mail-sync-server binary");
        return;
    };
    if Command::new("gpg").arg("--version").output().is_err() {
        eprintln!("skipped: GnuPG is not installed");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let fingerprint = signing_key(home.path(), "Me <me@example.com>");
    // SAFETY: this binary holds this test alone, and no other thread runs
    // yet: the runtime starts below.
    unsafe { std::env::set_var("GNUPGHOME", home.path()) };
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(scheduled_signed_message(binary, fingerprint));
}

async fn scheduled_signed_message(binary: String, fingerprint: String) {
    let server = start_server(&binary).await;
    let laptop_dir = tempfile::tempdir().unwrap();
    let phone_dir = tempfile::tempdir().unwrap();
    let laptop = start_core(laptop_dir.path()).await;
    let phone = start_core(phone_dir.path()).await;
    laptop
        .import_account_configs(vec![imap_account("me@example.com")])
        .await
        .unwrap();
    let words = laptop
        .device_sync_turn_on(Some(server.url.clone()))
        .await
        .unwrap();
    laptop.device_sync_run().await.unwrap();
    phone
        .device_sync_recover(Some(server.url.clone()), words)
        .await
        .unwrap();
    phone.device_sync_run().await.unwrap();
    let laptop_account = laptop.list_accounts().await.unwrap()[0].id;
    let phone_account = phone.list_accounts().await.unwrap()[0].id;
    for (core, account) in [(&laptop, laptop_account), (&phone, phone_account)] {
        core.db
            .write(move |conn| {
                flectar_mail_core::db::repo::folders::upsert(
                    conn,
                    account,
                    "Drafts",
                    None,
                    Some(roles::DRAFTS),
                )?;
                Ok(())
            })
            .await
            .unwrap();
    }
    // The laptop signs everything it sends.
    laptop
        .set_mail_security(
            laptop_account,
            MailSecurity {
                signing_fingerprint: fingerprint,
                sign_by_default: true,
                require_encryption: false,
                recipient_keys: BTreeMap::new(),
            },
        )
        .await
        .unwrap();

    let draft = laptop
        .save_draft(SaveDraftArgs {
            draft_id: None,
            account_id: laptop_account,
            from: None,
            to: vec![Address {
                name: None,
                email: "alice@example.com".into(),
            }],
            cc: vec![],
            bcc: vec![],
            subject: "Signed plans".into(),
            body_text: "See you on Monday.".into(),
            body_html: None,
            mode: "new".into(),
            in_reply_to_message_id: None,
            in_reply_to_header: None,
            attachments: vec![],
        })
        .await
        .unwrap();
    let send_at = now_ms() + 3_600_000;
    laptop
        .queue_send(QueueSendArgs {
            draft_id: draft,
            send_at: Some(send_at),
            remind_at: None,
        })
        .await
        .unwrap();
    laptop.device_sync_run().await.unwrap();
    laptop.device_sync_run().await.unwrap();
    phone.device_sync_run().await.unwrap();

    let scheduled = phone.scheduled_sends().await.unwrap();
    assert_eq!(scheduled.len(), 1, "it shows in the phone's Scheduled view");
    assert_eq!(scheduled[0].send_at, send_at);
    let shared: Vec<u8> = phone
        .db
        .read(|conn| Ok(conn.query_row("SELECT mime FROM synced_sends", [], |row| row.get(0))?))
        .await
        .unwrap();
    let shared = String::from_utf8_lossy(&shared);
    assert!(shared.contains("multipart/signed"), "{shared}");
    assert!(shared.contains("BEGIN PGP SIGNATURE"), "{shared}");
}
