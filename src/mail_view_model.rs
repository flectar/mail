//! Mail list/detail projection, paging, avatars, and rendered-email bridge.

use super::*;

pub(super) fn refresh_from_source(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    preserve_loaded_rows: bool,
    acted_on_ids: &[i32],
) -> Result<(), String> {
    let using_core = state.borrow().using_core;

    if using_core {
        return mail_work::refresh(app, state, runtime, preserve_loaded_rows, acted_on_ids);
    }

    render_current(app, state, runtime)
}

/// Apply a worker-loaded first page without rebuilding the selected email
/// document. Historical Gmail pages can then make newly indexed mail visible
/// while scrolling, selection, and the retained Blitz renderer stay on the UI
/// thread and avoid repeated body preparation.
///
/// The retained tail (everything beyond the refreshed head) is kept as-is
/// without re-querying it, so it can go stale relative to the backend.
/// `drop_ids` names messages known to no longer belong in the current view
/// (e.g. messages the user just archived/spammed/trashed) — the only staleness
/// this function actively corrects rather than a general tail re-sync, which a
/// background refresh has no cheap way to verify. Pass an empty slice when the
/// refresh does not follow a message-moving action.
fn merge_refreshed_mail_head(
    current: &[MailMessage],
    refreshed: Vec<MailMessage>,
    next_cursor: Option<ThreadCursor>,
    drop_ids: &[i32],
) -> (Vec<MailMessage>, bool) {
    if next_cursor.is_none() || current.len() <= PAGE_SIZE {
        return (refreshed, false);
    }

    // The oldest refreshed row is normally present in the retained model.
    // Everything after it is the already-loaded tail and can stay untouched.
    // Looking for the oldest shared row also handles new messages inserted at
    // the head without requiring a growing refresh query.
    let tail_start = refreshed
        .iter()
        .rev()
        .find_map(|fresh| current.iter().position(|old| old.id == fresh.id))
        .map(|index| index + 1)
        .unwrap_or_else(|| PAGE_SIZE.min(current.len()));
    let refreshed_len = refreshed.len();
    let mut merged = refreshed;
    let mut known = merged
        .iter()
        .map(|message| message.id)
        .collect::<HashSet<_>>();
    merged.extend(
        current[tail_start..]
            .iter()
            .filter(|message| known.insert(message.id))
            .filter(|message| !drop_ids.contains(&message.id))
            .cloned(),
    );
    let retained_tail = merged.len() > refreshed_len;
    (merged, retained_tail)
}

pub(super) fn apply_background_mail_page(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    page: mail::MailPage,
    acted_on_ids: &[i32],
) {
    let mail::MailPage {
        mut messages,
        labels,
        mailboxes,
        next_cursor,
        ..
    } = page;
    let selection_removed = {
        let mut state = state.borrow_mut();
        state.labels = labels;
        let selected_detail = state.selected_id.and_then(|selected_id| {
            state
                .messages
                .iter()
                .find(|message| message.id == selected_id && !message.body_pending)
                .cloned()
        });
        if let Some(selected_detail) = selected_detail
            && let Some(summary) = messages
                .iter_mut()
                .find(|message| message.id == selected_detail.id)
        {
            summary.html = selected_detail.html;
            summary.text = selected_detail.text;
            summary.to = selected_detail.to;
            summary.attachments = selected_detail.attachments;
            summary.body_pending = false;
        }

        let old_counts = state
            .mailboxes
            .iter()
            .map(|mailbox| (mailbox.scope.clone(), mailbox.count.clone()))
            .collect::<HashMap<_, _>>();
        let mut mailboxes = mailboxes;
        for mailbox in &mut mailboxes {
            if let Some(count) = old_counts.get(&mailbox.scope) {
                mailbox.count.clone_from(count);
            }
        }

        let old_next_cursor = state.next_cursor;
        let (merged, retained_tail) =
            merge_refreshed_mail_head(&state.messages, messages, next_cursor, acted_on_ids);
        state.messages = merged;
        state.mailboxes = mailboxes;
        state.next_cursor = if retained_tail {
            old_next_cursor
        } else {
            next_cursor
        };
        state.total_count = state.total_count.max(state.messages.len());
        // Check against `rendered_id` (what the reading pane actually shows),
        // not `selected_id`: callers are free to clear or redirect the latter
        // before refreshing, and doing so must not be able to make this look
        // like "nothing needs repainting" when the previously-rendered
        // message is in fact gone.
        let removed = state
            .rendered_id
            .is_some_and(|rendered_id| !state.messages.iter().any(|row| row.id == rendered_id));
        if removed {
            state.selected_id = None;
            state.preview_closed = false;
        }
        removed
    };

    if selection_removed {
        if let Err(error) = render_current(app, state, runtime) {
            app.set_render_status(UiMessage::detail(
                "Background mail refresh failed: {}",
                error,
            ));
        }
    } else {
        refresh_rows_only(app, state, runtime);
        refresh_list_metadata(app, state);
    }
    // A live refresh may change both the list height and its continuation
    // cursor. Let the one-shot viewport check prefetch again when necessary.
    app.set_mail_list_revision(app.get_mail_list_revision().wrapping_add(1));
}

pub(super) fn append_mail_page(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    cursor: ThreadCursor,
    page: mail::MailPage,
) -> Result<(), String> {
    if page.next_cursor == Some(cursor) {
        return Err("mail pagination cursor did not advance".to_owned());
    }

    let mut state_mut = state.borrow_mut();
    let mut known_ids = state_mut
        .messages
        .iter()
        .map(|message| message.id)
        .collect::<HashSet<_>>();
    state_mut.messages.extend(
        page.messages
            .into_iter()
            .filter(|message| known_ids.insert(message.id)),
    );
    state_mut.labels = page.labels;
    state_mut.next_cursor = page.next_cursor;
    drop(state_mut);

    // Pagination changes only the list. Re-preparing the selected HTML body
    // here discarded already decoded images and launched another batch of
    // remote requests; opening one of the newly appended rows could then sit
    // behind those stale requests. Keep the retained Blitz document intact.
    refresh_rows_only(app, state, runtime);
    refresh_list_metadata(app, state);
    app.set_mail_list_revision(app.get_mail_list_revision().wrapping_add(1));
    Ok(())
}

pub(super) fn select_message(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    id: i32,
) -> Result<(), String> {
    {
        let mut state = state.borrow_mut();
        if !state.messages.iter().any(|row| row.id == id) {
            return Ok(());
        }
        state.selected_id = Some(id);
        state.preview_closed = false;
    }
    render_current(app, state, runtime)
}

fn adjacent_message_ids(messages: &[MailMessage], selected_id: Option<i32>) -> (i32, i32) {
    let Some(selected_index) =
        selected_id.and_then(|id| messages.iter().position(|message| message.id == id))
    else {
        return (-1, -1);
    };
    let previous_id = selected_index
        .checked_sub(1)
        .and_then(|index| messages.get(index))
        .map(|message| message.id)
        .unwrap_or(-1);
    let next_id = messages
        .get(selected_index + 1)
        .map(|message| message.id)
        .unwrap_or(-1);
    (previous_id, next_id)
}

pub(super) fn render_current(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
) -> Result<(), String> {
    // Keep list rows lightweight. SQLite already owns the durable body cache.
    {
        let mut state = state.borrow_mut();
        if state.using_core {
            let selected = state.selected_id.filter(|_| !state.preview_closed);
            release_unselected_bodies(&mut state.messages, selected);
        }
    }
    let (email_renderer, use_wgpu) = {
        let state = state.borrow();
        (Rc::clone(&state.email_renderer), state.use_wgpu)
    };
    let (
        messages,
        page,
        scope,
        query,
        selected_id,
        preview_closed,
        mailboxes,
        unified_mailboxes,
        next_cursor,
        using_core,
        favicon_icons,
        search_filter,
        total_count,
        inbox_count,
        labels,
    ) = {
        let state = state.borrow();
        (
            filtered_messages(
                &state.messages,
                &state.scope,
                &state.query,
                &state.search_filter,
            ),
            state.page,
            state.scope.clone(),
            state.query.clone(),
            state.selected_id,
            state.preview_closed,
            state.mailboxes.clone(),
            state.unified_mailboxes.clone(),
            state.next_cursor,
            state.using_core,
            state.favicon_icons.clone(),
            state.search_filter.clone(),
            state.total_count,
            state.inbox_count,
            state.labels.clone(),
        )
    };

    let visible_count = if using_core {
        messages.len()
    } else {
        paged_visible_count(page, messages.len())
    };
    let total_count = if using_core {
        total_count
    } else {
        messages.len()
    };
    let visible = &messages[..visible_count];
    let selected_id = if preview_closed {
        None
    } else {
        selected_id
            .filter(|id| visible.iter().any(|email| email.id == *id))
            .or_else(|| visible.first().map(|email| email.id))
    };
    let selected_email =
        selected_id.and_then(|id| visible.iter().find(|email| email.id == id).cloned());

    let (selection_changed, allow_remote_images, checked_ids) = {
        let mut state = state.borrow_mut();
        let selection_changed = state.rendered_id != selected_id;
        state.selected_id = selected_id;
        state.rendered_id = selected_id;
        state
            .checked_ids
            .retain(|id| visible.iter().any(|message| message.id == *id));
        let allow_remote_images = state.remote_images_enabled
            || selected_id.is_some_and(|id| state.remote_images_override_id == Some(id));
        (
            selection_changed,
            allow_remote_images,
            state.checked_ids.clone(),
        )
    };
    if selection_changed {
        // "View plain text" is a message action, not a global display mode.
        app.set_text_mode(false);
        app.set_source_mode(false);
        app.set_rendering_info_open(false);
    }

    let email_rows = Rc::clone(&state.borrow().email_rows);
    reconcile_model_rows_by(
        &email_rows,
        make_rows(visible, selected_id, &checked_ids, &favicon_icons, &labels),
        |row| row.id,
        same_email_row,
    );
    app.set_mail_selection_count(checked_ids.len() as i32);
    refresh_sidebar(state);
    app.set_selected_scope_title(
        mailbox_scope_title(&scope, &mailboxes, &unified_mailboxes, &labels).into(),
    );
    app.set_selected_scope(scope.into());
    app.set_search_query(query.clone().into());
    app.set_unified_count(sidebar_badge_text(inbox_count).into());
    app.set_total_count(total_count.to_string().into());
    app.set_search_filter(search_filter.into());
    app.set_list_status(list_status(
        &query,
        visible_count,
        total_count,
        using_core,
        next_cursor.is_some(),
    ));
    app.set_can_load_more(if using_core {
        next_cursor.is_some()
    } else {
        visible_count < messages.len()
    });
    app.set_has_selected(selected_email.is_some());
    apply_label_rows(app, &labels, selected_email.as_ref());
    let (previous_email_id, next_email_id) = adjacent_message_ids(visible, selected_id);
    app.set_previous_email_id(previous_email_id);
    app.set_next_email_id(next_email_id);
    state.borrow().queue_warm_start_update();
    schedule_favicon_fetches(app, state, runtime, visible);

    if let Some(email) = selected_email {
        app.global::<EmailReader>().invoke_ensure_body(email.id);
        apply_selected_favicon(app, favicon_icons.get(&email.domain));
        apply_email(app, email, &email_renderer, use_wgpu, allow_remote_images)
    } else {
        app.global::<EmailReader>().invoke_ensure_body(-1);
        email_renderer.borrow_mut().clear();
        app.set_selected_sender("".into());
        app.set_selected_address("".into());
        app.set_selected_subject(if query.trim().is_empty() {
            translated(app, &UiMessage::plain("No messages in this folder"))
        } else {
            translated(app, &UiMessage::plain("No messages match this search"))
        });
        app.set_selected_time("".into());
        app.set_selected_to("".into());
        app.set_selected_label("".into());
        app.set_selected_is_draft(false);
        app.set_selected_initials("?".into());
        app.set_selected_starred(false);
        app.set_selected_unread(false);
        app.set_selected_sender_verification("".into());
        apply_selected_favicon(app, None);
        app.set_email_tiles(ModelRc::new(VecModel::default()));
        app.set_email_scroll_y(0.0);
        app.set_email_content_aspect(900.0 / 520.0);
        app.set_email_links(ModelRc::new(VecModel::default()));
        clear_reader_projection(app);
        crate::attachment_controller::clear(app);
        app.global::<EmailReader>().set_message_id(-1);
        app.global::<EmailReader>().set_authored_text("".into());
        app.global::<EmailReader>().set_notice("".into());
        app.set_selected_plain_text("".into());
        app.set_selected_source("".into());
        app.set_selected_text("".into());
        app.set_has_selection(false);
        app.set_remote_images_blocked(false);
        app.set_render_status(UiMessage::plain(
            "Mail core is ready for account synchronization and message actions.",
        ));
        Ok(())
    }
}

pub(super) fn apply_label_rows(
    app: &AppWindow,
    labels: &[flectar_mail_core::models::Label],
    selected: Option<&MailMessage>,
) {
    let rows = make_label_rows(labels, selected, "");
    app.set_selected_mail_label_count(rows.iter().filter(|label| label.applied).count() as i32);
    app.set_mail_labels(ModelRc::new(VecModel::from(rows.clone())));
    app.set_mail_label_results(ModelRc::new(VecModel::from(rows)));
}

pub(super) fn make_label_rows(
    labels: &[flectar_mail_core::models::Label],
    selected: Option<&MailMessage>,
    query: &str,
) -> Vec<MailLabelRow> {
    let applied = selected
        .map(|message| message.labels.as_slice())
        .unwrap_or_default();
    project_label_rows(labels, applied, query)
}

fn project_label_rows(
    labels: &[flectar_mail_core::models::Label],
    applied: &[i64],
    query: &str,
) -> Vec<MailLabelRow> {
    let query = query.trim().to_lowercase();
    labels
        .iter()
        .filter(|label| query.is_empty() || label.name.to_lowercase().contains(&query))
        .filter_map(|label| {
            Some(MailLabelRow {
                id: i32::try_from(label.id).ok()?,
                name: label.name.clone().into(),
                name_has_emoji: contains_emoji(&label.name),
                color: label_color(&label.color),
                applied: applied.contains(&label.id),
                is_auto: label.is_auto,
            })
        })
        .collect()
}

/// Label rows for a message's own chip list: unlike [`make_label_rows`]
/// (the "apply label" picker, which needs the whole catalog with an
/// `applied` flag per entry for its checkboxes), a chip list only ever
/// shows labels actually on the message, so the catalog is filtered down
/// *before* building rows rather than after.
fn applied_label_rows(
    labels: &[flectar_mail_core::models::Label],
    applied: &[i64],
) -> Vec<MailLabelRow> {
    labels
        .iter()
        .filter(|label| applied.contains(&label.id))
        .filter_map(|label| {
            Some(MailLabelRow {
                id: i32::try_from(label.id).ok()?,
                name: label.name.clone().into(),
                name_has_emoji: contains_emoji(&label.name),
                color: label_color(&label.color),
                applied: true,
                is_auto: label.is_auto,
            })
        })
        .collect()
}

fn label_color(value: &str) -> slint::Color {
    value
        .strip_prefix('#')
        .filter(|hex| hex.len() == 6)
        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
        .map(|rgb| {
            slint::Color::from_rgb_u8(
                ((rgb >> 16) & 0xff) as u8,
                ((rgb >> 8) & 0xff) as u8,
                (rgb & 0xff) as u8,
            )
        })
        .unwrap_or_else(|| slint::Color::from_rgb_u8(107, 114, 128))
}

pub(super) fn release_unselected_bodies(messages: &mut [MailMessage], selected: Option<i32>) {
    for message in messages {
        if Some(message.id) != selected {
            message.html = None;
            message.text = None;
            message.body_pending = true;
        }
    }
}

pub(super) fn filtered_messages(
    messages: &[MailMessage],
    scope: &str,
    query: &str,
    search_filter: &str,
) -> Vec<MailMessage> {
    let query = query.trim().to_lowercase();
    messages
        .iter()
        .filter(|email| scope_matches(email, scope))
        .filter(|email| match search_filter {
            "Unread" => email.unread,
            "Starred" => email.starred,
            "Has attachments" => email.has_attachments,
            _ => true,
        })
        .filter(|email| {
            if query.is_empty() {
                return true;
            }
            [
                email.sender.as_str(),
                email.address.as_str(),
                email.subject.as_str(),
                email.preview.as_str(),
                email.account.as_str(),
                email.folder.as_str(),
            ]
            .into_iter()
            .any(|field| field.to_lowercase().contains(&query))
        })
        .cloned()
        .collect()
}

pub(super) fn scope_matches(email: &MailMessage, scope: &str) -> bool {
    if matches!(scope, "Important" | "Other") || scope.starts_with("Folder:") {
        // These scopes are filtered by the core query before projection. Their
        // stable route/folder ids are intentionally not duplicated in every UI
        // row.
        return true;
    }
    if let Some(label_id) = scope
        .strip_prefix("Label:")
        .and_then(|id| id.parse::<i64>().ok())
    {
        return email.labels.contains(&label_id);
    }
    if scope == "Unified Inbox" {
        return email.folder == "Inbox";
    }
    if let Some(folder) = scope.strip_prefix("Unified ") {
        return match folder {
            "Starred" => email.starred,
            "Sent" | "Archive" | "Spam" | "Trash" | "Drafts" => email.folder == folder,
            _ => false,
        };
    }
    if let Some((account, folder)) = scope.split_once(" / ") {
        return email.account == account
            && if folder == "Starred" {
                email.starred
            } else {
                email.folder == folder
            };
    }
    email.account == scope
}

pub(super) fn list_status(
    query: &str,
    visible_count: usize,
    total_count: usize,
    using_core: bool,
    can_load_more: bool,
) -> UiMessage {
    let singular = total_count == 1;
    match (
        query.trim().is_empty(),
        singular,
        using_core && can_load_more,
        using_core && !can_load_more,
    ) {
        (true, true, true, _) => UiMessage::arguments(
            "Showing {} of {} message · more available",
            visible_count,
            total_count,
        ),
        (true, true, _, true) => UiMessage::arguments(
            "Showing {} of {} message · current results complete",
            visible_count,
            total_count,
        ),
        (true, true, _, _) => {
            UiMessage::arguments("Showing {} of {} message", visible_count, total_count)
        }
        (true, false, true, _) => UiMessage::arguments(
            "Showing {} of {} messages · more available",
            visible_count,
            total_count,
        ),
        (true, false, _, true) => UiMessage::arguments(
            "Showing {} of {} messages · current results complete",
            visible_count,
            total_count,
        ),
        (true, false, _, _) => {
            UiMessage::arguments("Showing {} of {} messages", visible_count, total_count)
        }
        (false, true, true, _) => UiMessage::three_arguments(
            "Showing {} of {} message matching \"{}\" · more available",
            visible_count,
            total_count,
            query.trim(),
        ),
        (false, true, _, true) => UiMessage::three_arguments(
            "Showing {} of {} message matching \"{}\" · current results complete",
            visible_count,
            total_count,
            query.trim(),
        ),
        (false, true, _, _) => UiMessage::three_arguments(
            "Showing {} of {} message matching \"{}\"",
            visible_count,
            total_count,
            query.trim(),
        ),
        (false, false, true, _) => UiMessage::three_arguments(
            "Showing {} of {} messages matching \"{}\" · more available",
            visible_count,
            total_count,
            query.trim(),
        ),
        (false, false, _, true) => UiMessage::three_arguments(
            "Showing {} of {} messages matching \"{}\" · current results complete",
            visible_count,
            total_count,
            query.trim(),
        ),
        (false, false, _, _) => UiMessage::three_arguments(
            "Showing {} of {} messages matching \"{}\"",
            visible_count,
            total_count,
            query.trim(),
        ),
    }
}

// Compare visible contents: empty Slint images are not reflexively equal,
// and freshly projected label models have different identities.
fn same_email_row(a: &EmailRow, b: &EmailRow) -> bool {
    a.id == b.id
        && a.account_id == b.account_id
        && a.account == b.account
        && a.folder == b.folder
        && a.sender == b.sender
        && a.address == b.address
        && a.initials == b.initials
        && a.has_favicon == b.has_favicon
        && a.subject == b.subject
        && a.preview == b.preview
        && a.time == b.time
        && a.unread == b.unread
        && a.starred == b.starred
        && a.has_attachments == b.has_attachments
        && a.has_replied == b.has_replied
        && a.label_summary == b.label_summary
        && a.selected == b.selected
        && a.checked == b.checked
        && (!a.has_favicon || (a.favicon == b.favicon && a.favicon_small == b.favicon_small))
        && a.labels.iter().eq(b.labels.iter())
}

pub(super) fn make_rows(
    messages: &[MailMessage],
    selected_id: Option<i32>,
    checked_ids: &HashSet<i32>,
    favicon_icons: &HashMap<String, FaviconImages>,
    labels: &[flectar_mail_core::models::Label],
) -> Vec<EmailRow> {
    messages
        .iter()
        .map(|email| {
            let favicons = favicon_icons.get(&email.domain);
            let favicon = favicons.map(|icons| slint_image(&icons.regular));
            let favicon_small = favicons.map(|icons| slint_image(&icons.small));
            EmailRow {
                id: email.id,
                account_id: i32::try_from(email.account_id).unwrap_or(-1),
                account: email.account.clone().into(),
                folder: email.folder.clone().into(),
                sender: email.sender.clone().into(),
                address: email.address.clone().into(),
                initials: email.initials.clone().into(),
                favicon: favicon.clone().unwrap_or_default(),
                favicon_small: favicon_small.unwrap_or_default(),
                has_favicon: favicon.is_some(),
                subject: email.subject.clone().into(),
                preview: display_preview(&email.preview).into(),
                time: email.time.clone().into(),
                unread: email.unread,
                starred: email.starred,
                has_attachments: email.has_attachments,
                has_replied: email.has_replied,
                label_summary: label_summary(&email.labels, labels).into(),
                labels: ModelRc::new(VecModel::from(applied_label_rows(labels, &email.labels))),
                selected: Some(email.id) == selected_id,
                checked: checked_ids.contains(&email.id),
            }
        })
        .collect()
}

fn label_summary(ids: &[i64], labels: &[flectar_mail_core::models::Label]) -> String {
    labels
        .iter()
        .filter(|label| ids.contains(&label.id))
        .map(|label| label.name.as_str())
        .collect::<Vec<_>>()
        .join(" · ")
}

pub(super) fn slint_image(icon: &FaviconImage) -> Image {
    // Cloning source RGBA and reconstructing Slint images on every selection
    // invalidates otherwise identical model rows and repeats texture uploads.
    type Entry = (std::sync::Weak<[u8]>, u32, u32, Image, usize);
    thread_local! { static IMAGES: RefCell<std::collections::VecDeque<Entry>> = const { RefCell::new(std::collections::VecDeque::new()) }; }
    IMAGES.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.retain(|entry| entry.0.strong_count() > 0);
        if let Some(index) = cache.iter().position(|entry| {
            entry.1 == icon.width
                && entry.2 == icon.height
                && entry
                    .0
                    .upgrade()
                    .is_some_and(|pixels| Arc::ptr_eq(&pixels, &icon.pixels))
        }) {
            let entry = cache.remove(index).unwrap();
            let image = entry.3.clone();
            cache.push_back(entry);
            return image;
        }
        let pixels = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
            &icon.pixels,
            icon.width,
            icon.height,
        );
        let image = Image::from_rgba8(pixels);
        const BUDGET: usize = 4 * 1024 * 1024;
        if icon.pixels.len() <= BUDGET {
            let mut bytes = cache.iter().map(|entry| entry.4).sum::<usize>();
            while cache.len() >= 128 || bytes + icon.pixels.len() > BUDGET {
                if let Some(entry) = cache.pop_front() {
                    bytes -= entry.4;
                } else {
                    break;
                }
            }
            cache.push_back((
                Arc::downgrade(&icon.pixels),
                icon.width,
                icon.height,
                image.clone(),
                icon.pixels.len(),
            ));
        }
        image
    })
}

pub(super) fn apply_selected_favicon(app: &AppWindow, icons: Option<&FaviconImages>) {
    app.set_selected_has_favicon(icons.is_some());
    app.set_selected_favicon(
        icons
            .map(|icons| slint_image(&icons.regular))
            .unwrap_or_default(),
    );
}

pub(super) fn refresh_rows_only(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
) {
    let (messages, page, using_core, selected_id, preview_closed, favicon_icons) = {
        let state = state.borrow();
        (
            filtered_messages(
                &state.messages,
                &state.scope,
                &state.query,
                &state.search_filter,
            ),
            state.page,
            state.using_core,
            state.selected_id,
            state.preview_closed,
            state.favicon_icons.clone(),
        )
    };
    let visible_count = if using_core {
        messages.len()
    } else {
        paged_visible_count(page, messages.len())
    };
    let visible = &messages[..visible_count];
    let selected_id = if preview_closed {
        None
    } else {
        selected_id
            .filter(|id| visible.iter().any(|email| email.id == *id))
            .or_else(|| visible.first().map(|email| email.id))
    };
    let checked_ids = {
        let mut state = state.borrow_mut();
        state.selected_id = selected_id;
        state
            .checked_ids
            .retain(|id| visible.iter().any(|message| message.id == *id));
        state.checked_ids.clone()
    };
    let selected_email = selected_id.and_then(|id| visible.iter().find(|email| email.id == id));
    let (email_rows, labels) = {
        let state = state.borrow();
        (Rc::clone(&state.email_rows), state.labels.clone())
    };
    reconcile_model_rows_by(
        &email_rows,
        make_rows(visible, selected_id, &checked_ids, &favicon_icons, &labels),
        |row| row.id,
        same_email_row,
    );
    app.set_mail_selection_count(checked_ids.len() as i32);
    apply_label_rows(app, &labels, selected_email);
    refresh_sidebar(state);
    let selected_icon = selected_id
        .and_then(|id| visible.iter().find(|email| email.id == id))
        .and_then(|email| favicon_icons.get(&email.domain));
    apply_selected_favicon(app, selected_icon);
    schedule_favicon_fetches(app, state, runtime, visible);
}

pub(super) fn refresh_list_metadata(app: &AppWindow, state: &Rc<RefCell<InboxState>>) {
    let (
        messages,
        page,
        using_core,
        query,
        scope,
        mailboxes,
        unified_mailboxes,
        next_cursor,
        search_filter,
        total_count,
        inbox_count,
        labels,
    ) = {
        let state = state.borrow();
        (
            filtered_messages(
                &state.messages,
                &state.scope,
                &state.query,
                &state.search_filter,
            ),
            state.page,
            state.using_core,
            state.query.clone(),
            state.scope.clone(),
            state.mailboxes.clone(),
            state.unified_mailboxes.clone(),
            state.next_cursor,
            state.search_filter.clone(),
            state.total_count,
            state.inbox_count,
            state.labels.clone(),
        )
    };
    let visible_count = if using_core {
        messages.len()
    } else {
        paged_visible_count(page, messages.len())
    };
    let total_count = if using_core {
        total_count
    } else {
        messages.len()
    };
    let can_load_more = if using_core {
        next_cursor.is_some()
    } else {
        visible_count < messages.len()
    };

    refresh_sidebar(state);
    app.set_selected_scope_title(
        mailbox_scope_title(&scope, &mailboxes, &unified_mailboxes, &labels).into(),
    );
    app.set_selected_scope(scope.into());
    app.set_search_query(query.clone().into());
    app.set_unified_count(sidebar_badge_text(inbox_count).into());
    app.set_total_count(total_count.to_string().into());
    app.set_search_filter(search_filter.into());
    app.set_list_status(list_status(
        &query,
        visible_count,
        total_count,
        using_core,
        can_load_more,
    ));
    app.set_can_load_more(can_load_more);
    state.borrow().queue_warm_start_update();
}

fn sidebar_badge_text(count: usize) -> String {
    if count > 0 {
        count.to_string()
    } else {
        String::new()
    }
}

pub(super) fn schedule_favicon_fetches(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    visible: &[MailMessage],
) {
    let pixel_sides = (
        physical_pixel_side(SENDER_AVATAR_SMALL_SIDE, app.window().scale_factor()),
        physical_pixel_side(SENDER_AVATAR_REGULAR_SIDE, app.window().scale_factor()),
    );
    let (loader, tx, pending_count) = {
        let mut state = state.borrow_mut();
        if !state.remote_images_enabled {
            return;
        }
        if state.favicon_pixel_sides != pixel_sides {
            state.favicon_pixel_sides = pixel_sides;
            state.favicon_icons.clear();
            state.favicon_pending.clear();
            state.favicon_missing.clear();
        }
        (
            state.favicon_loader.clone(),
            state.favicon_tx.clone(),
            state.favicon_pending.len(),
        )
    };
    let Some(loader) = loader else {
        return;
    };
    let slots = FAVICON_CONCURRENCY.saturating_sub(pending_count);
    if slots == 0 {
        return;
    }

    let mut to_fetch = Vec::new();
    {
        let mut state = state.borrow_mut();
        let mut domains = HashSet::new();
        for domain in visible
            .iter()
            .map(|email| email.domain.as_str())
            .filter(|domain| !domain.is_empty())
        {
            if to_fetch.len() >= slots || !domains.insert(domain.to_owned()) {
                continue;
            }
            if state.favicon_icons.contains_key(domain)
                || state.favicon_missing.contains(domain)
                || state.favicon_pending.contains(domain)
            {
                continue;
            }
            let domain = domain.to_owned();
            state.favicon_pending.insert(domain.clone());
            to_fetch.push(domain);
        }
    }

    for domain in to_fetch {
        let loader = loader.clone();
        let tx = tx.clone();
        runtime.spawn(async move {
            let icons = loader.load(&domain, pixel_sides.0, pixel_sides.1).await;
            let _ = tx
                .send(FaviconUpdate {
                    domain,
                    pixel_sides,
                    icons,
                })
                .await;
        });
    }
}

pub(super) fn schedule_profile_avatar_fetches(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
) {
    let pixel_sides = (
        physical_pixel_side(ACCOUNT_AVATAR_SMALL_SIDE, app.window().scale_factor()),
        physical_pixel_side(ACCOUNT_AVATAR_REGULAR_SIDE, app.window().scale_factor()),
    );
    let (loader, tx, to_fetch) = {
        let mut state = state.borrow_mut();
        if state.profile_avatar_pixel_sides != pixel_sides {
            state.profile_avatar_pixel_sides = pixel_sides;
            state.profile_avatar_images.clear();
            state.profile_avatar_pending.clear();
            state.profile_avatar_missing.clear();
        }
        let Some(loader) = state.profile_avatar_loader.clone() else {
            return;
        };
        let slots = FAVICON_CONCURRENCY.saturating_sub(state.profile_avatar_pending.len());
        let candidates = state
            .connected_accounts
            .iter()
            .filter_map(|account| {
                account
                    .avatar_url
                    .as_ref()
                    .map(|source_url| (account.id, source_url.clone()))
            })
            .collect::<Vec<_>>();
        let mut to_fetch = Vec::new();
        for (account_id, source_url) in candidates {
            if to_fetch.len() >= slots {
                break;
            }
            if state.profile_avatar_images.contains_key(&account_id)
                || state.profile_avatar_pending.contains(&account_id)
                || state.profile_avatar_missing.contains(&account_id)
            {
                continue;
            }
            state.profile_avatar_pending.insert(account_id);
            to_fetch.push((account_id, source_url));
        }
        (loader, state.profile_avatar_tx.clone(), to_fetch)
    };

    for (account_id, source_url) in to_fetch {
        let loader = loader.clone();
        let tx = tx.clone();
        runtime.spawn(async move {
            let images = loader.load(&source_url, pixel_sides.0, pixel_sides.1).await;
            let _ = tx
                .send(ProfileAvatarUpdate {
                    account_id,
                    source_url,
                    pixel_sides,
                    images,
                })
                .await;
        });
    }
}

fn contains_emoji(text: &str) -> bool {
    use unicode_properties::UnicodeEmoji as _;

    text.chars()
        .any(|character| character.is_emoji_char() && !character.is_ascii())
}

pub(super) fn make_mailbox_rows(
    mailboxes: &[MailboxEntry],
    avatars: &HashMap<i64, ProfileAvatarImages>,
    labels: &[flectar_mail_core::models::Label],
    collapsed_folder_ids: &HashSet<i64>,
) -> Vec<MailboxRow> {
    // Index label colors once; matching every folder against the entire label
    // catalog makes large provider trees unnecessarily quadratic.
    let mut label_colors = HashMap::new();
    for label in labels {
        label_colors
            .entry(label.name.to_ascii_lowercase())
            .or_insert_with(|| label_color(&label.color));
    }
    let parents = mailboxes
        .iter()
        .filter(|mailbox| mailbox.folder_id >= 0)
        .map(|mailbox| (mailbox.folder_id, mailbox.parent_folder_id))
        .collect::<HashMap<_, _>>();
    mailboxes
        .iter()
        .filter(|mailbox| {
            if collapsed_folder_ids.is_empty() {
                return true;
            }
            let mut parent = mailbox.parent_folder_id;
            let mut visited = HashSet::new();
            while parent >= 0 && visited.insert(parent) {
                if collapsed_folder_ids.contains(&parent) {
                    return false;
                }
                parent = parents.get(&parent).copied().unwrap_or(-1);
            }
            true
        })
        .map(|mailbox| {
            let avatar = mailbox
                .is_account
                .then(|| avatars.get(&mailbox.account_id))
                .flatten();
            let custom_color = (!mailbox.is_account)
                .then(|| {
                    label_colors
                        .get(&mailbox.label.to_ascii_lowercase())
                        .copied()
                })
                .flatten();
            MailboxRow {
                account_id: i32::try_from(mailbox.account_id).unwrap_or(-1),
                folder_id: i32::try_from(mailbox.folder_id).unwrap_or(-1),
                parent_folder_id: i32::try_from(mailbox.parent_folder_id).unwrap_or(-1),
                depth: i32::try_from(mailbox.depth).unwrap_or(i32::MAX),
                has_children: mailbox.has_children,
                expanded: !collapsed_folder_ids.contains(&mailbox.folder_id),
                is_standard: mailbox.is_standard,
                label_has_emoji: contains_emoji(&mailbox.label),
                label: mailbox.label.clone().into(),
                scope: mailbox.scope.clone().into(),
                context: mailbox.context.clone().into(),
                detail: mailbox.detail.clone().into(),
                avatar: mailbox.avatar.clone().into(),
                avatar_image: avatar
                    .map(|images| slint_image(&images.small))
                    .unwrap_or_default(),
                has_avatar: avatar.is_some(),
                is_account: mailbox.is_account,
                count: mailbox.count.clone().into(),
                color: custom_color.unwrap_or_default(),
                has_custom_color: custom_color.is_some(),
            }
        })
        .collect()
}

fn mailbox_scope_title(
    scope: &str,
    mailboxes: &[MailboxEntry],
    unified_mailboxes: &[MailboxEntry],
    labels: &[flectar_mail_core::models::Label],
) -> String {
    if scope.starts_with("Folder:") {
        mailboxes
            .iter()
            .chain(unified_mailboxes)
            .find(|mailbox| mailbox.scope == scope)
            .map(|mailbox| mailbox.label.clone())
            .unwrap_or_else(|| "Folder".into())
    } else if let Some(label_id) = scope
        .strip_prefix("Label:")
        .and_then(|id| id.parse::<i64>().ok())
    {
        labels
            .iter()
            .find(|label| label.id == label_id)
            .map(|label| label.name.clone())
            .unwrap_or_else(|| "Label".into())
    } else {
        scope.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_mail_rows_do_not_emit_updates() {
        use slint::private_unstable_api::re_exports::{
            ModelChangeListener, ModelChangeListenerContainer,
        };
        use std::pin::Pin;
        #[derive(Default)]
        struct Listener(Rc<RefCell<Vec<usize>>>);
        impl ModelChangeListener for Listener {
            fn row_changed(self: Pin<&Self>, row: usize) {
                self.0.borrow_mut().push(row);
            }
            fn row_added(self: Pin<&Self>, _: usize, _: usize) {
                panic!("unexpected insertion");
            }
            fn row_removed(self: Pin<&Self>, _: usize, _: usize) {
                panic!("unexpected removal");
            }
            fn reset(self: Pin<&Self>) {
                panic!("unexpected reset");
            }
        }
        let messages = vec![message(1), message(2), message(3)];
        let project = |selected| {
            make_rows(
                &messages,
                Some(selected),
                &HashSet::new(),
                &HashMap::new(),
                &[],
            )
        };
        let model = VecModel::from(project(1));
        let changes = Rc::new(RefCell::new(Vec::new()));
        let listener = Box::pin(ModelChangeListenerContainer::new(Listener(changes.clone())));
        model
            .model_tracker()
            .attach_peer(listener.as_ref().model_peer());
        reconcile_model_rows_by(&model, project(1), |row| row.id, same_email_row);
        assert!(changes.borrow().is_empty());
        reconcile_model_rows_by(&model, project(2), |row| row.id, same_email_row);
        assert_eq!(*changes.borrow(), vec![0, 1]);
        let mut updated = project(2);
        updated[2].unread = !updated[2].unread;
        assert!(!same_email_row(&model.row_data(2).unwrap(), &updated[2]));
    }

    #[test]
    fn unchanged_images_keep_their_identity() {
        let icon = FaviconImage {
            width: 1,
            height: 1,
            pixels: vec![0, 0, 0, 255].into(),
        };
        let image = slint_image(&icon);
        assert_eq!(image, slint_image(&icon.clone()));
    }

    #[test]
    fn browsing_keeps_only_the_selected_body_in_memory() {
        let mut rows: Vec<_> = (0..100).map(message).collect();
        for id in 0..100 {
            rows[id].html = Some("<p>Large cached body</p>".repeat(10_000));
            rows[id].text = Some("Large cached body".repeat(10_000));
            rows[id].body_pending = false;
            release_unselected_bodies(&mut rows, Some(id as i32));
            assert_eq!(rows.iter().filter(|r| r.html.is_some()).count(), 1);
            assert_eq!(rows.iter().filter(|r| r.text.is_some()).count(), 1);
            assert!(rows.iter().all(|r| r.id == id as i32 || r.body_pending));
        }
        release_unselected_bodies(&mut rows, None);
        assert!(rows.iter().all(|r| r.html.is_none() && r.text.is_none()));
    }

    fn message(id: i32) -> MailMessage {
        MailMessage {
            id,
            thread_id: Some(i64::from(id)),
            account_id: 1,
            account: String::new(),
            folder: String::new(),
            sender: String::new(),
            address: String::new(),
            domain: String::new(),
            initials: String::new(),
            subject: String::new(),
            preview: String::new(),
            time: String::new(),
            to: String::new(),
            label: String::new(),
            unread: false,
            starred: false,
            has_attachments: false,
            has_replied: false,
            labels: Vec::new(),
            html: None,
            text: None,
            attachments: Vec::new(),
            body_pending: true,
            sender_verification: String::new(),
        }
    }

    #[test]
    fn head_refresh_merges_into_the_retained_tail() {
        let current = (1..=50).map(message).collect::<Vec<_>>();
        let refreshed = [101, 102]
            .into_iter()
            .chain(1..=23)
            .map(message)
            .collect::<Vec<_>>();
        let (merged, retained_tail) = merge_refreshed_mail_head(
            &current,
            refreshed,
            Some(ThreadCursor {
                last_message_at: 0,
                thread_id: 23,
            }),
            &[],
        );

        assert!(retained_tail);
        assert_eq!(merged.len(), 52);
        assert_eq!(
            merged
                .iter()
                .map(|row| row.id)
                .collect::<HashSet<_>>()
                .len(),
            52
        );
        assert_eq!(merged[0].id, 101);
        assert_eq!(merged[24].id, 23);
        assert_eq!(merged[25].id, 24);
        assert_eq!(merged.last().map(|row| row.id), Some(50));
    }

    #[test]
    fn exhausted_head_refresh_replaces_the_old_tail() {
        let current = (1..=50).map(message).collect::<Vec<_>>();
        let refreshed = (1..=12).map(message).collect::<Vec<_>>();
        let (merged, retained_tail) = merge_refreshed_mail_head(&current, refreshed, None, &[]);
        assert!(!retained_tail);
        assert_eq!(merged.len(), 12);
    }

    #[test]
    fn acted_on_message_beyond_the_first_page_is_dropped_from_the_retained_tail() {
        // 50 messages loaded; the acted-on message (id 40) sits in the
        // retained tail, well beyond the refreshed head.
        let current = (1..=50).map(message).collect::<Vec<_>>();
        let refreshed = (1..=23).map(message).collect::<Vec<_>>();
        let (merged, retained_tail) = merge_refreshed_mail_head(
            &current,
            refreshed,
            Some(ThreadCursor {
                last_message_at: 0,
                thread_id: 23,
            }),
            &[40],
        );

        assert!(retained_tail);
        assert!(
            merged.iter().all(|row| row.id != 40),
            "the archived/spammed/trashed message must not survive in the retained tail"
        );
        assert_eq!(merged.len(), 49);
    }

    #[test]
    fn acted_on_message_still_present_in_the_refreshed_head_is_kept() {
        // The acted-on message is in the freshly refreshed head, not the
        // retained tail (e.g. it's still visible in the current scope) — it
        // must not be dropped just because `drop_id` names it.
        let current = (1..=50).map(message).collect::<Vec<_>>();
        let refreshed = (1..=23).map(message).collect::<Vec<_>>();
        let (merged, _) = merge_refreshed_mail_head(
            &current,
            refreshed,
            Some(ThreadCursor {
                last_message_at: 0,
                thread_id: 23,
            }),
            &[10],
        );

        assert!(merged.iter().any(|row| row.id == 10));
    }

    #[test]
    fn adjacent_message_ids_follow_visible_list_order() {
        let messages = [10, 20, 30].into_iter().map(message).collect::<Vec<_>>();

        assert_eq!(adjacent_message_ids(&messages, Some(10)), (-1, 20));
        assert_eq!(adjacent_message_ids(&messages, Some(20)), (10, 30));
        assert_eq!(adjacent_message_ids(&messages, Some(30)), (20, -1));
        assert_eq!(adjacent_message_ids(&messages, None), (-1, -1));
        assert_eq!(adjacent_message_ids(&messages, Some(99)), (-1, -1));
    }

    #[test]
    fn label_summary_uses_label_order_and_ignores_unknown_ids() {
        let labels = vec![
            flectar_mail_core::models::Label {
                id: 2,
                name: "Work".into(),
                color: "#2563eb".into(),
                keyword: "Work".into(),
                position: 0,
                is_auto: false,
            },
            flectar_mail_core::models::Label {
                id: 4,
                name: "Follow up".into(),
                color: "#7c3aed".into(),
                keyword: "Follow_up".into(),
                position: 1,
                is_auto: false,
            },
        ];

        assert_eq!(label_summary(&[4, 99, 2], &labels), "Work · Follow up");
    }

    #[test]
    fn label_search_is_case_insensitive_and_keeps_membership() {
        let mut selected = message(12);
        selected.labels = vec![4];
        let labels = vec![
            flectar_mail_core::models::Label {
                id: 2,
                name: "Work".into(),
                color: "#2563eb".into(),
                keyword: "Work".into(),
                position: 0,
                is_auto: false,
            },
            flectar_mail_core::models::Label {
                id: 4,
                name: "Follow Up".into(),
                color: "#7c3aed".into(),
                keyword: "Follow_Up".into(),
                position: 1,
                is_auto: false,
            },
        ];

        let results = make_label_rows(&labels, Some(&selected), "FOLLOW");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Follow Up");
        assert!(results[0].applied);
    }

    #[test]
    fn mail_rows_only_keep_applied_colored_labels_for_list_chips() {
        let mut email = message(12);
        email.labels = vec![4];
        let labels = vec![
            flectar_mail_core::models::Label {
                id: 2,
                name: "Personal".into(),
                color: "#ef4444".into(),
                keyword: "Personal".into(),
                position: 0,
                is_auto: false,
            },
            flectar_mail_core::models::Label {
                id: 4,
                name: "Viaje".into(),
                color: "#7c3aed".into(),
                keyword: "Viaje".into(),
                position: 1,
                is_auto: false,
            },
        ];

        let rows = make_rows(&[email], None, &HashSet::new(), &HashMap::new(), &labels);
        assert_eq!(rows[0].labels.row_count(), 1);
        let label = rows[0].labels.row_data(0).expect("projected label");
        assert_eq!(label.name, "Viaje");
        assert_eq!(label.color, slint::Color::from_rgb_u8(0x7c, 0x3a, 0xed));
    }

    #[test]
    fn matching_sidebar_folder_uses_the_label_color() {
        let mailboxes = vec![MailboxEntry {
            account_id: 1,
            folder_id: 7,
            parent_folder_id: -1,
            depth: 0,
            has_children: false,
            is_standard: false,
            label: "Projects".into(),
            scope: "Folder:7".into(),
            context: "Account".into(),
            detail: String::new(),
            avatar: String::new(),
            is_account: false,
            count: String::new(),
        }];
        let labels = vec![flectar_mail_core::models::Label {
            id: 7,
            name: "Projects".into(),
            color: "#22c55e".into(),
            keyword: "Projects".into(),
            position: 0,
            is_auto: false,
        }];

        let rows = make_mailbox_rows(&mailboxes, &HashMap::new(), &labels, &HashSet::new());
        assert!(rows[0].has_custom_color);
        assert_eq!(rows[0].color, slint::Color::from_rgb_u8(0x22, 0xc5, 0x5e));
        assert_eq!(
            mailbox_scope_title("Folder:7", &mailboxes, &[], &labels),
            "Projects"
        );
    }

    #[test]
    fn collapsed_sidebar_folder_hides_all_of_its_descendants() {
        let folder = |folder_id, parent_folder_id, depth, has_children, label: &str| MailboxEntry {
            account_id: 1,
            folder_id,
            parent_folder_id,
            depth,
            has_children,
            is_standard: false,
            label: label.into(),
            scope: format!("Folder:{folder_id}"),
            context: "Account".into(),
            detail: String::new(),
            avatar: String::new(),
            is_account: false,
            count: String::new(),
        };
        let mailboxes = vec![
            folder(10, -1, 0, true, "Projects"),
            folder(11, 10, 1, true, "2026"),
            folder(12, 11, 2, false, "Launch"),
            folder(20, -1, 0, false, "Receipts"),
        ];

        let rows = make_mailbox_rows(&mailboxes, &HashMap::new(), &[], &HashSet::from([10]));

        assert_eq!(
            rows.iter().map(|row| row.folder_id).collect::<Vec<_>>(),
            vec![10, 20]
        );
        assert!(!rows[0].expanded);
    }

    #[test]
    fn mail_ui_detects_emoji_names() {
        assert!(contains_emoji("🚗🚗"));
        assert!(contains_emoji("Cars 🚗"));
        assert!(!contains_emoji("Cars"));
        assert!(!contains_emoji("Folder 1"));

        let labels = [flectar_mail_core::models::Label {
            id: 1,
            name: "🚗🚗".into(),
            color: "#4a86e8".into(),
            keyword: "Cars".into(),
            position: 0,
            is_auto: false,
        }];
        assert!(project_label_rows(&labels, &[1], "")[0].name_has_emoji);
    }

    #[test]
    fn label_color_parses_hex_and_has_a_safe_fallback() {
        assert_eq!(
            label_color("#123abc"),
            slint::Color::from_rgb_u8(0x12, 0x3a, 0xbc)
        );
        assert_eq!(
            label_color("invalid"),
            slint::Color::from_rgb_u8(107, 114, 128)
        );
    }
}
