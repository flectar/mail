//! Settings for files sent by link: the storage they go from, a Gmail or Microsoft account's
//! own Google Drive or OneDrive, turned on through the account's sign-in, or a Nextcloud or
//! ownCloud server, signed in to in the browser or with an app password; how long links work,
//! and whether they ask for a password.

use crate::browser::ExternalUrlOpener;
use crate::documents::DocumentFuture;
use crate::mail::CoreMailSource;
use crate::ui_message::{AppWindowMessages, UiMessage, translated};
use crate::{AppWindow, InboxState, LinkServiceRow, LinkServices};
use flectar_mail_core::links::{LinkService, LinkServiceKind};
use flectar_mail_core::models::{Account, Provider};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// How long links work, by the card's choices: 7, 14, 30 and 90 days, and as long as the file
/// is kept.
const EXPIRY_DAYS: [u32; 5] = [7, 14, 30, 90, 0];
/// The choice links start at, 30 days.
const DEFAULT_EXPIRY_INDEX: i32 = 2;

pub(super) fn install(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    opener: ExternalUrlOpener,
) {
    let ui = app.global::<LinkServices>();
    // The sign-in in the browser being waited for, to stop.
    let waiting: Rc<RefCell<Option<tokio::task::AbortHandle>>> = Rc::default();
    let core = {
        let state = Rc::clone(state);
        move || state.borrow().core.clone()
    };
    // The accounts whose storage the card lists.
    let accounts = {
        let state = Rc::clone(state);
        move || state.borrow().connected_accounts.clone()
    };

    let weak = app.as_weak();
    let refresh_core = core.clone();
    let refresh_runtime = runtime.clone();
    let refresh_accounts = accounts.clone();
    ui.on_refresh(move || {
        if let (Some(app), Some(core)) = (weak.upgrade(), refresh_core()) {
            refresh(&app, &core, &refresh_runtime, &refresh_accounts());
        }
    });

    let weak = app.as_weak();
    let sign_in_core = core.clone();
    let sign_in_runtime = runtime.clone();
    let sign_in_waiting = Rc::clone(&waiting);
    let sign_in_accounts = accounts.clone();
    let sign_in_opener = opener.clone();
    ui.on_sign_in(move |server| {
        let (Some(app), Some(core)) = (weak.upgrade(), sign_in_core()) else {
            return;
        };
        let ui = app.global::<LinkServices>();
        ui.set_state("connecting".into());
        ui.set_error("".into());
        let start_core = core.clone();
        let request: DocumentFuture<_> =
            Box::pin(async move { start_core.start_link_service_login(server.into()).await });
        let weak = app.as_weak();
        let opener = sign_in_opener.clone();
        let runtime = sign_in_runtime.clone();
        let waiting = Rc::clone(&sign_in_waiting);
        let accounts = sign_in_accounts.clone();
        crate::documents::run_request(&sign_in_runtime, request, move |login| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let ui = app.global::<LinkServices>();
            let login = match login {
                Ok(login) => login,
                Err(error) => {
                    ui.set_state("idle".into());
                    ui.set_error(error.into());
                    return;
                }
            };
            if let Err(error) = opener.open_external(&login.login_url) {
                ui.set_state("idle".into());
                ui.set_error(translated(
                    &app,
                    &UiMessage::detail("Could not open link: {}", error),
                ));
                return;
            }
            ui.set_state("waiting".into());
            let finish_core = core.clone();
            let task =
                runtime.spawn(async move { finish_core.finish_link_service_login(login).await });
            *waiting.borrow_mut() = Some(task.abort_handle());
            let weak = app.as_weak();
            let runtime = runtime.clone();
            let waiting = Rc::clone(&waiting);
            let _ = slint::spawn_local(async move {
                let finished = task.await;
                waiting.borrow_mut().take();
                let Some(app) = weak.upgrade() else {
                    return;
                };
                let ui = app.global::<LinkServices>();
                match finished {
                    // Stopped, as Cancel does.
                    Err(join) if join.is_cancelled() => {}
                    Err(join) => {
                        ui.set_state("idle".into());
                        ui.set_error(join.to_string().into());
                    }
                    Ok(Err(error)) => {
                        ui.set_state("idle".into());
                        ui.set_error(error.into());
                    }
                    Ok(Ok(_)) => {
                        ui.set_state("idle".into());
                        refresh(&app, &core, &runtime, &accounts());
                        services_changed();
                    }
                }
            });
        });
    });

    let weak = app.as_weak();
    let cancel_waiting = Rc::clone(&waiting);
    ui.on_cancel_sign_in(move || {
        if let Some(task) = cancel_waiting.borrow_mut().take() {
            task.abort();
        }
        if let Some(app) = weak.upgrade() {
            app.global::<LinkServices>().set_state("idle".into());
        }
    });

    // Turning on an account's storage widens its sign-in in the browser, as the app's other
    // sign-ins do, one at a time.
    let cancelled = Rc::new(Cell::new(false));
    let weak = app.as_weak();
    let turn_on_core = core.clone();
    let turn_on_runtime = runtime.clone();
    let turn_on_accounts = accounts.clone();
    let turn_on_cancelled = Rc::clone(&cancelled);
    ui.on_turn_on(move |account_id| {
        let (Some(app), Some(core)) = (weak.upgrade(), turn_on_core()) else {
            return;
        };
        if app.get_oauth_in_progress() || app.get_oauth_settings_saving() {
            return;
        }
        let ui = app.global::<LinkServices>();
        ui.set_error("".into());
        ui.set_authorizing(account_id);
        turn_on_cancelled.set(false);
        app.set_oauth_authorization_url("".into());
        app.set_oauth_browser_error(false);
        app.set_oauth_in_progress(true);
        let browser_app = app.as_weak();
        let opener = opener.clone();
        let add_core = core.clone();
        let task = turn_on_runtime.spawn(async move {
            add_core
                .add_cloud_link_service(i64::from(account_id), move |url| {
                    crate::oauth_browser::begin(browser_app, url, opener)
                })
                .await
        });
        let weak = app.as_weak();
        let runtime = turn_on_runtime.clone();
        let accounts = turn_on_accounts.clone();
        let cancelled = Rc::clone(&turn_on_cancelled);
        let _ = slint::spawn_local(async move {
            let finished = task.await;
            let Some(app) = weak.upgrade() else {
                return;
            };
            app.set_oauth_in_progress(false);
            app.set_oauth_authorization_url("".into());
            app.set_oauth_browser_error(false);
            let ui = app.global::<LinkServices>();
            ui.set_authorizing(0);
            match finished {
                Ok(Ok(_)) => {
                    refresh(&app, &core, &runtime, &accounts());
                    services_changed();
                }
                // Stopped, as Cancel here or the app's other Cancel sign-in does.
                Ok(Err(error)) if cancelled.get() || error.ends_with("sign-in cancelled") => {}
                Ok(Err(error)) => ui.set_error(error.into()),
                Err(join) => ui.set_error(join.to_string().into()),
            }
        });
    });

    let cancel_core = core.clone();
    ui.on_cancel_turn_on(move || {
        cancelled.set(true);
        if let Some(core) = cancel_core() {
            core.cancel_oauth();
        }
    });

    let weak = app.as_weak();
    let password_core = core.clone();
    let password_runtime = runtime.clone();
    let password_accounts = accounts.clone();
    ui.on_connect_with_password(move |server, login_name, app_password| {
        let (Some(app), Some(core)) = (weak.upgrade(), password_core()) else {
            return;
        };
        let ui = app.global::<LinkServices>();
        ui.set_state("connecting".into());
        ui.set_error("".into());
        let add_core = core.clone();
        let request: DocumentFuture<_> = Box::pin(async move {
            add_core
                .add_link_service(server.into(), login_name.into(), app_password.into())
                .await
        });
        let weak = app.as_weak();
        let runtime = password_runtime.clone();
        let accounts = password_accounts.clone();
        crate::documents::run_request(&password_runtime, request, move |added| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let ui = app.global::<LinkServices>();
            ui.set_state("idle".into());
            match added {
                Ok(_) => {
                    refresh(&app, &core, &runtime, &accounts());
                    services_changed();
                }
                Err(error) => ui.set_error(error.into()),
            }
        });
    });

    let weak = app.as_weak();
    let remove_core = core.clone();
    let remove_runtime = runtime.clone();
    ui.on_remove(move |id| {
        let (Some(app), Some(core)) = (weak.upgrade(), remove_core()) else {
            return;
        };
        if let Err(error) = remove_runtime.block_on(core.remove_link_service(i64::from(id))) {
            app.global::<LinkServices>().set_error(error.into());
        }
        refresh(&app, &core, &remove_runtime, &accounts());
        services_changed();
    });

    let weak = app.as_weak();
    let expiry_core = core.clone();
    let expiry_runtime = runtime.clone();
    ui.on_expiry_chosen(move |_| {
        if let (Some(app), Some(core)) = (weak.upgrade(), expiry_core()) {
            save_preferences(&app, &core, &expiry_runtime);
        }
    });

    let weak = app.as_weak();
    let password_runtime = runtime.clone();
    ui.on_password_toggled(move |_| {
        if let (Some(app), Some(core)) = (weak.upgrade(), core()) {
            save_preferences(&app, &core, &password_runtime);
        }
    });
}

/// Every message shows or hides Share files by link, as a service came or went.
fn services_changed() {
    crate::composer::with_composers(|composers| composers.link_services_changed());
}

/// Lists the storage of `accounts` that have their own, on or not, then the servers, and how
/// long links work and whether they ask for a password.
fn refresh(
    app: &AppWindow,
    core: &CoreMailSource,
    runtime: &tokio::runtime::Handle,
    accounts: &[Account],
) {
    let ui = app.global::<LinkServices>();
    match runtime.block_on(core.link_services()) {
        Ok(services) => {
            let rows = rows(accounts, &services);
            ui.set_services(ModelRc::new(VecModel::from(rows)));
            ui.set_has_server(
                services
                    .iter()
                    .any(|service| service.kind == LinkServiceKind::Nextcloud),
            );
        }
        Err(error) => ui.set_error(error.into()),
    }
    if let Ok(settings) = runtime.block_on(core.load_settings()) {
        let index = EXPIRY_DAYS
            .iter()
            .position(|days| *days == settings.link_expiry_days)
            .and_then(|index| i32::try_from(index).ok())
            .unwrap_or(DEFAULT_EXPIRY_INDEX);
        ui.set_expiry_index(index);
        ui.set_password(settings.link_password);
    }
}

/// The card's rows: each Gmail and Microsoft account's storage, with its service where it's
/// on, then the servers.
fn rows(accounts: &[Account], services: &[LinkService]) -> Vec<LinkServiceRow> {
    let own = accounts.iter().filter_map(|account| {
        let name = match account.provider {
            Provider::Gmail => "Google Drive",
            Provider::Microsoft => "OneDrive",
            Provider::Generic => return None,
        };
        let service = services
            .iter()
            .find(|service| service.account_id == Some(account.id));
        Some(LinkServiceRow {
            id: service
                .and_then(|service| i32::try_from(service.id).ok())
                .unwrap_or_default(),
            account_id: i32::try_from(account.id).unwrap_or_default(),
            name: name.into(),
            detail: account.email.as_str().into(),
        })
    });
    let servers = services
        .iter()
        .filter(|service| service.kind == LinkServiceKind::Nextcloud)
        .map(|service| {
            let host = url::Url::parse(&service.server_url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .unwrap_or_else(|| service.server_url.clone());
            LinkServiceRow {
                id: i32::try_from(service.id).unwrap_or_default(),
                account_id: 0,
                name: service.display_name.as_str().into(),
                detail: format!("{} · {host}", service.login_name).into(),
            }
        });
    own.chain(servers).collect()
}

fn save_preferences(app: &AppWindow, core: &CoreMailSource, runtime: &tokio::runtime::Handle) {
    let ui = app.global::<LinkServices>();
    let days = usize::try_from(ui.get_expiry_index())
        .ok()
        .and_then(|index| EXPIRY_DAYS.get(index).copied())
        .unwrap_or(EXPIRY_DAYS[2]);
    match runtime.block_on(core.set_link_preferences(days, ui.get_password())) {
        Ok(()) => app.set_sync_status(UiMessage::plain("Large files preference saved.")),
        Err(error) => app.set_sync_status(UiMessage::detail(
            "Could not save the large files preference: {}",
            error,
        )),
    }
}
