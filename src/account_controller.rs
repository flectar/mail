//! Connected-account model projection and desktop host utilities.

use super::*;

pub(super) fn apply_connected_accounts(
    app: &AppWindow,
    accounts: &[Account],
    configs: &[AccountConfig],
    calendar_connections: &[CalendarConnection],
    calendar_errors: &HashMap<i64, String>,
    avatars: &HashMap<i64, ProfileAvatarImages>,
) {
    let configs: HashMap<i64, &AccountConfig> =
        configs.iter().map(|config| (config.id, config)).collect();
    let calendar_connections: HashMap<i64, &CalendarConnection> = calendar_connections
        .iter()
        .map(|connection| (connection.account_id, connection))
        .collect();
    let rows = accounts
        .iter()
        .filter_map(|account| {
            let config = configs.get(&account.id)?;
            let name = account
                .display_name
                .as_ref()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(&account.email)
                .clone();
            let avatar = avatars.get(&account.id);
            let calendar = calendar_connections.get(&account.id);
            Some(AccountRow {
                id: i32::try_from(account.id).ok()?,
                drag_key: account.id.to_string().into(),
                name: name.clone().into(),
                email: account.email.clone().into(),
                provider: account.provider.as_str().to_owned().into(),
                mail_protocol: account.mail_protocol.as_str().to_owned().into(),
                status: account.sync_state.clone().into(),
                sync_error: account.sync_error.clone().unwrap_or_default().into(),
                initials: avatar_initials(&name).into(),
                avatar: avatar
                    .map(|images| slint_image(&images.regular))
                    .unwrap_or_default(),
                avatar_small: avatar
                    .map(|images| slint_image(&images.small))
                    .unwrap_or_default(),
                has_avatar: avatar.is_some(),
                username: config.username.clone().into(),
                jmap_url: config.jmap_url.clone().into(),
                imap_host: config.imap_host.clone().into(),
                imap_port: config.imap_port.to_string().into(),
                smtp_host: config.smtp_host.clone().into(),
                smtp_port: config.smtp_port.to_string().into(),
                mail_history: config.settings.mail_history.as_str().into(),
                calendar_connected: calendar.is_some(),
                calendar_enabled: calendar.is_some_and(|connection| connection.enabled),
                calendar_status: calendar
                    .and_then(|connection| connection.last_error.clone())
                    .or_else(|| calendar_errors.get(&account.id).cloned())
                    .unwrap_or_default()
                    .into(),
            })
        })
        .collect::<Vec<_>>();
    apply_connected_account_rows(app, rows);
}

pub(super) fn refresh_connected_accounts(app: &AppWindow, state: &Rc<RefCell<InboxState>>) {
    let state = state.borrow();
    apply_connected_accounts(
        app,
        &state.connected_accounts,
        &state.account_configs,
        &state.calendar_connections,
        &state.calendar_errors,
        &state.profile_avatar_images,
    );
}

pub(super) fn update_connected_accounts(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    accounts: Vec<Account>,
    configs: Vec<AccountConfig>,
) {
    {
        let mut state = state.borrow_mut();
        let previous_urls = state
            .connected_accounts
            .iter()
            .map(|account| (account.id, account.avatar_url.clone()))
            .collect::<HashMap<_, _>>();
        let current_urls = accounts
            .iter()
            .map(|account| (account.id, account.avatar_url.clone()))
            .collect::<HashMap<_, _>>();
        let unchanged = |account_id: &i64| {
            current_urls.get(account_id) == previous_urls.get(account_id)
                && current_urls
                    .get(account_id)
                    .and_then(Option::as_ref)
                    .is_some()
        };
        state.profile_avatar_images.retain(|id, _| unchanged(id));
        state.profile_avatar_missing.retain(unchanged);
        state.profile_avatar_pending.retain(unchanged);
        state.connected_accounts = accounts;
        state.account_configs = configs;
    }
    refresh_connected_accounts(app, state);
    schedule_profile_avatar_fetches(app, state, runtime);
}

pub(super) fn connected_account_rows(app: &AppWindow) -> Vec<AccountRow> {
    let model = app.get_connected_accounts();
    (0..model.row_count())
        .filter_map(|index| model.row_data(index))
        .collect()
}

pub(super) fn apply_connected_account_rows(app: &AppWindow, rows: Vec<AccountRow>) {
    let selected = rows
        .iter()
        .find(|account| account.id == app.get_compose_account_id())
        .or_else(|| rows.first());
    if let Some(account) = selected {
        app.set_compose_account_id(account.id);
        app.set_compose_from_label(if account.name == account.email {
            account.email.clone()
        } else {
            format!("{}  <{}>", account.name, account.email).into()
        });
    } else {
        app.set_compose_account_id(-1);
        app.set_compose_from_label(app.global::<I18n>().invoke_no_connected_account());
    }
    app.set_connected_accounts(ModelRc::new(VecModel::from(rows)));
}

pub(super) fn apply_account_states(
    app: &AppWindow,
    inbox_state: &Rc<RefCell<InboxState>>,
    states: &HashMap<i64, (String, Option<String>)>,
) {
    let mut state_guard = inbox_state.borrow_mut();
    let mut changed = false;
    for account in &mut state_guard.connected_accounts {
        if let Some((sync_state, sync_error)) = states.get(&account.id) {
            if account.sync_state != *sync_state {
                account.sync_state.clone_from(sync_state);
                changed = true;
            }
            if account.sync_error != *sync_error {
                account.sync_error.clone_from(sync_error);
                changed = true;
            }
        }
    }
    drop(state_guard);
    if changed {
        refresh_connected_accounts(app, inbox_state);
    }
}

pub(super) fn reorder_connected_account_rows(
    app: &AppWindow,
    source_id: i64,
    target_id: i64,
    after: bool,
) -> bool {
    let mut rows = connected_account_rows(app);
    let Some(source_index) = rows
        .iter()
        .position(|account| i64::from(account.id) == source_id)
    else {
        return false;
    };
    let source = rows.remove(source_index);
    let Some(target_index) = rows
        .iter()
        .position(|account| i64::from(account.id) == target_id)
    else {
        return false;
    };
    rows.insert(target_index + usize::from(after), source);
    apply_connected_account_rows(app, rows);
    true
}

pub(super) fn print_selected_message(state: &Rc<RefCell<InboxState>>) -> Result<(), String> {
    let html = {
        let state = state.borrow();
        let selected_id = state
            .selected_id
            .ok_or_else(|| "no message is selected".to_owned())?;
        state
            .messages
            .iter()
            .find(|message| message.id == selected_id)
            .and_then(|message| message.html.clone())
            .ok_or_else(|| "message body is not ready".to_owned())?
    };
    let printable = crate::email_document::export_html(&html, true);
    open_temporary_html("flectar-mail-print-", &printable)
}

pub(super) fn open_selected_message_in_browser(
    state: &Rc<RefCell<InboxState>>,
) -> Result<(), String> {
    let html = {
        let state = state.borrow();
        let selected_id = state
            .selected_id
            .ok_or_else(|| "no message is selected".to_owned())?;
        let message = state
            .messages
            .iter()
            .find(|message| message.id == selected_id)
            .ok_or_else(|| "selected message is no longer available".to_owned())?;
        message.html.clone().unwrap_or_else(|| {
            let preview = display_preview(&message.preview);
            format!(
                "<html><body style=\"font-family:Arial,sans-serif;padding:32px;line-height:1.6;color:#303348\"><p>{preview}</p></body></html>"
            )
        })
    };
    open_temporary_html(
        "flectar-mail-message-",
        &crate::email_document::export_html(&html, false),
    )
}

/// Materialize browser/print HTML without a predictable path or permissive
/// mode, then remove it after the browser has had ample time to load it.
fn open_temporary_html(prefix: &str, html: &str) -> Result<(), String> {
    use std::io::Write;

    let mut file = tempfile::Builder::new()
        .prefix(prefix)
        .suffix(".html")
        .tempfile()
        .map_err(|error| error.to_string())?;
    file.write_all(html.as_bytes())
        .and_then(|_| file.flush())
        .map_err(|error| error.to_string())?;
    let (_file, path) = file.keep().map_err(|error| error.error.to_string())?;
    let url = reqwest::Url::from_file_path(&path)
        .map_err(|()| "could not convert the temporary HTML path to a URL".to_owned())?;
    if let Err(error) = webbrowser::open(url.as_str()) {
        let _ = std::fs::remove_file(path);
        return Err(error.to_string());
    }
    let _ = std::thread::Builder::new()
        .name("flectar-mail-temp-cleanup".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(10 * 60));
            let _ = std::fs::remove_file(path);
        });
    Ok(())
}

pub(super) fn normalize_appimage_environment() {
    let Ok(xdg_data_dirs) = std::env::var("XDG_DATA_DIRS") else {
        return;
    };

    // Older cargo-appimage launchers can accidentally include the variable
    // name in its value. Let desktop/font discovery fall back to the host's
    // normal search paths instead of feeding that malformed value to Slint.
    if xdg_data_dirs.starts_with("XDG_DATA_DIRS=") {
        // SAFETY: `run` calls this before backend selection, the Tokio runtime,
        // or any application worker thread exists, so no concurrent environment
        // access can race this process-wide mutation.
        unsafe { std::env::remove_var("XDG_DATA_DIRS") };
    }
}
