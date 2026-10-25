//! Connected-account model projection and desktop host utilities.

use super::*;

fn mail_domain(address: &str) -> Option<&str> {
    let address = address.trim();
    if address.is_empty() || address.chars().any(char::is_whitespace) {
        return None;
    }
    let (local, domain) = address.rsplit_once('@')?;
    (!local.is_empty()
        && !local.contains('@')
        && !domain.is_empty()
        && !domain.contains('@')
        && !domain.starts_with('.')
        && !domain.ends_with('.'))
    .then_some(domain)
}

fn same_mail_transport(left: &AccountConfig, right: &AccountConfig) -> bool {
    left.incoming_host
        .eq_ignore_ascii_case(&right.incoming_host)
        && left.incoming_port == right.incoming_port
        && left.smtp_host.eq_ignore_ascii_case(&right.smtp_host)
        && left.smtp_port == right.smtp_port
        && left.settings.connection == right.settings.connection
}

/// Reuse transport details only when every manual account of the form's
/// protocol for the domain agrees. Ambiguous configurations deliberately leave
/// the new form untouched.
pub(super) fn reusable_mail_transport<'a>(
    configs: &'a [AccountConfig],
    address: &str,
    protocol: MailProtocol,
) -> Option<&'a AccountConfig> {
    let domain = mail_domain(address)?;
    let mut matches = configs.iter().filter(|config| {
        config.provider == Provider::Generic
            && config.mail_protocol == protocol
            && mail_domain(&config.email)
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(domain))
    });
    let first = matches.next()?;
    matches
        .all(|candidate| same_mail_transport(first, candidate))
        .then_some(first)
}

pub(super) struct AccountRemovalUpdate {
    pub account_id: i64,
    pub removed: bool,
    pub scope: Scope,
    pub query: String,
    pub metadata: Option<mail::MailMetadata>,
    pub page: Option<mail::MailPage>,
    pub calendar_month: NaiveDate,
    pub calendar_events: Option<Vec<calendar::LocalCalendarEvent>>,
    pub calendar_sources: Option<Vec<calendar::LocalCalendarSource>>,
}

pub(super) fn account_was_removed(
    account_id: i64,
    result: &Result<(), String>,
    snapshot: Option<&mail::AccountSnapshot>,
) -> bool {
    result.is_ok()
        || snapshot.is_some_and(|snapshot| {
            !snapshot
                .accounts
                .iter()
                .any(|account| account.id == account_id)
        })
}

pub(super) fn snapshot_is_current(state: &Rc<RefCell<InboxState>>, revision: u64) -> bool {
    state
        .borrow()
        .core
        .as_ref()
        .is_some_and(|core| core.account_revision() == revision)
}

/// Whether the selected scope shows only mail of `account_id`.
pub(super) fn account_owns_scope(state: &InboxState, account_id: i64) -> bool {
    scope_owned_by(state, state.scope, account_id)
}

/// Whether `scope` is one of the account's own mailboxes or labels.
pub(super) fn scope_owned_by(state: &InboxState, scope: Scope, account_id: i64) -> bool {
    match scope {
        Scope::Account {
            account_id: owner, ..
        } => owner == account_id,
        Scope::Folder(_) => state
            .mailboxes
            .iter()
            .any(|mailbox| mailbox.account_id == account_id && mailbox.scope == Some(scope)),
        Scope::Label { id, .. } => state
            .labels
            .iter()
            .any(|label| label.id == id && label.owner_account_id == Some(account_id)),
        Scope::Unified(_) | Scope::Important | Scope::Other => false,
    }
}

/// Apply the local half of deletion before projecting any account or mail
/// models. Even if reloading fails after deletion, no deleted rows survive.
pub(super) fn reconcile_removed_account(state: &mut InboxState, account_id: i64) {
    let selection_removed = account_owns_scope(state, account_id);
    if state
        .messages
        .iter()
        .any(|message| message.account_id == account_id && Some(message.id) == state.selected_id)
    {
        state.selected_id = None;
        state.preview_closed = true;
        state.remote_images_override_id = None;
    }
    state
        .messages
        .retain(|message| message.account_id != account_id);
    state
        .mailboxes
        .retain(|mailbox| mailbox.account_id != account_id);
    let accounts = std::mem::take(&mut state.connected_accounts)
        .into_iter()
        .filter(|account| account.id != account_id)
        .collect();
    // Paging and the worker generation are reset below either way.
    let _ = state.set_connected_accounts(accounts);
    if selection_removed {
        state.set_scope(Scope::UNIFIED_INBOX);
    }
    state
        .account_configs
        .retain(|config| config.id != account_id);
    state.account_presentation.remove_account(account_id);
    state
        .calendar_connections
        .retain(|connection| connection.account_id != account_id);
    state.calendar_errors.remove(&account_id);
    state.profile_avatar_images.remove(&account_id);
    state.profile_avatar_missing.remove(&account_id);
    state.profile_avatar_pending.remove(&account_id);
    state.initialized_sidebar_accounts.remove(&account_id);
    state
        .collapsed_sidebar_sections
        .remove(&format!("account:{account_id}"));
    state
        .collapsed_sidebar_sections
        .remove(&format!("account-labels:{account_id}"));
    state.collapsed_folder_ids.retain(|id| {
        state
            .mailboxes
            .iter()
            .any(|mailbox| mailbox.folder_id == *id)
    });
    state.initialized_sidebar_folders.retain(|id| {
        state
            .mailboxes
            .iter()
            .any(|mailbox| mailbox.folder_id == *id)
    });
    state.checked_ids.clear();
    state.next_cursor = None;
    state.page = 1;
    state.inbox_count = 0;
    for mailbox in &mut state.unified_mailboxes {
        mailbox.count.clear();
    }
    if state.connected_accounts.is_empty() {
        state.messages.clear();
        state.mailboxes.clear();
        state.unified_mailboxes.clear();
        state.labels.clear();
        state.set_scope(Scope::UNIFIED_INBOX);
        state.query.clear();
        state.selected_id = None;
    }
    mail_work::invalidate(state);
}

#[allow(
    clippy::too_many_arguments,
    reason = "Projects the independently loaded account, connection, avatar, and presentation data into one UI model."
)]
pub(super) fn apply_connected_accounts(
    app: &AppWindow,
    accounts: &[Account],
    configs: &[AccountConfig],
    calendar_connections: &[CalendarConnection],
    carddav_connections: &[CardDavConnection],
    calendar_errors: &HashMap<i64, String>,
    avatars: &HashMap<i64, ProfileAvatarImages>,
    presentation: &AccountPresentationSettings,
) {
    let configs: HashMap<i64, &AccountConfig> =
        configs.iter().map(|config| (config.id, config)).collect();
    let mut account_calendars: HashMap<i64, Vec<&CalendarConnection>> = HashMap::new();
    for connection in calendar_connections {
        account_calendars
            .entry(connection.account_id)
            .or_default()
            .push(connection);
    }
    let carddav_connections: HashMap<i64, &CardDavConnection> = carddav_connections
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
            let calendars = account_calendars
                .get(&account.id)
                .map(Vec::as_slice)
                .unwrap_or_default();
            let carddav = carddav_connections.get(&account.id);
            let profile = presentation.profile(account.id);
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
                profile_id: profile
                    .map(|profile| profile.id.clone())
                    .unwrap_or_default()
                    .into(),
                profile_name: profile
                    .map(|profile| profile.name.clone())
                    .unwrap_or_default()
                    .into(),
                profile_color: presentation.marker_color(account.id),
                has_profile: profile.is_some(),
                has_account_color: presentation.has_account_color(account.id),
                username: config.username.clone().into(),
                jmap_url: config.jmap_url.clone().into(),
                incoming_security: config.settings.connection.incoming_security.as_str().into(),
                smtp_security: config.settings.connection.smtp_security.as_str().into(),
                trusted_certificate_pem: config
                    .settings
                    .connection
                    .trusted_certificate_pem
                    .clone()
                    .into(),
                incoming_host: config.incoming_host.clone().into(),
                incoming_port: config.incoming_port.to_string().into(),
                smtp_host: config.smtp_host.clone().into(),
                smtp_port: config.smtp_port.to_string().into(),
                mail_history: config.settings.mail_history.as_str().into(),
                sent_copy_policy: config.settings.sent_copy_policy.as_str().into(),
                keep_on_server: config.settings.keep_on_server.as_str().into(),
                server_keeps_mail_days: account
                    .server_keeps_mail_days
                    .and_then(|days| i32::try_from(days).ok())
                    .unwrap_or(-1),
                calendar_connections: calendar_connection_rows(calendars),
                provider_calendar_connected: calendars.iter().any(|connection| {
                    connection.kind != flectar_mail_core::models::CalendarConnectionKind::Generic
                }),
                provider_calendar_error: calendar_errors
                    .get(&account.id)
                    .cloned()
                    .unwrap_or_default()
                    .into(),
                carddav_connected: carddav.is_some(),
                carddav_enabled: carddav.is_some_and(|connection| connection.enabled),
                carddav_status: carddav
                    .and_then(|connection| connection.last_error.clone())
                    .unwrap_or_default()
                    .into(),
                carddav_url: carddav
                    .map(|connection| connection.base_url.clone())
                    .unwrap_or_default()
                    .into(),
                carddav_username: carddav
                    .map(|connection| connection.username.clone())
                    .unwrap_or_else(|| config.username.clone())
                    .into(),
            })
        })
        .collect::<Vec<_>>();
    apply_connected_account_rows(app, rows);
    app.set_has_device_mail(
        accounts
            .iter()
            .any(|account| account.mail_protocol == MailProtocol::Pop3),
    );
    let known_accounts = accounts
        .iter()
        .map(|account| account.id)
        .collect::<HashSet<_>>();
    let profile_row = |profile: &MailProfile| MailProfileRow {
        id: profile.id.clone().into(),
        name: profile.name.clone().into(),
        color: parse_marker_color(&profile.color)
            .unwrap_or_else(|| slint::Color::from_rgb_u8(107, 114, 128)),
        account_count: i32::try_from(
            profile
                .account_ids
                .iter()
                .filter(|account_id| known_accounts.contains(account_id))
                .count(),
        )
        .unwrap_or(i32::MAX),
    };
    app.set_mail_profiles(ModelRc::new(VecModel::from(
        presentation
            .profiles
            .iter()
            .map(profile_row)
            .collect::<Vec<_>>(),
    )));
    app.set_active_mail_profile(
        presentation
            .active_profile()
            .map(profile_row)
            .unwrap_or_default(),
    );
    app.set_show_account_markers(presentation.show_markers);
}

pub(super) fn refresh_connected_accounts(app: &AppWindow, state: &Rc<RefCell<InboxState>>) {
    let state = state.borrow();
    apply_connected_accounts(
        app,
        &state.connected_accounts,
        &state.account_configs,
        &state.calendar_connections,
        &state.carddav_connections,
        &state.calendar_errors,
        &state.profile_avatar_images,
        &state.account_presentation,
    );
    prefer_shown_compose_account(app, &state);
    drop(state);
    app.global::<AccountMailPreferences>()
        .invoke_context_changed();
    app.global::<FilesUi>().invoke_context_changed();
}

pub(super) fn update_connected_accounts(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    accounts: Vec<Account>,
    configs: Vec<AccountConfig>,
) {
    // A new account is in no profile yet: show every account rather than
    // hide its mail behind the active one.
    let shows_new_account = {
        let state = state.borrow();
        state.profile_accounts.is_some()
            && !state.connected_accounts.is_empty()
            && accounts.iter().any(|account| {
                !state
                    .connected_accounts
                    .iter()
                    .any(|known| known.id == account.id)
            })
    };
    let scope_changed = {
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
        state.account_configs = configs;
        state.set_connected_accounts(accounts)
    };
    refresh_connected_accounts(app, state);
    // Adding or removing an account can make the selected mailbox invisible.
    // Reload as a navigation so pages for the previous selection are retired.
    if scope_changed {
        {
            let mut state = state.borrow_mut();
            state.page = 1;
            state.next_cursor = None;
            state.checked_ids.clear();
        }
        if let Err(error) = refresh_from_source(app, state, runtime, false, &[]) {
            app.set_render_status(UiMessage::detail("Mail refresh failed: {}", error));
        }
    }
    refresh_sidebar(state);
    schedule_profile_avatar_fetches(app, state, runtime);
    if shows_new_account {
        app.invoke_select_mail_profile("".into());
    }
}

/// One account's calendar connections as settings shows them.
fn calendar_connection_rows(connections: &[&CalendarConnection]) -> ModelRc<CalendarConnectionRow> {
    let rows = connections
        .iter()
        .filter_map(|connection| {
            Some(CalendarConnectionRow {
                id: i32::try_from(connection.id).ok()?,
                kind: connection.kind.as_str().into(),
                base_url: connection.base_url.as_str().into(),
                host: url::Url::parse(&connection.base_url)
                    .ok()
                    .and_then(|url| url.host_str().map(str::to_owned))
                    .unwrap_or_default()
                    .into(),
                username: connection.username.as_str().into(),
                calendar_name: match connection.calendars.as_slice() {
                    [name] => name.as_str().into(),
                    _ => Default::default(),
                },
                calendar_count: i32::try_from(connection.calendars.len()).unwrap_or(i32::MAX),
                enabled: connection.enabled,
                error: connection.last_error.as_deref().unwrap_or_default().into(),
            })
        })
        .collect::<Vec<_>>();
    ModelRc::new(VecModel::from(rows))
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
    set_compose_account(app, selected);
    app.set_connected_accounts(ModelRc::new(VecModel::from(rows)));
}

fn set_compose_account(app: &AppWindow, account: Option<&AccountRow>) {
    if let Some(account) = account {
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
}

/// New mail starts from an account the mail view shows, so writing from a
/// profile sends from one of its accounts.
pub(super) fn prefer_shown_compose_account(app: &AppWindow, state: &InboxState) {
    if state.shows_account(i64::from(app.get_compose_account_id())) {
        return;
    }
    let rows = connected_account_rows(app);
    if let Some(account) = rows
        .iter()
        .find(|account| state.shows_account(i64::from(account.id)))
    {
        set_compose_account(app, Some(account));
    }
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
        let selected = state
            .conversation_messages
            .get(state.conversation_selected_index)
            .filter(|_| state.conversation_owner_id == Some(selected_id))
            .or_else(|| {
                state
                    .messages
                    .iter()
                    .find(|message| message.id == selected_id)
            });
        selected
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
            .conversation_messages
            .get(state.conversation_selected_index)
            .filter(|_| state.conversation_owner_id == Some(selected_id))
            .or_else(|| {
                state
                    .messages
                    .iter()
                    .find(|message| message.id == selected_id)
            })
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

#[cfg(test)]
mod mail_transport_suggestion_tests {
    use super::*;

    fn config(id: i64, email: &str, incoming_host: &str, smtp_host: &str) -> AccountConfig {
        AccountConfig {
            id,
            email: email.into(),
            display_name: None,
            avatar_url: None,
            provider: Provider::Generic,
            auth_kind: flectar_mail_core::models::AuthKind::Password,
            mail_protocol: MailProtocol::Imap,
            username: email.into(),
            jmap_url: String::new(),
            jmap_account_id: None,
            incoming_host: incoming_host.into(),
            incoming_port: 993,
            smtp_host: smtp_host.into(),
            smtp_port: 465,
            settings: flectar_mail_core::models::AccountSettings::new(MailConnectionSettings::new(
                ConnectionSecurity::Tls,
                ConnectionSecurity::Tls,
            )),
        }
    }

    #[test]
    fn reuses_the_unique_transport_for_a_matching_domain() {
        let configs = vec![config(
            1,
            "first@Example.com",
            "imap.example.com",
            "smtp.example.com",
        )];
        let suggestion =
            reusable_mail_transport(&configs, "second@example.COM", MailProtocol::Imap).unwrap();
        assert_eq!(suggestion.incoming_host, "imap.example.com");
        assert_eq!(suggestion.smtp_host, "smtp.example.com");
    }

    #[test]
    fn leaves_ambiguous_or_incomplete_addresses_untouched() {
        let configs = vec![
            config(
                1,
                "first@example.com",
                "imap-a.example.com",
                "smtp.example.com",
            ),
            config(
                2,
                "other@example.com",
                "imap-b.example.com",
                "smtp.example.com",
            ),
        ];
        assert!(reusable_mail_transport(&configs, "new@example.com", MailProtocol::Imap).is_none());
        assert!(reusable_mail_transport(&configs, "new@", MailProtocol::Imap).is_none());
    }

    #[test]
    fn only_accounts_of_the_form_protocol_are_reused() {
        let mut pop3 = config(2, "pop@example.com", "pop.example.com", "smtp.example.com");
        pop3.mail_protocol = MailProtocol::Pop3;
        let configs = vec![
            config(
                1,
                "imap@example.com",
                "imap.example.com",
                "smtp.example.com",
            ),
            pop3,
        ];
        let host = |protocol| {
            reusable_mail_transport(&configs, "new@example.com", protocol)
                .map(|config| config.incoming_host.as_str())
        };
        assert_eq!(host(MailProtocol::Pop3), Some("pop.example.com"));
        assert_eq!(host(MailProtocol::Imap), Some("imap.example.com"));
        assert_eq!(host(MailProtocol::Jmap), None);
    }
}
