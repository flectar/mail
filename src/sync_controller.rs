//! Flectar Sync's settings page and dialogs (issue #39).
//!
//! The core runs sync. This controller starts what the user asks for, shows
//! where sync stands from the core's events, and draws pairing QR codes.
//! What another device changed reaches the window the way a change made here
//! does: preferences through the settings they belong to, and accounts
//! through the window's own account updates.

use super::*;
use crate::retained_model::RetainedModel;
use chrono::{Datelike, Timelike};
use flectar_mail_core::Core;
use flectar_mail_core::device_sync::{
    DeviceSyncState, DeviceSyncStatus, PairingCode, PairingEvent,
};
use flectar_mail_core::error::CoreError;
use slint::SharedString;
use startup::DeviceSyncUpdates;

/// The most steps of other pairings kept while the dialog waits to learn
/// which pairing its request started.
const MAX_EARLY_STEPS: usize = 16;

/// State the page's callbacks and the core's events share.
#[derive(Default)]
pub(super) struct SyncController {
    /// The devices the page lists. Updated in place, so a press on a row's
    /// button survives the status updates every sync brings.
    devices: Rc<RetainedModel<SyncDeviceRow>>,
    /// Bumped whenever the dialog starts a pairing or closes, so the answer
    /// to an earlier request is dropped.
    attempt: Cell<u64>,
    /// The core's number for the pairing the dialog follows.
    pairing: Cell<Option<u64>>,
    /// Steps that arrived before the request starting the pairing returned
    /// its number, oldest first.
    early: RefCell<Vec<(u64, PairingEvent)>>,
}

impl SyncController {
    /// Connect the page's callbacks. The page shows sync as off until the
    /// core reports otherwise.
    pub(super) fn register(
        self: &Rc<Self>,
        app: &AppWindow,
        state: &Rc<RefCell<InboxState>>,
        runtime: &tokio::runtime::Handle,
    ) {
        let ui = app.global::<SyncUi>();
        ui.set_devices(Rc::clone(&self.devices).into());

        let actions = Actions {
            app: app.as_weak(),
            state: Rc::clone(state),
            runtime: runtime.clone(),
            controller: Rc::clone(self),
        };

        let turn_on = actions.clone();
        ui.on_turn_on(move || turn_on.turn_on());
        let join = actions.clone();
        ui.on_join_with_code(move |code| join.join_with_code(code.to_string()));
        let show = actions.clone();
        ui.on_join_show_qr(move || show.join_show_qr());
        let recover = actions.clone();
        ui.on_recover(move |words| recover.recover(words.to_string()));
        let invite = actions.clone();
        ui.on_invite(move |with_code| invite.invite(with_code));
        let accept = actions.clone();
        ui.on_accept(move || accept.decide(true));
        let decline = actions.clone();
        ui.on_decline(move || decline.decide(false));
        let cancel = actions.clone();
        ui.on_cancel_pairing(move || cancel.cancel_pairing());
        let remove = actions.clone();
        ui.on_remove_device(move |id| remove.remove_device(id.to_string()));
        let reset = actions.clone();
        ui.on_reset_recovery(move || reset.reset_recovery());
        let turn_off = actions.clone();
        ui.on_turn_off(move || turn_off.turn_off());
        let delete = actions.clone();
        ui.on_delete_everywhere(move || delete.delete_everywhere());
        ui.on_sync_now(move || {
            if let Some(core) = actions.core() {
                core.device_sync_now();
            }
        });
    }

    /// Name this device to its sync group and show where sync stands, once
    /// the window has the core.
    pub(super) fn attach(&self, app: &AppWindow, core: &CoreMailSource) {
        let core = core.device_sync_core();
        core.describe_device(platform::device_description());
        self.show_status(app, &core.device_sync_status());
    }

    /// Take on what the core reported since the window last looked.
    pub(super) fn apply(
        &self,
        app: &AppWindow,
        state: &Rc<RefCell<InboxState>>,
        runtime: &tokio::runtime::Handle,
        ui_tasks: &UiSender<UiTaskUpdate>,
        calendar_month: NaiveDate,
        updates: DeviceSyncUpdates,
    ) {
        if let Some(status) = updates.status {
            self.show_status(app, &status);
        }
        for (pairing, event) in updates.pairing {
            self.show_pairing(app, pairing, event);
        }
        if updates.settings_changed {
            reload_preferences(app, state, runtime);
        }
        if updates.accounts_changed {
            reload_accounts(
                state,
                runtime,
                ui_tasks,
                calendar_month,
                updates.removed_accounts,
            );
        }
    }

    fn show_status(&self, app: &AppWindow, status: &DeviceSyncStatus) {
        let ui = app.global::<SyncUi>();
        let (state, error) = match &status.state {
            DeviceSyncState::Off => ("off", ""),
            DeviceSyncState::Idle => ("idle", ""),
            DeviceSyncState::Syncing => ("syncing", ""),
            DeviceSyncState::Error(error) => ("error", error.as_str()),
            DeviceSyncState::Removed => ("removed", ""),
            DeviceSyncState::Deleted => ("deleted", ""),
        };
        ui.set_state(state.into());
        ui.set_error(sentence(error).into());
        let (last_synced, today) = last_synced(status.last_synced_at, &Local::now());
        ui.set_last_synced(last_synced.into());
        ui.set_last_synced_today(today);
        ui.set_server_url(status.server_url.as_deref().unwrap_or_default().into());
        self.devices.reconcile_by(
            status
                .devices
                .iter()
                .map(|device| SyncDeviceRow {
                    id: device.id.as_str().into(),
                    name: device.name.as_str().into(),
                    platform: device.platform.as_str().into(),
                    this_device: device.this_device,
                })
                .collect(),
            |row| row.id.clone(),
            |a, b| a == b,
        );
        // Adding a device needs sync on; one that stopped can't finish.
        let off = !matches!(
            status.state,
            DeviceSyncState::Idle | DeviceSyncState::Syncing | DeviceSyncState::Error(_)
        );
        if off && ui.get_dialog() == "invite" {
            self.end_attempt(&ui);
            ui.set_dialog("".into());
        }
    }

    /// Show a step of the pairing the dialog follows; steps of others are
    /// dropped.
    fn show_pairing(&self, app: &AppWindow, pairing: u64, event: PairingEvent) {
        let ui = app.global::<SyncUi>();
        if ui.get_dialog().is_empty() {
            return;
        }
        match self.pairing.get() {
            Some(current) if current == pairing => show_step(&ui, event),
            Some(_) => {}
            // The request that started it hasn't returned its number yet.
            None => {
                let mut early = self.early.borrow_mut();
                if early.len() == MAX_EARLY_STEPS {
                    early.remove(0);
                }
                early.push((pairing, event));
            }
        }
    }

    /// Start a new attempt in the dialog. Returns its number.
    fn start_attempt(&self, ui: &SyncUi, stage: &str) -> u64 {
        self.end_attempt(ui);
        ui.set_pairing_stage(stage.into());
        self.attempt.get()
    }

    /// Forget the dialog's pairing and what it showed. Returns the pairing,
    /// if the core had numbered it.
    fn end_attempt(&self, ui: &SyncUi) -> Option<u64> {
        self.attempt.set(self.attempt.get().wrapping_add(1));
        self.early.borrow_mut().clear();
        ui.set_pairing_stage("".into());
        ui.set_pairing_has_qr(false);
        ui.set_pairing_qr(Image::default());
        ui.set_pairing_code("".into());
        ui.set_pairing_words(ModelRc::default());
        ui.set_pairing_error("".into());
        self.pairing.take()
    }

    /// The core numbered the attempt's pairing: follow it, including any
    /// step that came first.
    fn follow(&self, ui: &SyncUi, pairing: u64) {
        self.pairing.set(Some(pairing));
        let early = std::mem::take(&mut *self.early.borrow_mut());
        for (_, event) in early.into_iter().filter(|(from, _)| *from == pairing) {
            show_step(ui, event);
        }
    }
}

/// What the page's callbacks need.
#[derive(Clone)]
struct Actions {
    app: slint::Weak<AppWindow>,
    state: Rc<RefCell<InboxState>>,
    runtime: tokio::runtime::Handle,
    controller: Rc<SyncController>,
}

impl Actions {
    fn core(&self) -> Option<Arc<Core>> {
        self.state
            .borrow()
            .core
            .as_ref()
            .map(CoreMailSource::device_sync_core)
    }

    /// Run `request` on the runtime with the page's buttons waiting, then
    /// `done` with its result on the UI thread.
    fn run<T: Send + 'static>(
        &self,
        request: impl Future<Output = flectar_mail_core::error::Result<T>> + Send + 'static,
        done: impl FnOnce(&SyncUi, &SyncController, Result<T, CoreError>) + 'static,
    ) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        app.global::<SyncUi>().set_busy(true);
        let app = self.app.clone();
        let controller = Rc::clone(&self.controller);
        documents::run_request(
            &self.runtime,
            Box::pin(async move { Ok::<_, String>(request.await) }),
            move |result| {
                let Some(app) = app.upgrade() else {
                    return;
                };
                let ui = app.global::<SyncUi>();
                ui.set_busy(false);
                let result = result.unwrap_or_else(|error| Err(CoreError::Other(error)));
                done(&ui, &controller, result);
            },
        );
    }

    /// The server address typed on the page, or `Err` after showing that it
    /// isn't one. `pairing` shows it in the dialog instead.
    fn server(&self, ui: &SyncUi, pairing: bool) -> Result<Option<String>, ()> {
        server_address(&ui.get_server()).inspect_err(|()| {
            if pairing {
                self.controller.end_attempt(ui);
                ui.set_pairing_stage("server".into());
            } else {
                show_failure(ui, "server", "");
            }
        })
    }

    fn turn_on(&self) {
        let (Some(app), Some(core)) = (self.app.upgrade(), self.core()) else {
            return;
        };
        let ui = app.global::<SyncUi>();
        let Ok(server) = self.server(&ui, false) else {
            return;
        };
        show_failure(&ui, "", "");
        self.run(
            async move { core.device_sync_turn_on(server).await },
            |ui, _, result| match result {
                Ok(words) => show_recovery_words(ui, &words),
                Err(error) => show_failure(ui, "turn-on", &error_text(&error)),
            },
        );
    }

    fn join_with_code(&self, code: String) {
        let (Some(app), Some(core)) = (self.app.upgrade(), self.core()) else {
            return;
        };
        let ui = app.global::<SyncUi>();
        let Ok(server) = self.server(&ui, true) else {
            return;
        };
        let attempt = self.controller.start_attempt(&ui, "starting");
        let cancel = Arc::clone(&core);
        self.run(
            async move { core.device_sync_join_with_code(server, code).await },
            move |ui, controller, result| {
                if controller.attempt.get() != attempt {
                    if let Ok(pairing) = result {
                        cancel.device_sync_cancel_pairing(pairing);
                    }
                    return;
                }
                match result {
                    Ok(pairing) => controller.follow(ui, pairing),
                    Err(error) => show_pairing_error(ui, &error),
                }
            },
        );
    }

    fn join_show_qr(&self) {
        let (Some(app), Some(core)) = (self.app.upgrade(), self.core()) else {
            return;
        };
        let ui = app.global::<SyncUi>();
        let Ok(server) = self.server(&ui, true) else {
            return;
        };
        let attempt = self.controller.start_attempt(&ui, "starting");
        let cancel = Arc::clone(&core);
        self.run(
            async move { core.device_sync_join_show_code(server).await },
            move |ui, controller, result| {
                show_code(ui, controller, attempt, result, &cancel);
            },
        );
    }

    fn recover(&self, words: String) {
        let (Some(app), Some(core)) = (self.app.upgrade(), self.core()) else {
            return;
        };
        let ui = app.global::<SyncUi>();
        let Ok(server) = self.server(&ui, true) else {
            return;
        };
        let attempt = self.controller.start_attempt(&ui, "starting");
        self.run(
            async move { core.device_sync_recover(server, words).await },
            move |ui, controller, result| {
                // Joining happened even if the dialog closed meanwhile; the
                // page shows sync on from the core's status.
                if controller.attempt.get() != attempt {
                    return;
                }
                match result {
                    Ok(()) => ui.set_pairing_stage("joined".into()),
                    Err(error) => show_pairing_error(ui, &error),
                }
            },
        );
    }

    fn invite(&self, with_code: bool) {
        let (Some(app), Some(core)) = (self.app.upgrade(), self.core()) else {
            return;
        };
        let ui = app.global::<SyncUi>();
        if let Some(pairing) = self.controller.pairing.get() {
            core.device_sync_cancel_pairing(pairing);
        }
        let attempt = self.controller.start_attempt(&ui, "starting");
        let cancel = Arc::clone(&core);
        self.run(
            async move { core.device_sync_invite(with_code).await },
            move |ui, controller, result| {
                show_code(ui, controller, attempt, result, &cancel);
            },
        );
    }

    fn decide(&self, accept: bool) {
        let (Some(core), Some(pairing)) = (self.core(), self.controller.pairing.get()) else {
            return;
        };
        if accept {
            core.device_sync_accept(pairing);
        } else {
            core.device_sync_decline(pairing);
        }
    }

    fn cancel_pairing(&self) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let pairing = self.controller.end_attempt(&app.global::<SyncUi>());
        if let (Some(core), Some(pairing)) = (self.core(), pairing) {
            core.device_sync_cancel_pairing(pairing);
        }
    }

    fn remove_device(&self, device: String) {
        let Some(core) = self.core() else {
            return;
        };
        self.run(
            async move { core.device_sync_remove_device(device).await },
            |ui, _, result| match result {
                Ok(()) => show_failure(ui, "", ""),
                Err(error) => show_failure(ui, "remove", &error_text(&error)),
            },
        );
    }

    fn reset_recovery(&self) {
        let Some(core) = self.core() else {
            return;
        };
        self.run(
            async move { core.device_sync_reset_recovery().await },
            |ui, _, result| match result {
                Ok(words) => {
                    show_failure(ui, "", "");
                    show_recovery_words(ui, &words);
                }
                Err(error) => show_failure(ui, "reset", &error_text(&error)),
            },
        );
    }

    fn turn_off(&self) {
        let Some(core) = self.core() else {
            return;
        };
        self.run(
            async move { core.device_sync_turn_off().await },
            |ui, _, result| match result {
                Ok(()) => show_failure(ui, "", ""),
                Err(error) => show_failure(ui, "turn-off", &error_text(&error)),
            },
        );
    }

    fn delete_everywhere(&self) {
        let Some(core) = self.core() else {
            return;
        };
        self.run(
            async move { core.device_sync_delete_everywhere().await },
            |ui, _, result| match result {
                Ok(()) => show_failure(ui, "", ""),
                Err(error) => show_failure(ui, "delete", &error_text(&error)),
            },
        );
    }
}

/// Show the code a new pairing returned, if the dialog still waits for it.
/// A pairing the user no longer waits for is cancelled.
fn show_code(
    ui: &SyncUi,
    controller: &SyncController,
    attempt: u64,
    result: Result<PairingCode, CoreError>,
    core: &Core,
) {
    if controller.attempt.get() != attempt {
        if let Ok(code) = result {
            core.device_sync_cancel_pairing(code.pairing);
        }
        return;
    }
    match result {
        Ok(code) => {
            let image = code.qr.as_deref().and_then(qr_image);
            ui.set_pairing_has_qr(image.is_some());
            ui.set_pairing_qr(image.unwrap_or_default());
            ui.set_pairing_code(
                code.code
                    .as_deref()
                    .map(spaced_code)
                    .unwrap_or_default()
                    .into(),
            );
            ui.set_pairing_stage("code".into());
            controller.follow(ui, code.pairing);
        }
        Err(error) => show_pairing_error(ui, &error),
    }
}

fn show_step(ui: &SyncUi, event: PairingEvent) {
    let stage = match event {
        PairingEvent::Request { words } => {
            ui.set_pairing_words(words_model(&words));
            "request"
        }
        PairingEvent::Waiting { words } => {
            ui.set_pairing_words(words_model(&words));
            "waiting"
        }
        PairingEvent::Joined => "joined",
        PairingEvent::Added => "added",
        PairingEvent::Declined => "declined",
        PairingEvent::Expired => "expired",
        PairingEvent::Failed(error) => {
            ui.set_pairing_error(error.into());
            "failed"
        }
    };
    ui.set_pairing_stage(stage.into());
}

/// A pairing that could not start: a code or recovery code that matches
/// nothing, or another failure the dialog shows as it is.
fn show_pairing_error(ui: &SyncUi, error: &CoreError) {
    if matches!(error, CoreError::NotFound(_)) {
        ui.set_pairing_stage("not-found".into());
    } else {
        ui.set_pairing_error(error_text(error).into());
        ui.set_pairing_stage("failed".into());
    }
}

/// What went wrong, in the client's own words when it was the sync server,
/// without the core's category prefix.
fn error_text(error: &CoreError) -> String {
    match error {
        CoreError::Network(text) | CoreError::NotFound(text) | CoreError::Other(text) => {
            text.clone()
        }
        error => error.to_string(),
    }
}

/// An error shown on a line of its own, which starts like a sentence.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The action that failed last and why, or none.
fn show_failure(ui: &SyncUi, action: &str, failure: &str) {
    ui.set_failed_action(action.into());
    ui.set_failure(failure.into());
}

fn show_recovery_words(ui: &SyncUi, words: &str) {
    let words: Vec<&str> = words.split_whitespace().collect();
    ui.set_recovery_text(words.join(" ").into());
    ui.set_recovery_words(words_model(&words));
}

fn words_model<S: AsRef<str>>(words: &[S]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        words
            .iter()
            .map(|word| SharedString::from(word.as_ref()))
            .collect::<Vec<_>>(),
    ))
}

/// Eight digits as two groups of four, which are easier to read out and
/// type. Joining takes the digits with or without the space.
fn spaced_code(code: &str) -> String {
    let digits: Vec<char> = code.chars().filter(char::is_ascii_digit).collect();
    if digits.len() != 8 {
        return code.to_owned();
    }
    let (first, second) = digits.split_at(4);
    format!(
        "{} {}",
        first.iter().collect::<String>(),
        second.iter().collect::<String>()
    )
}

/// The sync server address typed on the page as the client takes it, or
/// `None` for Flectar's. Without a scheme it is HTTPS; plain HTTP is only
/// for a server on this device, as a session token must not cross a network
/// in the clear.
fn server_address(typed: &str) -> Result<Option<String>, ()> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Ok(None);
    }
    let url = if typed.contains("://") {
        url::Url::parse(typed)
    } else {
        url::Url::parse(&format!("https://{typed}"))
    }
    .map_err(|_| ())?;
    let loopback = match url.host() {
        Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => return Err(()),
    };
    let secure = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    if !secure
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(());
    }
    Ok(Some(url.as_str().trim_end_matches('/').to_owned()))
}

/// When sync last finished, as the page words it: a time when it was today,
/// else a date. Empty before the first sync.
fn last_synced(at: Option<i64>, now: &chrono::DateTime<Local>) -> (String, bool) {
    let Some(at) = at
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map(|at| at.with_timezone(&Local))
    else {
        return (String::new(), false);
    };
    let formats = date_format::current();
    if at.date_naive() == now.date_naive() {
        (formats.time(at.hour(), at.minute()), true)
    } else if at.year() == now.year() {
        (
            formats.date(DateStyle::ShortDayMonth, at.date_naive()),
            false,
        )
    } else {
        (formats.date(DateStyle::Short, at.date_naive()), false)
    }
}

/// A QR code for the screen: one pixel per module plus the four-module quiet
/// zone scanners need. The dialog scales it up without smoothing.
fn qr_image(text: &str) -> Option<Image> {
    const QUIET_ZONE: usize = 4;
    let code =
        qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).ok()?;
    let modules = code.width();
    let side = modules + 2 * QUIET_ZONE;
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(side as u32, side as u32);
    let pixels = buffer.make_mut_slice();
    pixels.fill(Rgba8Pixel::new(255, 255, 255, 255));
    for (index, color) in code.into_colors().into_iter().enumerate() {
        if color == qrcode::Color::Dark {
            let (x, y) = (index % modules + QUIET_ZONE, index / modules + QUIET_ZONE);
            pixels[y * side + x] = Rgba8Pixel::new(0, 0, 0, 255);
        }
    }
    Some(Image::from_rgba8(buffer))
}

/// Another device changed synced preferences: show them as if they had
/// changed here.
fn reload_preferences(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
) {
    let Some(core) = state.borrow().core.clone() else {
        return;
    };
    let app = app.as_weak();
    let state = Rc::clone(state);
    let apply_runtime = runtime.clone();
    documents::run_request(
        runtime,
        Box::pin(async move { core.load_settings().await }),
        move |settings| match (app.upgrade(), settings) {
            (Some(app), Ok(settings)) => show_preferences(&app, &state, &apply_runtime, &settings),
            (_, Err(error)) => tracing::warn!(%error, "reading synced preferences failed"),
            _ => {}
        },
    );
}

/// The synced preferences (`device_sync::SYNCED_PREFERENCES` in the core),
/// applied to the window the way each one's own setting applies it.
fn show_preferences(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    settings: &Settings,
) {
    startup::apply_theme(app, settings);
    app.global::<MessageColors>()
        .set_preference(settings.message_appearance.as_str().into());
    app.set_monochrome_sidebar_icons(settings.monochrome_sidebar_icons);
    app.set_show_avatars(settings.show_avatars);
    app.set_show_tabs(settings.show_tabs);
    app.set_workspace_layout(
        match settings.workspace_layout.as_str() {
            "minimal" => "minimal",
            _ => "default",
        }
        .into(),
    );
    if app.get_time_format().as_str() != settings.time_format.as_str() {
        app.set_time_format(settings.time_format.as_str().into());
        date_format::reload(app);
    }
    if app.get_language_mode().as_str() != normalized_language_preference(&settings.language) {
        apply_language(app, &settings.language);
    }
    app.set_mark_read_on_open(settings.mark_read_on_open);
    state.borrow_mut().mark_read_on_open = settings.mark_read_on_open;
    app.set_collect_outgoing_contacts(settings.collect_outgoing_contacts);
    app.set_collect_incoming_contacts(settings.collect_incoming_contacts);
    app.set_contact_suggest_all_accounts(settings.contact_suggest_all_accounts);
    app.set_suggest_learned_contacts(settings.suggest_learned_contacts);

    if app.get_group_mail_by_date() != settings.group_mail_by_date {
        app.set_group_mail_by_date(settings.group_mail_by_date);
        state.borrow_mut().mail_groups.clear();
        refresh_rows_only(app, state, runtime);
    }
    apply_mail_grouping(app, state, runtime, settings.mail_grouping());
    app.set_show_account_markers(settings.show_account_badges);
    settings_controller::apply_account_presentation_settings(app, state, runtime, settings);
    let remote_images = settings.load_remote_images && cfg!(feature = "remote-content");
    if app.get_remote_images_enabled() != remote_images {
        // Through the toggle's own handler: it also starts or stops loading.
        app.invoke_set_remote_images(remote_images);
    }
}

/// Another device added, changed or removed accounts. Removed ones leave
/// the window as a removal here does; the others are listed again.
fn reload_accounts(
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Handle,
    ui_tasks: &UiSender<UiTaskUpdate>,
    calendar_month: NaiveDate,
    removed: Vec<i64>,
) {
    let (core, removals) = {
        let state = state.borrow();
        let Some(core) = state.core.clone() else {
            return;
        };
        let removals: Vec<_> = removed
            .into_iter()
            .filter(|account_id| {
                state
                    .connected_accounts
                    .iter()
                    .any(|account| account.id == *account_id)
            })
            .map(|account_id| {
                (
                    account_id,
                    RemovalView::of(&state, account_id, calendar_month),
                )
            })
            .collect();
        (core, removals)
    };
    let updates = ui_tasks.clone();
    runtime.spawn(async move {
        for (account_id, view) in removals {
            let accounts = core.load_account_snapshot().await.ok();
            // A removal that failed leaves the account listed.
            let removed = accounts.as_ref().is_some_and(|snapshot| {
                !snapshot
                    .accounts
                    .iter()
                    .any(|account| account.id == account_id)
            });
            if !removed {
                continue;
            }
            let removal =
                read_account_removal(&core, account_id, true, accounts.as_ref(), view).await;
            let _ = updates
                .send(UiTaskUpdate {
                    message: UiMessage::plain("Account removed on another device."),
                    accounts,
                    account_removal: Some(removal),
                    calendar_connections: core.load_calendar_connections().await.ok(),
                    carddav_connections: core.load_carddav_connections().await.ok(),
                    calendar_error: None,
                    clear_account_form: false,
                    finishes_account_setup: false,
                    finishes_oauth: false,
                    close_to_tray: None,
                })
                .await;
        }
        let _ = updates
            .send(UiTaskUpdate {
                message: UiMessage::plain("Accounts updated from your other devices."),
                accounts: core.load_account_snapshot().await.ok(),
                account_removal: None,
                calendar_connections: core.load_calendar_connections().await.ok(),
                carddav_connections: core.load_carddav_connections().await.ok(),
                calendar_error: None,
                clear_account_form: false,
                finishes_account_setup: false,
                finishes_oauth: false,
                close_to_tray: None,
            })
            .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses_default_to_https_and_keep_plain_http_local() {
        assert_eq!(server_address("  "), Ok(None));
        assert_eq!(
            server_address("sync.example.com"),
            Ok(Some("https://sync.example.com".into()))
        );
        assert_eq!(
            server_address("https://Sync.Example.com/flectar/"),
            Ok(Some("https://sync.example.com/flectar".into()))
        );
        assert_eq!(
            server_address("http://localhost:8080"),
            Ok(Some("http://localhost:8080".into()))
        );
        assert_eq!(
            server_address("http://[::1]:8080/"),
            Ok(Some("http://[::1]:8080".into()))
        );
        assert_eq!(server_address("http://sync.example.com"), Err(()));
        assert_eq!(server_address("http://192.168.1.10"), Err(()));
        assert_eq!(server_address("ftp://sync.example.com"), Err(()));
        assert_eq!(
            server_address("https://user:pass@sync.example.com"),
            Err(())
        );
        assert_eq!(server_address("https://sync.example.com/?a=b"), Err(()));
        assert_eq!(server_address("not a url"), Err(()));
    }

    #[test]
    fn errors_on_their_own_line_start_like_sentences() {
        assert_eq!(
            sentence("could not reach the sync server"),
            "Could not reach the sync server"
        );
        assert_eq!(sentence(""), "");
    }

    #[test]
    fn short_codes_show_as_two_groups() {
        assert_eq!(spaced_code("12345678"), "1234 5678");
        assert_eq!(spaced_code("1234 5678"), "1234 5678");
        assert_eq!(spaced_code("123"), "123");
    }

    #[test]
    fn qr_codes_keep_a_quiet_zone() {
        let image = qr_image(
            "flectar-mail-sync:pair?v=1&server=https%3A%2F%2Fsync.flectar.com&slot=AAAAAAAAAAAAAAAAAAAAAA&secret=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        )
        .unwrap();
        let size = image.size();
        assert_eq!(size.width, size.height);
        // Version 1 is 21 modules a side; a pairing link needs more.
        assert!(size.width > 21 + 8);
        let pixels = image.to_rgba8().unwrap();
        let corner = pixels.as_slice()[0];
        assert_eq!(corner, Rgba8Pixel::new(255, 255, 255, 255));
        // The finder pattern starts right after the quiet zone.
        let finder = pixels.as_slice()[4 * size.width as usize + 4];
        assert_eq!(finder, Rgba8Pixel::new(0, 0, 0, 255));
    }

    #[test]
    fn last_synced_shows_a_time_today_and_a_date_before() {
        let now = Local.with_ymd_and_hms(2026, 11, 7, 15, 0, 0).unwrap();
        let today = Local.with_ymd_and_hms(2026, 11, 7, 9, 5, 0).unwrap();
        let (text, is_today) = last_synced(Some(today.timestamp_millis()), &now);
        assert!(is_today);
        assert!(text.contains("05"), "{text}");
        let earlier = Local.with_ymd_and_hms(2026, 10, 22, 9, 5, 0).unwrap();
        let (text, is_today) = last_synced(Some(earlier.timestamp_millis()), &now);
        assert!(!is_today);
        assert!(text.contains("22"), "{text}");
        assert_eq!(last_synced(None, &now), (String::new(), false));
    }
}
