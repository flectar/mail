mod account_controller;
mod calendar;
mod compose_controller;
mod compose_editor;
mod contacts;
mod data_controller;
mod email_document;
pub mod favicon;
mod latest_load;
mod mail;
mod mail_render_projection;
mod mail_view_model;
mod reader_clipboard;
#[cfg(test)]
mod reader_validation;
mod remote;
mod renderer;
mod renderer_input_controller;
mod rich_compose;
mod settings_controller;
mod startup;
mod startup_metrics;
mod theme;
mod ui_dispatch;
mod ui_message;
mod window_controller;

slint::include_modules!();

mod tray_ui {
    include!(concat!(env!("OUT_DIR"), "/tray.rs"));
}

use account_controller::*;
#[cfg(test)]
use calendar::start_of_week;
use calendar::{
    LocalCalendarState, apply_calendar, first_of_month, refresh_calendar_events, shift_month,
};
use chrono::{Duration as ChronoDuration, Local, NaiveDate, NaiveTime, TimeZone};
use compose_controller::*;
use compose_editor::{ComposeEditorStyle, CosmicComposeEditor, RenderedComposeEditor};
use contacts::{
    ContactDirectoryState, apply_contact_directory, apply_contact_rows, clear_contact_form,
};
use data_controller::register_data_management_callbacks;
use favicon::{
    FaviconImage, FaviconImages, FaviconLoader, ProfileAvatarImages, ProfileAvatarLoader,
    physical_pixel_side,
};
use flectar_mail_core::config::Paths;
use flectar_mail_core::models::{
    Account, AccountConfig, AddPasswordAccountArgs, CalendarConnection, ContactRecord,
    ContactRecordCursor, ContactRecordPage, CreateEventArgs, DraftAttachmentIn, Label,
    MailProtocol, Provider, ThreadCursor, UpdateEventArgs,
};
#[cfg(test)]
use mail::fixture_messages;
use mail::{
    ComposeMessage, ComposeSource, CoreMailSource, MailMessage, MailboxEntry, display_preview,
};
use mail_render_projection::*;
use mail_view_model::*;
use renderer::{GpuEmailRenderer, RenderedEmail};
use renderer_input_controller::register_renderer_input_callbacks;
use rich_compose::{ComposeSelection, RichComposeDocument};
use settings_controller::register_settings_preference_callbacks;
use slint::{DataTransfer, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, VecModel};
use startup::{
    PendingCoreUpdates, StartupCalendarSnapshot, StartupSnapshot, StartupUpdate,
    WarmStartCacheWriter, WarmStartProjection, WarmStartSnapshot, apply_settings,
    load_startup_calendar_snapshot, load_startup_mail_metadata, load_startup_snapshot,
    load_warm_start_snapshot, spawn_core_event_listener,
};
use startup_metrics::StartupMetrics;
use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::mpsc::error::TryRecvError;
use ui_dispatch::{UiSender, UiWake, bounded_ui_channel};
use ui_message::{AppWindowMessages, UiMessage, translated};
use window_controller::{
    create_and_register_window_lifecycle, register_window_preference_callbacks,
};

const PAGE_SIZE: usize = 25;
const FAVICON_CONCURRENCY: usize = 4;
const MAX_FAVICON_CACHE_ENTRIES: usize = 256;
const MAX_FAVICON_CACHE_BYTES: usize = 8 * 1024 * 1024;
const SENDER_AVATAR_SMALL_SIDE: f32 = 28.0;
const SENDER_AVATAR_REGULAR_SIDE: f32 = 38.0;
const ACCOUNT_AVATAR_SMALL_SIDE: f32 = 22.0;
const ACCOUNT_AVATAR_REGULAR_SIDE: f32 = 38.0;
const MAX_COMPOSE_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;

fn favicon_bytes(images: &FaviconImages) -> usize {
    images
        .small
        .pixels
        .len()
        .saturating_add(images.regular.pixels.len())
}

fn insert_bounded_favicon_result(
    icons: &mut HashMap<String, FaviconImages>,
    missing: &mut HashSet<String>,
    domain: String,
    result: Option<FaviconImages>,
) {
    insert_bounded_favicon_result_with_limits(
        icons,
        missing,
        domain,
        result,
        MAX_FAVICON_CACHE_ENTRIES,
        MAX_FAVICON_CACHE_BYTES,
    );
}

fn insert_bounded_favicon_result_with_limits(
    icons: &mut HashMap<String, FaviconImages>,
    missing: &mut HashSet<String>,
    domain: String,
    mut result: Option<FaviconImages>,
    max_entries: usize,
    max_bytes: usize,
) {
    icons.remove(&domain);
    missing.remove(&domain);
    if max_entries == 0 {
        return;
    }

    if result
        .as_ref()
        .is_some_and(|images| favicon_bytes(images) > max_bytes)
    {
        result = None;
    }
    let incoming_bytes = result.as_ref().map_or(0, favicon_bytes);
    let mut retained_bytes = icons.values().map(favicon_bytes).sum::<usize>();
    while icons.len().saturating_add(missing.len()) >= max_entries
        || retained_bytes.saturating_add(incoming_bytes) > max_bytes
    {
        // Byte pressure must evict a decoded raster. For entry-count pressure,
        // discard a negative lookup first so successfully loaded brand marks
        // remain warm as the user pages through mail.
        let byte_pressure = retained_bytes.saturating_add(incoming_bytes) > max_bytes;
        if !byte_pressure && let Some(victim) = missing.iter().next().cloned() {
            missing.remove(&victim);
            continue;
        }
        let Some(victim) = icons.keys().next().cloned() else {
            break;
        };
        if let Some(removed) = icons.remove(&victim) {
            retained_bytes = retained_bytes.saturating_sub(favicon_bytes(&removed));
        }
    }

    if let Some(images) = result {
        icons.insert(domain, images);
    } else if max_entries > 0 {
        missing.insert(domain);
    }
}

fn paged_visible_count(page: usize, total: usize) -> usize {
    page.saturating_mul(PAGE_SIZE).min(total)
}

/// Reconcile a retained Slint model by stable row id.
///
/// Keeping the same `VecModel` lets `ListView` retain its virtualization and
/// scroll anchor. The common pagination path is one batched tail insertion;
/// live updates near the top use granular insert/remove notifications so the
/// row currently under the pointer does not move on screen.
fn reconcile_model_rows<T>(model: &VecModel<T>, rows: Vec<T>, key: impl Fn(&T) -> i32)
where
    T: Clone + PartialEq + 'static,
{
    let mut current = model.iter().collect::<Vec<_>>();

    if rows.is_empty() {
        if !current.is_empty() {
            model.clear();
        }
        return;
    }
    if current.is_empty() {
        model.extend(rows);
        return;
    }

    let shared_prefix = current
        .iter()
        .zip(&rows)
        .take_while(|(old, new)| key(old) == key(new))
        .count();
    if shared_prefix == current.len().min(rows.len()) {
        for index in 0..shared_prefix {
            if current[index] != rows[index] {
                model.set_row_data(index, rows[index].clone());
            }
        }
        if rows.len() > current.len() {
            model.extend(rows[current.len()..].iter().cloned());
        } else {
            while current.len() > rows.len() {
                current.pop();
                model.remove(current.len());
            }
        }
        return;
    }

    // Unrelated result sets should reset in one notification. Related sets
    // (new mail inserted, a thread removed, or rows reordered) are edited by
    // id so Slint can keep the visible delegate anchored.
    let current_ids = current.iter().map(&key).collect::<HashSet<_>>();
    let overlap = rows
        .iter()
        .filter(|row| current_ids.contains(&key(row)))
        .count();
    if overlap.saturating_mul(2) < current.len().min(rows.len()) {
        model.set_vec(rows);
        return;
    }

    let mut index = 0;
    while index < rows.len() {
        if index >= current.len() {
            model.extend(rows[index..].iter().cloned());
            break;
        }
        if key(&current[index]) == key(&rows[index]) {
            if current[index] != rows[index] {
                model.set_row_data(index, rows[index].clone());
                current[index] = rows[index].clone();
            }
            index += 1;
            continue;
        }
        if let Some(relative) = current[index + 1..]
            .iter()
            .position(|row| key(row) == key(&rows[index]))
        {
            let found = index + 1 + relative;
            for _ in index..found {
                current.remove(index);
                model.remove(index);
            }
        } else {
            current.insert(index, rows[index].clone());
            model.insert(index, rows[index].clone());
        }
    }
    while current.len() > rows.len() {
        current.pop();
        model.remove(current.len());
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn pick_compose_attachment_paths(title: String) -> Option<Vec<PathBuf>> {
    rfd::FileDialog::new().set_title(title).pick_files()
}

#[cfg(any(target_os = "android", target_os = "ios"))]
fn pick_compose_attachment_paths(_title: String) -> Option<Vec<PathBuf>> {
    None
}

#[derive(Clone, Debug)]
struct ComposeFile {
    path: PathBuf,
    filename: String,
    size: u64,
}

struct FaviconUpdate {
    domain: String,
    pixel_sides: (u32, u32),
    icons: Option<FaviconImages>,
}

struct ProfileAvatarUpdate {
    account_id: i64,
    source_url: String,
    pixel_sides: (u32, u32),
    images: Option<ProfileAvatarImages>,
}

struct SyncUpdate {
    result: Result<(), String>,
    metadata: Option<mail::MailMetadata>,
}

struct MailListUpdate {
    scope: String,
    query: String,
    kind: MailListUpdateKind,
    result: Result<mail::MailPage, String>,
}

enum MailListUpdateKind {
    Refresh,
    Pagination {
        cursor: ThreadCursor,
        generation: u64,
    },
}

struct MailMetadataUpdate {
    scope: String,
    result: Result<mail::MailMetadata, String>,
}

struct FolderMutationUpdate {
    message: UiMessage,
    metadata: Option<mail::MailMetadata>,
}

struct MessageLoadUpdate {
    generation: u64,
    id: i32,
    result: Result<MailMessage, String>,
}

struct ContactLoadUpdate {
    scope: String,
    query: String,
    cursor: Option<ContactRecordCursor>,
    generation: u64,
    result: Result<ContactRecordPage, String>,
}

fn spawn_contact_page(
    runtime: &tokio::runtime::Runtime,
    core: CoreMailSource,
    updates: UiSender<ContactLoadUpdate>,
    scope: String,
    query: String,
    cursor: Option<ContactRecordCursor>,
    generation: u64,
) {
    runtime.spawn(async move {
        let result = core
            .load_contact_page(
                scope.clone(),
                query.clone(),
                cursor.clone(),
                PAGE_SIZE as i64,
            )
            .await;
        let _ = updates
            .send(ContactLoadUpdate {
                scope,
                query,
                cursor,
                generation,
                result,
            })
            .await;
    });
}

struct UiTaskUpdate {
    message: UiMessage,
    accounts: Option<(Vec<Account>, Vec<AccountConfig>)>,
    calendar_connections: Option<Vec<CalendarConnection>>,
    calendar_error: Option<(i64, Option<String>)>,
    clear_account_form: bool,
    finishes_oauth: bool,
    close_to_tray: Option<bool>,
}

struct InboxState {
    core: Option<CoreMailSource>,
    email_renderer: Rc<RefCell<GpuEmailRenderer>>,
    use_wgpu: bool,
    using_core: bool,
    messages: Vec<MailMessage>,
    labels: Vec<Label>,
    email_rows: Rc<VecModel<EmailRow>>,
    mailboxes: Vec<MailboxEntry>,
    unified_mailboxes: Vec<MailboxEntry>,
    collapsed_folder_ids: HashSet<i64>,
    scope: String,
    query: String,
    search_filter: String,
    total_count: usize,
    inbox_count: usize,
    page: usize,
    next_cursor: Option<ThreadCursor>,
    selected_id: Option<i32>,
    checked_ids: HashSet<i32>,
    rendered_id: Option<i32>,
    preview_closed: bool,
    favicon_loader: Option<FaviconLoader>,
    favicon_icons: HashMap<String, FaviconImages>,
    favicon_pending: HashSet<String>,
    favicon_missing: HashSet<String>,
    favicon_pixel_sides: (u32, u32),
    favicon_tx: UiSender<FaviconUpdate>,
    connected_accounts: Vec<Account>,
    account_configs: Vec<AccountConfig>,
    calendar_connections: Vec<CalendarConnection>,
    calendar_errors: HashMap<i64, String>,
    profile_avatar_loader: Option<ProfileAvatarLoader>,
    profile_avatar_images: HashMap<i64, ProfileAvatarImages>,
    profile_avatar_pending: HashSet<i64>,
    profile_avatar_missing: HashSet<i64>,
    profile_avatar_pixel_sides: (u32, u32),
    profile_avatar_tx: UiSender<ProfileAvatarUpdate>,
    remote_images_enabled: bool,
    remote_images_override_id: Option<i32>,
    mark_read_on_open: bool,
    warm_start_cache: WarmStartCacheWriter,
}

#[derive(Debug)]
struct MailDragPayload {
    items: Vec<MailDragItem>,
}

#[derive(Clone, Copy, Debug)]
struct MailDragItem {
    message_id: i32,
    account_id: i32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum MailDropDestination {
    Action(&'static str),
    Folder(i64),
    Label(i64),
    Route(String),
}

fn mail_drag_payload(data: &DataTransfer) -> Option<Rc<MailDragPayload>> {
    data.user_data()?.downcast::<MailDragPayload>().ok()
}

fn mail_operation_ids(
    messages: &[MailMessage],
    checked_ids: &HashSet<i32>,
    trigger_id: i32,
) -> Vec<i32> {
    if checked_ids.contains(&trigger_id) {
        messages
            .iter()
            .filter(|message| checked_ids.contains(&message.id))
            .map(|message| message.id)
            .collect()
    } else {
        vec![trigger_id]
    }
}

fn standard_mailbox_folder(state: &InboxState, account_id: i64, label: &str) -> Option<i64> {
    state
        .mailboxes
        .iter()
        .find(|mailbox| {
            !mailbox.is_account
                && mailbox.account_id == account_id
                && mailbox.is_standard
                && mailbox.label == label
                && mailbox.folder_id >= 0
        })
        .map(|mailbox| mailbox.folder_id)
}

fn resolve_single_mail_drop(
    state: &InboxState,
    item: &MailDragItem,
    target_scope: &str,
    target_account_id: i32,
    target_folder_id: i32,
) -> Result<(i64, MailDropDestination), String> {
    if !state.using_core || state.core.is_none() {
        return Err("mail account is not ready".to_owned());
    }
    let message = state
        .messages
        .iter()
        .find(|message| message.id == item.message_id)
        .ok_or_else(|| "message is no longer available".to_owned())?;
    let thread_id = message
        .thread_id
        .ok_or_else(|| "message thread is unavailable".to_owned())?;
    if message.account_id != i64::from(item.account_id) {
        return Err("dragged message account is stale".to_owned());
    }
    if state.scope == target_scope {
        return Err("message is already in this destination".to_owned());
    }

    let destination = match target_scope {
        "Important" => MailDropDestination::Route("important".to_owned()),
        "Other" => MailDropDestination::Route("other".to_owned()),
        scope if scope.starts_with("Label:") => {
            let label_id = scope
                .strip_prefix("Label:")
                .and_then(|id| id.parse::<i64>().ok())
                .ok_or_else(|| "label destination is invalid".to_owned())?;
            let label = state
                .labels
                .iter()
                .find(|label| label.id == label_id)
                .ok_or_else(|| "label destination is no longer available".to_owned())?;
            if message.labels.contains(&label_id) {
                return Err("message already has this label".to_owned());
            }
            if label.is_auto {
                MailDropDestination::Route(format!("label:{label_id}"))
            } else {
                MailDropDestination::Label(label_id)
            }
        }
        "Unified Starred" if !message.starred => MailDropDestination::Action("star"),
        "Unified Inbox" if message.folder == "Spam" => MailDropDestination::Action("not_spam"),
        "Unified Inbox" if message.folder == "Archive" => MailDropDestination::Action("unarchive"),
        "Unified Inbox" if message.folder != "Inbox" => MailDropDestination::Folder(
            standard_mailbox_folder(state, message.account_id, "Inbox")
                .ok_or_else(|| "this account has no inbox destination".to_owned())?,
        ),
        "Unified Archive" if message.folder != "Archive" => MailDropDestination::Folder(
            standard_mailbox_folder(state, message.account_id, "Archive")
                .ok_or_else(|| "this account has no archive destination".to_owned())?,
        ),
        "Unified Spam" if message.folder != "Spam" => {
            standard_mailbox_folder(state, message.account_id, "Spam")
                .ok_or_else(|| "this account has no spam destination".to_owned())?;
            MailDropDestination::Action("spam")
        }
        "Unified Trash" if message.folder != "Trash" => {
            standard_mailbox_folder(state, message.account_id, "Trash")
                .ok_or_else(|| "this account has no trash destination".to_owned())?;
            MailDropDestination::Action("trash")
        }
        scope if scope.starts_with("Unified ") => {
            return Err("this unified mailbox is not a drop destination".to_owned());
        }
        _ => {
            if i64::from(target_account_id) != message.account_id {
                return Err("messages cannot be moved between accounts".to_owned());
            }
            let target = state
                .mailboxes
                .iter()
                .find(|mailbox| {
                    !mailbox.is_account
                        && mailbox.account_id == message.account_id
                        && mailbox.scope == target_scope
                        && mailbox.folder_id == i64::from(target_folder_id)
                })
                .ok_or_else(|| "mailbox destination is no longer available".to_owned())?;
            if target.label == message.folder {
                return Err("message is already in this destination".to_owned());
            }
            match target.label.as_str() {
                "Starred" if !message.starred => MailDropDestination::Action("star"),
                "Inbox" if message.folder == "Spam" => MailDropDestination::Action("not_spam"),
                "Inbox" if message.folder == "Archive" => MailDropDestination::Action("unarchive"),
                "Archive" => MailDropDestination::Folder(target.folder_id),
                "Spam" => MailDropDestination::Action("spam"),
                "Trash" => MailDropDestination::Action("trash"),
                "Sent" | "Drafts" | "Starred" => {
                    return Err("this mailbox is not a drop destination".to_owned());
                }
                _ if target.folder_id >= 0 => MailDropDestination::Folder(target.folder_id),
                _ => return Err("mailbox destination is unavailable".to_owned()),
            }
        }
    };

    Ok((thread_id, destination))
}

fn resolve_mail_drop(
    state: &InboxState,
    payload: &MailDragPayload,
    target_scope: &str,
    target_account_id: i32,
    target_folder_id: i32,
) -> Result<Vec<(i32, i64, MailDropDestination)>, String> {
    if payload.items.is_empty() {
        return Err("no messages were dragged".to_owned());
    }
    payload
        .items
        .iter()
        .map(|item| {
            resolve_single_mail_drop(
                state,
                item,
                target_scope,
                target_account_id,
                target_folder_id,
            )
            .map(|(thread_id, destination)| (item.message_id, thread_id, destination))
        })
        .collect()
}

fn perform_mail_drop(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    payload: &MailDragPayload,
    target_scope: &str,
    target_account_id: i32,
    target_folder_id: i32,
) -> Result<(), String> {
    let (core, operations) = {
        let state = state.borrow();
        let operations = resolve_mail_drop(
            &state,
            payload,
            target_scope,
            target_account_id,
            target_folder_id,
        )?;
        (
            state
                .core
                .clone()
                .ok_or_else(|| "mail core is unavailable".to_owned())?,
            operations,
        )
    };

    let mut completed_ids = Vec::new();
    let mut moved_ids = Vec::new();
    let mut first_error = None;
    for (message_id, thread_id, destination) in operations {
        let result = match &destination {
            MailDropDestination::Action(action) => {
                runtime.block_on(core.perform_message_action(thread_id, action))
            }
            MailDropDestination::Folder(folder_id) => {
                runtime.block_on(core.move_thread_to_folder(thread_id, *folder_id))
            }
            MailDropDestination::Label(label_id) => {
                runtime.block_on(core.perform_label_action(thread_id, *label_id, true))
            }
            MailDropDestination::Route(target) => {
                runtime.block_on(core.route_thread_to_tab(thread_id, target.clone()))
            }
        };
        match result {
            Ok(()) => {
                completed_ids.push(message_id);
                if matches!(
                    destination,
                    MailDropDestination::Action(
                        "archive" | "spam" | "trash" | "not_spam" | "unarchive"
                    ) | MailDropDestination::Folder(_)
                ) {
                    moved_ids.push(message_id);
                }
            }
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }

    {
        let mut state = state.borrow_mut();
        for id in &completed_ids {
            state.checked_ids.remove(id);
        }
        if state.selected_id.is_some_and(|id| moved_ids.contains(&id)) {
            state.selected_id = None;
        }
    }
    let refresh_result = refresh_from_source(app, state, runtime, true, &moved_ids);
    if let Some(error) = first_error {
        return Err(error);
    }
    refresh_result
}

fn perform_mail_list_action(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    trigger_id: i32,
    action: &str,
) -> Result<(), String> {
    let (core, operations) = {
        let state = state.borrow();
        if !state.using_core {
            return Err("mail account is not ready".to_owned());
        }
        let ids = mail_operation_ids(&state.messages, &state.checked_ids, trigger_id);
        let operations = ids
            .into_iter()
            .map(|id| {
                let thread_id = state
                    .messages
                    .iter()
                    .find(|message| message.id == id)
                    .ok_or_else(|| "message is no longer available".to_owned())?
                    .thread_id
                    .ok_or_else(|| "message thread is unavailable".to_owned())?;
                Ok((id, thread_id))
            })
            .collect::<Result<Vec<_>, String>>()?;
        (
            state
                .core
                .clone()
                .ok_or_else(|| "mail core is unavailable".to_owned())?,
            operations,
        )
    };

    let mut completed_ids = Vec::new();
    let mut first_error = None;
    for (id, thread_id) in operations {
        match runtime.block_on(core.perform_message_action(thread_id, action)) {
            Ok(()) => completed_ids.push(id),
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
    }

    {
        let mut state = state.borrow_mut();
        for id in &completed_ids {
            state.checked_ids.remove(id);
        }
        if completed_ids.len() == 1 {
            state.selected_id = completed_ids.first().copied();
        }
    }
    let had_completed = !completed_ids.is_empty();
    let moved_ids = if matches!(action, "archive" | "spam" | "trash") {
        completed_ids
    } else {
        Vec::new()
    };
    let refresh_result = if !had_completed && first_error.is_some() {
        refresh_rows_only(app, state, runtime);
        Ok(())
    } else {
        refresh_from_source(app, state, runtime, true, &moved_ids)
    };
    if let Some(error) = first_error {
        return Err(error);
    }
    refresh_result
}

#[cfg(any(test, not(any(target_os = "android", target_os = "ios"))))]
fn parse_preview_window_size(value: &str) -> Result<slint::LogicalSize, String> {
    let normalized = value.trim().to_ascii_lowercase();
    let (width, height) = normalized
        .split_once('x')
        .ok_or_else(|| "expected WIDTHxHEIGHT, for example 390x844".to_owned())?;
    let width = width
        .trim()
        .parse::<u32>()
        .map_err(|_| "preview width must be a positive integer".to_owned())?;
    let height = height
        .trim()
        .parse::<u32>()
        .map_err(|_| "preview height must be a positive integer".to_owned())?;
    if !(320..=4096).contains(&width) || !(320..=4096).contains(&height) {
        return Err("preview dimensions must each be between 320 and 4096 logical pixels".into());
    }
    Ok(slint::LogicalSize::new(width as f32, height as f32))
}

fn apply_language(app: &AppWindow, preference: &str) {
    let system_locale = sys_locale::get_locale();
    let selected = match preference {
        "es" => "es",
        "en" => "en",
        _ => system_locale
            .as_deref()
            .and_then(|locale| locale.split(['-', '_', '@']).next())
            .filter(|language| *language == "es")
            .unwrap_or("en"),
    };
    if let Err(error) = slint::select_bundled_translation(selected) {
        eprintln!("could not select {selected} translation: {error}");
    }
    app.set_language_mode(
        match preference {
            "es" => "es",
            "en" => "en",
            _ => "system",
        }
        .into(),
    );
}

impl InboxState {
    fn empty(
        favicon_loader: Option<FaviconLoader>,
        favicon_tx: UiSender<FaviconUpdate>,
        profile_avatar_loader: Option<ProfileAvatarLoader>,
        profile_avatar_tx: UiSender<ProfileAvatarUpdate>,
        use_wgpu: bool,
        remote_images_enabled: bool,
        warm_start_cache: WarmStartCacheWriter,
    ) -> Self {
        Self {
            core: None,
            email_renderer: Rc::new(RefCell::new(GpuEmailRenderer::default())),
            use_wgpu,
            using_core: false,
            mailboxes: Vec::new(),
            unified_mailboxes: Vec::new(),
            collapsed_folder_ids: HashSet::new(),
            total_count: 0,
            inbox_count: 0,
            messages: Vec::new(),
            labels: Vec::new(),
            email_rows: Rc::new(VecModel::default()),
            scope: "Unified Inbox".to_owned(),
            query: String::new(),
            search_filter: "All mail".to_owned(),
            page: 1,
            next_cursor: None,
            selected_id: None,
            checked_ids: HashSet::new(),
            rendered_id: None,
            preview_closed: false,
            favicon_loader,
            favicon_icons: HashMap::new(),
            favicon_pending: HashSet::new(),
            favicon_missing: HashSet::new(),
            favicon_pixel_sides: (0, 0),
            favicon_tx,
            connected_accounts: Vec::new(),
            account_configs: Vec::new(),
            calendar_connections: Vec::new(),
            calendar_errors: HashMap::new(),
            profile_avatar_loader,
            profile_avatar_images: HashMap::new(),
            profile_avatar_pending: HashSet::new(),
            profile_avatar_missing: HashSet::new(),
            profile_avatar_pixel_sides: (0, 0),
            profile_avatar_tx,
            remote_images_enabled,
            remote_images_override_id: None,
            mark_read_on_open: true,
            warm_start_cache,
        }
    }

    fn queue_warm_start_update(&self) {
        // A warm projection loaded before Core exists is not authoritative and
        // must never overwrite a newer cache if startup subsequently fails.
        if self.core.is_none() {
            return;
        }
        if self.connected_accounts.is_empty() {
            self.warm_start_cache.clear();
            return;
        }
        // Search results and temporary filters are intentionally session-only;
        // keep the last ordinary mailbox page as the repeat-launch surface.
        if !self.query.trim().is_empty() || self.search_filter != "All mail" {
            return;
        }
        self.warm_start_cache
            .save(WarmStartSnapshot::capture(WarmStartProjection {
                scope: &self.scope,
                total_count: self.total_count,
                inbox_count: self.inbox_count,
                next_cursor: self.next_cursor,
                accounts: &self.connected_accounts,
                messages: &self.messages,
                mailboxes: &self.mailboxes,
                unified_mailboxes: &self.unified_mailboxes,
            }));
    }
}

#[derive(Clone)]
pub struct PlatformContext {
    pub paths: Paths,
    pub credentials: flectar_mail_core::accounts::credentials::CredentialStoreHandle,
    pub oauth_redirects: flectar_mail_core::oauth::redirect::OAuthRedirectBrokerHandle,
}

impl PlatformContext {
    pub fn desktop() -> Result<Self, flectar_mail_core::error::CoreError> {
        Ok(Self {
            paths: Paths::default_dirs()?,
            credentials: Arc::new(flectar_mail_core::accounts::credentials::SystemCredentialStore),
            oauth_redirects: Arc::new(flectar_mail_core::oauth::redirect::LoopbackRedirectBroker),
        })
    }

    pub fn app_private(
        data_root: PathBuf,
        cache_root: PathBuf,
        credentials: flectar_mail_core::accounts::credentials::CredentialStoreHandle,
        oauth_redirects: flectar_mail_core::oauth::redirect::OAuthRedirectBrokerHandle,
    ) -> Self {
        Self {
            paths: Paths::new(data_root.join("data"), cache_root.join("cache")),
            credentials,
            oauth_redirects,
        }
    }
}

fn startup_diagnostic_id(error: &str) -> String {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(error.as_bytes());
    format!(
        "START-{:02X}{:02X}{:02X}{:02X}",
        digest[0], digest[1], digest[2], digest[3]
    )
}

fn startup_diagnostics(paths: &Paths, diagnostic_id: &str, error: &str) -> String {
    format!(
        "Flectar Mail {}\nplatform={}\nabi={}\ndiagnostic_id={}\nmail_db={}\ncalendar_db={}\nmail_db_exists={}\ncalendar_db_exists={}\nstartup_error={}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        diagnostic_id,
        paths.db_file().display(),
        paths.calendar_db_file().display(),
        paths.db_file().is_file(),
        paths.calendar_db_file().is_file(),
        error,
    )
}

fn spawn_startup_load(
    runtime: &tokio::runtime::Runtime,
    paths: Paths,
    credentials: flectar_mail_core::accounts::credentials::CredentialStoreHandle,
    oauth_redirects: flectar_mail_core::oauth::redirect::OAuthRedirectBrokerHandle,
    startup_tx: UiSender<StartupUpdate>,
    metrics: StartupMetrics,
) {
    runtime.spawn(async move {
        let warm = load_warm_start_snapshot(&paths.warm_start_file()).await;
        let preferred_scope = warm
            .as_ref()
            .map(|snapshot| snapshot.scope.clone())
            .unwrap_or_else(|| "Unified Inbox".to_owned());
        metrics.emit(
            "warm_cache_loaded",
            serde_json::json!({
                "hit": warm.is_some(),
            }),
        );
        if let Some(warm) = warm {
            let (applied_tx, applied_rx) = tokio::sync::oneshot::channel();
            if startup_tx
                .send(StartupUpdate::Warm {
                    snapshot: Box::new(warm),
                    applied: applied_tx,
                })
                .await
                .is_err()
            {
                return;
            }
            // Give the saved rows one event-loop turn to paint before database
            // initialization can replace them. The timeout preserves startup
            // progress if a platform suspends or tears down the window.
            let _ = tokio::time::timeout(Duration::from_millis(250), applied_rx).await;
        }

        let result = load_startup_snapshot(
            paths,
            credentials,
            oauth_redirects,
            &preferred_scope,
            &metrics,
        )
        .await;
        metrics.emit(
            "local_startup_loaded",
            serde_json::json!({
                "success": result.is_ok(),
            }),
        );
        let follow_up = result.as_ref().ok().map(|snapshot| {
            (
                snapshot.core.clone(),
                snapshot.accounts.clone(),
                snapshot.scope.clone(),
            )
        });
        let _ = startup_tx
            .send(StartupUpdate::Ready(result.map(Box::new)))
            .await;
        let Some((core, accounts, scope)) = follow_up else {
            return;
        };

        let metadata_tx = startup_tx.clone();
        let metadata_core = core.clone();
        tokio::spawn(async move {
            let result = load_startup_mail_metadata(&metadata_core, &scope).await;
            let _ = metadata_tx.send(StartupUpdate::MailMetadata(result)).await;
        });

        let calendar = load_startup_calendar_snapshot(&core, &accounts).await;
        let _ = startup_tx.send(StartupUpdate::Calendar(calendar)).await;
    });
}

pub fn run(platform: PlatformContext) -> Result<(), Box<dyn std::error::Error>> {
    let startup_metrics = StartupMetrics::from_environment();
    let benchmark_disable_background =
        std::env::var("FLECTAR_BENCHMARK_DISABLE_SYNC").as_deref() == Ok("1");
    normalize_appimage_environment();

    #[cfg(all(
        feature = "gpu-renderer",
        not(any(target_os = "android", target_os = "ios"))
    ))]
    let use_wgpu = {
        // Keep Slint and the Blitz email surface on the same WGPU 29 device. The
        // email renderer imports its render target as a Slint image, so selecting
        // the software backend here would put the CPU bitmap bridge back in the
        // hot path.
        let mut wgpu_settings = slint::wgpu_29::WGPUSettings::default();
        // Slint's automatic configuration intentionally starts with WebGL2
        // limits. Vello is a compute renderer and needs native storage buffers;
        // without these limits its first bind-group layout is rejected with
        // `max_storage_buffers_per_shader_stage = 0`.
        wgpu_settings.device_required_limits = slint::wgpu_29::wgpu::Limits::default();
        let wgpu_configuration = slint::wgpu_29::WGPUConfiguration::Automatic(wgpu_settings);

        match slint::BackendSelector::new()
            .require_wgpu_29(wgpu_configuration)
            .select()
        {
            Ok(()) => true,
            Err(error) => {
                eprintln!("WGPU unavailable; using the software email fallback: {error}");
                // The software renderer is compiled as a compatibility path for
                // machines without a GPU adapter (remote desktops and CI). The
                // normal path remains the shared WGPU surface above.
                slint::BackendSelector::new()
                    .backend_name("winit".to_owned())
                    .renderer_name("software".to_owned())
                    .select()?;
                false
            }
        }
    };

    #[cfg(all(
        not(feature = "gpu-renderer"),
        not(any(target_os = "android", target_os = "ios"))
    ))]
    let use_wgpu = {
        slint::BackendSelector::new()
            .backend_name("winit".to_owned())
            .renderer_name("software".to_owned())
            .select()?;
        false
    };

    // Android installs its backend in android_main before entering this shared
    // function. iOS uses Slint's documented winit + Skia combination.
    #[cfg(target_os = "android")]
    let use_wgpu = false;

    #[cfg(target_os = "ios")]
    let use_wgpu = {
        slint::BackendSelector::new()
            .backend_name("winit".to_owned())
            .renderer_name("skia".to_owned())
            .select()?;
        false
    };

    // Match resources/com.flectar.mail.desktop so Wayland compositors and XDG
    // window managers can associate the native window with the installed icon.
    #[cfg(all(
        unix,
        not(any(target_os = "android", target_os = "ios", target_os = "macos"))
    ))]
    slint::set_xdg_app_id("com.flectar.mail")?;

    let runtime = Rc::new(
        tokio::runtime::Builder::new_multi_thread()
            // Mail sync is I/O-bound. Avoid reserving one Tokio worker per
            // logical CPU in a desktop shell while leaving room for the UI,
            // Blitz renderer, and the core's dedicated DB threads.
            .worker_threads(2)
            .max_blocking_threads(2)
            .enable_all()
            .build()?,
    );

    // Register the deterministic emoji face before any initial text is measured.
    configure_emoji_font_fallback()?;

    // Construct and map the window before opening or migrating either database.
    // Until startup completes it paints the inert mailbox shell; mapping now
    // avoids making callback wiring part of first-window latency.
    let app = AppWindow::new()?;
    theme::register_theme_utilities(&app);
    app.set_print_supported(!cfg!(any(target_os = "android", target_os = "ios")));
    app.set_document_apis_supported(!cfg!(any(target_os = "android", target_os = "ios")));
    app.show()?;
    startup_metrics.emit(
        "window_shown",
        serde_json::json!({
            "renderer": if use_wgpu { "wgpu" } else { "software" },
            "slint_backend": std::env::var("SLINT_BACKEND").ok(),
            "display": std::env::var("DISPLAY").ok(),
            "wayland_display": std::env::var("WAYLAND_DISPLAY").ok(),
        }),
    );
    startup_metrics.schedule_rendered_frame(app.as_weak(), "first_frame");
    let favicon_wake = UiWake::new(app.as_weak(), |app| app.invoke_drain_favicon_updates());
    let profile_avatar_wake = UiWake::new(app.as_weak(), |app| {
        app.invoke_drain_profile_avatar_updates()
    });
    let resource_wake = UiWake::new(app.as_weak(), |app| app.invoke_refresh_email_resources());
    let (favicon_raw_tx, favicon_rx) = bounded_ui_channel();
    let favicon_tx = UiSender::new(favicon_raw_tx, favicon_wake);
    let (profile_avatar_raw_tx, profile_avatar_rx) = bounded_ui_channel();
    let profile_avatar_tx = UiSender::new(profile_avatar_raw_tx, profile_avatar_wake);

    let profile_avatar_loader = if benchmark_disable_background {
        None
    } else {
        match ProfileAvatarLoader::new() {
            Ok(loader) => Some(loader),
            Err(error) => {
                eprintln!("account avatars unavailable: {error}");
                None
            }
        }
    };
    let warm_start_cache = WarmStartCacheWriter::spawn(&runtime, platform.paths.warm_start_file());
    let initial_state = InboxState::empty(
        None,
        favicon_tx,
        profile_avatar_loader,
        profile_avatar_tx,
        use_wgpu,
        false,
        warm_start_cache,
    );
    initial_state
        .email_renderer
        .borrow_mut()
        .set_resource_notifier(Arc::new(move || resource_wake.wake()));
    if let Err(error) = initial_state
        .email_renderer
        .borrow_mut()
        .configure_resources(runtime.handle().clone(), false)
    {
        eprintln!("email images unavailable: {error}");
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    if let Some(value) = std::env::var_os("FLECTAR_PREVIEW_SIZE") {
        match value.to_str().map(parse_preview_window_size) {
            Some(Ok(size)) => app.window().set_size(size),
            Some(Err(error)) => eprintln!("ignoring FLECTAR_PREVIEW_SIZE: {error}"),
            None => eprintln!("ignoring FLECTAR_PREVIEW_SIZE: value is not valid UTF-8"),
        }
    }
    app.set_compose_from_label(app.global::<I18n>().invoke_no_connected_account());
    app.set_remote_images_available(cfg!(feature = "remote-content"));
    app.set_remote_images_enabled(false);
    app.set_google_oauth_bundled(
        flectar_mail_core::oauth::providers::has_bundled_credentials(Provider::Gmail),
    );
    app.set_microsoft_oauth_bundled(
        flectar_mail_core::oauth::providers::has_bundled_credentials(Provider::Microsoft),
    );
    let tray = create_and_register_window_lifecycle(&app)?;
    let email_renderer = Rc::clone(&initial_state.email_renderer);

    #[cfg(feature = "gpu-renderer")]
    if use_wgpu {
        let email_renderer_for_notifier = Rc::clone(&email_renderer);
        let app_weak_for_notifier = app.as_weak();
        app.window()
            .set_rendering_notifier(move |state, graphics_api| {
                if matches!(&state, slint::RenderingState::RenderingTeardown) {
                    email_renderer_for_notifier.borrow_mut().teardown_gpu();
                    return;
                }

                let slint::RenderingState::BeforeRendering = &state else {
                    return;
                };
                let slint::GraphicsAPI::WGPU29 { device, queue, .. } = graphics_api else {
                    return;
                };
                let Some(app) = app_weak_for_notifier.upgrade() else {
                    return;
                };

                let logical_width = app.get_email_viewport_width();
                let logical_height = app.get_email_viewport_height();
                let scale_factor = app.window().scale_factor();
                let result = email_renderer_for_notifier.borrow_mut().render_if_needed(
                    device,
                    queue,
                    logical_width,
                    logical_height,
                    scale_factor,
                );
                match result {
                    Ok(Some(frame)) => {
                        apply_gpu_frame(&app, frame);
                        sync_reader_metadata(&app, &email_renderer_for_notifier);
                    }
                    Ok(None) => {}
                    Err(gpu_error) => {
                        let fallback = email_renderer_for_notifier
                            .borrow_mut()
                            .render_cpu_if_needed(
                                logical_width.max(1.0).ceil() as u32,
                                logical_height.max(1.0).ceil() as u32,
                                scale_factor,
                            );
                        match fallback {
                            Ok(Some(frame)) => {
                                apply_cpu_frame(&app, frame);
                                sync_reader_metadata(&app, &email_renderer_for_notifier);
                                app.set_render_status(UiMessage::detail(
                                    "Rendered with software fallback after GPU error: {}",
                                    gpu_error,
                                ));
                            }
                            Ok(None) => {
                                sync_reader_metadata(&app, &email_renderer_for_notifier);
                            }
                            Err(cpu_error) => {
                                app.global::<EmailReader>()
                                    .set_notice(cpu_error.clone().into());
                                clear_reader_projection(&app);
                                app.set_text_mode(true);
                                app.set_render_status(UiMessage::arguments(
                                    "Blitz render failed (GPU: {}; software: {})",
                                    gpu_error,
                                    cpu_error,
                                ));
                            }
                        }
                    }
                }
            })?;
    }

    app.set_emails(Rc::clone(&initial_state.email_rows).into());
    let state = Rc::new(RefCell::new(initial_state));

    let state_for_mail_drag = Rc::clone(&state);
    app.global::<MailDragApi>()
        .on_make_transfer(move |message_id, account_id| {
            let items = {
                let state = state_for_mail_drag.borrow();
                mail_operation_ids(&state.messages, &state.checked_ids, message_id)
                    .into_iter()
                    .map(|id| {
                        state
                            .messages
                            .iter()
                            .find(|message| message.id == id)
                            .map(|message| MailDragItem {
                                message_id: message.id,
                                account_id: i32::try_from(message.account_id).unwrap_or(-1),
                            })
                            .unwrap_or(MailDragItem {
                                message_id: id,
                                account_id,
                            })
                    })
                    .collect()
            };
            let mut transfer = DataTransfer::default();
            transfer.set_user_data(Rc::new(MailDragPayload { items }));
            transfer
        });
    let state_for_mail_drop_check = Rc::clone(&state);
    app.global::<MailDragApi>().on_can_drop_on_mailbox(
        move |data, target_scope, target_account_id, target_folder_id| {
            let Some(payload) = mail_drag_payload(&data) else {
                return false;
            };
            resolve_mail_drop(
                &state_for_mail_drop_check.borrow(),
                &payload,
                target_scope.as_str(),
                target_account_id,
                target_folder_id,
            )
            .is_ok()
        },
    );
    let app_for_mail_drop = app.as_weak();
    let state_for_mail_drop = Rc::clone(&state);
    let runtime_for_mail_drop = Rc::clone(&runtime);
    app.global::<MailDragApi>().on_drop_on_mailbox(
        move |data, target_scope, target_account_id, target_folder_id| {
            let Some(app) = app_for_mail_drop.upgrade() else {
                return false;
            };
            let Some(payload) = mail_drag_payload(&data) else {
                app.set_render_status(UiMessage::detail(
                    "Message action failed: {}",
                    "invalid drag payload",
                ));
                return false;
            };
            match perform_mail_drop(
                &app,
                &state_for_mail_drop,
                &runtime_for_mail_drop,
                &payload,
                target_scope.as_str(),
                target_account_id,
                target_folder_id,
            ) {
                Ok(()) => {
                    app.set_render_status(UiMessage::plain("Message action completed."));
                    true
                }
                Err(error) => {
                    app.set_render_status(UiMessage::detail("Message action failed: {}", error));
                    false
                }
            }
        },
    );

    let contact_state = Rc::new(RefCell::new(ContactDirectoryState::new(Vec::new(), false)));
    app.set_contacts(Rc::clone(&contact_state.borrow().rows).into());
    let contacts_loaded = Rc::new(Cell::new(false));
    let contacts_loading = Rc::new(Cell::new(false));
    apply_contact_directory(&app, &contact_state);

    // Contact pages use the same bounded worker-to-UI handoff as mail. The
    // generation discards a stale search/scope result without ever replacing
    // the retained Slint model.
    let contact_load_generation = Rc::new(Cell::new(0_u64));
    let (contact_load_raw_tx, contact_load_rx) = bounded_ui_channel::<ContactLoadUpdate>();
    let contact_load_tx = UiSender::new(
        contact_load_raw_tx,
        UiWake::new(app.as_weak(), |app| app.invoke_drain_contact_load_updates()),
    );
    let contact_load_rx = Rc::new(RefCell::new(contact_load_rx));
    let contact_load_app = app.as_weak();
    let contact_load_state = Rc::clone(&contact_state);
    let contact_load_loaded = Rc::clone(&contacts_loaded);
    let contact_load_loading = Rc::clone(&contacts_loading);
    let contact_load_generation_for_result = Rc::clone(&contact_load_generation);
    app.on_drain_contact_load_updates(move || {
        while let Ok(update) = contact_load_rx.borrow_mut().try_recv() {
            if update.generation != contact_load_generation_for_result.get() {
                continue;
            }
            let Some(app) = contact_load_app.upgrade() else {
                return;
            };
            let is_current = {
                let directory = contact_load_state.borrow();
                directory.scope == update.scope
                    && directory.query == update.query
                    && (update.cursor.is_none()
                        || directory.next_cursor.as_ref() == update.cursor.as_ref())
            };
            if !is_current {
                continue;
            }

            contact_load_loading.set(false);
            app.set_contact_loading_more(false);
            match update.result {
                Ok(page) => {
                    if contact_load_state
                        .borrow_mut()
                        .apply_core_page(update.cursor.as_ref(), page)
                    {
                        contact_load_loaded.set(true);
                        app.set_contact_save_status(UiMessage::EMPTY);
                        apply_contact_directory(&app, &contact_load_state);
                        app.set_contact_list_revision(
                            app.get_contact_list_revision().wrapping_add(1),
                        );
                    }
                }
                Err(error) => {
                    if update.cursor.is_none() {
                        contact_load_loaded.set(false);
                    }
                    app.set_contact_save_status(UiMessage::detail(
                        "Could not load contacts: {}",
                        error,
                    ));
                }
            }
        }
    });

    let app_weak = app.as_weak();
    let contacts_for_select = Rc::clone(&contact_state);
    app.on_select_contact(move |id| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut directory = contacts_for_select.borrow_mut();
        directory.selected_id = Some(i64::from(id));
        directory.editing_new = false;
        drop(directory);
        app.set_contact_save_status(UiMessage::EMPTY);
        apply_contact_directory(&app, &contacts_for_select);
    });

    let app_weak = app.as_weak();
    let contacts_for_search = Rc::clone(&contact_state);
    let inbox_for_contact_search = Rc::clone(&state);
    let runtime_for_contact_search = Rc::clone(&runtime);
    let contact_load_tx_for_search = contact_load_tx.clone();
    let contact_load_generation_for_search = Rc::clone(&contact_load_generation);
    let contacts_loading_for_search = Rc::clone(&contacts_loading);
    app.on_search_contacts(move |query| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let query = query.to_string();
        if !contacts_for_search.borrow().using_core {
            let mut directory = contacts_for_search.borrow_mut();
            directory.query = query;
            directory.page = 1;
            drop(directory);
            apply_contact_directory(&app, &contacts_for_search);
            return;
        }
        let Some(core) = inbox_for_contact_search.borrow().core.clone() else {
            return;
        };
        let (scope, generation) = {
            let mut directory = contacts_for_search.borrow_mut();
            directory.query = query.clone();
            directory.begin_core_query();
            let generation = contact_load_generation_for_search.get().wrapping_add(1);
            contact_load_generation_for_search.set(generation);
            (directory.scope.clone(), generation)
        };
        contacts_loading_for_search.set(true);
        app.set_contact_loading_more(true);
        app.set_contact_save_status(UiMessage::plain("Loading contacts…"));
        apply_contact_directory(&app, &contacts_for_search);
        spawn_contact_page(
            &runtime_for_contact_search,
            core,
            contact_load_tx_for_search.clone(),
            scope,
            query,
            None,
            generation,
        );
    });

    let app_weak = app.as_weak();
    let contacts_for_scope = Rc::clone(&contact_state);
    let inbox_for_contact_scope = Rc::clone(&state);
    let runtime_for_contact_scope = Rc::clone(&runtime);
    let contact_load_tx_for_scope = contact_load_tx.clone();
    let contact_load_generation_for_scope = Rc::clone(&contact_load_generation);
    let contacts_loading_for_scope = Rc::clone(&contacts_loading);
    app.on_select_contact_scope(move |scope| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let scope = match scope.as_str() {
            "Favorites" => "Favorites".to_owned(),
            value if value.starts_with("Account:") => value.to_owned(),
            _ => "All contacts".to_owned(),
        };
        if !contacts_for_scope.borrow().using_core {
            let mut directory = contacts_for_scope.borrow_mut();
            directory.scope = scope;
            directory.page = 1;
            directory.editing_new = false;
            drop(directory);
            apply_contact_directory(&app, &contacts_for_scope);
            return;
        }
        let Some(core) = inbox_for_contact_scope.borrow().core.clone() else {
            return;
        };
        let (query, generation) = {
            let mut directory = contacts_for_scope.borrow_mut();
            directory.scope = scope.clone();
            directory.begin_core_query();
            let generation = contact_load_generation_for_scope.get().wrapping_add(1);
            contact_load_generation_for_scope.set(generation);
            (directory.query.clone(), generation)
        };
        contacts_loading_for_scope.set(true);
        app.set_contact_loading_more(true);
        app.set_contact_save_status(UiMessage::plain("Loading contacts…"));
        apply_contact_directory(&app, &contacts_for_scope);
        spawn_contact_page(
            &runtime_for_contact_scope,
            core,
            contact_load_tx_for_scope.clone(),
            scope,
            query,
            None,
            generation,
        );
    });

    let app_weak = app.as_weak();
    let contacts_for_more = Rc::clone(&contact_state);
    let inbox_for_contact_more = Rc::clone(&state);
    let runtime_for_contact_more = Rc::clone(&runtime);
    let contact_load_tx_for_more = contact_load_tx.clone();
    let contact_load_generation_for_more = Rc::clone(&contact_load_generation);
    let contacts_loading_for_more = Rc::clone(&contacts_loading);
    app.on_load_more_contacts(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        if !contacts_for_more.borrow().using_core {
            let mut directory = contacts_for_more.borrow_mut();
            directory.page = directory.page.saturating_add(1);
            drop(directory);
            apply_contact_rows(&app, &contacts_for_more);
            app.set_contact_list_revision(app.get_contact_list_revision().wrapping_add(1));
            return;
        }
        if contacts_loading_for_more.replace(true) {
            return;
        }
        let Some(core) = inbox_for_contact_more.borrow().core.clone() else {
            contacts_loading_for_more.set(false);
            return;
        };
        let (scope, query, cursor) = {
            let directory = contacts_for_more.borrow();
            (
                directory.scope.clone(),
                directory.query.clone(),
                directory.next_cursor.clone(),
            )
        };
        let Some(cursor) = cursor else {
            contacts_loading_for_more.set(false);
            return;
        };
        app.set_contact_loading_more(true);
        spawn_contact_page(
            &runtime_for_contact_more,
            core,
            contact_load_tx_for_more.clone(),
            scope,
            query,
            Some(cursor),
            contact_load_generation_for_more.get(),
        );
    });

    let app_weak = app.as_weak();
    let contacts_for_new = Rc::clone(&contact_state);
    app.on_new_contact(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut directory = contacts_for_new.borrow_mut();
        directory.selected_id = None;
        directory.editing_new = true;
        drop(directory);
        clear_contact_form(&app);
        app.set_contact_has_selection(true);
        app.set_contact_save_status(UiMessage::plain(
            "Add an email address, then save the contact.",
        ));
        apply_contact_rows(&app, &contacts_for_new);
    });

    let app_weak = app.as_weak();
    let contacts_for_save = Rc::clone(&contact_state);
    let inbox_for_contact_save = Rc::clone(&state);
    let runtime_for_save = Rc::clone(&runtime);
    let contact_load_tx_for_save = contact_load_tx.clone();
    let contact_load_generation_for_save = Rc::clone(&contact_load_generation);
    let contacts_loading_for_save = Rc::clone(&contacts_loading);
    app.on_save_contact(
        move |id,
              name,
              email,
              phone,
              company,
              job_title,
              website,
              birthday,
              address,
              notes,
              tags,
              favorite| {
            let Some(app) = app_weak.upgrade() else {
                return;
            };
            let email = email.trim().to_lowercase();
            if email.is_empty() || !email.contains('@') {
                app.set_contact_save_status(UiMessage::plain(
                    "Enter a valid email address before saving.",
                ));
                return;
            }
            let previous = contacts_for_save
                .borrow()
                .contacts
                .iter()
                .find(|contact| contact.id == i64::from(id))
                .cloned();
            let record = ContactRecord {
                id: i64::from(id),
                name: name.trim().to_owned(),
                email,
                phone: phone.trim().to_owned(),
                company: company.trim().to_owned(),
                job_title: job_title.trim().to_owned(),
                website: website.trim().to_owned(),
                birthday: birthday.trim().to_owned(),
                postal_address: address.trim().to_owned(),
                notes: notes.trim().to_owned(),
                tags: tags.trim().to_owned(),
                is_favorite: favorite,
                interactions: previous.as_ref().map_or(0, |contact| contact.interactions),
                last_interacted: previous
                    .as_ref()
                    .and_then(|contact| contact.last_interacted),
                account_ids: previous
                    .as_ref()
                    .map(|contact| contact.account_ids.clone())
                    .unwrap_or_default(),
                is_managed: true,
            };
            let using_core = contacts_for_save.borrow().using_core;
            let core = if using_core {
                let Some(core) = inbox_for_contact_save.borrow().core.clone() else {
                    app.set_contact_save_status(UiMessage::plain(
                        "Could not save: contact storage is unavailable.",
                    ));
                    return;
                };
                Some(core)
            } else {
                None
            };
            let saved = if let Some(core) = core.as_ref() {
                match runtime_for_save.block_on(core.save_contact(record)) {
                    Ok(saved) => saved,
                    Err(error) => {
                        app.set_contact_save_status(UiMessage::detail(
                            "Could not save contact: {}",
                            error,
                        ));
                        return;
                    }
                }
            } else {
                let mut saved = record;
                if saved.id <= 0 {
                    saved.id = contacts_for_save
                        .borrow()
                        .contacts
                        .iter()
                        .map(|contact| contact.id)
                        .max()
                        .unwrap_or(0)
                        + 1;
                }
                saved
            };
            if let Some(core) = core {
                let (scope, query, generation) = {
                    let mut directory = contacts_for_save.borrow_mut();
                    directory.begin_core_query();
                    directory.selected_id = Some(saved.id);
                    let generation = contact_load_generation_for_save.get().wrapping_add(1);
                    contact_load_generation_for_save.set(generation);
                    (directory.scope.clone(), directory.query.clone(), generation)
                };
                contacts_loading_for_save.set(true);
                app.set_contact_loading_more(true);
                app.set_contact_save_status(UiMessage::plain("Contact saved."));
                apply_contact_directory(&app, &contacts_for_save);
                spawn_contact_page(
                    &runtime_for_save,
                    core,
                    contact_load_tx_for_save.clone(),
                    scope,
                    query,
                    None,
                    generation,
                );
                return;
            }
            let mut directory = contacts_for_save.borrow_mut();
            if let Some(existing) = directory
                .contacts
                .iter_mut()
                .find(|contact| contact.id == saved.id)
            {
                *existing = saved.clone();
            } else {
                directory.contacts.push(saved.clone());
            }
            directory.selected_id = Some(saved.id);
            directory.editing_new = false;
            drop(directory);
            apply_contact_directory(&app, &contacts_for_save);
            app.set_contact_save_status(UiMessage::plain("Contact saved."));
        },
    );

    let app_weak = app.as_weak();
    let contacts_for_delete = Rc::clone(&contact_state);
    let inbox_for_contact_delete = Rc::clone(&state);
    let runtime_for_delete = Rc::clone(&runtime);
    let contact_load_tx_for_delete = contact_load_tx.clone();
    let contact_load_generation_for_delete = Rc::clone(&contact_load_generation);
    let contacts_loading_for_delete = Rc::clone(&contacts_loading);
    app.on_delete_contact(move |id| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let using_core = contacts_for_delete.borrow().using_core;
        if using_core {
            let Some(core) = inbox_for_contact_delete.borrow().core.clone() else {
                app.set_contact_save_status(UiMessage::plain(
                    "Could not delete: contact storage is unavailable.",
                ));
                return;
            };
            if let Err(error) = runtime_for_delete.block_on(core.delete_contact(i64::from(id))) {
                app.set_contact_save_status(UiMessage::detail(
                    "Could not delete contact: {}",
                    error,
                ));
                return;
            }
            let (scope, query, generation) = {
                let mut directory = contacts_for_delete.borrow_mut();
                directory.begin_core_query();
                let generation = contact_load_generation_for_delete.get().wrapping_add(1);
                contact_load_generation_for_delete.set(generation);
                (directory.scope.clone(), directory.query.clone(), generation)
            };
            contacts_loading_for_delete.set(true);
            app.set_contact_loading_more(true);
            app.set_contact_save_status(UiMessage::plain("Contact deleted."));
            apply_contact_directory(&app, &contacts_for_delete);
            spawn_contact_page(
                &runtime_for_delete,
                core,
                contact_load_tx_for_delete.clone(),
                scope,
                query,
                None,
                generation,
            );
            return;
        }
        let mut directory = contacts_for_delete.borrow_mut();
        directory
            .contacts
            .retain(|contact| contact.id != i64::from(id));
        directory.selected_id = None;
        directory.editing_new = false;
        drop(directory);
        apply_contact_directory(&app, &contacts_for_delete);
        app.set_contact_save_status(UiMessage::plain("Contact deleted."));
    });

    // Contacts are not part of the launch-critical snapshot. Loading the
    // directory only when its workspace is opened keeps startup I/O and the
    // retained Slint model proportional to what the user is actually viewing.
    let app_weak = app.as_weak();
    let contacts_for_load = Rc::clone(&contact_state);
    let contacts_loaded_for_load = Rc::clone(&contacts_loaded);
    let contacts_loading_for_load = Rc::clone(&contacts_loading);
    let inbox_for_contact_load = Rc::clone(&state);
    let runtime_for_contact_load = Rc::clone(&runtime);
    let contact_load_tx_for_load = contact_load_tx.clone();
    let contact_load_generation_for_load = Rc::clone(&contact_load_generation);
    app.on_load_contacts(move || {
        if contacts_loaded_for_load.get() || contacts_loading_for_load.replace(true) {
            return;
        }
        let Some(app) = app_weak.upgrade() else {
            contacts_loading_for_load.set(false);
            return;
        };
        let Some(core) = inbox_for_contact_load.borrow().core.clone() else {
            contacts_loading_for_load.set(false);
            app.set_contact_save_status(UiMessage::plain(
                "Local contact storage is still starting…",
            ));
            return;
        };
        let (scope, query, generation) = {
            let mut directory = contacts_for_load.borrow_mut();
            directory.begin_core_query();
            let generation = contact_load_generation_for_load.get().wrapping_add(1);
            contact_load_generation_for_load.set(generation);
            (directory.scope.clone(), directory.query.clone(), generation)
        };
        app.set_contact_loading_more(true);
        app.set_contact_save_status(UiMessage::plain("Loading contacts…"));
        apply_contact_directory(&app, &contacts_for_load);
        spawn_contact_page(
            &runtime_for_contact_load,
            core,
            contact_load_tx_for_load.clone(),
            scope,
            query,
            None,
            generation,
        );
    });

    // The calendar is always backed by the standalone local calendar store.
    // Provider sync enriches the same store but is never required to use it.
    let calendar_today = Local::now().date_naive();
    let calendar_state = Rc::new(RefCell::new(LocalCalendarState::new(calendar_today)));
    let calendar_editing_event_id = Rc::new(Cell::new(None::<i64>));
    apply_calendar(&app, &calendar_state.borrow(), calendar_today);

    let app_weak = app.as_weak();
    let calendar_for_navigation = Rc::clone(&calendar_state);
    let inbox_for_calendar_navigation = Rc::clone(&state);
    let runtime_for_navigation = Rc::clone(&runtime);
    app.on_calendar_navigate(move |scope, direction| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let today = Local::now().date_naive();
        let mut calendar = calendar_for_navigation.borrow_mut();
        if scope.as_str() == "month" {
            calendar.visible_month = shift_month(calendar.visible_month, direction);
        } else if calendar.view_mode == "month" {
            calendar.visible_month = shift_month(calendar.visible_month, direction);
            calendar.selected_date = calendar.visible_month;
        } else {
            calendar.selected_date += ChronoDuration::days(direction as i64 * 7);
            calendar.visible_month = first_of_month(calendar.selected_date);
        }
        if let Some(core) = inbox_for_calendar_navigation.borrow().core.clone()
            && let Err(error) = refresh_calendar_events(
                &core,
                &runtime_for_navigation,
                &mut calendar,
                &inbox_for_calendar_navigation.borrow().connected_accounts,
            )
        {
            app.set_sync_status(UiMessage::detail("Could not load calendar: {}", error));
        }
        apply_calendar(&app, &calendar, today);
    });

    let app_weak = app.as_weak();
    let calendar_for_selection = Rc::clone(&calendar_state);
    let inbox_for_calendar_selection = Rc::clone(&state);
    let runtime_for_selection = Rc::clone(&runtime);
    app.on_calendar_select_date(move |date| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Ok(date) = NaiveDate::parse_from_str(date.as_str(), "%Y-%m-%d") else {
            return;
        };
        let today = Local::now().date_naive();
        let mut calendar = calendar_for_selection.borrow_mut();
        calendar.selected_date = date;
        calendar.visible_month = first_of_month(date);
        if let Some(core) = inbox_for_calendar_selection.borrow().core.clone() {
            let _ = refresh_calendar_events(
                &core,
                &runtime_for_selection,
                &mut calendar,
                &inbox_for_calendar_selection.borrow().connected_accounts,
            );
        }
        apply_calendar(&app, &calendar, today);
    });

    let app_weak = app.as_weak();
    let calendar_for_view = Rc::clone(&calendar_state);
    let inbox_for_calendar_view = Rc::clone(&state);
    let runtime_for_view = Rc::clone(&runtime);
    app.on_calendar_set_view(move |view| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let view = if view.as_str() == "month" {
            "month"
        } else {
            "week"
        };
        let today = Local::now().date_naive();
        let mut calendar = calendar_for_view.borrow_mut();
        calendar.view_mode = view.to_owned();
        calendar.visible_month = first_of_month(calendar.selected_date);
        if let Some(core) = inbox_for_calendar_view.borrow().core.clone() {
            let _ = refresh_calendar_events(
                &core,
                &runtime_for_view,
                &mut calendar,
                &inbox_for_calendar_view.borrow().connected_accounts,
            );
        }
        apply_calendar(&app, &calendar, today);
    });

    let app_weak = app.as_weak();
    let calendar_for_source_toggle = Rc::clone(&calendar_state);
    let inbox_for_source_toggle = Rc::clone(&state);
    let runtime_for_source_toggle = Rc::clone(&runtime);
    app.on_calendar_set_source_enabled(move |calendar_id, enabled| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(core) = inbox_for_source_toggle.borrow().core.clone() else {
            return;
        };
        match runtime_for_source_toggle
            .block_on(core.set_calendar_enabled(i64::from(calendar_id), enabled))
        {
            Ok(()) => {
                app.set_sync_status(if enabled {
                    UiMessage::plain("Calendar shown and syncing.")
                } else {
                    UiMessage::plain("Calendar hidden and sync paused.")
                });
                let today = Local::now().date_naive();
                let mut calendar = calendar_for_source_toggle.borrow_mut();
                let _ = refresh_calendar_events(
                    &core,
                    &runtime_for_source_toggle,
                    &mut calendar,
                    &inbox_for_source_toggle.borrow().connected_accounts,
                );
                apply_calendar(&app, &calendar, today);
            }
            Err(error) => {
                app.set_sync_status(UiMessage::detail("Calendar update failed: {}", error))
            }
        }
    });

    let app_weak = app.as_weak();
    let calendar_for_today = Rc::clone(&calendar_state);
    let inbox_for_calendar_today = Rc::clone(&state);
    let runtime_for_today = Rc::clone(&runtime);
    app.on_calendar_today(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let today = Local::now().date_naive();
        let mut calendar = calendar_for_today.borrow_mut();
        calendar.selected_date = today;
        calendar.visible_month = first_of_month(today);
        if let Some(core) = inbox_for_calendar_today.borrow().core.clone() {
            let _ = refresh_calendar_events(
                &core,
                &runtime_for_today,
                &mut calendar,
                &inbox_for_calendar_today.borrow().connected_accounts,
            );
        }
        apply_calendar(&app, &calendar, today);
    });

    let app_weak = app.as_weak();
    let calendar_for_editor = Rc::clone(&calendar_state);
    let editing_event_for_new = Rc::clone(&calendar_editing_event_id);
    app.on_calendar_open_editor(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let calendar = calendar_for_editor.borrow();
        editing_event_for_new.set(None);
        app.set_calendar_event_editing(false);
        app.set_calendar_event_title("".into());
        app.set_calendar_event_date(calendar.selected_date.format("%Y-%m-%d").to_string().into());
        app.set_calendar_event_time("09:00".into());
        app.set_calendar_event_duration("60".into());
        app.set_calendar_event_color(0);
        app.set_calendar_editor_open(true);
    });

    let app_weak = app.as_weak();
    let calendar_for_edit = Rc::clone(&calendar_state);
    let editing_event_for_edit = Rc::clone(&calendar_editing_event_id);
    app.on_calendar_edit_event(move |event_id| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let event = calendar_for_edit
            .borrow()
            .events
            .iter()
            .find(|event| event.id == event_id && event.is_local)
            .cloned();
        let Some(event) = event else {
            app.set_sync_status(UiMessage::plain(
                "Only events created in Flectar Mail can be edited.",
            ));
            return;
        };
        editing_event_for_edit.set(Some(i64::from(event.id)));
        app.set_calendar_event_editing(true);
        app.set_calendar_event_title(event.title.into());
        app.set_calendar_event_date(event.date.format("%Y-%m-%d").to_string().into());
        app.set_calendar_event_time(
            format!(
                "{:02}:{:02}",
                event.start_minutes / 60,
                event.start_minutes % 60
            )
            .into(),
        );
        app.set_calendar_event_duration(event.duration_minutes.to_string().into());
        app.set_calendar_event_color(event.color_index);
        app.set_calendar_editor_open(true);
    });

    let app_weak = app.as_weak();
    let calendar_for_delete = Rc::clone(&calendar_state);
    let runtime_for_calendar_delete = Rc::clone(&runtime);
    let inbox_for_calendar_delete = Rc::clone(&state);
    let editing_event_for_delete = Rc::clone(&calendar_editing_event_id);
    app.on_calendar_delete_event(move |event_id| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let is_local = calendar_for_delete
            .borrow()
            .events
            .iter()
            .any(|event| event.id == event_id && event.is_local);
        if !is_local {
            app.set_sync_status(UiMessage::plain(
                "Only events created in Flectar Mail can be deleted.",
            ));
            return;
        }
        let Some(core) = inbox_for_calendar_delete.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain("Calendar storage is unavailable."));
            return;
        };
        if let Err(error) =
            runtime_for_calendar_delete.block_on(core.delete_event(i64::from(event_id)))
        {
            app.set_sync_status(UiMessage::detail("Could not delete event: {}", error));
            return;
        }
        if editing_event_for_delete.get() == Some(i64::from(event_id)) {
            editing_event_for_delete.set(None);
            app.set_calendar_editor_open(false);
            app.set_calendar_event_editing(false);
        }
        let today = Local::now().date_naive();
        let mut calendar = calendar_for_delete.borrow_mut();
        if let Err(error) = refresh_calendar_events(
            &core,
            &runtime_for_calendar_delete,
            &mut calendar,
            &inbox_for_calendar_delete.borrow().connected_accounts,
        ) {
            app.set_sync_status(UiMessage::detail(
                "Event deleted, but refresh failed: {}",
                error,
            ));
        } else {
            app.set_sync_status(UiMessage::plain("Event deleted."));
        }
        apply_calendar(&app, &calendar, today);
    });

    let app_weak = app.as_weak();
    let calendar_for_save = Rc::clone(&calendar_state);
    let runtime_for_calendar_save = Rc::clone(&runtime);
    let inbox_for_calendar_save = Rc::clone(&state);
    let editing_event_for_save = Rc::clone(&calendar_editing_event_id);
    app.on_calendar_save_event(move |title, date, time, duration, _color| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let parsed = NaiveDate::parse_from_str(date.trim(), "%Y-%m-%d").and_then(|date| {
            NaiveTime::parse_from_str(time.trim(), "%H:%M").map(|time| (date, time))
        });
        let Ok((date, time)) = parsed else {
            app.set_sync_status(UiMessage::plain(
                "Use YYYY-MM-DD and a 24-hour time such as 09:30.",
            ));
            return;
        };
        let Ok(duration) = duration.trim().parse::<i32>() else {
            app.set_sync_status(UiMessage::plain(
                "Event duration must be a number of minutes.",
            ));
            return;
        };
        if title.trim().is_empty() || !(15..=1440).contains(&duration) {
            app.set_sync_status(UiMessage::plain(
                "Add a title and choose a duration from 15 to 1440 minutes.",
            ));
            return;
        }
        let Some(start) = Local.from_local_datetime(&date.and_time(time)).earliest() else {
            app.set_sync_status(UiMessage::plain(
                "That local time does not exist in the current time zone.",
            ));
            return;
        };
        let Some(core) = inbox_for_calendar_save.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain("Calendar storage is unavailable."));
            return;
        };
        let starts_at = start.timestamp_millis();
        let ends_at = (start + ChronoDuration::minutes(i64::from(duration))).timestamp_millis();
        let editing_event = editing_event_for_save.get().and_then(|event_id| {
            calendar_for_save
                .borrow()
                .events
                .iter()
                .find(|event| i64::from(event.id) == event_id && event.is_local)
                .cloned()
        });
        let result = if let Some(event) = editing_event {
            runtime_for_calendar_save
                .block_on(core.update_event(UpdateEventArgs {
                    event_id: i64::from(event.id),
                    summary: title.trim().to_owned(),
                    description: (!event.description.is_empty()).then_some(event.description),
                    location: (!event.location.is_empty()).then_some(event.location),
                    join_url: (!event.join_url.is_empty()).then_some(event.join_url),
                    starts_at,
                    ends_at,
                    all_day: event.all_day,
                    attendees: event.attendee_addresses,
                    notify: false,
                }))
                .map(|_| ())
        } else {
            let account_id = inbox_for_calendar_save
                .borrow()
                .connected_accounts
                .first()
                .map(|account| account.id)
                .unwrap_or(0);
            runtime_for_calendar_save
                .block_on(core.create_event(CreateEventArgs {
                    account_id,
                    calendar_id: None,
                    summary: title.trim().to_owned(),
                    description: None,
                    location: None,
                    join_url: None,
                    starts_at,
                    ends_at,
                    all_day: false,
                    attendees: Vec::new(),
                }))
                .map(|_| ())
        };
        if let Err(error) = result {
            app.set_sync_status(UiMessage::detail("Could not save event: {}", error));
            return;
        }
        let today = Local::now().date_naive();
        let mut calendar = calendar_for_save.borrow_mut();
        calendar.selected_date = date;
        calendar.visible_month = first_of_month(date);
        let refresh_result = refresh_calendar_events(
            &core,
            &runtime_for_calendar_save,
            &mut calendar,
            &inbox_for_calendar_save.borrow().connected_accounts,
        );
        apply_calendar(&app, &calendar, today);
        editing_event_for_save.set(None);
        app.set_calendar_event_editing(false);
        app.set_calendar_editor_open(false);
        match refresh_result {
            Ok(()) => app.set_sync_status(UiMessage::plain(
                "Event saved. Provider sync will run when enabled.",
            )),
            Err(error) => app.set_sync_status(UiMessage::detail(
                "Event saved, but refresh failed: {}",
                error,
            )),
        }
    });

    // Blitz wakes Slint only when a decoded image/font resource is ready. A
    // coalesced one-shot gives its message queue time to settle and prevents a
    // renderer feedback loop; unlike the old repeated timer it has no idle
    // wakeups.
    let resource_renderer = Rc::clone(&email_renderer);
    let resource_app = app.as_weak();
    let resource_callback_pending = Rc::new(Cell::new(false));
    app.on_refresh_email_resources(move || {
        if resource_callback_pending.replace(true) {
            return;
        }
        let renderer = Rc::clone(&resource_renderer);
        let app = resource_app.clone();
        let pending = Rc::clone(&resource_callback_pending);
        Timer::single_shot(Duration::from_millis(50), move || {
            pending.set(false);
            if !renderer.borrow_mut().poll_resources() {
                return;
            }
            let Some(app) = app.upgrade() else {
                return;
            };
            if !use_wgpu {
                let (width, height) = email_viewport_size(&app);
                match renderer.borrow_mut().render_cpu_if_needed(
                    width,
                    height,
                    app.window().scale_factor(),
                ) {
                    Ok(Some(frame)) => apply_cpu_frame(&app, frame),
                    Ok(None) => {}
                    Err(error) => {
                        app.global::<EmailReader>().set_notice(error.clone().into());
                        app.set_text_mode(true);
                        app.set_render_status(UiMessage::detail(
                            "Email resource render failed: {}",
                            error,
                        ));
                    }
                }
            }
            sync_reader_metadata(&app, &renderer);
            app.window().request_redraw();
        });
    });
    // Message bodies are fetched lazily by the core. Keep a receiver alive
    // from startup so opening an uncached rich message cannot miss the event
    // that replaces its temporary snippet with the real body.
    let pending_core_updates = Arc::new(Mutex::new(PendingCoreUpdates::default()));
    let core_updates_wake = UiWake::new(app.as_weak(), |app| app.invoke_drain_core_updates());

    let (message_load_raw_tx, message_load_rx) = bounded_ui_channel::<MessageLoadUpdate>();
    let message_load_tx = UiSender::new(
        message_load_raw_tx,
        UiWake::new(app.as_weak(), |app| app.invoke_drain_message_load_updates()),
    );
    // One active read and one replaceable pending selection. Never build a
    // queue of expanded bodies while the user moves quickly through the list.
    let (body_requests, body_request_rx) =
        tokio::sync::watch::channel(None::<(u64, (CoreMailSource, MailMessage))>);
    let body_requests = Rc::new(body_requests);
    let body_generation = Rc::new(Cell::new(0_u64));
    let body_pending = Rc::new(Cell::new(None::<(i32, u64)>));
    let body_updates = message_load_tx.clone();
    runtime.spawn(latest_load::run(
        body_request_rx,
        |(core, row)| async move {
            let result = core.load_message(&row).await;
            (row.id, result)
        },
        move |generation, (id, result)| {
            let updates = body_updates.clone();
            async move {
                let _ = updates
                    .send(MessageLoadUpdate {
                        generation,
                        id,
                        result,
                    })
                    .await;
            }
        },
    ));
    let body_state = Rc::clone(&state);
    let pending = body_pending.clone();
    let generation = body_generation.clone();
    let requests = body_requests.clone();
    app.global::<EmailReader>().on_ensure_body(move |id| {
        let load = {
            let state = body_state.borrow();
            state.core.clone().zip(
                state
                    .messages
                    .iter()
                    .find(|row| row.id == id && row.body_pending && row.thread_id.is_some())
                    .cloned(),
            )
        };
        if let Some((core, mut row)) = load {
            if pending.get().is_some_and(|(active, _)| active == id) {
                return;
            }
            let next = generation.get().wrapping_add(1);
            generation.set(next);
            pending.set(Some((id, next)));
            row.html = None;
            row.text = None;
            requests.send_replace(Some((next, (core, row))));
        } else if pending.get().is_some_and(|(active, _)| active != id) {
            generation.set(generation.get().wrapping_add(1));
            pending.set(None);
            requests.send_replace(None);
        }
    });
    let (mail_list_raw_tx, mail_list_rx) = bounded_ui_channel::<MailListUpdate>();
    let mail_list_tx = UiSender::new(
        mail_list_raw_tx,
        UiWake::new(app.as_weak(), |app| app.invoke_drain_mail_list_updates()),
    );
    let mail_list_rx = Rc::new(RefCell::new(mail_list_rx));
    let mail_list_refresh_requested = Rc::new(Cell::new(false));
    let mail_list_refresh_in_progress = Rc::new(Cell::new(false));
    let mail_pagination_generation = Rc::new(Cell::new(0_u64));
    let mail_pagination_in_progress = Rc::new(Cell::new(false));
    let (mail_metadata_raw_tx, mail_metadata_rx) = bounded_ui_channel::<MailMetadataUpdate>();
    let mail_metadata_tx = UiSender::new(
        mail_metadata_raw_tx,
        UiWake::new(app.as_weak(), |app| {
            app.invoke_drain_mail_metadata_updates()
        }),
    );
    let mail_metadata_rx = Rc::new(RefCell::new(mail_metadata_rx));
    let mail_metadata_refresh_requested = Rc::new(Cell::new(false));
    let mail_metadata_refresh_in_progress = Rc::new(Cell::new(false));
    let (folder_raw_tx, folder_rx) = bounded_ui_channel::<FolderMutationUpdate>();
    let folder_tx = UiSender::new(
        folder_raw_tx,
        UiWake::new(app.as_weak(), |app| app.invoke_drain_folder_updates()),
    );
    let folder_rx = Rc::new(RefCell::new(folder_rx));
    let folder_update_state = Rc::clone(&state);
    let folder_update_runtime = Rc::clone(&runtime);
    let folder_update_app = app.as_weak();
    app.on_drain_folder_updates(move || {
        while let Ok(update) = folder_rx.borrow_mut().try_recv() {
            let Some(app) = folder_update_app.upgrade() else {
                return;
            };
            if let Some(metadata) = update.metadata {
                let mut state = folder_update_state.borrow_mut();
                state.scope = metadata.scope.clone();
                state.mailboxes = metadata.mailboxes;
                state.unified_mailboxes = metadata.unified_mailboxes;
                state.inbox_count = metadata.inbox_count;
                state.total_count = metadata.scope_total;
                drop(state);
                if let Err(error) = refresh_from_source(
                    &app,
                    &folder_update_state,
                    &folder_update_runtime,
                    false,
                    &[],
                ) {
                    app.set_render_status(UiMessage::detail("Mail refresh failed: {}", error));
                }
            }
            app.set_sync_status(update.message);
        }
    });

    let folder_toggle_state = Rc::clone(&state);
    let folder_toggle_app = app.as_weak();
    app.on_toggle_folder(move |folder_id, expanded| {
        let folder_id = i64::from(folder_id);
        if expanded {
            folder_toggle_state
                .borrow_mut()
                .collapsed_folder_ids
                .remove(&folder_id);
        } else {
            folder_toggle_state
                .borrow_mut()
                .collapsed_folder_ids
                .insert(folder_id);
        }
        if let Some(app) = folder_toggle_app.upgrade() {
            refresh_list_metadata(&app, &folder_toggle_state);
        }
    });

    let create_folder_state = Rc::clone(&state);
    let create_folder_runtime = Rc::clone(&runtime);
    let create_folder_updates = folder_tx.clone();
    app.on_create_folder(move |account_id, parent_id, name| {
        let Some(core) = create_folder_state.borrow().core.clone() else {
            return;
        };
        let scope = create_folder_state.borrow().scope.clone();
        let updates = create_folder_updates.clone();
        let name = name.to_string();
        create_folder_runtime.spawn(async move {
            let result = core
                .create_folder(
                    i64::from(account_id),
                    (parent_id >= 0).then_some(i64::from(parent_id)),
                    name,
                )
                .await;
            let metadata = if result.is_ok() {
                core.load_mail_metadata(&scope).await.ok()
            } else {
                None
            };
            let message = match result {
                Ok(()) => UiMessage::plain("Folder created."),
                Err(error) => UiMessage::detail("Could not create folder: {}", error),
            };
            let _ = updates
                .send(FolderMutationUpdate { message, metadata })
                .await;
        });
    });

    let rename_folder_state = Rc::clone(&state);
    let rename_folder_runtime = Rc::clone(&runtime);
    let rename_folder_updates = folder_tx.clone();
    app.on_rename_folder(move |folder_id, name| {
        let Some(core) = rename_folder_state.borrow().core.clone() else {
            return;
        };
        let scope = rename_folder_state.borrow().scope.clone();
        let updates = rename_folder_updates.clone();
        let name = name.to_string();
        rename_folder_runtime.spawn(async move {
            let result = core.rename_folder(i64::from(folder_id), name).await;
            let metadata = if result.is_ok() {
                core.load_mail_metadata(&scope).await.ok()
            } else {
                None
            };
            let message = match result {
                Ok(()) => UiMessage::plain("Folder renamed."),
                Err(error) => UiMessage::detail("Could not rename folder: {}", error),
            };
            let _ = updates
                .send(FolderMutationUpdate { message, metadata })
                .await;
        });
    });

    let delete_folder_state = Rc::clone(&state);
    let delete_folder_runtime = Rc::clone(&runtime);
    let delete_folder_updates = folder_tx;
    app.on_delete_folder(move |folder_id| {
        let Some(core) = delete_folder_state.borrow().core.clone() else {
            return;
        };
        let folder_id = i64::from(folder_id);
        let (current_scope, selected_folder_is_deleted) = {
            let state = delete_folder_state.borrow();
            let selected_folder_id = state
                .scope
                .strip_prefix("Folder:")
                .and_then(|value| value.parse::<i64>().ok());
            let parents = state
                .mailboxes
                .iter()
                .filter(|mailbox| mailbox.folder_id >= 0)
                .map(|mailbox| (mailbox.folder_id, mailbox.parent_folder_id))
                .collect::<HashMap<_, _>>();
            let mut selected_folder_is_deleted = false;
            let mut candidate = selected_folder_id.unwrap_or(-1);
            let mut visited = HashSet::new();
            while candidate >= 0 && visited.insert(candidate) {
                if candidate == folder_id {
                    selected_folder_is_deleted = true;
                    break;
                }
                candidate = parents.get(&candidate).copied().unwrap_or(-1);
            }
            (state.scope.clone(), selected_folder_is_deleted)
        };
        let scope = if selected_folder_is_deleted {
            "Unified Inbox".to_owned()
        } else {
            current_scope
        };
        let updates = delete_folder_updates.clone();
        delete_folder_runtime.spawn(async move {
            let result = core.delete_folder(folder_id).await;
            let metadata = if result.is_ok() {
                core.load_mail_metadata(&scope).await.ok()
            } else {
                None
            };
            let message = match result {
                Ok(()) => UiMessage::plain("Folder deleted."),
                Err(error) => UiMessage::detail("Could not delete folder: {}", error),
            };
            let _ = updates
                .send(FolderMutationUpdate { message, metadata })
                .await;
        });
    });
    let mail_update_state = Rc::clone(&state);
    let mail_update_runtime = Rc::clone(&runtime);
    let mail_update_app = app.as_weak();
    let pending_core_updates_for_ui = Arc::clone(&pending_core_updates);
    let calendar_for_core_updates = Rc::clone(&calendar_state);
    let mail_list_app = app.as_weak();
    let mail_list_refresh_in_progress_for_result = Rc::clone(&mail_list_refresh_in_progress);
    let mail_pagination_generation_for_result = Rc::clone(&mail_pagination_generation);
    let mail_pagination_in_progress_for_result = Rc::clone(&mail_pagination_in_progress);
    app.on_drain_mail_list_updates(move || {
        while let Ok(update) = mail_list_rx.borrow_mut().try_recv() {
            let current_scope = mail_update_state.borrow().scope.clone();
            let current_query = mail_update_state.borrow().query.clone();
            let Some(app) = mail_update_app.upgrade() else {
                return;
            };
            match update.kind {
                MailListUpdateKind::Refresh => {
                    mail_list_refresh_in_progress_for_result.set(false);
                    if update.scope == current_scope && update.query == current_query {
                        match update.result {
                            Ok(page) => apply_background_mail_page(
                                &app,
                                &mail_update_state,
                                &mail_update_runtime,
                                page,
                                &[],
                            ),
                            Err(error) => app.set_render_status(UiMessage::detail(
                                "Background mail refresh failed: {}",
                                error,
                            )),
                        }
                    }
                }
                MailListUpdateKind::Pagination { cursor, generation } => {
                    if generation != mail_pagination_generation_for_result.get() {
                        continue;
                    }
                    mail_pagination_in_progress_for_result.set(false);
                    let still_current = update.scope == current_scope
                        && update.query == current_query
                        && mail_update_state.borrow().next_cursor == Some(cursor);
                    if still_current {
                        match update.result {
                            Ok(page) => {
                                match append_mail_page(
                                    &app,
                                    &mail_update_state,
                                    &mail_update_runtime,
                                    cursor,
                                    page,
                                ) {
                                    Ok(()) => {}
                                    Err(error) => app.set_render_status(UiMessage::detail(
                                        "Mail pagination failed: {}",
                                        error,
                                    )),
                                }
                            }
                            Err(error) => app.set_render_status(UiMessage::detail(
                                "Mail pagination failed: {}",
                                error,
                            )),
                        }
                    }
                    app.set_mail_loading_more(false);
                    if !still_current {
                        // A live refresh advanced the cursor while this page
                        // was in flight. Re-check the current viewport so it
                        // can immediately request the replacement cursor.
                        app.set_mail_list_revision(app.get_mail_list_revision().wrapping_add(1));
                    }
                }
            }
        }
        if let Some(app) = mail_list_app.upgrade() {
            // If another core event arrived during the query, this immediately
            // starts the one coalesced follow-up refresh.
            app.invoke_drain_core_updates();
        }
    });

    let mail_metadata_state = Rc::clone(&state);
    let mail_metadata_app = app.as_weak();
    let mail_metadata_refresh_in_progress_for_result =
        Rc::clone(&mail_metadata_refresh_in_progress);
    app.on_drain_mail_metadata_updates(move || {
        while let Ok(update) = mail_metadata_rx.borrow_mut().try_recv() {
            mail_metadata_refresh_in_progress_for_result.set(false);
            let Some(app) = mail_metadata_app.upgrade() else {
                return;
            };
            match update.result {
                Ok(metadata) => {
                    let mut state = mail_metadata_state.borrow_mut();
                    state.mailboxes = metadata.mailboxes;
                    state.unified_mailboxes = metadata.unified_mailboxes;
                    state.inbox_count = metadata.inbox_count;
                    if update.scope == state.scope && state.query.trim().is_empty() {
                        state.total_count = metadata.scope_total;
                    }
                    drop(state);
                    refresh_list_metadata(&app, &mail_metadata_state);
                }
                Err(error) => {
                    eprintln!("mailbox badge refresh failed: {error}");
                }
            }
            // Start one coalesced follow-up if another mutation arrived while
            // the grouped count query was running.
            app.invoke_drain_core_updates();
        }
    });

    let mail_update_state = Rc::clone(&state);
    let mail_update_runtime = Rc::clone(&runtime);
    let mail_update_app = app.as_weak();
    let mail_list_refresh_requested_for_core = Rc::clone(&mail_list_refresh_requested);
    let mail_list_refresh_in_progress_for_core = Rc::clone(&mail_list_refresh_in_progress);
    let mail_list_tx_for_core = mail_list_tx.clone();
    let mail_metadata_refresh_requested_for_core = Rc::clone(&mail_metadata_refresh_requested);
    let mail_metadata_refresh_in_progress_for_core = Rc::clone(&mail_metadata_refresh_in_progress);
    let pending_body_for_events = body_pending.clone();
    app.on_drain_core_updates(move || {
        let pending = {
            let mut pending = pending_core_updates_for_ui
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::take(&mut *pending)
        };
        if pending.all_mail_changed || !pending.changed_threads.is_empty() {
            mail_list_refresh_requested_for_core.set(true);
            mail_metadata_refresh_requested_for_core.set(true);
        }
        if mail_metadata_refresh_requested_for_core.get()
            && !mail_metadata_refresh_in_progress_for_core.get()
        {
            let (using_core, core, scope) = {
                let state = mail_update_state.borrow();
                (state.using_core, state.core.clone(), state.scope.clone())
            };
            if using_core && let Some(core) = core {
                mail_metadata_refresh_requested_for_core.set(false);
                mail_metadata_refresh_in_progress_for_core.set(true);
                let updates = mail_metadata_tx.clone();
                mail_update_runtime.spawn(async move {
                    let result = core.load_mail_metadata(&scope).await;
                    let _ = updates.send(MailMetadataUpdate { scope, result }).await;
                });
            }
        }
        if mail_list_refresh_requested_for_core.get()
            && !mail_list_refresh_in_progress_for_core.get()
        {
            let (using_core, core, scope, query) = {
                let state = mail_update_state.borrow();
                (
                    state.using_core,
                    state.core.clone(),
                    state.scope.clone(),
                    state.query.clone(),
                )
            };
            if using_core && let Some(core) = core {
                mail_list_refresh_requested_for_core.set(false);
                mail_list_refresh_in_progress_for_core.set(true);
                let updates = mail_list_tx_for_core.clone();
                mail_update_runtime.spawn(async move {
                    let result = core
                        .load_page(&scope, &query, None, PAGE_SIZE as i64, false)
                        .await;
                    let _ = updates
                        .send(MailListUpdate {
                            scope,
                            query,
                            kind: MailListUpdateKind::Refresh,
                            result,
                        })
                        .await;
                });
            }
        }
        if !pending.account_states.is_empty()
            && let Some(app) = mail_update_app.upgrade()
        {
            apply_account_states(&app, &mail_update_state, &pending.account_states);
        }
        if pending.calendar_changed
            && let Some(app) = mail_update_app.upgrade()
            && let Some(core) = mail_update_state.borrow().core.clone()
        {
            let today = Local::now().date_naive();
            let mut calendar = calendar_for_core_updates.borrow_mut();
            if refresh_calendar_events(
                &core,
                &mail_update_runtime,
                &mut calendar,
                &mail_update_state.borrow().connected_accounts,
            )
            .is_ok()
            {
                apply_calendar(&app, &calendar, today);
            }
        }
        let selected_thread = {
            let state = mail_update_state.borrow();
            let selected = state.selected_id.and_then(|selected_id| {
                state
                    .messages
                    .iter()
                    .find(|message| message.id == selected_id)
            });
            selected.and_then(|message| message.thread_id)
        };

        let selected_thread_changed =
            selected_thread.is_some_and(|thread_id| pending.changed_threads.contains(&thread_id));
        let selected_body_changed = pending.all_mail_changed || selected_thread_changed;
        if !selected_body_changed {
            return;
        }

        if mail_update_app.upgrade().is_none() {
            return;
        }
        let id = {
            let mut state = mail_update_state.borrow_mut();
            let id = state.selected_id;
            if let Some(row) = state.messages.iter_mut().find(|row| Some(row.id) == id) {
                row.body_pending = true;
            }
            id
        };
        if let Some(id) = id
            && let Some(app) = mail_update_app.upgrade()
        {
            // A completion may arrive while the previous database read is
            // still returning its pending snapshot. Queue a fresh read rather
            // than losing that only body-ready notification to deduplication.
            pending_body_for_events.set(None);
            app.global::<EmailReader>().invoke_ensure_body(id);
        }
    });

    // Loading the selected message includes database reads, CID image
    // expansion, and body-fetch scheduling. Apply its result on the UI thread,
    // but keep all of that work on Tokio so list input remains responsive.
    let message_load_rx = Rc::new(RefCell::new(message_load_rx));
    let message_load_state = Rc::clone(&state);
    let message_load_runtime = Rc::clone(&runtime);
    let message_load_pending_for_ui = body_pending.clone();
    let message_load_app = app.as_weak();
    app.on_drain_message_load_updates(move || {
        while let Ok(update) = message_load_rx.borrow_mut().try_recv() {
            if message_load_pending_for_ui.get() != Some((update.id, update.generation)) {
                continue;
            }
            message_load_pending_for_ui.set(None);
            let Some(app) = message_load_app.upgrade() else {
                return;
            };
            match update.result {
                Ok(message) => {
                    let body_pending = message.body_pending;
                    let is_selected = {
                        let mut state = message_load_state.borrow_mut();
                        let is_selected =
                            state.selected_id == Some(update.id) && !state.preview_closed;
                        if is_selected
                            && let Some(current) = state
                                .messages
                                .iter_mut()
                                .find(|current| current.id == update.id)
                        {
                            *current = message;
                        }
                        is_selected
                    };
                    // A user may select another row while this load is in flight.
                    // Discard superseded bodies instead of caching them in list rows.
                    if is_selected
                        && !body_pending
                        && let Err(error) =
                            render_current(&app, &message_load_state, &message_load_runtime)
                    {
                        app.set_render_status(UiMessage::detail(
                            "Message refresh failed: {}",
                            error,
                        ));
                    }
                }
                Err(error) => {
                    if message_load_state.borrow().selected_id == Some(update.id) {
                        app.set_render_status(UiMessage::detail("Message load failed: {}", error));
                    }
                }
            }
        }
    });

    // Network/image work wakes Slint only when a bounded result is ready. The
    // native UI state remains !Send and is touched only by this callback.
    let favicon_rx = Rc::new(RefCell::new(favicon_rx));
    let favicon_state = Rc::clone(&state);
    let favicon_runtime = Rc::clone(&runtime);
    let favicon_app = app.as_weak();
    app.on_drain_favicon_updates(move || {
        let mut changed = false;
        loop {
            let update = match favicon_rx.borrow_mut().try_recv() {
                Ok(update) => update,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            };
            let mut state = favicon_state.borrow_mut();
            if update.pixel_sides != state.favicon_pixel_sides {
                continue;
            }
            state.favicon_pending.remove(&update.domain);
            if !state.remote_images_enabled {
                continue;
            }
            let state = &mut *state;
            insert_bounded_favicon_result(
                &mut state.favicon_icons,
                &mut state.favicon_missing,
                update.domain,
                update.icons,
            );
            changed = true;
        }

        if let Some(app) = favicon_app.upgrade() {
            let pixel_sides = (
                physical_pixel_side(SENDER_AVATAR_SMALL_SIDE, app.window().scale_factor()),
                physical_pixel_side(SENDER_AVATAR_REGULAR_SIDE, app.window().scale_factor()),
            );
            {
                let mut state = favicon_state.borrow_mut();
                if state.favicon_pixel_sides != (0, 0) && state.favicon_pixel_sides != pixel_sides {
                    state.favicon_pixel_sides = pixel_sides;
                    state.favicon_icons.clear();
                    state.favicon_pending.clear();
                    state.favicon_missing.clear();
                    changed = true;
                }
            }
            if changed {
                refresh_rows_only(&app, &favicon_state, &favicon_runtime);
            }
        }
    });

    // OAuth profile photos are first-party account identity, so they are kept
    // separate from the opt-in sender/body remote-image switch. Like sender
    // marks, both sizes are rasterized for the current monitor before Slint's
    // software renderer sees them.
    let profile_avatar_rx = Rc::new(RefCell::new(profile_avatar_rx));
    let profile_avatar_state = Rc::clone(&state);
    let profile_avatar_runtime = Rc::clone(&runtime);
    let profile_avatar_app = app.as_weak();
    app.on_drain_profile_avatar_updates(move || {
        let mut changed = false;
        loop {
            let update = match profile_avatar_rx.borrow_mut().try_recv() {
                Ok(update) => update,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            };
            let mut state = profile_avatar_state.borrow_mut();
            let still_current = state.profile_avatar_pixel_sides == update.pixel_sides
                && state.connected_accounts.iter().any(|account| {
                    account.id == update.account_id
                        && account.avatar_url.as_deref() == Some(update.source_url.as_str())
                });
            if !still_current {
                continue;
            }
            state.profile_avatar_pending.remove(&update.account_id);
            if let Some(images) = update.images {
                state.profile_avatar_missing.remove(&update.account_id);
                state
                    .profile_avatar_images
                    .insert(update.account_id, images);
            } else {
                state.profile_avatar_missing.insert(update.account_id);
            }
            changed = true;
        }

        let Some(app) = profile_avatar_app.upgrade() else {
            return;
        };
        let pixel_sides = (
            physical_pixel_side(ACCOUNT_AVATAR_SMALL_SIDE, app.window().scale_factor()),
            physical_pixel_side(ACCOUNT_AVATAR_REGULAR_SIDE, app.window().scale_factor()),
        );
        {
            let mut state = profile_avatar_state.borrow_mut();
            if state.profile_avatar_pixel_sides != (0, 0)
                && state.profile_avatar_pixel_sides != pixel_sides
            {
                state.profile_avatar_pixel_sides = pixel_sides;
                state.profile_avatar_images.clear();
                state.profile_avatar_pending.clear();
                state.profile_avatar_missing.clear();
                changed = true;
            }
        }
        if changed {
            refresh_connected_accounts(&app, &profile_avatar_state);
            refresh_list_metadata(&app, &profile_avatar_state);
        }
        schedule_profile_avatar_fetches(&app, &profile_avatar_state, &profile_avatar_runtime);
    });

    let (startup_raw_tx, startup_rx) = bounded_ui_channel::<StartupUpdate>();
    let startup_tx = UiSender::new(
        startup_raw_tx,
        UiWake::new(app.as_weak(), |app| app.invoke_drain_startup_updates()),
    );
    let startup_rx = Rc::new(RefCell::new(startup_rx));
    let startup_state = Rc::clone(&state);
    let startup_calendar = Rc::clone(&calendar_state);
    let startup_runtime = Rc::clone(&runtime);
    let startup_pending_core_updates = Arc::clone(&pending_core_updates);
    let startup_core_wake = core_updates_wake.clone();
    let startup_app = app.as_weak();
    let startup_paths_for_ui = platform.paths.clone();
    let startup_tray = tray.as_ref().map(|tray| tray.as_weak());
    let startup_metrics_for_ui = startup_metrics.clone();
    let startup_benchmark_disable_background = benchmark_disable_background;
    app.on_drain_startup_updates(move || {
        while let Ok(update) = startup_rx.borrow_mut().try_recv() {
            let Some(app) = startup_app.upgrade() else {
                return;
            };
            match update {
                StartupUpdate::Warm { snapshot, applied } => {
                    let snapshot = *snapshot;
                    let saved_at_ms = snapshot.saved_at_ms;
                    {
                        let mut state = startup_state.borrow_mut();
                        // Core is deliberately left unset. The global startup
                        // guard keeps callbacks inert while this saved view is
                        // visible behind it.
                        state.using_core = true;
                        state.scope = snapshot.scope;
                        state.total_count = snapshot.total_count;
                        state.inbox_count = snapshot.inbox_count;
                        state.next_cursor = snapshot.next_cursor;
                        state.connected_accounts = snapshot.accounts;
                        state.messages = snapshot
                            .messages
                            .into_iter()
                            .map(MailMessage::from)
                            .collect();
                        state.mailboxes = snapshot
                            .mailboxes
                            .into_iter()
                            .map(MailboxEntry::from)
                            .collect();
                        state.unified_mailboxes = snapshot
                            .unified_mailboxes
                            .into_iter()
                            .map(MailboxEntry::from)
                            .collect();
                        state.preview_closed = true;
                    }
                    refresh_rows_only(&app, &startup_state, &startup_runtime);
                    refresh_list_metadata(&app, &startup_state);
                    app.set_startup_hydrated(true);
                    startup_metrics_for_ui.emit_once(
                        "warm_cache_applied",
                        serde_json::json!({
                            "saved_at_ms": saved_at_ms,
                            "rows": startup_state.borrow().messages.len(),
                        }),
                    );
                    startup_metrics_for_ui
                        .schedule_rendered_frame(app.as_weak(), "warm_cache_frame");
                    Timer::single_shot(Duration::from_millis(16), move || {
                        let _ = applied.send(());
                    });
                }
                StartupUpdate::Ready(result) => {
                    let snapshot = match result {
                        Ok(snapshot) => *snapshot,
                        Err(error) => {
                            let diagnostic_id = startup_diagnostic_id(&error);
                            tracing::error!(%diagnostic_id, error = %error, "mail core startup failed");
                            app.set_startup_diagnostic_id(diagnostic_id.clone().into());
                            app.set_startup_error(UiMessage::plain(
                                "Your local mailbox could not be opened. No account operation was started. Retry, or copy the diagnostics for support.",
                            ));
                            app.set_diagnostics_text(
                                startup_diagnostics(
                                    &startup_paths_for_ui,
                                    &diagnostic_id,
                                    &error,
                                )
                                .into(),
                            );
                            app.set_sync_status(UiMessage::plain(
                                "Local storage startup failed.",
                            ));
                            app.set_startup_failed(true);
                            app.set_startup_ready(false);
                            continue;
                        }
                    };

                    let StartupSnapshot {
                        core,
                        scope,
                        page,
                        remote_images_enabled,
                        accounts,
                        account_configs,
                        settings,
                    } = snapshot;

                    let allow_remote_images = remote_images_enabled
                        && cfg!(feature = "remote-content")
                        && !startup_benchmark_disable_background;
                    {
                        let mut state = startup_state.borrow_mut();
                        state.core = Some(core.clone());
                        state.connected_accounts = accounts;
                        state.account_configs = account_configs;
                        state.scope = scope;
                        state.remote_images_enabled = allow_remote_images;
                        state.mark_read_on_open = settings
                            .as_ref()
                            .is_none_or(|settings| settings.mark_read_on_open);
                        state.favicon_loader = if allow_remote_images {
                            match FaviconLoader::new() {
                                Ok(loader) => Some(loader),
                                Err(error) => {
                                    eprintln!("sender icons unavailable: {error}");
                                    None
                                }
                            }
                        } else {
                            None
                        };
                        if let Some(page) = page.filter(|page| page.account_count > 0) {
                            state.using_core = true;
                            state.total_count = page.messages.len();
                            if state.scope == "Unified Inbox" {
                                state.inbox_count = page.messages.len();
                            }
                            state.messages = page.messages;
                            state.labels = page.labels;
                            state.mailboxes = page.mailboxes;
                            state.unified_mailboxes = page.unified_mailboxes;
                            state.next_cursor = page.next_cursor;
                            // Preparing the first HTML body can be much more
                            // expensive than projecting the mailbox rows. Let
                            // the shell and list paint first; body hydration is
                            // demand-driven when the user selects a message.
                            state.preview_closed = true;
                        } else {
                            state.using_core = true;
                            state.messages.clear();
                            state.labels.clear();
                            state.mailboxes.clear();
                            state.unified_mailboxes.clear();
                            state.total_count = 0;
                            state.inbox_count = 0;
                            state.next_cursor = None;
                            state.selected_id = None;
                            state.rendered_id = None;
                        }

                        let configure_result = state
                            .email_renderer
                            .borrow_mut()
                            .configure_resources(
                                startup_runtime.handle().clone(),
                                allow_remote_images,
                            );
                        if let Err(error) = configure_result {
                            eprintln!("email images unavailable: {error}");
                            state.remote_images_enabled = false;
                            state.favicon_loader = None;
                        }
                    }

                    if let Some(settings) = settings.as_ref() {
                        apply_settings(&app, settings);
                        #[cfg(any(target_os = "android", target_os = "ios"))]
                        app.set_close_to_tray(false);
                        if let Some(tray) =
                            startup_tray.as_ref().and_then(|tray| tray.upgrade())
                        {
                            tray.set_enabled(app.get_close_to_tray());
                        }
                    }
                    app.set_remote_images_enabled(
                        startup_state.borrow().remote_images_enabled,
                    );

                    refresh_connected_accounts(&app, &startup_state);
                    refresh_rows_only(&app, &startup_state, &startup_runtime);
                    refresh_list_metadata(&app, &startup_state);
                    app.set_startup_hydrated(true);
                    app.set_startup_ready(true);
                    app.set_startup_failed(false);
                    app.set_list_status(UiMessage::plain("Loading local data…"));
                    schedule_profile_avatar_fetches(
                        &app,
                        &startup_state,
                        &startup_runtime,
                    );
                    spawn_core_event_listener(
                        &startup_runtime,
                        core.clone(),
                        Arc::clone(&startup_pending_core_updates),
                        startup_core_wake.clone(),
                    );
                    if !startup_benchmark_disable_background {
                        core.notify_ui_ready();
                    }
                    app.set_sync_status(UiMessage::plain("Local data ready."));
                    startup_metrics_for_ui.emit_once(
                        "core_ready",
                        serde_json::json!({
                            "rows": startup_state.borrow().messages.len(),
                            "accounts": startup_state.borrow().connected_accounts.len(),
                        }),
                    );
                    startup_metrics_for_ui
                        .schedule_rendered_frame(app.as_weak(), "core_ready_frame");
                    startup_metrics_for_ui.schedule_benchmark_exit();
                }
                StartupUpdate::MailMetadata(result) => match result {
                    Ok(metadata) => {
                        {
                            let mut state = startup_state.borrow_mut();
                            state.mailboxes = metadata.mailboxes;
                            state.unified_mailboxes = metadata.unified_mailboxes;
                            state.inbox_count = metadata.inbox_count;
                            if state.scope == metadata.scope
                                && state.query.trim().is_empty()
                            {
                                state.total_count = metadata.scope_total;
                            }
                        }
                        refresh_list_metadata(&app, &startup_state);
                    }
                    Err(error) => {
                        eprintln!("mailbox totals unavailable: {error}");
                        refresh_list_metadata(&app, &startup_state);
                    }
                },
                StartupUpdate::Calendar(StartupCalendarSnapshot {
                    calendar_connections,
                    calendar_events,
                    calendar_accounts,
                    calendar_sources,
                }) => {
                    startup_state.borrow_mut().calendar_connections =
                        calendar_connections;
                    {
                        let mut calendar = startup_calendar.borrow_mut();
                        calendar.events = calendar_events;
                        calendar.accounts = calendar_accounts;
                        calendar.sources = calendar_sources;
                        apply_calendar(&app, &calendar, Local::now().date_naive());
                    }
                    refresh_connected_accounts(&app, &startup_state);
                }
            }
        }
    });

    app.set_sync_status(UiMessage::plain("Loading local data…"));
    let retry_runtime = Rc::clone(&runtime);
    let retry_paths = platform.paths.clone();
    let retry_credentials = platform.credentials.clone();
    let retry_oauth_redirects = platform.oauth_redirects.clone();
    let retry_tx = startup_tx.clone();
    let retry_app = app.as_weak();
    let retry_metrics = startup_metrics.clone();
    app.on_retry_startup(move || {
        if let Some(app) = retry_app.upgrade() {
            app.set_startup_ready(false);
            app.set_startup_failed(false);
            app.set_sync_status(UiMessage::plain("Loading local data…"));
        }
        spawn_startup_load(
            &retry_runtime,
            retry_paths.clone(),
            retry_credentials.clone(),
            retry_oauth_redirects.clone(),
            retry_tx.clone(),
            retry_metrics.clone(),
        );
    });
    spawn_startup_load(
        &runtime,
        platform.paths.clone(),
        platform.credentials.clone(),
        platform.oauth_redirects.clone(),
        startup_tx,
        startup_metrics,
    );

    render_current(&app, &state, &runtime)?;

    // Long-running account and OAuth work always stays on Tokio. This bridge
    // applies only small, already-materialized results on the Slint thread.
    let (ui_task_raw_tx, ui_task_rx) = bounded_ui_channel::<UiTaskUpdate>();
    let ui_task_tx = UiSender::new(
        ui_task_raw_tx,
        UiWake::new(app.as_weak(), |app| app.invoke_drain_ui_task_updates()),
    );
    let ui_task_rx = Rc::new(RefCell::new(ui_task_rx));
    let ui_task_state = Rc::clone(&state);
    let ui_task_runtime = Rc::clone(&runtime);
    let ui_task_app = app.as_weak();
    let ui_task_tray = tray.as_ref().map(|tray| tray.as_weak());
    app.on_drain_ui_task_updates(move || {
        loop {
            let update = match ui_task_rx.borrow_mut().try_recv() {
                Ok(update) => update,
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            };
            let Some(app) = ui_task_app.upgrade() else {
                return;
            };
            if update.finishes_oauth {
                app.set_oauth_in_progress(false);
            }
            if let Some(enabled) = update.close_to_tray {
                app.set_close_to_tray(enabled);
                if let Some(tray) = ui_task_tray.as_ref().and_then(|tray| tray.upgrade()) {
                    tray.set_enabled(enabled);
                }
            }
            if let Some((accounts, configs)) = update.accounts {
                ui_task_state.borrow_mut().using_core = true;
                update_connected_accounts(
                    &app,
                    &ui_task_state,
                    &ui_task_runtime,
                    accounts,
                    configs,
                );
            }
            let mut refresh_account_rows = false;
            if let Some(connections) = update.calendar_connections {
                ui_task_state.borrow_mut().calendar_connections = connections;
                refresh_account_rows = true;
            }
            if let Some((account_id, error)) = update.calendar_error {
                let mut state = ui_task_state.borrow_mut();
                match error.filter(|error| !error.trim().is_empty()) {
                    Some(error) => {
                        state.calendar_errors.insert(account_id, error);
                    }
                    None => {
                        state.calendar_errors.remove(&account_id);
                    }
                }
                refresh_account_rows = true;
            }
            if refresh_account_rows {
                refresh_connected_accounts(&app, &ui_task_state);
            }
            if update.clear_account_form {
                app.set_account_email("".into());
                app.set_account_username("".into());
                app.set_account_password("".into());
                app.set_account_protocol("imap".into());
                app.set_jmap_url("".into());
                app.set_imap_host("".into());
                app.set_smtp_host("".into());
            }
            app.set_sync_status(update.message);
        }
    });

    register_window_preference_callbacks(&app, tray.as_ref(), &state, &runtime, &ui_task_tx);

    app.on_make_account_transfer(DataTransfer::from);
    app.on_read_account_transfer(|data| data.plain_text().unwrap_or_default());

    let app_weak = app.as_weak();
    let state_for_account_order = Rc::clone(&state);
    let runtime_for_account_order = Rc::clone(&runtime);
    let ui_task_tx_for_account_order = ui_task_tx.clone();
    app.on_reorder_account(move |source, target, after| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Ok(source_id) = source.as_str().parse::<i64>() else {
            app.set_sync_status(UiMessage::plain("Could not identify the dragged account."));
            return;
        };
        let target_id = i64::from(target);
        if source_id == target_id {
            return;
        }
        let Some(core) = state_for_account_order.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Account ordering requires local mail data.",
            ));
            return;
        };
        if !reorder_connected_account_rows(&app, source_id, target_id, after) {
            app.set_sync_status(UiMessage::plain("Could not find the account to reorder."));
            return;
        }

        app.set_sync_status(UiMessage::plain("Saving account order…"));
        let updates = ui_task_tx_for_account_order.clone();
        runtime_for_account_order.spawn(async move {
            let result = core.reorder_account(source_id, target_id, after).await;
            // Always reload the authoritative order. On failure this also
            // rolls back the optimistic drag shown by the Slint model.
            let accounts = match (
                core.load_accounts().await,
                core.load_account_configs().await,
            ) {
                (Ok(accounts), Ok(configs)) => Some((accounts, configs)),
                _ => None,
            };
            let message = match result {
                Ok(()) => UiMessage::plain("Account order saved."),
                Err(error) => UiMessage::detail("Could not save account order: {}", error),
            };
            let _ = updates
                .send(UiTaskUpdate {
                    message,
                    accounts,
                    calendar_connections: None,
                    calendar_error: None,
                    clear_account_form: false,
                    finishes_oauth: false,
                    close_to_tray: None,
                })
                .await;
        });
    });

    let app_weak = app.as_weak();
    let state_for_mail_history = Rc::clone(&state);
    let runtime_for_mail_history = Rc::clone(&runtime);
    let ui_task_tx_for_mail_history = ui_task_tx.clone();
    app.on_save_account_mail_history(move |account_id, value| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(core) = state_for_mail_history.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain("Mail history requires local mail data."));
            return;
        };
        let account_id = i64::from(account_id);
        let value = value.to_string();
        app.set_sync_status(UiMessage::plain("Updating mail history…"));
        let updates = ui_task_tx_for_mail_history.clone();
        runtime_for_mail_history.spawn(async move {
            let message = match core.set_account_mail_history(account_id, &value).await {
                Ok(_) => {
                    UiMessage::plain("Mail history updated. Background sync is adjusting now.")
                }
                Err(error) => UiMessage::detail("Could not update mail history: {}", error),
            };
            // Reload on both success and failure so the selector always shows
            // the authoritative persisted value.
            let accounts = match (
                core.load_accounts().await,
                core.load_account_configs().await,
            ) {
                (Ok(accounts), Ok(configs)) => Some((accounts, configs)),
                _ => None,
            };
            let _ = updates
                .send(UiTaskUpdate {
                    message,
                    accounts,
                    calendar_connections: None,
                    calendar_error: None,
                    clear_account_form: false,
                    finishes_oauth: false,
                    close_to_tray: None,
                })
                .await;
        });
    });

    let app_weak = app.as_weak();
    let state_for_remote_images = Rc::clone(&state);
    let runtime_for_remote_images = Rc::clone(&runtime);
    app.on_set_remote_images(move |enabled| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        if !cfg!(feature = "remote-content") {
            app.set_sync_status(UiMessage::plain(
                "Remote images are unavailable in this minimal build.",
            ));
            app.set_remote_images_enabled(false);
            return;
        }

        let core = state_for_remote_images.borrow().core.clone();
        if let Some(core) = core.as_ref()
            && let Err(error) =
                runtime_for_remote_images.block_on(core.set_load_remote_images(enabled))
        {
            app.set_sync_status(UiMessage::detail(
                "Could not save remote-image setting: {}",
                error,
            ));
            return;
        }

        {
            let mut state = state_for_remote_images.borrow_mut();
            state.remote_images_enabled = enabled;
            state.remote_images_override_id = None;
            state.favicon_icons.clear();
            state.favicon_pending.clear();
            state.favicon_missing.clear();
            state.favicon_loader = if enabled {
                match FaviconLoader::new() {
                    Ok(loader) => Some(loader),
                    Err(error) => {
                        app.set_sync_status(UiMessage::detail(
                            "Remote sender icons unavailable: {}",
                            error,
                        ));
                        None
                    }
                }
            } else {
                None
            };

            let renderer = Rc::clone(&state.email_renderer);
            let configure_result = renderer
                .borrow_mut()
                .configure_resources(runtime_for_remote_images.handle().clone(), enabled);
            if let Err(error) = configure_result {
                state.remote_images_enabled = false;
                state.favicon_loader = None;
                if let Some(core) = core.as_ref() {
                    let _ = runtime_for_remote_images.block_on(core.set_load_remote_images(false));
                }
                let _ = renderer
                    .borrow_mut()
                    .configure_resources(runtime_for_remote_images.handle().clone(), false);
                app.set_remote_images_enabled(false);
                app.set_sync_status(UiMessage::detail("Remote images unavailable: {}", error));
                return;
            }
        }

        app.set_remote_images_enabled(enabled);
        app.set_sync_status(if enabled {
            UiMessage::plain("Remote body images and sender favicons enabled.")
        } else {
            UiMessage::plain("Remote images blocked.")
        });
        if let Err(error) =
            render_current(&app, &state_for_remote_images, &runtime_for_remote_images)
        {
            app.set_render_status(UiMessage::detail(
                "Could not refresh message images: {}",
                error,
            ));
        }
    });

    let retry_app = app.as_weak();
    let retry_state = Rc::clone(&state);
    let retry_runtime = Rc::clone(&runtime);
    app.global::<EmailReader>().on_retry_images(move || {
        let Some(app) = retry_app.upgrade() else {
            return;
        };
        retry_state.borrow().email_renderer.borrow_mut().loaded_key = None;
        if let Err(error) = render_current(&app, &retry_state, &retry_runtime) {
            app.set_render_status(UiMessage::detail(
                "Could not load message images: {}",
                error,
            ));
        }
    });

    let app_weak = app.as_weak();
    let state_for_message_images = Rc::clone(&state);
    let runtime_for_message_images = Rc::clone(&runtime);
    app.on_load_remote_images_for_message(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        if !cfg!(feature = "remote-content") {
            app.set_render_status(UiMessage::plain("Remote-image loading is disabled."));
            return;
        }
        {
            state_for_message_images
                .borrow()
                .email_renderer
                .borrow_mut()
                .loaded_key = None;
            let mut state = state_for_message_images.borrow_mut();
            state.remote_images_override_id = state.selected_id;
        }
        if let Err(error) =
            render_current(&app, &state_for_message_images, &runtime_for_message_images)
        {
            app.set_render_status(UiMessage::detail(
                "Could not load message images: {}",
                error,
            ));
        }
    });

    // Sync is network-bound and may take many seconds. Run it (including the
    // expensive exact sidebar recount) on Tokio, then wake Slint once the small
    // result is ready.
    let (sync_raw_tx, sync_rx) = bounded_ui_channel::<SyncUpdate>();
    let sync_tx = UiSender::new(
        sync_raw_tx,
        UiWake::new(app.as_weak(), |app| app.invoke_drain_sync_updates()),
    );
    let sync_in_progress = Rc::new(Cell::new(false));
    let sync_rx = Rc::new(RefCell::new(sync_rx));
    let sync_state = Rc::clone(&state);
    let sync_runtime = Rc::clone(&runtime);
    let sync_progress = Rc::clone(&sync_in_progress);
    let sync_app = app.as_weak();
    app.on_drain_sync_updates(move || {
        while let Ok(update) = sync_rx.borrow_mut().try_recv() {
            sync_progress.set(false);
            let Some(app) = sync_app.upgrade() else {
                return;
            };
            app.set_sync_in_progress(false);
            match update.result {
                Ok(()) => {
                    {
                        let mut state = sync_state.borrow_mut();
                        state.using_core = true;
                        if let Some(metadata) = update.metadata {
                            state.mailboxes = metadata.mailboxes;
                            state.unified_mailboxes = metadata.unified_mailboxes;
                            state.inbox_count = metadata.inbox_count;
                            if metadata.scope == state.scope && state.query.trim().is_empty() {
                                state.total_count = metadata.scope_total;
                            }
                        }
                    }
                    if let Err(error) =
                        refresh_from_source(&app, &sync_state, &sync_runtime, true, &[])
                    {
                        app.set_sync_status(UiMessage::detail(
                            "Sync finished, refresh failed: {}",
                            error,
                        ));
                    } else {
                        app.set_sync_status(UiMessage::plain("Sync complete."));
                    }
                }
                Err(error) => app.set_sync_status(UiMessage::detail("Sync failed: {}", error)),
            }
        }
    });

    let app_weak = app.as_weak();
    let state_for_sync = Rc::clone(&state);
    let runtime_for_sync = Rc::clone(&runtime);
    let sync_progress_for_click = Rc::clone(&sync_in_progress);
    app.on_sync_mail(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        if sync_progress_for_click.replace(true) {
            return;
        }
        let (core, scope) = {
            let state = state_for_sync.borrow();
            (state.core.clone(), state.scope.clone())
        };
        let Some(core) = core else {
            sync_progress_for_click.set(false);
            app.set_sync_status(UiMessage::plain(
                "Local mail data is unavailable; preview data is read-only.",
            ));
            return;
        };
        app.set_sync_in_progress(true);
        app.set_sync_status(UiMessage::plain("Synchronizing accounts…"));
        let sync_tx = sync_tx.clone();
        runtime_for_sync.spawn(async move {
            let result = core.sync_now(None).await;
            let metadata = if result.is_ok() {
                core.load_mail_metadata(&scope).await.ok()
            } else {
                None
            };
            let _ = sync_tx.send(SyncUpdate { result, metadata }).await;
        });
    });

    let app_weak = app.as_weak();
    let state_for_check = Rc::clone(&state);
    let runtime_for_check = Rc::clone(&runtime);
    app.on_set_email_checked(move |id, checked| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let rows = Rc::clone(&state_for_check.borrow().email_rows);
        let is_visible = (0..rows.row_count())
            .filter_map(|index| rows.row_data(index))
            .any(|row| row.id == id);
        if !is_visible {
            return;
        }
        if checked {
            state_for_check.borrow_mut().checked_ids.insert(id);
        } else {
            state_for_check.borrow_mut().checked_ids.remove(&id);
        }
        refresh_rows_only(&app, &state_for_check, &runtime_for_check);
    });

    let app_weak = app.as_weak();
    let state_for_check_all = Rc::clone(&state);
    let runtime_for_check_all = Rc::clone(&runtime);
    app.on_set_all_emails_checked(move |checked| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let rows = Rc::clone(&state_for_check_all.borrow().email_rows);
        let visible_ids = (0..rows.row_count())
            .filter_map(|index| rows.row_data(index))
            .map(|row| row.id)
            .collect::<Vec<_>>();
        {
            let mut state = state_for_check_all.borrow_mut();
            if checked {
                state.checked_ids.extend(visible_ids);
            } else {
                for id in visible_ids {
                    state.checked_ids.remove(&id);
                }
            }
        }
        refresh_rows_only(&app, &state_for_check_all, &runtime_for_check_all);
    });

    let app_weak = app.as_weak();
    let state_for_action = Rc::clone(&state);
    let runtime_for_action = Rc::clone(&runtime);
    app.on_message_action(move |action| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        match perform_selected_action(&app, &state_for_action, &runtime_for_action, &action) {
            Ok(()) => app.set_render_status(UiMessage::plain("Message action completed.")),
            Err(error) => {
                app.set_render_status(UiMessage::detail("Message action failed: {}", error))
            }
        }
    });

    let app_weak = app.as_weak();
    let state_for_row_action = Rc::clone(&state);
    let runtime_for_row_action = Rc::clone(&runtime);
    app.on_message_action_for_email(move |id, action| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        match perform_mail_list_action(
            &app,
            &state_for_row_action,
            &runtime_for_row_action,
            id,
            &action,
        ) {
            Ok(()) => app.set_render_status(UiMessage::plain("Message action completed.")),
            Err(error) => {
                app.set_render_status(UiMessage::detail("Message action failed: {}", error))
            }
        }
    });

    let app_weak = app.as_weak();
    let state_for_label = Rc::clone(&state);
    let runtime_for_label = Rc::clone(&runtime);
    app.on_toggle_mail_label(move |label_id, applied| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let (core, thread_id) = {
            let state = state_for_label.borrow();
            let thread_id = state
                .selected_id
                .and_then(|id| state.messages.iter().find(|message| message.id == id))
                .and_then(|message| message.thread_id);
            (state.core.clone(), thread_id)
        };
        let result = (|| {
            let core = core.ok_or_else(|| "mail core is unavailable".to_owned())?;
            let thread_id = thread_id.ok_or_else(|| "no message is selected".to_owned())?;
            runtime_for_label.block_on(core.perform_label_action(
                thread_id,
                i64::from(label_id),
                applied,
            ))?;
            refresh_from_source(&app, &state_for_label, &runtime_for_label, true, &[])
        })();
        match result {
            Ok(()) if applied => app.set_render_status(UiMessage::plain("Label added.")),
            Ok(()) => app.set_render_status(UiMessage::plain("Label removed.")),
            Err(error) => {
                app.set_render_status(UiMessage::detail("Could not update label: {}", error))
            }
        }
    });

    let app_weak = app.as_weak();
    let state_for_save_label = Rc::clone(&state);
    let runtime_for_save_label = Rc::clone(&runtime);
    app.on_save_mail_label(move |label_id, name, color| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let (core, thread_id) = {
            let state = state_for_save_label.borrow();
            let thread_id = state
                .selected_id
                .and_then(|id| state.messages.iter().find(|message| message.id == id))
                .and_then(|message| message.thread_id);
            (state.core.clone(), thread_id)
        };
        let existing_id = (label_id >= 0).then_some(i64::from(label_id));
        let color = format!(
            "#{:02x}{:02x}{:02x}",
            color.red(),
            color.green(),
            color.blue()
        );
        let result: Result<bool, String> = (|| {
            let core = core.ok_or_else(|| "mail core is unavailable".to_owned())?;
            let label = runtime_for_save_label.block_on(core.save_label(
                existing_id,
                name.as_str(),
                color.as_str(),
            ))?;
            if existing_id.is_none() {
                let thread_id = thread_id.ok_or_else(|| "no message is selected".to_owned())?;
                runtime_for_save_label
                    .block_on(core.perform_label_action(thread_id, label.id, true))?;
            }
            refresh_from_source(
                &app,
                &state_for_save_label,
                &runtime_for_save_label,
                true,
                &[],
            )?;
            Ok(existing_id.is_some())
        })();
        match result {
            Ok(true) => app.set_render_status(UiMessage::plain("Label updated.")),
            Ok(false) => app.set_render_status(UiMessage::plain("Label created and added.")),
            Err(error) => {
                app.set_render_status(UiMessage::detail("Could not save label: {}", error))
            }
        }
    });

    let app_weak = app.as_weak();
    let state_for_delete_label = Rc::clone(&state);
    let runtime_for_delete_label = Rc::clone(&runtime);
    app.on_delete_mail_label(move |label_id| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let label_id = i64::from(label_id);
        let result = (|| {
            let core = {
                let state = state_for_delete_label.borrow();
                let label = state
                    .labels
                    .iter()
                    .find(|label| label.id == label_id)
                    .ok_or_else(|| "label no longer exists".to_owned())?;
                if label.is_auto {
                    return Err("automatic categories cannot be deleted".to_owned());
                }
                state
                    .core
                    .clone()
                    .ok_or_else(|| "mail core is unavailable".to_owned())?
            };
            runtime_for_delete_label.block_on(core.delete_label(label_id))?;
            {
                let mut state = state_for_delete_label.borrow_mut();
                if state.scope == format!("Label:{label_id}") {
                    state.scope = "Unified Inbox".to_owned();
                    state.page = 1;
                    state.next_cursor = None;
                    state.selected_id = None;
                    state.preview_closed = false;
                }
            }
            refresh_from_source(
                &app,
                &state_for_delete_label,
                &runtime_for_delete_label,
                true,
                &[],
            )
        })();
        match result {
            Ok(()) => app.set_render_status(UiMessage::plain("Label deleted.")),
            Err(error) => {
                app.set_render_status(UiMessage::detail("Could not delete label: {}", error))
            }
        }
    });

    let app_weak = app.as_weak();
    let state_for_label_search = Rc::clone(&state);
    app.on_search_mail_labels(move |query| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let state = state_for_label_search.borrow();
        let selected = state
            .selected_id
            .and_then(|id| state.messages.iter().find(|message| message.id == id));
        app.set_mail_label_results(ModelRc::new(VecModel::from(make_label_rows(
            &state.labels,
            selected,
            query.as_str(),
        ))));
    });

    let app_weak = app.as_weak();
    let state_for_close_preview = Rc::clone(&state);
    let runtime_for_close_preview = Rc::clone(&runtime);
    app.on_close_preview(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        {
            let mut state = state_for_close_preview.borrow_mut();
            state.selected_id = None;
            state.preview_closed = true;
        }
        if let Err(error) =
            render_current(&app, &state_for_close_preview, &runtime_for_close_preview)
        {
            app.set_render_status(UiMessage::detail("Could not close preview: {}", error));
        }
    });

    let app_weak = app.as_weak();
    let state_for_print = Rc::clone(&state);
    app.on_print_message(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        match print_selected_message(&state_for_print) {
            Ok(()) => app.set_render_status(UiMessage::plain("Opened the system print view.")),
            Err(error) => {
                app.set_render_status(UiMessage::detail("Could not print message: {}", error))
            }
        }
    });

    let app_weak = app.as_weak();
    let state_for_browser = Rc::clone(&state);
    app.on_open_message_in_browser(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        match open_selected_message_in_browser(&state_for_browser) {
            Ok(()) => {
                app.set_render_status(UiMessage::plain("Opened the message in your browser."))
            }
            Err(error) => app.set_render_status(UiMessage::detail(
                "Could not open message in browser: {}",
                error,
            )),
        }
    });

    register_settings_preference_callbacks(&app, &state, &runtime);

    let compose_files = Rc::new(RefCell::new(Vec::<ComposeFile>::new()));
    let compose_document = Rc::new(RefCell::new(RichComposeDocument::default()));
    let compose_editor = Rc::new(RefCell::new(CosmicComposeEditor::default()));
    let compose_contacts = Rc::new(RefCell::new(
        Vec::<flectar_mail_core::models::Address>::new(),
    ));
    let compose_intent = Rc::new(RefCell::new(ComposeIntent::default()));
    apply_compose_files(&app, &compose_files.borrow());
    {
        let document = compose_document.borrow();
        let mut editor = compose_editor.borrow_mut();
        apply_rich_compose(&app, &document, ComposeSelection::default(), &mut editor);
    }
    apply_compose_contacts(&app, &[]);

    let app_weak = app.as_weak();
    let intent_for_open = Rc::clone(&compose_intent);
    app.on_open_compose(move || {
        if let Some(app) = app_weak.upgrade() {
            *intent_for_open.borrow_mut() = ComposeIntent::default();
            app.set_compose_mode("new".into());
            app.set_compose_notice(UiMessage::EMPTY);
            app.set_compose_notice_is_error(false);
            app.set_compose_open(true);
        }
    });

    let app_weak = app.as_weak();
    let state_for_message_compose = Rc::clone(&state);
    let runtime_for_message_compose = Rc::clone(&runtime);
    let files_for_message_compose = Rc::clone(&compose_files);
    let document_for_message_compose = Rc::clone(&compose_document);
    let editor_for_message_compose = Rc::clone(&compose_editor);
    let contacts_for_message_compose = Rc::clone(&compose_contacts);
    let intent_for_message_compose = Rc::clone(&compose_intent);
    app.on_compose_message_action(move |action| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let (core, row) = {
            let state = state_for_message_compose.borrow();
            let row = state
                .selected_id
                .and_then(|id| state.messages.iter().find(|message| message.id == id))
                .cloned();
            (state.core.clone(), row)
        };
        let Some(core) = core else {
            app.set_render_status(UiMessage::plain(
                "Connect an account before replying or forwarding.",
            ));
            return;
        };
        let Some(row) = row else {
            app.set_render_status(UiMessage::plain("Choose a message first."));
            return;
        };

        if action.as_str() == "edit_draft" {
            if row.folder != "Drafts" && row.label != "DRAFT" {
                app.set_render_status(UiMessage::plain("Only drafts can be reopened for editing."));
                return;
            }
            let draft = match runtime_for_message_compose.block_on(core.load_draft(&row)) {
                Ok(draft) => draft,
                Err(error) => {
                    app.set_render_status(UiMessage::detail("Could not open draft: {}", error));
                    return;
                }
            };
            let Ok(account_id) = i32::try_from(draft.account_id) else {
                app.set_render_status(UiMessage::plain(
                    "Could not open draft: invalid account identifier",
                ));
                return;
            };
            let account_label = state_for_message_compose
                .borrow()
                .connected_accounts
                .iter()
                .find(|account| account.id == draft.account_id)
                .map(|account| {
                    let name = account
                        .display_name
                        .as_deref()
                        .map(str::trim)
                        .filter(|name| !name.is_empty());
                    match name {
                        Some(name) if name != account.email => {
                            format!("{name}  <{}>", account.email)
                        }
                        _ => account.email.clone(),
                    }
                })
                .unwrap_or_else(|| {
                    translated(&app, &UiMessage::plain("Connected account")).to_string()
                });

            clear_compose(
                &app,
                &files_for_message_compose,
                &document_for_message_compose,
                &editor_for_message_compose,
                &contacts_for_message_compose,
            );
            app.set_compose_account_id(account_id);
            app.set_compose_from_label(account_label.into());
            app.set_compose_to(compose_addresses(&draft.to).into());
            app.set_compose_cc(compose_addresses(&draft.cc).into());
            app.set_compose_bcc(compose_addresses(&draft.bcc).into());
            app.set_compose_subject(draft.subject.clone().into());

            let loaded_files = draft
                .attachments
                .iter()
                .map(|attachment| {
                    let path = PathBuf::from(&attachment.file_path);
                    let size = std::fs::metadata(&path)
                        .map(|metadata| metadata.len())
                        .unwrap_or_default();
                    ComposeFile {
                        path,
                        filename: attachment.filename.clone(),
                        size,
                    }
                })
                .collect::<Vec<_>>();
            *files_for_message_compose.borrow_mut() = loaded_files;
            apply_compose_files(&app, &files_for_message_compose.borrow());

            {
                let mut document = document_for_message_compose.borrow_mut();
                let cursor = i32::try_from(draft.body_text.len()).unwrap_or(i32::MAX);
                let selection = document.synchronize(&draft.body_text, cursor, cursor);
                editor_for_message_compose.borrow_mut().reset();
                apply_rich_compose(
                    &app,
                    &document,
                    selection,
                    &mut editor_for_message_compose.borrow_mut(),
                );
            }
            *intent_for_message_compose.borrow_mut() = ComposeIntent {
                mode: draft.mode,
                in_reply_to_message_id: draft.in_reply_to_message_id,
                draft_id: draft.draft_id,
            };
            app.set_compose_mode("draft".into());
            app.set_compose_open(true);
            return;
        }

        let source = match runtime_for_message_compose.block_on(core.load_compose_source(&row)) {
            Ok(source) => source,
            Err(error) => {
                app.set_render_status(UiMessage::detail("Could not open composer: {}", error));
                return;
            }
        };
        let account_name = state_for_message_compose
            .borrow()
            .connected_accounts
            .iter()
            .find(|account| account.id == source.account_id)
            .map(|account| {
                account
                    .display_name
                    .clone()
                    .filter(|name| !name.trim().is_empty())
                    .unwrap_or_else(|| account.email.clone())
            });
        let prepared = match prepare_message_compose(
            &source,
            action.as_str(),
            app.get_selected_plain_text().as_str(),
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                app.set_render_status(UiMessage::detail("Could not open composer: {}", error));
                return;
            }
        };
        let Ok(account_id) = i32::try_from(prepared.account_id) else {
            app.set_render_status(UiMessage::plain(
                "Could not open composer: invalid account identifier",
            ));
            return;
        };

        clear_compose(
            &app,
            &files_for_message_compose,
            &document_for_message_compose,
            &editor_for_message_compose,
            &contacts_for_message_compose,
        );
        app.set_compose_account_id(account_id);
        let display_name = account_name.unwrap_or_else(|| source.account_email.clone());
        app.set_compose_from_label(if display_name == source.account_email {
            source.account_email.clone().into()
        } else {
            format!("{display_name}  <{}>", source.account_email).into()
        });
        app.set_compose_to(prepared.to.into());
        app.set_compose_cc(prepared.cc.into());
        app.set_compose_bcc("".into());
        app.set_compose_subject(prepared.subject.into());
        {
            let mut document = document_for_message_compose.borrow_mut();
            let selection = document.synchronize(&prepared.body, 0, 0);
            editor_for_message_compose.borrow_mut().reset();
            apply_rich_compose(
                &app,
                &document,
                selection,
                &mut editor_for_message_compose.borrow_mut(),
            );
        }
        *intent_for_message_compose.borrow_mut() = prepared.intent;
        app.set_compose_mode(action);
        app.set_compose_open(true);
    });

    let app_weak = app.as_weak();
    let files_for_close = Rc::clone(&compose_files);
    let document_for_close = Rc::clone(&compose_document);
    let editor_for_close = Rc::clone(&compose_editor);
    let contacts_for_close = Rc::clone(&compose_contacts);
    let intent_for_close = Rc::clone(&compose_intent);
    app.on_close_compose(move || {
        if let Some(app) = app_weak.upgrade() {
            clear_compose(
                &app,
                &files_for_close,
                &document_for_close,
                &editor_for_close,
                &contacts_for_close,
            );
            *intent_for_close.borrow_mut() = ComposeIntent::default();
        }
    });

    let app_weak = app.as_weak();
    let files_for_picker = Rc::clone(&compose_files);
    app.on_pick_compose_attachments(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Some(paths) = pick_compose_attachment_paths(
            translated(&app, &UiMessage::plain("Attach files to this email")).to_string(),
        ) else {
            #[cfg(any(target_os = "android", target_os = "ios"))]
            {
                app.set_compose_notice(UiMessage::plain(
                    "The platform host does not provide a document picker.",
                ));
                app.set_compose_notice_is_error(true);
            }
            return;
        };

        let mut compose_files = files_for_picker.borrow_mut();
        let mut total_size = compose_files.iter().map(|file| file.size).sum::<u64>();
        let mut added = 0usize;
        let mut skipped = 0usize;
        for path in paths {
            let path = std::fs::canonicalize(&path).unwrap_or(path);
            if compose_files.iter().any(|file| file.path == path) {
                skipped += 1;
                continue;
            }
            let Ok(metadata) = std::fs::metadata(&path) else {
                skipped += 1;
                continue;
            };
            if !metadata.is_file()
                || total_size.saturating_add(metadata.len()) > MAX_COMPOSE_ATTACHMENT_BYTES
            {
                skipped += 1;
                continue;
            }
            let Some(filename) = path.file_name().map(|name| name.to_string_lossy().into_owned())
            else {
                skipped += 1;
                continue;
            };
            total_size += metadata.len();
            compose_files.push(ComposeFile {
                path,
                filename,
                size: metadata.len(),
            });
            added += 1;
        }
        apply_compose_files(&app, &compose_files);
        if skipped == 0 {
            app.set_compose_notice(if added == 1 {
                UiMessage::plain("Added 1 attachment.")
            } else {
                UiMessage::detail("Added {} attachments.", added)
            });
            app.set_compose_notice_is_error(false);
        } else {
            app.set_compose_notice(UiMessage::arguments(
                "Attachments added: {}. Skipped: {} (duplicate, unreadable, or over the 25 MB total limit).",
                added,
                skipped,
            ));
            app.set_compose_notice_is_error(added == 0);
        }
    });

    let app_weak = app.as_weak();
    let files_for_remove = Rc::clone(&compose_files);
    app.on_remove_compose_attachment(move |index| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Ok(index) = usize::try_from(index) else {
            return;
        };
        let mut files = files_for_remove.borrow_mut();
        if index < files.len() {
            files.remove(index);
            apply_compose_files(&app, &files);
            app.set_compose_notice(UiMessage::plain("Attachment removed."));
            app.set_compose_notice_is_error(false);
        }
    });

    let app_weak = app.as_weak();
    let document_for_edit = Rc::clone(&compose_document);
    let editor_for_edit = Rc::clone(&compose_editor);
    app.on_edit_compose_body(move |body, anchor, cursor| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut document = document_for_edit.borrow_mut();
        let selection = document.synchronize(body.as_str(), anchor, cursor);
        apply_rich_compose(
            &app,
            &document,
            selection,
            &mut editor_for_edit.borrow_mut(),
        );
    });

    let app_weak = app.as_weak();
    let document_for_selection = Rc::clone(&compose_document);
    let editor_for_selection = Rc::clone(&compose_editor);
    app.on_update_compose_selection(move |anchor, cursor| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut document = document_for_selection.borrow_mut();
        let selection = document.update_selection(anchor, cursor);
        apply_rich_compose_state(&app, &document, selection);
        apply_compose_editor_surface(
            &app,
            &document,
            selection,
            &mut editor_for_selection.borrow_mut(),
        );
    });

    let app_weak = app.as_weak();
    let document_for_format = Rc::clone(&compose_document);
    let editor_for_format = Rc::clone(&compose_editor);
    app.on_format_compose(move |kind, body, anchor, cursor| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut document = document_for_format.borrow_mut();
        let selection = document.format(kind.as_str(), body.as_str(), anchor, cursor);
        apply_rich_compose(
            &app,
            &document,
            selection,
            &mut editor_for_format.borrow_mut(),
        );
    });

    let app_weak = app.as_weak();
    let document_for_link = Rc::clone(&compose_document);
    let editor_for_link = Rc::clone(&compose_editor);
    app.on_link_compose(move |url, body, anchor, cursor| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut document = document_for_link.borrow_mut();
        let selection = document.set_link(url.as_str(), body.as_str(), anchor, cursor);
        apply_rich_compose(
            &app,
            &document,
            selection,
            &mut editor_for_link.borrow_mut(),
        );
    });

    let app_weak = app.as_weak();
    let document_for_history = Rc::clone(&compose_document);
    let editor_for_history = Rc::clone(&compose_editor);
    app.on_compose_history(move |direction| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let mut document = document_for_history.borrow_mut();
        if let Some(selection) = document.history(direction.as_str()) {
            apply_rich_compose(
                &app,
                &document,
                selection,
                &mut editor_for_history.borrow_mut(),
            );
        }
    });

    let app_weak = app.as_weak();
    let document_for_editor_pointer = Rc::clone(&compose_document);
    let editor_for_pointer = Rc::clone(&compose_editor);
    app.on_compose_editor_pointer_event(move |x, y, kind, _control, shift, _alt, _meta| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let width = app.get_compose_editor_width().max(1.0);
        let scale_factor = app.window().scale_factor();
        let style = compose_editor_style(&app);
        let preedit_text = app.get_compose_editor_preedit_text();
        let next_selection = {
            let document = document_for_editor_pointer.borrow();
            editor_for_pointer.borrow_mut().handle_pointer(
                &document,
                document.selection(),
                width,
                scale_factor,
                style,
                preedit_text.as_str(),
                x,
                y,
                kind.as_str(),
                shift,
            )
        };
        let Some(next_selection) = next_selection else {
            return;
        };
        let mut document = document_for_editor_pointer.borrow_mut();
        let selection = document.update_selection(next_selection.start, next_selection.end);
        apply_rich_compose_state(&app, &document, selection);
        apply_compose_editor_surface(
            &app,
            &document,
            selection,
            &mut editor_for_pointer.borrow_mut(),
        );
    });

    let app_weak = app.as_weak();
    let document_for_editor_key = Rc::clone(&compose_document);
    let editor_for_key = Rc::clone(&compose_editor);
    app.on_compose_editor_key_event(move |key, control, shift, alt, meta| {
        let Some(app) = app_weak.upgrade() else {
            return false;
        };
        let width = app.get_compose_editor_width().max(1.0);
        let scale_factor = app.window().scale_factor();
        let style = compose_editor_style(&app);
        let preedit_text = app.get_compose_editor_preedit_text();
        let next_selection = {
            let document = document_for_editor_key.borrow();
            editor_for_key.borrow_mut().handle_navigation(
                &document,
                document.selection(),
                width,
                scale_factor,
                style,
                preedit_text.as_str(),
                key.as_str(),
                control,
                shift,
                alt,
                meta,
            )
        };
        let Some(next_selection) = next_selection else {
            return false;
        };
        let mut document = document_for_editor_key.borrow_mut();
        let selection = document.update_selection(next_selection.start, next_selection.end);
        apply_rich_compose_state(&app, &document, selection);
        apply_compose_editor_surface(&app, &document, selection, &mut editor_for_key.borrow_mut());
        true
    });

    let app_weak = app.as_weak();
    let document_for_editor_layout = Rc::clone(&compose_document);
    let editor_for_layout = Rc::clone(&compose_editor);
    app.on_compose_editor_layout_changed(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let Ok(document) = document_for_editor_layout.try_borrow() else {
            return;
        };
        let Ok(mut editor) = editor_for_layout.try_borrow_mut() else {
            return;
        };
        apply_compose_editor_surface(&app, &document, document.selection(), &mut editor);
    });

    let app_weak = app.as_weak();
    let state_for_contacts = Rc::clone(&state);
    let runtime_for_contacts = Rc::clone(&runtime);
    let contacts_for_search = Rc::clone(&compose_contacts);
    app.on_search_compose_contacts(move |recipients, account_id| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let query = compose_recipient_query(recipients.as_str());
        if query.is_empty() {
            contacts_for_search.borrow_mut().clear();
            apply_compose_contacts(&app, &[]);
            return;
        }
        let Some(core) = state_for_contacts.borrow().core.clone() else {
            contacts_for_search.borrow_mut().clear();
            apply_compose_contacts(&app, &[]);
            return;
        };
        let account_id = (account_id > 0).then_some(i64::from(account_id));
        match runtime_for_contacts.block_on(core.list_contacts(query, account_id, 6)) {
            Ok(mut contacts) => {
                let existing = recipients.to_lowercase();
                contacts.retain(|contact| !existing.contains(&contact.email.to_lowercase()));
                *contacts_for_search.borrow_mut() = contacts;
                apply_compose_contacts(&app, &contacts_for_search.borrow());
            }
            Err(error) => {
                contacts_for_search.borrow_mut().clear();
                apply_compose_contacts(&app, &[]);
                app.set_compose_notice(UiMessage::detail("Could not load contacts: {}", error));
                app.set_compose_notice_is_error(true);
            }
        }
    });

    let app_weak = app.as_weak();
    let contacts_for_select = Rc::clone(&compose_contacts);
    app.on_select_compose_contact(move |index, current_recipients| {
        let Some(app) = app_weak.upgrade() else {
            return "".into();
        };
        let Ok(index) = usize::try_from(index) else {
            return current_recipients;
        };
        let Some(contact) = contacts_for_select.borrow().get(index).cloned() else {
            return current_recipients;
        };
        let recipients = complete_compose_recipient(current_recipients.as_str(), &contact);
        contacts_for_select.borrow_mut().clear();
        apply_compose_contacts(&app, &[]);
        recipients.into()
    });

    let app_weak = app.as_weak();
    let state_for_compose = Rc::clone(&state);
    let runtime_for_compose = Rc::clone(&runtime);
    let files_for_save = Rc::clone(&compose_files);
    let document_for_save = Rc::clone(&compose_document);
    let editor_for_save = Rc::clone(&compose_editor);
    let contacts_for_save = Rc::clone(&compose_contacts);
    let intent_for_save = Rc::clone(&compose_intent);
    app.on_save_compose(move |account_id, to, cc, bcc, subject, body, send| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        if account_id <= 0 {
            app.set_compose_notice(UiMessage::plain(
                "Choose the account this message should be sent from.",
            ));
            app.set_compose_notice_is_error(true);
            return;
        }
        let Some(core) = state_for_compose.borrow().core.clone() else {
            let message = UiMessage::plain("Connect an account before saving or sending mail.");
            app.set_compose_notice(message.clone());
            app.set_compose_notice_is_error(true);
            app.set_sync_status(message);
            return;
        };
        let attachments = files_for_save
            .borrow()
            .iter()
            .map(|file| DraftAttachmentIn {
                file_path: file.path.to_string_lossy().into_owned(),
                filename: file.filename.clone(),
            })
            .collect::<Vec<_>>();
        let body_html = {
            let mut document = document_for_save.borrow_mut();
            if document.text() != body.as_str() {
                document.synchronize(body.as_str(), body.len() as i32, body.len() as i32);
            }
            document.body_html()
        };
        let intent = intent_for_save.borrow().clone();
        let result = if send {
            runtime_for_compose
                .block_on(core.send_new_message(ComposeMessage {
                    draft_id: intent.draft_id,
                    account_id: i64::from(account_id),
                    to: to.as_str(),
                    cc: cc.as_str(),
                    bcc: bcc.as_str(),
                    subject: subject.as_str(),
                    body: body.as_str(),
                    body_html: body_html.as_deref(),
                    attachments: &attachments,
                    mode: intent.mode.as_str(),
                    in_reply_to_message_id: intent.in_reply_to_message_id,
                }))
                .map(|queued| {
                    UiMessage::detail("Message queued for delivery (action {}).", queued.action_id)
                })
        } else {
            runtime_for_compose
                .block_on(core.save_new_draft(ComposeMessage {
                    draft_id: intent.draft_id,
                    account_id: i64::from(account_id),
                    to: to.as_str(),
                    cc: cc.as_str(),
                    bcc: bcc.as_str(),
                    subject: subject.as_str(),
                    body: body.as_str(),
                    body_html: body_html.as_deref(),
                    attachments: &attachments,
                    mode: intent.mode.as_str(),
                    in_reply_to_message_id: intent.in_reply_to_message_id,
                }))
                .map(|draft_id| UiMessage::detail("Draft saved locally (#{}).", draft_id))
        };
        match result {
            Ok(message) => {
                clear_compose(
                    &app,
                    &files_for_save,
                    &document_for_save,
                    &editor_for_save,
                    &contacts_for_save,
                );
                *intent_for_save.borrow_mut() = ComposeIntent::default();
                app.set_sync_status(message);
                let _ =
                    refresh_from_source(&app, &state_for_compose, &runtime_for_compose, true, &[]);
            }
            Err(error) => {
                let message = UiMessage::detail("Compose failed: {}", error);
                app.set_compose_notice(message.clone());
                app.set_compose_notice_is_error(true);
                app.set_sync_status(message);
            }
        }
    });

    let app_weak = app.as_weak();
    let state_for_account = Rc::clone(&state);
    let runtime_for_account = Rc::clone(&runtime);
    let ui_task_tx_for_account = ui_task_tx.clone();
    app.on_add_password_account(
        move |protocol,
              email,
              username,
              password,
              jmap_url,
              imap_host,
              imap_port,
              smtp_host,
              smtp_port| {
            let Some(app) = app_weak.upgrade() else {
                return;
            };
            let mail_protocol = MailProtocol::from_storage(protocol.as_str());
            let imap_port = if mail_protocol == MailProtocol::Jmap {
                993
            } else {
                match imap_port.trim().parse::<u16>() {
                    Ok(port) => port,
                    Err(_) => {
                        app.set_sync_status(UiMessage::plain("IMAP port must be a number."));
                        return;
                    }
                }
            };
            let smtp_port = if mail_protocol == MailProtocol::Jmap {
                465
            } else {
                match smtp_port.trim().parse::<u16>() {
                    Ok(port) => port,
                    Err(_) => {
                        app.set_sync_status(UiMessage::plain("SMTP port must be a number."));
                        return;
                    }
                }
            };
            let Some(core) = state_for_account.borrow().core.clone() else {
                app.set_sync_status(UiMessage::plain(
                    "Local mail data is unavailable. Retry startup.",
                ));
                return;
            };
            app.set_sync_status(if mail_protocol == MailProtocol::Jmap {
                UiMessage::plain("Discovering JMAP and checking capabilities…")
            } else {
                UiMessage::plain("Testing IMAP connection…")
            });
            let args = AddPasswordAccountArgs {
                email: email.to_string(),
                display_name: None,
                username: username.to_string(),
                password: password.to_string(),
                mail_protocol,
                jmap_url: jmap_url.to_string(),
                imap_host: imap_host.to_string(),
                imap_port,
                smtp_host: smtp_host.to_string(),
                smtp_port,
            };
            let updates = ui_task_tx_for_account.clone();
            runtime_for_account.spawn(async move {
                let update = match core.add_password_account(args).await {
                    Ok(account) => {
                        // The account actor starts its first sync as soon as it
                        // is created. Waiting for that potentially enormous
                        // mailbox here would only hold the setup UI hostage.
                        let accounts = match (
                            core.load_accounts().await,
                            core.load_account_configs().await,
                        ) {
                            (Ok(accounts), Ok(configs)) => Some((accounts, configs)),
                            _ => None,
                        };
                        UiTaskUpdate {
                            message: UiMessage::detail(
                                "Connected {}. Initial sync is running in the background.",
                                account.email,
                            ),
                            accounts,
                            calendar_connections: None,
                            calendar_error: None,
                            clear_account_form: true,
                            finishes_oauth: false,
                            close_to_tray: None,
                        }
                    }
                    Err(error) => UiTaskUpdate {
                        message: UiMessage::detail("Account setup failed: {}", error),
                        accounts: None,
                        calendar_connections: None,
                        calendar_error: None,
                        clear_account_form: false,
                        finishes_oauth: false,
                        close_to_tray: None,
                    },
                };
                let _ = updates.send(update).await;
            });
        },
    );

    let app_weak = app.as_weak();
    let state_for_oauth = Rc::clone(&state);
    let runtime_for_oauth = Rc::clone(&runtime);
    let ui_task_tx_for_oauth = ui_task_tx.clone();
    app.on_start_oauth(move |provider| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        if app.get_oauth_in_progress() {
            app.set_sync_status(UiMessage::plain(
                "Another browser authorization is already in progress; finish it or choose Cancel.",
            ));
            return;
        }
        let provider = Provider::from_storage(provider.as_str());
        if provider == Provider::Imap {
            app.set_sync_status(UiMessage::plain("Choose Gmail or Outlook OAuth."));
            return;
        }
        let Some(core) = state_for_oauth.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Local mail data is unavailable. Retry startup.",
            ));
            return;
        };
        app.set_oauth_in_progress(true);
        let connect_calendar = app.get_connect_calendar_on_add();
        app.set_sync_status(UiMessage::plain(
            "Complete sign-in in your browser, or choose Cancel here.",
        ));
        let updates = ui_task_tx_for_oauth.clone();
        runtime_for_oauth.spawn(async move {
            let result = core
                .start_oauth_with_calendar(provider, connect_calendar, |url| {
                    webbrowser::open(&url).map_err(|error| error.to_string())
                })
                .await;
            let update = match result {
                Ok(account) => {
                    let accounts = match (
                        core.load_accounts().await,
                        core.load_account_configs().await,
                    ) {
                        (Ok(accounts), Ok(configs)) => Some((accounts, configs)),
                        _ => None,
                    };
                    UiTaskUpdate {
                        message: UiMessage::detail(
                            "Connected {}. Initial sync is running in the background.",
                            account.email,
                        ),
                        accounts,
                        calendar_connections: core.load_calendar_connections().await.ok(),
                        calendar_error: None,
                        clear_account_form: false,
                        finishes_oauth: true,
                        close_to_tray: None,
                    }
                }
                Err(error) => UiTaskUpdate {
                    message: UiMessage::detail("OAuth failed: {}", error),
                    accounts: None,
                    calendar_connections: None,
                    calendar_error: None,
                    clear_account_form: false,
                    finishes_oauth: true,
                    close_to_tray: None,
                },
            };
            let _ = updates.send(update).await;
        });
    });

    let app_weak = app.as_weak();
    let state_for_reauth = Rc::clone(&state);
    let runtime_for_reauth = Rc::clone(&runtime);
    let ui_task_tx_for_reauth = ui_task_tx.clone();
    app.on_reauth_account(move |account_id| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        if app.get_oauth_in_progress() {
            app.set_sync_status(UiMessage::plain(
                "Another browser authorization is already in progress; finish it or choose Cancel.",
            ));
            return;
        }
        let Some(core) = state_for_reauth.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Local mail data is unavailable. Retry startup.",
            ));
            return;
        };
        app.set_oauth_in_progress(true);
        app.set_sync_status(UiMessage::plain(
            "Complete sign-in in your browser, or choose Cancel here.",
        ));
        let updates = ui_task_tx_for_reauth.clone();
        runtime_for_reauth.spawn(async move {
            let result = core
                .reauth_account(i64::from(account_id), |url| {
                    webbrowser::open(&url).map_err(|error| error.to_string())
                })
                .await;
            let update = match result {
                Ok(account) => {
                    let accounts = match (
                        core.load_accounts().await,
                        core.load_account_configs().await,
                    ) {
                        (Ok(accounts), Ok(configs)) => Some((accounts, configs)),
                        _ => None,
                    };
                    UiTaskUpdate {
                        message: UiMessage::detail(
                            "Reconnected {}. Synchronization is resuming.",
                            account.email,
                        ),
                        accounts,
                        calendar_connections: core.load_calendar_connections().await.ok(),
                        calendar_error: None,
                        clear_account_form: false,
                        finishes_oauth: true,
                        close_to_tray: None,
                    }
                }
                Err(error) => UiTaskUpdate {
                    message: UiMessage::detail("Reconnection failed: {}", error),
                    accounts: None,
                    calendar_connections: None,
                    calendar_error: None,
                    clear_account_form: false,
                    finishes_oauth: true,
                    close_to_tray: None,
                },
            };
            let _ = updates.send(update).await;
        });
    });

    let app_weak = app.as_weak();
    let state_for_oauth_cancel = Rc::clone(&state);
    app.on_cancel_oauth(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        if let Some(core) = state_for_oauth_cancel.borrow().core.as_ref() {
            core.cancel_oauth();
            app.set_sync_status(UiMessage::plain("Cancelling browser sign-in…"));
        }
    });

    let app_weak = app.as_weak();
    let state_for_calendar_connect = Rc::clone(&state);
    let runtime_for_calendar_connect = Rc::clone(&runtime);
    let updates_for_calendar_connect = ui_task_tx.clone();
    app.on_connect_account_calendar(move |account_id| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        if app.get_oauth_in_progress() {
            app.set_sync_status(UiMessage::plain(
                "Another browser authorization is already in progress; finish it or choose Cancel.",
            ));
            return;
        }
        let (core, provider) = {
            let state = state_for_calendar_connect.borrow();
            let Some(core) = state.core.clone() else {
                app.set_sync_status(UiMessage::plain("Calendar core is unavailable."));
                return;
            };
            let Some(provider) = state
                .connected_accounts
                .iter()
                .find(|account| account.id == i64::from(account_id))
                .map(|account| account.provider)
            else {
                app.set_sync_status(UiMessage::plain("Account no longer exists."));
                return;
            };
            (core, provider)
        };
        app.set_oauth_in_progress(true);
        app.set_sync_status(UiMessage::plain(
            "Authorize calendar access in your browser.",
        ));
        let updates = updates_for_calendar_connect.clone();
        runtime_for_calendar_connect.spawn(async move {
            let result = core
                .connect_provider_calendar(i64::from(account_id), provider, |url| {
                    webbrowser::open(&url).map_err(|error| error.to_string())
                })
                .await;
            let connections = core.load_calendar_connections().await.ok();
            let calendar_error = Some((i64::from(account_id), result.as_ref().err().cloned()));
            let message = match result {
                Ok(_) => UiMessage::plain("Calendar connected and initial sync started."),
                Err(error) => UiMessage::detail("Could not connect calendar: {}", error),
            };
            let _ = updates
                .send(UiTaskUpdate {
                    message,
                    accounts: None,
                    calendar_connections: connections,
                    calendar_error,
                    clear_account_form: false,
                    finishes_oauth: true,
                    close_to_tray: None,
                })
                .await;
        });
    });

    let state_for_calendar_toggle = Rc::clone(&state);
    let runtime_for_calendar_toggle = Rc::clone(&runtime);
    let updates_for_calendar_toggle = ui_task_tx.clone();
    app.on_set_account_calendar_enabled(move |account_id, enabled| {
        let Some(core) = state_for_calendar_toggle.borrow().core.clone() else {
            return;
        };
        let updates = updates_for_calendar_toggle.clone();
        runtime_for_calendar_toggle.spawn(async move {
            let message = match core
                .set_account_calendar_enabled(i64::from(account_id), enabled)
                .await
            {
                Ok(_) => {
                    if enabled {
                        UiMessage::plain("Calendar sync enabled.")
                    } else {
                        UiMessage::plain("Calendar sync paused.")
                    }
                }
                Err(error) => UiMessage::detail("Could not update calendar sync: {}", error),
            };
            let connections = core.load_calendar_connections().await.ok();
            let _ = updates
                .send(UiTaskUpdate {
                    message,
                    accounts: None,
                    calendar_connections: connections,
                    calendar_error: None,
                    clear_account_form: false,
                    finishes_oauth: false,
                    close_to_tray: None,
                })
                .await;
        });
    });

    let state_for_caldav = Rc::clone(&state);
    let runtime_for_caldav = Rc::clone(&runtime);
    let updates_for_caldav = ui_task_tx.clone();
    app.on_connect_caldav(move |account_id, url, username, password| {
        let Some(core) = state_for_caldav.borrow().core.clone() else {
            return;
        };
        let updates = updates_for_caldav.clone();
        runtime_for_caldav.spawn(async move {
            let message = match core
                .connect_caldav(
                    i64::from(account_id),
                    url.to_string(),
                    username.to_string(),
                    password.to_string(),
                )
                .await
            {
                Ok(_) => UiMessage::plain("CalDAV connected and initial sync started."),
                Err(error) => UiMessage::detail("Could not connect CalDAV: {}", error),
            };
            let connections = core.load_calendar_connections().await.ok();
            let _ = updates
                .send(UiTaskUpdate {
                    message,
                    accounts: None,
                    calendar_connections: connections,
                    calendar_error: None,
                    clear_account_form: false,
                    finishes_oauth: false,
                    close_to_tray: None,
                })
                .await;
        });
    });

    let app_weak = app.as_weak();
    let state_for_oauth_settings = Rc::clone(&state);
    let runtime_for_oauth_settings = Rc::clone(&runtime);
    app.on_save_oauth_app(move |provider, client_id, client_secret| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let provider = Provider::from_storage(provider.as_str());
        let Some(core) = state_for_oauth_settings.borrow().core.clone() else {
            app.set_sync_status(UiMessage::plain(
                "Local mail data is unavailable. Retry startup.",
            ));
            return;
        };
        match runtime_for_oauth_settings.block_on(core.set_oauth_app(
            provider,
            client_id.as_str(),
            client_secret.as_str(),
        )) {
            Ok(()) => {
                app.set_custom_oauth_configured(
                    !app.get_google_client_id().as_str().trim().is_empty()
                        || !app.get_ms_client_id().as_str().trim().is_empty(),
                );
                app.set_sync_status(UiMessage::detail(
                    "{} OAuth settings saved.",
                    provider.as_str(),
                ))
            }
            Err(error) => {
                app.set_sync_status(UiMessage::detail("OAuth settings failed: {}", error))
            }
        }
    });

    register_data_management_callbacks(
        &app,
        &state,
        &runtime,
        &ui_task_tx,
        &contact_state,
        &contacts_loaded,
        &contacts_loading,
        &contact_load_generation,
        &calendar_state,
    );

    register_renderer_input_callbacks(&app, &email_renderer, use_wgpu);

    let app_weak = app.as_weak();
    let state_for_search = Rc::clone(&state);
    let runtime_for_search = Rc::clone(&runtime);
    let mail_pagination_generation_for_search = Rc::clone(&mail_pagination_generation);
    let mail_pagination_in_progress_for_search = Rc::clone(&mail_pagination_in_progress);
    app.on_search_changed(move |query| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        mail_pagination_generation_for_search
            .set(mail_pagination_generation_for_search.get().wrapping_add(1));
        mail_pagination_in_progress_for_search.set(false);
        app.set_mail_loading_more(false);
        {
            let mut state = state_for_search.borrow_mut();
            state.query = query.to_string();
            state.page = 1;
            state.next_cursor = None;
            state.selected_id = None;
            state.checked_ids.clear();
            state.preview_closed = false;
        }
        if let Err(error) =
            refresh_from_source(&app, &state_for_search, &runtime_for_search, false, &[])
        {
            app.set_render_status(UiMessage::detail("Mail refresh failed: {}", error));
        }
    });

    let app_weak = app.as_weak();
    let state_for_filter = Rc::clone(&state);
    let runtime_for_filter = Rc::clone(&runtime);
    app.on_set_search_filter(move |filter| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        {
            let mut state = state_for_filter.borrow_mut();
            state.search_filter = filter.to_string();
            state.page = 1;
            state.selected_id = None;
            state.checked_ids.clear();
        }
        if let Err(error) = render_current(&app, &state_for_filter, &runtime_for_filter) {
            app.set_render_status(UiMessage::detail("Mail filter failed: {}", error));
        }
    });

    let app_weak = app.as_weak();
    let state_for_scope = Rc::clone(&state);
    let runtime_for_scope = Rc::clone(&runtime);
    let mail_metadata_refresh_requested_for_scope = Rc::clone(&mail_metadata_refresh_requested);
    let mail_pagination_generation_for_scope = Rc::clone(&mail_pagination_generation);
    let mail_pagination_in_progress_for_scope = Rc::clone(&mail_pagination_in_progress);
    app.on_select_scope(move |scope| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        mail_pagination_generation_for_scope
            .set(mail_pagination_generation_for_scope.get().wrapping_add(1));
        mail_pagination_in_progress_for_scope.set(false);
        app.set_mail_loading_more(false);
        {
            let mut state = state_for_scope.borrow_mut();
            state.scope = scope.to_string();
            state.page = 1;
            state.next_cursor = None;
            state.selected_id = None;
            state.checked_ids.clear();
            state.preview_closed = false;
        }
        if let Err(error) =
            refresh_from_source(&app, &state_for_scope, &runtime_for_scope, false, &[])
        {
            app.set_render_status(UiMessage::detail("Mail refresh failed: {}", error));
        }
        mail_metadata_refresh_requested_for_scope.set(true);
        app.invoke_drain_core_updates();
    });

    let app_weak = app.as_weak();
    let state_for_more = Rc::clone(&state);
    let runtime_for_more = Rc::clone(&runtime);
    let mail_list_tx_for_more = mail_list_tx.clone();
    let mail_pagination_generation_for_more = Rc::clone(&mail_pagination_generation);
    let mail_pagination_in_progress_for_more = Rc::clone(&mail_pagination_in_progress);
    app.on_load_more(move || {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        if mail_pagination_in_progress_for_more.replace(true) {
            return;
        }
        let (using_core, core, scope, query, cursor) = {
            let state = state_for_more.borrow();
            (
                state.using_core,
                state.core.clone(),
                state.scope.clone(),
                state.query.clone(),
                state.next_cursor,
            )
        };

        if !using_core {
            let mut state = state_for_more.borrow_mut();
            state.page = state.page.saturating_add(1);
            drop(state);
            refresh_rows_only(&app, &state_for_more, &runtime_for_more);
            refresh_list_metadata(&app, &state_for_more);
            app.set_mail_list_revision(app.get_mail_list_revision().wrapping_add(1));
            mail_pagination_in_progress_for_more.set(false);
            return;
        }
        let (Some(core), Some(cursor)) = (core, cursor) else {
            mail_pagination_in_progress_for_more.set(false);
            refresh_list_metadata(&app, &state_for_more);
            return;
        };

        let generation = mail_pagination_generation_for_more.get();
        app.set_mail_loading_more(true);
        let updates = mail_list_tx_for_more.clone();
        runtime_for_more.spawn(async move {
            let result = core
                .load_page(&scope, &query, Some(cursor), PAGE_SIZE as i64, false)
                .await;
            let _ = updates
                .send(MailListUpdate {
                    scope,
                    query,
                    kind: MailListUpdateKind::Pagination { cursor, generation },
                    result,
                })
                .await;
        });
    });

    let app_weak = app.as_weak();
    let state_for_selection = Rc::clone(&state);
    let runtime_for_selection = Rc::clone(&runtime);
    app.on_select_email(move |id| {
        let Some(app) = app_weak.upgrade() else {
            return;
        };
        let should_mark_read = {
            let state = state_for_selection.borrow();
            state.mark_read_on_open
                && state.using_core
                && state.messages.iter().any(|message| {
                    message.id == id && message.unread && message.thread_id.is_some()
                })
        };
        match select_message(&app, &state_for_selection, &runtime_for_selection, id) {
            Ok(_) => {}
            Err(error) => {
                app.set_render_status(UiMessage::detail("Message load failed: {}", error));
                return;
            }
        };
        if should_mark_read
            && let Err(error) = perform_selected_action(
                &app,
                &state_for_selection,
                &runtime_for_selection,
                "mark_read",
            )
        {
            app.set_render_status(UiMessage::detail(
                "Could not mark message as read: {}",
                error,
            ));
        }
    });

    // The main window is already shown above and the independently compiled
    // tray participates in the same process-wide event loop. This is Slint's
    // documented multi-component pattern; calling AppWindow::run() here would
    // redundantly show the main window a second time.
    slint::run_event_loop()?;
    Ok(())
}

/// Put the bundled outline emoji face in Parley's dedicated emoji slot.
///
/// Slint's software renderer does not reliably rasterize the color emoji fonts
/// supplied by every platform. Parley already detects emoji clusters while
/// shaping mixed text, so configuring its generic family keeps the normal UI
/// font and emoji font separate without rewriting folder names into image runs.
fn configure_emoji_font_fallback() -> Result<(), Box<dyn std::error::Error>> {
    use slint::fontique_010::fontique::{Blob, GenericFamily};

    const NOTO_EMOJI: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/resources/fonts/noto-emoji/NotoEmoji[wght].ttf"
    ));

    let mut fonts = slint::fontique_010::shared_collection();
    let noto_emoji = fonts
        .register_fonts(Blob::new(Arc::new(NOTO_EMOJI)), None)
        .first()
        .map(|(family, _)| *family)
        .ok_or_else(|| std::io::Error::other("bundled Noto Emoji font is invalid"))?;
    fonts.set_generic_families(GenericFamily::Emoji, std::iter::once(noto_emoji));
    Ok(())
}

#[cfg(test)]
mod calendar_tests {
    use super::*;

    #[test]
    fn week_starts_on_monday() {
        let sunday = NaiveDate::from_ymd_opt(2026, 8, 23).unwrap();
        assert_eq!(
            start_of_week(sunday),
            NaiveDate::from_ymd_opt(2026, 8, 17).unwrap()
        );
    }

    #[test]
    fn month_navigation_crosses_year_boundaries() {
        let december = NaiveDate::from_ymd_opt(2026, 12, 1).unwrap();
        assert_eq!(
            shift_month(december, 1),
            NaiveDate::from_ymd_opt(2027, 1, 1).unwrap()
        );
        assert_eq!(
            shift_month(december, -12),
            NaiveDate::from_ymd_opt(2025, 12, 1).unwrap()
        );
    }

    #[test]
    fn local_calendar_starts_empty_until_database_load() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 23).unwrap();
        let state = LocalCalendarState::new(today);
        assert!(state.events.is_empty());
        assert_eq!(state.selected_date, today);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_values(model: &VecModel<i32>) -> Vec<i32> {
        model.iter().collect()
    }

    #[test]
    fn retained_list_model_reconciles_pages_and_live_head_changes() {
        let model = VecModel::from(vec![1, 2, 3]);

        reconcile_model_rows(&model, vec![1, 2, 3, 4, 5], |value| *value);
        assert_eq!(model_values(&model), [1, 2, 3, 4, 5]);

        // A new first row and a bounded refresh that drops the old tail are
        // the combination that used to make a scrolled ListView jump.
        reconcile_model_rows(&model, vec![0, 1, 2, 3, 4], |value| *value);
        assert_eq!(model_values(&model), [0, 1, 2, 3, 4]);

        reconcile_model_rows(&model, vec![0, 2, 3, 4], |value| *value);
        assert_eq!(model_values(&model), [0, 2, 3, 4]);
    }

    #[test]
    fn checked_trigger_expands_mail_operations_in_visible_order() {
        let messages = fixture_messages();
        let checked = HashSet::from([messages[2].id, messages[0].id]);

        assert_eq!(
            mail_operation_ids(&messages, &checked, messages[0].id),
            [messages[0].id, messages[2].id]
        );
    }

    #[test]
    fn unchecked_trigger_keeps_mail_operation_single() {
        let messages = fixture_messages();
        let checked = HashSet::from([messages[0].id, messages[1].id]);

        assert_eq!(
            mail_operation_ids(&messages, &checked, messages[2].id),
            [messages[2].id]
        );
    }

    #[test]
    fn unified_inbox_combines_only_incoming_messages() {
        let messages = fixture_messages();
        let filtered = filtered_messages(&messages, "Unified Inbox", "", "All mail");

        assert_eq!(filtered.len(), 8);
        assert!(filtered.iter().all(|email| email.folder == "Inbox"));
        assert!(filtered.iter().any(|email| email.account == "Personal"));
        assert!(filtered.iter().any(|email| email.account == "Flectar"));
    }

    #[test]
    fn folder_scope_and_search_can_be_combined() {
        let messages = fixture_messages();
        let filtered =
            filtered_messages(&messages, "Northstar Work / Inbox", "calendar", "All mail");

        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].sender, "Calendar Bot");
    }

    #[test]
    fn unified_folders_and_search_filters_are_applied() {
        let mut messages = fixture_messages();
        messages[0].starred = true;
        messages[1].has_attachments = true;

        let starred = filtered_messages(&messages, "Unified Starred", "", "All mail");
        let attachments = filtered_messages(&messages, "Unified Inbox", "", "Has attachments");

        assert_eq!(starred.len(), 1);
        assert_eq!(starred[0].id, messages[0].id);
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].id, messages[1].id);
    }

    #[test]
    fn every_fixture_account_gets_the_standard_mailboxes_in_order() {
        let mailboxes = fixture_mailboxes(&fixture_messages());
        let account = mailboxes
            .iter()
            .find(|mailbox| mailbox.is_account)
            .expect("fixture account");
        let labels: Vec<&str> = mailboxes
            .iter()
            .filter(|mailbox| !mailbox.is_account && mailbox.context == account.context)
            .take(7)
            .map(|mailbox| mailbox.label.as_str())
            .collect();

        assert_eq!(
            labels,
            [
                "Inbox", "Starred", "Sent", "Archive", "Spam", "Trash", "Drafts"
            ]
        );
    }

    #[test]
    fn account_starred_scope_filters_within_the_account() {
        let mut messages = fixture_messages();
        messages[0].starred = true;
        let scope = format!("{} / Starred", messages[0].account);
        let filtered = filtered_messages(&messages, &scope, "", "All mail");

        assert!(filtered.iter().any(|message| message.id == messages[0].id));
        assert!(
            filtered
                .iter()
                .all(|message| message.account == messages[0].account && message.starred)
        );
    }

    #[test]
    fn empty_mail_preview_has_an_explicit_placeholder() {
        assert_eq!(display_preview(""), "(No content)");
        assert_eq!(display_preview(" \n\t"), "(No content)");
        assert_eq!(display_preview("Message body"), "Message body");
    }

    #[test]
    fn contact_search_uses_only_the_recipient_being_typed() {
        assert_eq!(compose_recipient_query("Ada <ada@example.com>, ma"), "ma");
        assert_eq!(compose_recipient_query("one@example.com;  Lin"), "Lin");
        assert_eq!(compose_recipient_query("   "), "");
    }

    #[test]
    fn selecting_a_contact_replaces_the_active_token() {
        let contact = flectar_mail_core::models::Address {
            name: Some("Maya Chen".to_owned()),
            email: "maya@example.com".to_owned(),
        };

        assert_eq!(
            complete_compose_recipient("ada@example.com, ma", &contact),
            "ada@example.com, Maya Chen <maya@example.com>, "
        );
    }

    #[test]
    fn preview_window_size_uses_logical_phone_dimensions() {
        let size = parse_preview_window_size("390x844").expect("phone preview size");
        assert_eq!((size.width, size.height), (390.0, 844.0));
        assert!(parse_preview_window_size("390-by-844").is_err());
        assert!(parse_preview_window_size("0x844").is_err());
    }

    #[test]
    fn favicon_cache_is_bounded_by_entries_and_decoded_bytes() {
        fn images(bytes: usize) -> FaviconImages {
            FaviconImages {
                small: FaviconImage {
                    width: 1,
                    height: 1,
                    pixels: vec![0; bytes / 2],
                },
                regular: FaviconImage {
                    width: 1,
                    height: 1,
                    pixels: vec![0; bytes - bytes / 2],
                },
            }
        }

        let mut icons = HashMap::new();
        let mut missing = HashSet::new();
        insert_bounded_favicon_result_with_limits(
            &mut icons,
            &mut missing,
            "one.test".into(),
            Some(images(8)),
            2,
            12,
        );
        insert_bounded_favicon_result_with_limits(
            &mut icons,
            &mut missing,
            "two.test".into(),
            Some(images(8)),
            2,
            12,
        );
        assert_eq!(icons.len(), 1);
        assert!(icons.values().map(favicon_bytes).sum::<usize>() <= 12);

        insert_bounded_favicon_result_with_limits(
            &mut icons,
            &mut missing,
            "missing.test".into(),
            None,
            2,
            12,
        );
        insert_bounded_favicon_result_with_limits(
            &mut icons,
            &mut missing,
            "another-missing.test".into(),
            None,
            2,
            12,
        );
        assert!(icons.len() + missing.len() <= 2);
    }
}
