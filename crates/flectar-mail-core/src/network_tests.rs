use super::*;
use crate::accounts::credentials::DevelopmentFileCredentialStore;
use crate::config::Paths;
use crate::net::proxy::{Credentials, ProxyServer};
use std::sync::Arc;

// The proxy in force is the process's: these tests take the turn
// (`proxy::test_turns`), so no other test's core changes it while they check
// it, and their proxies skip every server (`*`), so other tests' connections
// stay direct.

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
            Ok(())
        })
        .await
        .unwrap();
    core
}

fn manual(kind: ProxyKind, user: &str) -> ManualProxy {
    ManualProxy {
        kind,
        host: " proxy.example ".into(),
        port: 1080,
        user: user.into(),
        bypass: "*".into(),
    }
}

fn reset() {
    proxy::set_app(AppProxy::System);
    proxy::set_account(1, None);
}

#[tokio::test]
async fn manual_proxy_is_saved_with_its_password_and_used() {
    let _turn = proxy::test_turns::take();
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    core.set_network_settings(
        ProxySettings {
            mode: ProxyMode::Manual,
            manual: manual(ProxyKind::Socks5, "ana"),
        },
        ProxyPassword::Set("s3cret".into()),
    )
    .await
    .unwrap();

    let saved = core.network_settings().await.unwrap();
    assert_eq!(saved.proxy.mode, ProxyMode::Manual);
    assert_eq!(saved.proxy.manual.host, "proxy.example");
    assert!(saved.password_saved);
    let in_force = ProxyServer {
        kind: ProxyKind::Socks5,
        host: "proxy.example".into(),
        port: 1080,
        credentials: Some(Credentials {
            user: "ana".into(),
            password: "s3cret".into(),
        }),
    };
    assert!(
        matches!(&proxy::app_in_force(), AppProxy::Manual(manual) if manual.server == in_force),
        "{:?}",
        proxy::app_in_force()
    );

    // Saving again without a new password keeps it.
    core.set_network_settings(
        ProxySettings {
            mode: ProxyMode::Manual,
            manual: manual(ProxyKind::Socks5, "ana"),
        },
        ProxyPassword::Keep,
    )
    .await
    .unwrap();
    assert!(core.network_settings().await.unwrap().password_saved);

    // A later start loads it before any connection opens.
    reset();
    drop(core);
    let core = Core::start_mail_ui_with_credentials(
        Paths::for_tests(directory.path()),
        Arc::new(DevelopmentFileCredentialStore::new(
            directory.path().join("credentials.json"),
        )),
    )
    .await
    .unwrap();
    assert!(
        matches!(&proxy::app_in_force(), AppProxy::Manual(manual) if manual.server == in_force),
        "{:?}",
        proxy::app_in_force()
    );

    // Without a user name the password goes.
    core.set_network_settings(
        ProxySettings {
            mode: ProxyMode::Manual,
            manual: manual(ProxyKind::Socks5, ""),
        },
        ProxyPassword::Keep,
    )
    .await
    .unwrap();
    assert!(!core.network_settings().await.unwrap().password_saved);
    reset();
}

#[tokio::test]
async fn a_manual_proxy_needs_a_server_it_can_use() {
    let _turn = proxy::test_turns::take();
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    for (manual, wrong) in [
        (
            ManualProxy {
                port: 0,
                ..manual(ProxyKind::Http, "")
            },
            "Enter the proxy's server and port.",
        ),
        (
            ManualProxy {
                host: "bad host".into(),
                ..manual(ProxyKind::Http, "")
            },
            "bad host isn't a server name or address.",
        ),
        (
            ManualProxy {
                bypass: "intranet.example, 10.0.0.0/40".into(),
                ..manual(ProxyKind::Http, "")
            },
            "10.0.0.0/40 can't be read as server names or addresses.",
        ),
    ] {
        let error = core
            .set_network_settings(
                ProxySettings {
                    mode: ProxyMode::Manual,
                    manual,
                },
                ProxyPassword::Keep,
            )
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), wrong);
    }
    assert_eq!(
        core.network_settings().await.unwrap().proxy,
        ProxySettings::default()
    );

    // A manual proxy that isn't chosen is kept as it is.
    let unused = ManualProxy {
        port: 0,
        ..manual(ProxyKind::Http, "")
    };
    core.set_network_settings(
        ProxySettings {
            mode: ProxyMode::Direct,
            manual: unused.clone(),
        },
        ProxyPassword::Keep,
    )
    .await
    .unwrap();
    assert_eq!(core.network_settings().await.unwrap().proxy.manual, unused);
    assert_eq!(proxy::app_in_force(), AppProxy::Direct);
    reset();
}

#[tokio::test]
async fn an_account_can_have_its_own_proxy_or_none() {
    let _turn = proxy::test_turns::take();
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    core.set_account_network(
        1,
        AccountProxy::Own(manual(ProxyKind::Http, "ana")),
        ProxyPassword::Set("s3cret".into()),
    )
    .await
    .unwrap();
    let chosen = core.account_network(1).await.unwrap();
    assert!(matches!(&chosen.proxy, AccountProxy::Own(own) if own.host == "proxy.example"));
    assert!(chosen.password_saved);
    assert!(matches!(
        proxy::account_in_force(1),
        Some(AccountOverride::Own(own)) if own.server.credentials.as_ref().is_some_and(|c| c.password == "s3cret")
    ));
    // The app's own password is another.
    assert!(!core.network_settings().await.unwrap().password_saved);

    core.set_account_network(1, AccountProxy::Direct, ProxyPassword::Keep)
        .await
        .unwrap();
    let chosen = core.account_network(1).await.unwrap();
    assert_eq!(chosen.proxy, AccountProxy::Direct);
    assert!(!chosen.password_saved);
    assert_eq!(proxy::account_in_force(1), Some(AccountOverride::Direct));

    core.set_account_network(1, AccountProxy::App, ProxyPassword::Keep)
        .await
        .unwrap();
    assert_eq!(proxy::account_in_force(1), None);
    reset();
}

#[tokio::test]
async fn removing_an_account_forgets_its_proxy() {
    let _turn = proxy::test_turns::take();
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    core.set_account_network(1, AccountProxy::Direct, ProxyPassword::Keep)
        .await
        .unwrap();
    core.remove_account(1).await.unwrap();
    assert_eq!(proxy::account_in_force(1), None);
    reset();
}

#[tokio::test]
async fn deleting_all_local_data_goes_back_to_the_default_proxy() {
    let _turn = proxy::test_turns::take();
    let directory = tempfile::tempdir().unwrap();
    let core = core(&directory).await;
    core.set_network_settings(
        ProxySettings {
            mode: ProxyMode::Manual,
            manual: manual(ProxyKind::Socks5, "ana"),
        },
        ProxyPassword::Set("s3cret".into()),
    )
    .await
    .unwrap();
    core.delete_all_local_data().await.unwrap();
    assert_eq!(proxy::app_in_force(), AppProxy::System);
    assert_eq!(
        core.network_settings().await.unwrap(),
        NetworkSettings::default()
    );
    reset();
}
