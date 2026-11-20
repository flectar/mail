//! Settings → Network (#250): the proxy the app's connections go through,
//! and the accounts that go another way. The core keeps the choices and
//! hands them to every connection (`flectar_mail_core::net::proxy`); this
//! shows them, saves them, and checks a proxy typed in by connecting through
//! it to a mail server.

use super::*;
use flectar_mail_core::error::CoreError;
use flectar_mail_core::net::proxy::{
    AccountProxy, ManualProxy, ProxyError, ProxyKind, ProxyMode, ProxySettings,
};
use flectar_mail_core::{Core, NetworkCheck, ProxyPassword};
use slint::SharedString;

struct Env {
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    runtime: tokio::runtime::Handle,
}

thread_local! {
    static ENV: RefCell<Option<Env>> = const { RefCell::new(None) };
    /// The app's settings as last loaded, so that choosing another mode
    /// keeps the proxy typed in.
    static SETTINGS: RefCell<ProxySettings> = RefCell::new(ProxySettings::default());
    /// Each account's choice as last loaded.
    static ACCOUNTS: RefCell<Vec<(i64, AccountProxy)>> = const { RefCell::new(Vec::new()) };
}

/// Run `work` with the core, the window and the runtime, once both are up.
fn with_core(work: impl FnOnce(Arc<Core>, slint::Weak<AppWindow>, &tokio::runtime::Handle)) {
    ENV.with_borrow(|env| {
        let Some(env) = env else { return };
        let Some(core) = env
            .state
            .borrow()
            .core
            .as_ref()
            .map(|core| core.file_core())
        else {
            return;
        };
        work(core, env.app.clone(), &env.runtime);
    });
}

const fn mode_index(mode: ProxyMode) -> i32 {
    match mode {
        ProxyMode::System => 0,
        ProxyMode::Direct => 1,
        ProxyMode::Manual => 2,
    }
}

const fn kind_index(kind: ProxyKind) -> i32 {
    match kind {
        ProxyKind::Http => 1,
        ProxyKind::Socks5 | ProxyKind::Socks4a => 0,
    }
}

fn kind(index: i32) -> ProxyKind {
    if index == 1 {
        ProxyKind::Http
    } else {
        ProxyKind::Socks5
    }
}

fn port_text(port: u16) -> SharedString {
    if port == 0 {
        SharedString::new()
    } else {
        port.to_string().into()
    }
}

/// `host:port` of a proxy typed in, as a row sums it up.
fn label(manual: &ManualProxy) -> String {
    if manual.host.contains(':') {
        format!("[{}]:{}", manual.host, manual.port)
    } else {
        format!("{}:{}", manual.host, manual.port)
    }
}

/// A proxy from the fields the page shows, or `None` for a port that isn't
/// one.
fn typed(kind_index: i32, host: &str, port: &str, user: &str, bypass: &str) -> Option<ManualProxy> {
    let port = port.trim().parse::<u16>().ok().filter(|port| *port != 0)?;
    Some(ManualProxy {
        kind: kind(kind_index),
        host: host.trim().to_owned(),
        port,
        user: user.trim().to_owned(),
        bypass: bypass.trim().to_owned(),
    })
}

fn password(typed: String) -> ProxyPassword {
    if typed.is_empty() {
        ProxyPassword::Keep
    } else {
        ProxyPassword::Set(typed)
    }
}

/// Why a save failed, as `NetworkWords.failure` reads it.
fn save_failure(error: &CoreError) -> (&'static str, String) {
    match error {
        CoreError::CredentialStoreUnavailable(_) => ("keyring", String::new()),
        error => ("other", error.to_string()),
    }
}

/// Why a check failed, as `NetworkWords.failure` reads it.
fn check_failure(check: NetworkCheck) -> Option<(&'static str, String)> {
    Some(match check {
        NetworkCheck::Answered(_) | NetworkCheck::NoServer => return None,
        NetworkCheck::Proxy(error) => match error {
            ProxyError::Unreachable { .. } => ("unreachable", String::new()),
            ProxyError::TimedOut { .. } => ("timeout", String::new()),
            ProxyError::CredentialsNeeded { .. } => ("credentials-needed", String::new()),
            ProxyError::CredentialsRejected { .. } => ("credentials-rejected", String::new()),
            ProxyError::UnsupportedSignIn { offered, .. } => ("sign-in", offered),
            ProxyError::Refused { .. } => ("refused", String::new()),
            ProxyError::Protocol { .. } | ProxyError::Io { .. } => ("protocol", String::new()),
            ProxyError::Settings(detail) => ("other", detail),
        },
        NetworkCheck::Failed(detail) => ("server", detail),
    })
}

fn state_after(check: &NetworkCheck) -> &'static str {
    match check {
        NetworkCheck::Answered(_) => "ready",
        NetworkCheck::NoServer => "saved",
        NetworkCheck::Proxy(_) | NetworkCheck::Failed(_) => "failed",
    }
}

fn set_state(app: &AppWindow, state: &str, failure: Option<(&str, String)>) {
    let ui = app.global::<Network>();
    ui.set_state(state.into());
    let (failure, detail) = failure.unwrap_or_default();
    ui.set_failure(failure.into());
    ui.set_failure_detail(detail.into());
}

fn set_account_state(app: &AppWindow, state: &str, failure: Option<(&str, String)>) {
    let ui = app.global::<Network>();
    ui.set_account_state(state.into());
    let (failure, detail) = failure.unwrap_or_default();
    ui.set_account_failure(failure.into());
    ui.set_account_failure_detail(detail.into());
}

/// Bring the page up to date with what the core keeps.
fn refresh() {
    with_core(|core, weak, runtime| {
        runtime.spawn(async move {
            let Ok(settings) = core.network_settings().await else {
                return;
            };
            let accounts = core
                .list_account_configs()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|config| (config.id, config.email, config.settings.connection.proxy))
                .collect::<Vec<_>>();
            let _ = weak.upgrade_in_event_loop(move |app| {
                let ui = app.global::<Network>();
                let manual = &settings.proxy.manual;
                ui.set_mode_index(mode_index(settings.proxy.mode));
                ui.set_kind_index(kind_index(manual.kind));
                ui.set_host(manual.host.as_str().into());
                ui.set_port(port_text(manual.port));
                ui.set_user(manual.user.as_str().into());
                ui.set_bypass(manual.bypass.as_str().into());
                ui.set_password_saved(settings.password_saved);
                SETTINGS.set(settings.proxy);
                let rows = accounts
                    .iter()
                    .map(|(id, email, proxy)| AccountProxyRow {
                        id: i32::try_from(*id).unwrap_or(-1),
                        email: email.as_str().into(),
                        mode_index: match proxy {
                            AccountProxy::App => 0,
                            AccountProxy::Direct => 1,
                            AccountProxy::Own(_) => 2,
                        },
                        own_label: match proxy {
                            AccountProxy::Own(manual) => label(manual).into(),
                            _ => SharedString::new(),
                        },
                    })
                    .collect::<Vec<_>>();
                ui.set_accounts(ModelRc::new(VecModel::from(rows)));
                ACCOUNTS.set(
                    accounts
                        .into_iter()
                        .map(|(id, _, proxy)| (id, proxy))
                        .collect(),
                );
            });
        });
    });
}

/// The system's settings and No proxy save at once. A proxy typed in waits
/// for Save, unless one is kept from before.
fn choose_mode(app: &AppWindow, index: i32) {
    let mode = match index {
        0 => ProxyMode::System,
        1 => ProxyMode::Direct,
        _ => ProxyMode::Manual,
    };
    let mut settings = SETTINGS.with_borrow(Clone::clone);
    if mode == ProxyMode::Manual && !settings.manual.is_complete() {
        set_state(app, "", None);
        return;
    }
    settings.mode = mode;
    save_settings(app, settings, ProxyPassword::Keep);
}

/// Save the proxy the page shows, then check it.
fn save(app: &AppWindow, typed_password: String) {
    let ui = app.global::<Network>();
    let Some(manual) = typed(
        ui.get_kind_index(),
        &ui.get_host(),
        &ui.get_port(),
        &ui.get_user(),
        &ui.get_bypass(),
    ) else {
        set_state(app, "failed", Some(("port", String::new())));
        return;
    };
    let settings = ProxySettings {
        mode: ProxyMode::Manual,
        manual,
    };
    save_settings(app, settings, password(typed_password));
}

fn save_settings(app: &AppWindow, settings: ProxySettings, password: ProxyPassword) {
    let check = settings.mode == ProxyMode::Manual;
    set_state(app, "saving", None);
    with_core(|core, weak, runtime| {
        runtime.spawn(async move {
            let saved = core.set_network_settings(settings, password).await;
            let checked = match &saved {
                Ok(()) if check => {
                    let weak = weak.clone();
                    let _ = weak.upgrade_in_event_loop(|app| set_state(&app, "checking", None));
                    Some(core.check_network(None).await)
                }
                _ => None,
            };
            let _ = weak.upgrade_in_event_loop(move |app| {
                match (saved, checked) {
                    (Err(error), _) => set_state(&app, "failed", Some(save_failure(&error))),
                    (Ok(()), Some(Ok(check))) => {
                        let state = state_after(&check);
                        set_state(&app, state, check_failure(check));
                    }
                    (Ok(()), Some(Err(error))) => {
                        set_state(&app, "failed", Some(("server", error.to_string())));
                    }
                    (Ok(()), None) => set_state(&app, "saved", None),
                }
                refresh();
            });
        });
    });
}

fn remove_password(app: &AppWindow) {
    let settings = SETTINGS.with_borrow(Clone::clone);
    set_state(app, "saving", None);
    with_core(|core, weak, runtime| {
        runtime.spawn(async move {
            let saved = core
                .set_network_settings(settings, ProxyPassword::Remove)
                .await;
            let _ = weak.upgrade_in_event_loop(move |app| {
                match saved {
                    Ok(()) => set_state(&app, "", None),
                    Err(error) => set_state(&app, "failed", Some(save_failure(&error))),
                }
                refresh();
            });
        });
    });
}

fn account_choice(account_id: i64) -> Option<AccountProxy> {
    ACCOUNTS.with_borrow(|accounts| {
        accounts
            .iter()
            .find(|(id, _)| *id == account_id)
            .map(|(_, proxy)| proxy.clone())
    })
}

/// Same as the app and No proxy save at once; its own proxy opens the
/// editor.
fn choose_account_mode(app: &AppWindow, account_id: i64, index: i32) {
    let proxy = match index {
        0 => AccountProxy::App,
        1 => AccountProxy::Direct,
        _ => {
            edit_account(app, account_id);
            return;
        }
    };
    app.global::<Network>().set_editing_account(-1);
    save_account(app, account_id, proxy, ProxyPassword::Keep);
}

/// Open an account's own proxy to change, filled in with the one saved.
fn edit_account(app: &AppWindow, account_id: i64) {
    let ui = app.global::<Network>();
    let manual = match account_choice(account_id) {
        Some(AccountProxy::Own(manual)) => manual,
        _ => ManualProxy::default(),
    };
    ui.set_editing_account(i32::try_from(account_id).unwrap_or(-1));
    ui.set_account_kind_index(kind_index(manual.kind));
    ui.set_account_host(manual.host.as_str().into());
    ui.set_account_port(port_text(manual.port));
    ui.set_account_user(manual.user.as_str().into());
    ui.set_account_password_saved(false);
    set_account_state(app, "", None);
    // Another account chosen but not saved shows its saved choice again.
    refresh();
    with_core(|core, weak, runtime| {
        runtime.spawn(async move {
            let saved = core
                .account_network(account_id)
                .await
                .is_ok_and(|network| network.password_saved);
            let _ = weak.upgrade_in_event_loop(move |app| {
                let ui = app.global::<Network>();
                if i64::from(ui.get_editing_account()) == account_id {
                    ui.set_account_password_saved(saved);
                }
            });
        });
    });
}

fn save_edited_account(app: &AppWindow, typed_password: String) {
    let ui = app.global::<Network>();
    let account_id = i64::from(ui.get_editing_account());
    if account_id < 0 {
        return;
    }
    // An account's own proxy has no list of servers to skip: it serves the
    // account's own servers.
    let Some(manual) = typed(
        ui.get_account_kind_index(),
        &ui.get_account_host(),
        &ui.get_account_port(),
        &ui.get_account_user(),
        "",
    ) else {
        set_account_state(app, "failed", Some(("port", String::new())));
        return;
    };
    save_account(
        app,
        account_id,
        AccountProxy::Own(manual),
        password(typed_password),
    );
}

fn save_account(app: &AppWindow, account_id: i64, proxy: AccountProxy, password: ProxyPassword) {
    let check = matches!(proxy, AccountProxy::Own(_));
    set_account_state(app, "saving", None);
    with_core(|core, weak, runtime| {
        runtime.spawn(async move {
            let saved = core.set_account_network(account_id, proxy, password).await;
            let checked = match &saved {
                Ok(()) if check => {
                    let weak = weak.clone();
                    let _ =
                        weak.upgrade_in_event_loop(|app| set_account_state(&app, "checking", None));
                    Some(core.check_network(Some(account_id)).await)
                }
                _ => None,
            };
            let _ = weak.upgrade_in_event_loop(move |app| {
                let editing = app.global::<Network>().get_editing_account();
                match (saved, checked) {
                    (Err(error), _) => {
                        set_account_state(&app, "failed", Some(save_failure(&error)));
                    }
                    (Ok(()), Some(Ok(check))) => {
                        let state = state_after(&check);
                        let failure = check_failure(check);
                        set_account_state(&app, state, failure);
                        // A proxy that works closes its editor.
                        if state != "failed" && i64::from(editing) == account_id {
                            app.global::<Network>().set_editing_account(-1);
                        }
                    }
                    (Ok(()), Some(Err(error))) => {
                        set_account_state(&app, "failed", Some(("server", error.to_string())));
                    }
                    (Ok(()), None) => set_account_state(&app, "", None),
                }
                refresh();
            });
        });
    });
}

fn remove_account_password(app: &AppWindow) {
    let ui = app.global::<Network>();
    let account_id = i64::from(ui.get_editing_account());
    let Some(AccountProxy::Own(manual)) = account_choice(account_id) else {
        // Nothing saved yet: the field shows again.
        ui.set_account_password_saved(false);
        return;
    };
    save_account(
        app,
        account_id,
        AccountProxy::Own(manual),
        ProxyPassword::Remove,
    );
    ui.set_account_password_saved(false);
}

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    ENV.set(Some(Env {
        app: app.as_weak(),
        state: Rc::clone(state),
        runtime: runtime.clone(),
    }));
    let ui = app.global::<Network>();
    ui.on_refresh(refresh);
    let weak = app.as_weak();
    ui.on_choose_mode(move |index| {
        if let Some(app) = weak.upgrade() {
            choose_mode(&app, index);
        }
    });
    let weak = app.as_weak();
    ui.on_save(move |typed_password| {
        if let Some(app) = weak.upgrade() {
            save(&app, typed_password.to_string());
        }
    });
    let weak = app.as_weak();
    ui.on_remove_password(move || {
        if let Some(app) = weak.upgrade() {
            remove_password(&app);
        }
    });
    let weak = app.as_weak();
    ui.on_choose_account_mode(move |account_id, index| {
        if let Some(app) = weak.upgrade() {
            choose_account_mode(&app, i64::from(account_id), index);
        }
    });
    let weak = app.as_weak();
    ui.on_edit_account(move |account_id| {
        if let Some(app) = weak.upgrade() {
            edit_account(&app, i64::from(account_id));
        }
    });
    let weak = app.as_weak();
    ui.on_save_account(move |typed_password| {
        if let Some(app) = weak.upgrade() {
            save_edited_account(&app, typed_password.to_string());
        }
    });
    let weak = app.as_weak();
    ui.on_remove_account_password(move || {
        if let Some(app) = weak.upgrade() {
            remove_account_password(&app);
        }
    });
    let weak = app.as_weak();
    ui.on_cancel_account(move || {
        if let Some(app) = weak.upgrade() {
            app.global::<Network>().set_editing_account(-1);
            set_account_state(&app, "", None);
            refresh();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_fields_the_page_shows() {
        assert_eq!(
            typed(1, " proxy.example ", " 3128 ", " ana ", " .example.com "),
            Some(ManualProxy {
                kind: ProxyKind::Http,
                host: "proxy.example".into(),
                port: 3128,
                user: "ana".into(),
                bypass: ".example.com".into(),
            })
        );
        for port in ["", "0", "65536", "80a"] {
            assert_eq!(typed(0, "proxy.example", port, "", ""), None, "{port}");
        }
        assert_eq!(password(String::new()), ProxyPassword::Keep);
        assert_eq!(password("x".into()), ProxyPassword::Set("x".into()));
    }

    #[test]
    fn sums_up_a_proxy_with_brackets_for_ipv6() {
        let manual = |host: &str| ManualProxy {
            host: host.into(),
            port: 1080,
            ..ManualProxy::default()
        };
        assert_eq!(label(&manual("proxy.example")), "proxy.example:1080");
        assert_eq!(label(&manual("2001:db8::7")), "[2001:db8::7]:1080");
    }

    #[test]
    fn says_why_a_check_failed() {
        assert_eq!(
            check_failure(NetworkCheck::Answered("imap.example".into())),
            None
        );
        assert_eq!(check_failure(NetworkCheck::NoServer), None);
        assert_eq!(
            check_failure(NetworkCheck::Proxy(ProxyError::UnsupportedSignIn {
                proxy: "proxy.example:3128".into(),
                offered: "NTLM".into(),
            })),
            Some(("sign-in", "NTLM".into()))
        );
        assert_eq!(
            check_failure(NetworkCheck::Proxy(ProxyError::CredentialsRejected {
                proxy: "proxy.example:3128".into(),
            })),
            Some(("credentials-rejected", String::new()))
        );
        assert_eq!(
            check_failure(NetworkCheck::Failed("connection refused".into())),
            Some(("server", "connection refused".into()))
        );
        assert_eq!(state_after(&NetworkCheck::NoServer), "saved");
    }
}
