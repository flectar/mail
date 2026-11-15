//! Settings for files sent by link: the storage service they go from, signed in to in the
//! browser or with an app password, how long links work, and whether they ask for a password.

use crate::browser::ExternalUrlOpener;
use crate::documents::DocumentFuture;
use crate::mail::CoreMailSource;
use crate::ui_message::{AppWindowMessages, UiMessage, translated};
use crate::{AppWindow, InboxState, LinkServiceRow, LinkServices};
use flectar_mail_core::links::LinkService;
use slint::{ComponentHandle, ModelRc, VecModel};
use std::cell::RefCell;
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

    let weak = app.as_weak();
    let refresh_core = core.clone();
    let refresh_runtime = runtime.clone();
    ui.on_refresh(move || {
        if let (Some(app), Some(core)) = (weak.upgrade(), refresh_core()) {
            refresh(&app, &core, &refresh_runtime);
        }
    });

    let weak = app.as_weak();
    let sign_in_core = core.clone();
    let sign_in_runtime = runtime.clone();
    let sign_in_waiting = Rc::clone(&waiting);
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
        let opener = opener.clone();
        let runtime = sign_in_runtime.clone();
        let waiting = Rc::clone(&sign_in_waiting);
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
                        refresh(&app, &core, &runtime);
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

    let weak = app.as_weak();
    let password_core = core.clone();
    let password_runtime = runtime.clone();
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
        crate::documents::run_request(&password_runtime, request, move |added| {
            let Some(app) = weak.upgrade() else {
                return;
            };
            let ui = app.global::<LinkServices>();
            ui.set_state("idle".into());
            match added {
                Ok(_) => {
                    refresh(&app, &core, &runtime);
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
        refresh(&app, &core, &remove_runtime);
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

/// Lists the services, and how long links work and whether they ask for a password.
fn refresh(app: &AppWindow, core: &CoreMailSource, runtime: &tokio::runtime::Handle) {
    let ui = app.global::<LinkServices>();
    match runtime.block_on(core.link_services()) {
        Ok(services) => {
            let rows = services.iter().map(row).collect::<Vec<_>>();
            ui.set_services(ModelRc::new(VecModel::from(rows)));
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

fn row(service: &LinkService) -> LinkServiceRow {
    let host = url::Url::parse(&service.server_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_else(|| service.server_url.clone());
    LinkServiceRow {
        id: i32::try_from(service.id).unwrap_or_default(),
        name: service.display_name.as_str().into(),
        detail: format!("{} · {host}", service.login_name).into(),
    }
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
