//! Mail list/detail projection, paging, avatars, and rendered-email bridge.

#[cfg(test)]
mod removal_tests;

use super::*;
use chrono::{Datelike, Timelike};

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
/// Refreshes re-read every loaded row up to a bound (`mail_work::refresh_limit`),
/// so the retained tail (everything beyond the refreshed head) is only rows
/// past that bound and the oldest rows that new mail pushed out of it. It is
/// kept as-is without re-querying it, so it can go stale relative to the
/// backend. `drop_ids` names messages known to no longer belong in the current
/// view (e.g. messages the user just archived/spammed/trashed) — the only
/// staleness this function actively corrects in that tail. Pass an empty
/// slice when the refresh does not follow a message-moving action.
fn merge_refreshed_mail_head(
    current: Vec<MailMessage>,
    refreshed: Vec<MailMessage>,
    next_cursor: Option<MailCursor>,
    drop_ids: &[i32],
) -> (Vec<MailMessage>, bool) {
    if next_cursor.is_none() {
        return (refreshed, false);
    }

    // The loaded rows after the oldest refreshed row come next in the
    // mailbox too, so they stay as they are, including rows new mail pushed
    // out of the refresh. When the oldest refreshed row was never loaded, the
    // refresh reached past rows that have left, which the loaded rows below
    // it would bring back, or new mail filled it, which would leave a gap
    // above them; the refreshed rows replace the list.
    let Some(tail_start) = refreshed
        .last()
        .and_then(|oldest| current.iter().position(|old| old.id == oldest.id))
        .map(|index| index + 1)
    else {
        return (refreshed, false);
    };
    let refreshed_len = refreshed.len();
    let mut merged = refreshed;
    let mut known = merged
        .iter()
        .map(|message| message.id)
        .collect::<HashSet<_>>();
    merged.extend(
        current
            .into_iter()
            .skip(tail_start)
            .filter(|message| known.insert(message.id))
            .filter(|message| !drop_ids.contains(&message.id)),
    );
    let retained_tail = merged.len() > refreshed_len;
    (merged, retained_tail)
}

pub(super) fn carry_selected_detail(
    old_messages: &mut [MailMessage],
    refreshed: &mut [MailMessage],
    selected_id: Option<i32>,
) {
    let Some(selected_id) = selected_id else {
        return;
    };
    let Some(selected_detail) = old_messages
        .iter_mut()
        .find(|message| message.id == selected_id && !message.body_pending)
    else {
        return;
    };
    let Some(summary) = refreshed.iter_mut().find(|message| message.id == selected_id) else {
        return;
    };
    summary.html = selected_detail.html.take();
    summary.text = selected_detail.text.take();
    summary.to = std::mem::take(&mut selected_detail.to);
    summary.attachments = std::mem::take(&mut selected_detail.attachments);
    summary.body_pending = false;
}

pub(super) fn apply_background_mail_page(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    page: mail::MailPage,
    acted_on_ids: &[i32],
) {
    if !snapshot_is_current(state, page.account_revision) {
        return;
    }
    let deeper = deeper_refresh(&state.borrow(), &page);
    if let Some(rows) = deeper {
        if let Err(error) = mail_work::refresh_rows(app, state, runtime, rows, acted_on_ids) {
            app.set_render_status(UiMessage::detail(
                "Background mail refresh failed: {}",
                error,
            ));
        }
        return;
    }
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
        let mut old_messages = std::mem::take(&mut state.messages);
        let old_order = filtered_messages(
            &old_messages,
            state.scope,
            if state.using_core { "" } else { &state.query },
            &state.search_filter,
            open_conversation(&state),
        )
        .into_iter()
        .map(|message| message.id)
        .collect::<Vec<_>>();
        carry_selected_detail(&mut old_messages, &mut messages, state.selected_id);

        let old_counts = state
            .mailboxes
            .iter()
            .filter_map(|mailbox| Some((mailbox.scope?, mailbox.count.clone())))
            .collect::<HashMap<_, _>>();
        let mut mailboxes = mailboxes;
        for mailbox in &mut mailboxes {
            if let Some(count) = mailbox.scope.and_then(|scope| old_counts.get(&scope)) {
                mailbox.count.clone_from(count);
            }
        }

        let old_next_cursor = state.next_cursor;
        let (merged, retained_tail) =
            merge_refreshed_mail_head(old_messages, messages, next_cursor, acted_on_ids);
        state.messages = merged;
        state.mailboxes = mailboxes;
        state.next_cursor = if retained_tail {
            old_next_cursor
        } else {
            next_cursor
        };
        // Check against `rendered_id` (what the reading pane actually shows),
        // not `selected_id`: callers are free to clear or redirect the latter
        // before refreshing, and doing so must not be able to make this look
        // like "nothing needs repainting" when the previously-rendered
        // message is in fact gone.
        let removed = state
            .rendered_id
            .filter(|rendered_id| !state.messages.iter().any(|row| row.id == *rendered_id));
        if let Some(removed_id) = removed {
            // Archiving or trashing the open message should advance to the
            // row that took its place, not jump back to the top of the list.
            let visible = filtered_messages(
                &state.messages,
                state.scope,
                if state.using_core { "" } else { &state.query },
                &state.search_filter,
                open_conversation(&state),
            )
            .into_iter()
            .map(|message| message.id)
            .collect::<HashSet<_>>();
            state.selected_id = successor_selection(&old_order, removed_id, &visible);
            // With nothing left to move to, the reader closes rather than
            // jumping to another conversation.
            state.preview_closed = state.selected_id.is_none();
        }
        removed.is_some()
    };

    if selection_removed {
        // Normal selection also reveals a successor in a collapsed date group.
        let selected_id = state.borrow().selected_id;
        let result = match selected_id {
            Some(id) => select_message(app, state, runtime, id),
            None => render_current(app, state, runtime),
        };
        if let Err(error) = result {
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

/// Rows to read instead when a refresh shares no row with the list while the
/// reader shows one of them: new mail filled the refresh and pushed the open
/// conversation below it. Each read is twice as long, up to one store read.
fn deeper_refresh(state: &InboxState, page: &mail::MailPage) -> Option<usize> {
    let open = state.rendered_id?;
    let read = page.messages.len();
    let most = usize::try_from(mail_work::refresh_limit(usize::MAX, &state.query)).ok()?;
    let loaded = state
        .messages
        .iter()
        .map(|row| row.id)
        .collect::<HashSet<_>>();
    (page.next_cursor.is_some()
        && read < most
        && loaded.contains(&open)
        && !page.messages.iter().any(|row| loaded.contains(&row.id)))
    .then_some(read * 2)
}

pub(super) fn append_mail_page(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    cursor: MailCursor,
    page: mail::MailPage,
) -> Result<(), String> {
    if !snapshot_is_current(state, page.account_revision) {
        return Ok(());
    }
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
        let Some(date_ms) = state.messages.iter().find(|row| row.id == id).map(|row| row.date_ms) else {
            return Ok(());
        };
        if state.query.trim().is_empty() && app.get_group_mail_by_date() {
            state.mail_groups.reveal(&mail_group_key(date_ms, Local::now()));
        }
        // A restored tab's open message belongs to its own conversation.
        if state
            .restore_thread_message
            .is_some_and(|(conversation, _)| conversation != id)
        {
            state.restore_thread_message = None;
        }
        state.selected_id = Some(id);
        state.preview_closed = false;
    }
    render_current(app, state, runtime)
}

/// The next surviving row below `removed_id` in the previous list order, or
/// the nearest surviving row above it when it was the last one.
fn successor_selection(old_order: &[i32], removed_id: i32, visible: &HashSet<i32>) -> Option<i32> {
    let index = old_order.iter().position(|id| *id == removed_id)?;
    old_order[index + 1..]
        .iter()
        .chain(old_order[..index].iter().rev())
        .find(|id| visible.contains(id))
        .copied()
}

fn adjacent_message_ids<M: std::borrow::Borrow<MailMessage>>(
    messages: &[M],
    selected_id: Option<i32>,
) -> (i32, i32) {
    let Some(selected_index) =
        selected_id.and_then(|id| {
            messages
                .iter()
                .position(|message| std::borrow::Borrow::borrow(message).id == id)
        })
    else {
        return (-1, -1);
    };
    let previous_id = selected_index
        .checked_sub(1)
        .and_then(|index| messages.get(index))
        .map(|message| std::borrow::Borrow::borrow(message).id)
        .unwrap_or(-1);
    let next_id = messages
        .get(selected_index + 1)
        .map(|message| std::borrow::Borrow::borrow(message).id)
        .unwrap_or(-1);
    (previous_id, next_id)
}

pub(super) fn render_current(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
) -> Result<(), String> {
    if app.get_active_view() != "mail" {
        let mut state = state.borrow_mut();
        state.email_renderer.borrow_mut().clear();
        release_unselected_bodies(&mut state.messages, None);
        state.conversation_owner_id = None;
        state.conversation_messages.clear();
        state.conversation_selected_index = 0;
        state.conversation_rows.set_vec(Vec::new());
        app.set_email_tiles(Default::default());
        app.set_email_links(Default::default());
        return Ok(());
    }
    // Keep list rows lightweight. SQLite already owns the durable body cache.
    {
        let mut state = state.borrow_mut();
        if state.using_core {
            let selected = state.selected_id.filter(|_| !state.preview_closed);
            release_unselected_bodies(&mut state.messages, selected);
        }
    }
    let (email_renderer, use_wgpu, open) = {
        let state = state.borrow();
        (
            Rc::clone(&state.email_renderer),
            state.use_wgpu,
            // Both projections below list the same rows.
            open_conversation(&state),
        )
    };
    let (
        scope,
        query,
        selected_id,
        visible_ids,
        visible_count,
        total_count,
        mailboxes,
        next_cursor,
        using_core,
        favicon_icons,
        search_filter,
        inbox_count,
        labels,
        account_presentation,
    ) = {
        let state = state.borrow();
        let messages = filtered_messages(
            &state.messages,
            state.scope,
            if state.using_core { "" } else { &state.query },
            &state.search_filter,
            open,
        );
        let visible_count = if state.using_core {
            messages.len()
        } else {
            paged_visible_count(state.page, messages.len())
        };
        let visible = &messages[..visible_count];
        let selected_id = if state.preview_closed {
            None
        } else {
            state
                .selected_id
                .filter(|id| visible.iter().any(|email| email.id == *id))
                .or_else(|| visible.first().map(|email| email.id))
        };
        (
            state.scope,
            state.query.clone(),
            selected_id,
            visible.iter().map(|email| email.id).collect::<HashSet<_>>(),
            visible_count,
            messages.len(),
            state.mailboxes.clone(),
            state.next_cursor,
            state.using_core,
            state.favicon_icons.clone(),
            state.search_filter.clone(),
            state.inbox_count,
            state.labels.clone(),
            state.account_presentation.clone(),
        )
    };

    let (selection_changed, allow_remote_images, checked_ids) = {
        let mut state = state.borrow_mut();
        let selection_changed = state.rendered_id != selected_id;
        state.selected_id = selected_id;
        state.rendered_id = selected_id;
        state.checked_ids.retain(|id| visible_ids.contains(id));
        let allow_remote_images = state.remote_images_enabled
            || selected_id.is_some_and(|id| state.remote_images_override_id == Some(id));
        (
            selection_changed,
            allow_remote_images,
            state.checked_ids.clone(),
        )
    };
    if selection_changed {
        let mut state = state.borrow_mut();
        state.conversation_owner_id = None;
        state.conversation_messages.clear();
        state.conversation_selected_index = 0;
        state.conversation_rows.set_vec(Vec::new());
        drop(state);
        // "View plain text" is a message action, not a global display mode.
        app.set_text_mode(false);
        app.set_source_mode(false);
        app.set_rendering_info_open(false);
    }

    let (
        selected_email,
        projected_rows,
        list_projection,
        previous_email_id,
        next_email_id,
        visible_domains,
        email_rows,
        mail_list_entries,
    ) = {
        let state = state.borrow();
        let messages = filtered_messages(
            &state.messages,
            state.scope,
            if state.using_core { "" } else { &state.query },
            &state.search_filter,
            open,
        );
        let visible = &messages[..visible_count];
        let selected_email = selected_id
            .and_then(|id| visible.iter().find(|email| email.id == id))
            .map(|email| (**email).clone());
        let projected_rows = make_rows(
            visible,
            selected_id,
            &checked_ids,
            &favicon_icons,
            &labels,
            &account_presentation,
        );
        let list_projection = project_mail_list(
            visible,
            &projected_rows,
            &state.mail_groups,
            query.trim().is_empty() && app.get_group_mail_by_date(),
            Local::now(),
        );
        let (previous_email_id, next_email_id) = adjacent_message_ids(visible, selected_id);
        (
            selected_email,
            projected_rows,
            list_projection,
            previous_email_id,
            next_email_id,
            visible
                .iter()
                .map(|message| message.domain.clone())
                .collect::<Vec<_>>(),
            Rc::clone(&state.email_rows),
            Rc::clone(&state.mail_list_entries),
        )
    };
    reconcile_model_rows_by(
        &email_rows,
        projected_rows,
        |row| row.id,
        same_email_row,
    );
    reconcile_model_rows_by(
        &mail_list_entries,
        list_projection,
        mail_list_entry_key,
        same_mail_list_entry,
    );
    app.set_mail_selection_count(checked_ids.len() as i32);
    refresh_sidebar(state);
    app.set_selected_scope_title(mailbox_scope_title(scope, &mailboxes, &labels).into());
    app.set_trash_view(is_trash_scope(scope));
    app.set_selected_scope(scope.into());
    app.set_search_query(query.clone().into());
    app.set_unified_count(sidebar_badge_text(inbox_count).into());
    app.set_search_filter(search_filter.into());
    app.set_can_load_more(if using_core {
        next_cursor.is_some()
    } else {
        visible_count < total_count
    });
    app.set_has_selected(selected_email.is_some());
    app.set_selected_account_uses_labels(selected_email.as_ref().is_some_and(|email| {
        state.borrow().connected_accounts.iter().any(|account| {
            account.id == email.account_id && account.mail_protocol == MailProtocol::Gmail
        })
    }));
    apply_label_rows(app, &labels, selected_email.as_ref());
    app.set_previous_email_id(previous_email_id);
    app.set_next_email_id(next_email_id);
    state.borrow().queue_warm_start_update();
    schedule_favicon_fetches(app, state, runtime, &visible_domains);

    if let Some(email) = selected_email {
        let email_id = email.id;
        let thread_subject = email.subject.clone();
        let (mut display_email, conversation_ready, conversation_selected_index) = {
            let state = state.borrow();
            let conversation_ready = state.conversation_owner_id == Some(email_id)
                && !state.conversation_messages.is_empty();
            let display_email = conversation_ready
                .then(|| {
                    state
                        .conversation_messages
                        .get(state.conversation_selected_index)
                        .cloned()
                })
                .flatten()
                .unwrap_or(email);
            if conversation_ready {
                reconcile_model_rows_by(
                    &state.conversation_rows,
                    make_thread_rows(
                        &state.conversation_messages,
                        state.conversation_selected_index,
                        &favicon_icons,
                    ),
                    |row| row.index,
                    PartialEq::eq,
                );
            }
            (
                display_email,
                conversation_ready,
                state.conversation_selected_index,
            )
        };
        if conversation_ready {
            display_email.subject = thread_subject;
        }
        app.set_selected_thread_index(if conversation_ready {
            i32::try_from(conversation_selected_index).unwrap_or(i32::MAX)
        } else {
            0
        });
        if (using_core || display_email.body_pending)
            && (!conversation_ready || display_email.html.is_none())
        {
            app.global::<EmailReader>()
                .invoke_ensure_body(if conversation_ready {
                    display_email.id
                } else {
                    email_id
                });
        }
        apply_selected_favicon(app, favicon_icons.get(&display_email.domain));
        apply_email(
            app,
            display_email,
            &email_renderer,
            use_wgpu,
            allow_remote_images,
        )
    } else {
        app.global::<EmailReader>().invoke_ensure_body(-1);
        email_renderer.borrow_mut().clear();
        app.set_selected_sender("".into());
        app.set_selected_address("".into());
        app.set_selected_subject(if query.trim().is_empty() {
            translated(app, &UiMessage::plain("No conversations in this folder"))
        } else {
            translated(app, &UiMessage::plain("No conversations match this search"))
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
        state.borrow().conversation_rows.set_vec(Vec::new());
        app.set_selected_thread_index(0);
        app.set_render_status(UiMessage::plain(
            "Mail core is ready for account synchronization and message actions.",
        ));
        Ok(())
    }
}

pub(super) fn select_thread_message(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
    index: i32,
) -> Result<(), String> {
    let index = usize::try_from(index).map_err(|_| "invalid thread message".to_owned())?;
    {
        let mut state = state.borrow_mut();
        if index >= state.conversation_messages.len() || state.conversation_selected_index == index
        {
            return Ok(());
        }
        state.conversation_selected_index = index;
    }
    app.set_text_mode(false);
    app.set_source_mode(false);
    app.set_rendering_info_open(false);
    render_current(app, state, runtime)
}

fn make_thread_rows(
    messages: &[MailMessage],
    selected_index: usize,
    favicon_icons: &HashMap<String, FaviconImages>,
) -> Vec<ThreadMessageRow> {
    let now = Local::now();
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let favicons = favicon_icons.get(&message.domain);
            let favicon = favicons.map(|icons| slint_image(&icons.regular));
            ThreadMessageRow {
                index: i32::try_from(index).unwrap_or(i32::MAX),
                sender: message.sender.clone().into(),
                address: message.address.clone().into(),
                initials: message.initials.clone().into(),
                favicon: favicon.clone().unwrap_or_default(),
                favicon_small: favicons
                    .map(|icons| slint_image(&icons.small))
                    .unwrap_or_default(),
                has_favicon: favicon.is_some(),
                preview: display_preview(&message.preview).into(),
                time: message_time(message.date_ms, &now).into(),
                to: message.to.clone().into(),
                unread: message.unread,
                outgoing: message.is_outgoing,
                body_pending: message.body_pending,
                selected: index == selected_index,
            }
        })
        .collect()
}

pub(super) fn apply_label_rows(
    app: &AppWindow,
    labels: &[flectar_mail_core::models::Label],
    selected: Option<&MailMessage>,
) {
    let picker_rows = make_label_rows(labels, selected, "");
    let chip_rows = selected
        .map(|message| applied_label_rows(labels, &message.labels))
        .unwrap_or_default();
    app.set_selected_mail_label_count(chip_rows.len() as i32);
    // The header strip must only receive rendered chips. Supplying the whole
    // picker catalog and hiding unapplied entries left layout-sized gaps
    // before labels such as "Social".
    app.set_mail_labels(ModelRc::new(VecModel::from(chip_rows)));
    app.set_mail_label_results(ModelRc::new(VecModel::from(picker_rows)));
}

pub(super) fn make_label_rows(
    labels: &[flectar_mail_core::models::Label],
    selected: Option<&MailMessage>,
    query: &str,
) -> Vec<MailLabelRow> {
    let applied = selected
        .map(|message| message.labels.as_slice())
        .unwrap_or_default();
    let mut rows = project_label_rows(
        labels,
        applied,
        selected.map(|message| message.account_id),
        query,
    );
    if let Some(message) = selected {
        for row in &mut rows {
            row.account_name = if row.is_global {
                "All accounts".into()
            } else if row.account_id >= 0 {
                message.account.clone().into()
            } else {
                Default::default()
            };
        }
    }
    rows
}

fn project_label_rows(
    labels: &[flectar_mail_core::models::Label],
    applied: &[i64],
    selected_account_id: Option<i64>,
    query: &str,
) -> Vec<MailLabelRow> {
    let query = query.trim().to_lowercase();
    labels
        .iter()
        .filter(|label| {
            selected_account_id.is_none_or(|account_id| {
                label.is_auto
                    || label.owner_account_id.is_none()
                    || label.owner_account_id == Some(account_id)
            })
        })
        .filter(|label| query.is_empty() || label.name.to_lowercase().contains(&query))
        .filter_map(|label| {
            Some(MailLabelRow {
                id: i32::try_from(label.id).ok()?,
                account_id: label
                    .owner_account_id
                    .and_then(|id| i32::try_from(id).ok())
                    .unwrap_or(-1),
                account_name: Default::default(),
                name: label.name.clone().into(),
                display_name: label.name.clone().into(),
                depth: 0,
                has_children: false,
                expanded: true,
                can_create_children: false,
                name_has_emoji: contains_emoji(&label.name),
                color: label_color(&label.color),
                applied: applied.contains(&label.id),
                is_auto: label.is_auto,
                is_global: !label.is_auto && label.owner_account_id.is_none(),
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
                account_id: label
                    .owner_account_id
                    .and_then(|id| i32::try_from(id).ok())
                    .unwrap_or(-1),
                account_name: Default::default(),
                name: label.name.clone().into(),
                display_name: label.name.clone().into(),
                depth: 0,
                has_children: false,
                expanded: true,
                can_create_children: false,
                name_has_emoji: contains_emoji(&label.name),
                color: label_color(&label.color),
                applied: true,
                is_auto: label.is_auto,
                is_global: !label.is_auto && label.owner_account_id.is_none(),
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

/// The rows the list shows. `open`, the conversation in the reader, stays
/// listed when it stops matching the quick filter, as when opening an unread
/// message marks it read; a refresh then cannot move the selection, which
/// reader actions apply to, onto another row.
pub(super) fn filtered_messages<'a>(
    messages: &'a [MailMessage],
    scope: Scope,
    query: &str,
    search_filter: &str,
    open: Option<i32>,
) -> Vec<&'a MailMessage> {
    let query = query.trim().to_lowercase();
    messages
        .iter()
        .filter(|email| scope_matches(email, scope))
        .filter(|email| {
            Some(email.id) == open
                || match search_filter {
                    "Unread" => email.unread,
                    "Starred" => email.starred,
                    "Has attachments" => email.has_attachments,
                    _ => true,
                }
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
        .collect()
}

/// The conversation the reader shows, which the list keeps showing; see
/// `filtered_messages`.
pub(super) fn open_conversation(state: &InboxState) -> Option<i32> {
    state.selected_id.filter(|_| !state.preview_closed)
}

pub(super) fn scope_matches(email: &MailMessage, scope: Scope) -> bool {
    let in_mailbox = |mailbox: StandardMailbox| match mailbox {
        StandardMailbox::Starred => email.starred,
        mailbox => email.folder == mailbox.label(),
    };
    match scope {
        Scope::Unified(mailbox) => in_mailbox(mailbox),
        Scope::Account {
            account_id,
            mailbox,
        } => email.account_id == account_id && in_mailbox(mailbox),
        Scope::Label { id, .. } => email.labels.contains(&id),
        // These scopes are filtered by the core query before projection. Their
        // stable route/folder ids are intentionally not duplicated in every UI
        // row.
        Scope::Important | Scope::Other | Scope::Folder(_) => true,
    }
}

// Compare visible contents: empty Slint images are not reflexively equal,
// and freshly projected label models have different identities.
pub(super) fn same_email_row(a: &EmailRow, b: &EmailRow) -> bool {
    a.id == b.id
        && a.account_id == b.account_id
        && a.account == b.account
        && a.account_color == b.account_color
        && a.account_profile == b.account_profile
        && a.show_account_marker == b.show_account_marker
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
        && a.message_count == b.message_count
        && a.label_summary == b.label_summary
        && a.account_uses_labels == b.account_uses_labels
        && a.verification_code == b.verification_code
        && a.attachment == b.attachment
        && a.attachment_total == b.attachment_total
        && a.selected == b.selected
        && a.checked == b.checked
        && (!a.has_favicon || (a.favicon == b.favicon && a.favicon_small == b.favicon_small))
        && a.labels.iter().eq(b.labels.iter())
}

pub(super) fn make_rows<M: std::borrow::Borrow<MailMessage>>(
    messages: &[M],
    selected_id: Option<i32>,
    checked_ids: &HashSet<i32>,
    favicon_icons: &HashMap<String, FaviconImages>,
    labels: &[flectar_mail_core::models::Label],
    account_presentation: &AccountPresentationSettings,
) -> Vec<EmailRow> {
    let now = Local::now();
    messages
        .iter()
        .map(|email| {
            let email = std::borrow::Borrow::borrow(email);
            let favicons = favicon_icons.get(&email.domain);
            let favicon = favicons.map(|icons| slint_image(&icons.regular));
            let favicon_small = favicons.map(|icons| slint_image(&icons.small));
            let profile = account_presentation.profile(email.account_id);
            EmailRow {
                id: email.id,
                account_id: i32::try_from(email.account_id).unwrap_or(-1),
                account: account_presentation
                    .account_label(email.account_id, &email.account)
                    .into(),
                account_color: account_presentation.marker_color(email.account_id),
                account_profile: profile
                    .map(|profile| profile.name.clone())
                    .unwrap_or_default()
                    .into(),
                show_account_marker: account_presentation.show_markers,
                folder: email.folder.clone().into(),
                sender: email.sender.clone().into(),
                address: email.address.clone().into(),
                initials: email.initials.clone().into(),
                favicon: favicon.clone().unwrap_or_default(),
                favicon_small: favicon_small.unwrap_or_default(),
                has_favicon: favicon.is_some(),
                subject: email.subject.clone().into(),
                preview: display_preview(&email.preview).into(),
                time: mail_list_time(email.date_ms, &now).into(),
                unread: email.unread,
                starred: email.starred,
                has_attachments: email.has_attachments,
                has_replied: email.has_replied,
                message_count: i32::try_from(email.message_count)
                    .unwrap_or(i32::MAX)
                    .max(1),
                label_summary: label_summary(&email.labels, labels).into(),
                labels: ModelRc::new(VecModel::from(applied_label_rows(labels, &email.labels))),
                account_uses_labels: email.account_uses_labels,
                verification_code: email.verification_code.clone().unwrap_or_default().into(),
                verification_code_spelled: email
                    .verification_code
                    .as_deref()
                    .map(spelled)
                    .unwrap_or_default()
                    .into(),
                attachment: email
                    .first_attachment
                    .as_ref()
                    .map(crate::attachment_controller::list_attachment)
                    .unwrap_or_default(),
                attachment_total: i32::try_from(email.attachment_total).unwrap_or(i32::MAX),
                selected: Some(email.id) == selected_id,
                checked: checked_ids.contains(&email.id),
            }
        })
        .collect()
}

/// One character at a time, so "482913" is read as digits rather than as a
/// number in the hundreds of thousands.
fn spelled(code: &str) -> String {
    code.chars().map(String::from).collect::<Vec<_>>().join(" ")
}

/// The time or date beside a mail list row. Yesterday's mail shows its time
/// too, since the list's date sections name the day.
fn mail_list_time(date_ms: i64, now: &chrono::DateTime<Local>) -> String {
    message_time_label(date_ms, now, false)
}

/// The time or date in a conversation's message headers.
pub(super) fn message_time(date_ms: i64, now: &chrono::DateTime<Local>) -> String {
    message_time_label(date_ms, now, true)
}

fn message_time_label(date_ms: i64, now: &chrono::DateTime<Local>, name_yesterday: bool) -> String {
    let Some(date) = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(date_ms)
        .filter(|_| date_ms > 0)
        .map(|date| date.with_timezone(&Local))
    else {
        return String::new();
    };
    let formats = date_format::current();
    let days = now
        .date_naive()
        .signed_duration_since(date.date_naive())
        .num_days();
    match days {
        1 if name_yesterday => formats.yesterday().to_owned(),
        0 | 1 => formats.time(date.hour(), date.minute()),
        _ if date.year() == now.year() => formats.date(DateStyle::ShortDayMonth, date.date_naive()),
        _ => formats.date(DateStyle::Short, date.date_naive()),
    }
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
    let (
        selected_id,
        checked_ids,
        selected_email,
        projected_rows,
        list_projection,
        visible_domains,
        email_rows,
        mail_list_entries,
        labels,
        favicon_icons,
    ) = {
        let state = state.borrow();
        let messages = filtered_messages(
            &state.messages,
            state.scope,
            if state.using_core { "" } else { &state.query },
            &state.search_filter,
            open_conversation(&state),
        );
        let visible_count = if state.using_core {
            messages.len()
        } else {
            paged_visible_count(state.page, messages.len())
        };
        let visible = &messages[..visible_count];
        let selected_id = if state.preview_closed {
            None
        } else {
            state
                .selected_id
                .filter(|id| visible.iter().any(|email| email.id == *id))
                .or_else(|| visible.first().map(|email| email.id))
        };
        let visible_ids = visible.iter().map(|message| message.id).collect::<HashSet<_>>();
        let checked_ids = state
            .checked_ids
            .intersection(&visible_ids)
            .copied()
            .collect::<HashSet<_>>();
        let selected_email = selected_id
            .and_then(|id| visible.iter().find(|email| email.id == id))
            .map(|email| (**email).clone());
        let projected_rows = make_rows(
            visible,
            selected_id,
            &checked_ids,
            &state.favicon_icons,
            &state.labels,
            &state.account_presentation,
        );
        let list_projection = project_mail_list(
            visible,
            &projected_rows,
            &state.mail_groups,
            state.query.trim().is_empty() && app.get_group_mail_by_date(),
            Local::now(),
        );
        (
            selected_id,
            checked_ids,
            selected_email,
            projected_rows,
            list_projection,
            visible
                .iter()
                .map(|message| message.domain.clone())
                .collect::<Vec<_>>(),
            Rc::clone(&state.email_rows),
            Rc::clone(&state.mail_list_entries),
            state.labels.clone(),
            state.favicon_icons.clone(),
        )
    };
    {
        let mut state = state.borrow_mut();
        state.selected_id = selected_id;
        state.checked_ids.clone_from(&checked_ids);
    }
    reconcile_model_rows_by(
        &email_rows,
        projected_rows,
        |row| row.id,
        same_email_row,
    );
    reconcile_model_rows_by(
        &mail_list_entries,
        list_projection,
        mail_list_entry_key,
        same_mail_list_entry,
    );
    app.set_mail_selection_count(checked_ids.len() as i32);
    app.set_selected_account_uses_labels(selected_email.as_ref().is_some_and(|email| {
        state.borrow().connected_accounts.iter().any(|account| {
            account.id == email.account_id && account.mail_protocol == MailProtocol::Gmail
        })
    }));
    apply_label_rows(app, &labels, selected_email.as_ref());
    refresh_sidebar(state);
    let selected_domain = selected_email.as_ref().map(|email| {
        let state = state.borrow();
        if state.conversation_owner_id == selected_id {
            state
                .conversation_messages
                .get(state.conversation_selected_index)
                .map(|message| message.domain.clone())
                .unwrap_or_else(|| email.domain.clone())
        } else {
            email.domain.clone()
        }
    });
    let selected_icon = selected_domain
        .as_deref()
        .and_then(|domain| favicon_icons.get(domain));
    apply_selected_favicon(app, selected_icon);
    let conversation_projection = {
        let state = state.borrow();
        (state.conversation_owner_id == selected_id && !state.conversation_messages.is_empty())
            .then(|| {
                (
                    make_thread_rows(
                        &state.conversation_messages,
                        state.conversation_selected_index,
                        &favicon_icons,
                    ),
                    Rc::clone(&state.conversation_rows),
                )
            })
    };
    if let Some((thread_rows, rows)) = conversation_projection {
        reconcile_model_rows_by(
            &rows,
            thread_rows,
            |row| row.index,
            PartialEq::eq,
        );
    }
    schedule_favicon_fetches(app, state, runtime, &visible_domains);
}

/// Rebuilds the mail text the date formats produce, after the language or the
/// time format changed.
pub(super) fn refresh_message_dates(
    app: &AppWindow,
    state: &Rc<RefCell<InboxState>>,
    runtime: &tokio::runtime::Runtime,
) {
    refresh_rows_only(app, state, runtime);
    let date_ms = {
        let state = state.borrow();
        state.selected_id.and_then(|id| {
            (state.conversation_owner_id == Some(id))
                .then(|| {
                    state
                        .conversation_messages
                        .get(state.conversation_selected_index)
                })
                .flatten()
                .or_else(|| state.messages.iter().find(|message| message.id == id))
                .map(|message| message.date_ms)
        })
    };
    if let Some(date_ms) = date_ms {
        app.set_selected_time(message_time(date_ms, &Local::now()).into());
    }
}

pub(super) fn refresh_list_metadata(app: &AppWindow, state: &Rc<RefCell<InboxState>>) {
    let (
        can_load_more,
        query,
        scope,
        mailboxes,
        search_filter,
        inbox_count,
        labels,
    ) = {
        let state = state.borrow();
        let can_load_more = if state.using_core {
            state.next_cursor.is_some()
        } else {
            let total = filtered_messages(
                &state.messages,
                state.scope,
                &state.query,
                &state.search_filter,
                open_conversation(&state),
            )
            .len();
            paged_visible_count(state.page, total) < total
        };
        (
            can_load_more,
            state.query.clone(),
            state.scope,
            state.mailboxes.clone(),
            state.search_filter.clone(),
            state.inbox_count,
            state.labels.clone(),
        )
    };

    refresh_sidebar(state);
    app.set_selected_scope_title(mailbox_scope_title(scope, &mailboxes, &labels).into());
    app.set_trash_view(is_trash_scope(scope));
    app.set_selected_scope(scope.into());
    app.set_search_query(query.clone().into());
    app.set_unified_count(sidebar_badge_text(inbox_count).into());
    app.set_search_filter(search_filter.into());
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
    visible_domains: &[String],
) {
    let pixel_sides = (
        physical_pixel_side(SENDER_AVATAR_SMALL_SIDE, app.window().scale_factor()),
        physical_pixel_side(SENDER_AVATAR_REGULAR_SIDE, app.window().scale_factor()),
    );
    let (loader, tx, pending_count, generation) = {
        let mut state = state.borrow_mut();
        if !state.remote_images_enabled {
            return;
        }
        if state.favicon_pixel_sides != pixel_sides {
            state.favicon_pixel_sides = pixel_sides;
            state.favicon_generation = state.favicon_generation.wrapping_add(1);
            state.favicon_icons.clear();
            state.favicon_pending.clear();
            state.favicon_missing.clear();
        }
        (
            state.favicon_loader.clone(),
            state.favicon_tx.clone(),
            state.favicon_pending.len(),
            state.favicon_generation,
        )
    };
    let Some(loader) = loader else {
        return;
    };
    let slots = FAVICON_CONCURRENCY.saturating_sub(pending_count);
    if slots == 0 {
        return;
    }

    let conversation_domains = state
        .borrow()
        .conversation_messages
        .iter()
        .map(|message| message.domain.clone())
        .collect::<Vec<_>>();
    let mut to_fetch = Vec::new();
    {
        let mut state = state.borrow_mut();
        let mut domains = HashSet::new();
        for domain in visible_domains
            .iter()
            .map(String::as_str)
            .chain(conversation_domains.iter().map(String::as_str))
            .filter(|domain| !domain.is_empty())
        {
            if to_fetch.len() >= slots || !domains.insert(domain.to_owned()) {
                continue;
            }
            if state.favicon_icons.contains_key(domain)
                || state.favicon_missing.contains(domain)
                || state.favicon_pending.contains(domain)
                || state
                    .favicon_retry
                    .get(domain)
                    .is_some_and(|retry| retry.retry_at > std::time::Instant::now())
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
            let result = loader.load(&domain, pixel_sides.0, pixel_sides.1).await;
            let _ = tx
                .send(FaviconUpdate {
                    domain,
                    pixel_sides,
                    generation,
                    result,
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
            .entry((label.owner_account_id, label.name.to_ascii_lowercase()))
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
                        .get(&(Some(mailbox.account_id), mailbox.label.to_ascii_lowercase()))
                        .or_else(|| label_colors.get(&(None, mailbox.label.to_ascii_lowercase())))
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
                is_selectable: mailbox.is_selectable,
                can_create_children: mailbox.can_create_children,
                can_rename: mailbox.can_rename,
                can_delete: mailbox.can_delete,
                label_has_emoji: contains_emoji(&mailbox.label),
                label: mailbox.label.clone().into(),
                scope: mailbox.scope.map(slint::SharedString::from).unwrap_or_default(),
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
    scope: Scope,
    mailboxes: &[MailboxEntry],
    labels: &[flectar_mail_core::models::Label],
) -> String {
    let mailbox = || {
        mailboxes
            .iter()
            .find(|mailbox| mailbox.scope == Some(scope))
    };
    match scope {
        Scope::Unified(_) | Scope::Important | Scope::Other => scope.to_string(),
        Scope::Folder(_) => mailbox().map_or_else(|| "Folder".into(), |mailbox| mailbox.label.clone()),
        Scope::Label { id, .. } => labels
            .iter()
            .find(|label| label.id == id)
            .map_or_else(|| "Label".into(), |label| label.name.clone()),
        Scope::Account { mailbox: label, .. } => mailbox().map_or_else(
            || label.label().to_owned(),
            |mailbox| format!("{} / {}", mailbox.context, label.label()),
        ),
    }
}

fn is_trash_scope(scope: Scope) -> bool {
    matches!(
        scope,
        Scope::Unified(StandardMailbox::Trash)
            | Scope::Account {
                mailbox: StandardMailbox::Trash,
                ..
            }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_times_show_the_clock_for_recent_mail_and_dates_for_older_mail() {
        let at_noon = |year, month, day| {
            let naive = chrono::NaiveDate::from_ymd_opt(year, month, day)
                .unwrap()
                .and_hms_opt(12, 0, 0)
                .unwrap();
            Local.from_local_datetime(&naive).single().unwrap()
        };
        let now = at_noon(2026, 1, 5);
        let today = now.timestamp_millis();
        let yesterday = at_noon(2026, 1, 4).timestamp_millis();
        let this_year = at_noon(2026, 1, 2).timestamp_millis();
        let last_year = at_noon(2025, 12, 20).timestamp_millis();

        assert_eq!(mail_list_time(today, &now), "12:00");
        assert_eq!(mail_list_time(yesterday, &now), "12:00");
        assert_eq!(mail_list_time(this_year, &now), "Jan 2");
        assert_eq!(mail_list_time(last_year, &now), "Dec 20, 2025");
        assert_eq!(mail_list_time(0, &now), "");
        assert_eq!(message_time(today, &now), "12:00");
        assert_eq!(message_time(yesterday, &now), "Yesterday");
        assert_eq!(message_time(last_year, &now), "Dec 20, 2025");
    }

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
        let presentation = AccountPresentationSettings::default();
        let project = |selected| {
            make_rows(
                &messages,
                Some(selected),
                &HashSet::new(),
                &HashMap::new(),
                &[],
                &presentation,
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

    #[test]
    fn conversation_rows_keep_messages_separate_and_select_one() {
        let mut received = message(10);
        received.sender = "Maya".into();
        received.preview = "Could we move the review to Tuesday?".into();
        received.unread = true;
        let mut sent = message(11);
        sent.sender = "Alex".into();
        sent.preview = "Tuesday works for me.".into();
        sent.is_outgoing = true;

        let rows = make_thread_rows(&[received, sent], 1, &HashMap::new());

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].sender, "Maya");
        assert!(rows[0].unread);
        assert!(!rows[0].selected);
        assert!(rows[1].outgoing);
        assert!(rows[1].selected);
    }

    #[test]
    fn account_scopes_match_by_id_whatever_the_display_name() {
        let mut row = message(1);
        row.starred = true;
        let scope = |account_id, mailbox| Scope::Account {
            account_id,
            mailbox,
        };
        for account in ["Work / Personal", "Unified Work", "Folder:Work"] {
            row.account = account.into();
            for mailbox in StandardMailbox::ALL {
                row.folder = mailbox.label().into();
                assert!(scope_matches(&row, scope(1, mailbox)));
                assert!(scope_matches(&row, Scope::Unified(mailbox)));
                assert!(!scope_matches(&row, scope(2, mailbox)));
                assert!(scope_matches(&row, scope(1, StandardMailbox::Starred)));
            }
        }
        row.starred = false;
        row.folder = "Inbox".into();
        assert!(!scope_matches(&row, scope(1, StandardMailbox::Starred)));
        assert!(!scope_matches(&row, scope(1, StandardMailbox::Sent)));
    }

    pub(super) fn message(id: i32) -> MailMessage {
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
            date_ms: 0,
            to: String::new(),
            label: String::new(),
            unread: false,
            starred: false,
            has_attachments: false,
            message_count: 1,
            has_replied: false,
            is_outgoing: false,
            labels: Vec::new(),
            html: None,
            text: None,
            attachments: Vec::new(),
            body_pending: true,
            sender_verification: String::new(),
            verification_code: None,
            account_uses_labels: false,
            first_attachment: None,
            attachment_total: 0,
        }
    }

    #[test]
    fn mail_rows_use_the_assigned_profile_color_and_name() {
        let mut email = message(1);
        email.account = "alice@example.org".into();
        let presentation = AccountPresentationSettings {
            profiles: vec![MailProfile {
                id: "work".into(),
                name: "Work".into(),
                color: "#EA580C".into(),
                account_ids: vec![1],
            }],
            active_profile: None,
            show_markers: true,
            account_colors: HashMap::new(),
            account_short_names: HashMap::from([("1".into(), "Client".into())]),
        };

        let rows = make_rows(
            &[email],
            None,
            &HashSet::new(),
            &HashMap::new(),
            &[],
            &presentation,
        );

        assert_eq!(rows[0].account, "Client");
        assert_eq!(rows[0].account_profile, "Work");
        assert_eq!(
            rows[0].account_color,
            slint::Color::from_rgb_u8(0xea, 0x58, 0x0c)
        );
        assert!(rows[0].show_account_marker);
    }

    #[test]
    fn head_refresh_merges_into_the_retained_tail() {
        let mut current = (1..=50).map(message).collect::<Vec<_>>();
        current[49].html = Some("retained body".repeat(1_000));
        let retained_body_ptr = current[49].html.as_ref().unwrap().as_ptr();
        let refreshed = [101, 102]
            .into_iter()
            .chain(1..=23)
            .map(message)
            .collect::<Vec<_>>();
        let (merged, retained_tail) = merge_refreshed_mail_head(
            current,
            refreshed,
            Some(MailCursor::Thread(ThreadCursor {
                last_message_at: 0,
                thread_id: 23,
            })),
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
        assert_eq!(
            merged.last().unwrap().html.as_ref().unwrap().as_ptr(),
            retained_body_ptr,
            "retained tail should move its body buffer without a deep copy"
        );
    }

    #[test]
    fn filtered_rows_borrow_loaded_message_bodies() {
        let mut messages = vec![message(1), message(2)];
        messages[0].folder = "Inbox".into();
        messages[1].folder = "Archive".into();
        messages[0].html = Some("large selected body".repeat(1_000));
        let filtered = filtered_messages(&messages, Scope::UNIFIED_INBOX, "", "All mail", None);
        assert_eq!(filtered.len(), 1);
        assert!(std::ptr::eq(filtered[0], &messages[0]));
        assert_eq!(filtered[0].html.as_ref().unwrap().as_ptr(), messages[0].html.as_ref().unwrap().as_ptr());
    }

    #[test]
    fn selected_detail_moves_into_refreshed_head() {
        let mut old = vec![message(1), message(2)];
        old[0].body_pending = false;
        old[0].html = Some("selected HTML".repeat(1_000));
        old[0].text = Some("selected text".repeat(1_000));
        old[0].to = "recipient@example.org".into();
        old[0].attachments = vec![flectar_mail_core::models::AttachmentMeta {
            id: 7,
            filename: Some("selected.txt".into()),
            mime_type: Some("text/plain".into()),
            size: Some(42),
            is_inline: false,
        }];
        let html_ptr = old[0].html.as_ref().unwrap().as_ptr();
        let text_ptr = old[0].text.as_ref().unwrap().as_ptr();
        let to_ptr = old[0].to.as_ptr();
        let attachments_ptr = old[0].attachments.as_ptr();
        let mut fresh = vec![message(1), message(2)];

        carry_selected_detail(&mut old, &mut fresh, Some(1));

        assert_eq!(fresh[0].html.as_ref().unwrap().as_ptr(), html_ptr);
        assert_eq!(fresh[0].text.as_ref().unwrap().as_ptr(), text_ptr);
        assert_eq!(fresh[0].to, "recipient@example.org");
        assert_eq!(fresh[0].to.as_ptr(), to_ptr);
        assert_eq!(fresh[0].attachments.as_ptr(), attachments_ptr);
        assert_eq!(fresh[0].attachments[0].id, 7);
        assert!(!fresh[0].body_pending);
        assert!(old[0].html.is_none() && old[0].text.is_none());
        assert!(fresh[1].html.is_none());
    }

    #[test]
    fn selected_detail_handoff_skips_pending_or_removed_rows() {
        let mut old = vec![message(1), message(2)];
        old[0].html = Some("outdated pending body".into());
        old[0].body_pending = true;
        let mut fresh = vec![message(1)];

        carry_selected_detail(&mut old, &mut fresh, Some(1));
        assert!(fresh[0].html.is_none());
        assert!(fresh[0].body_pending);
        assert!(old[0].html.is_some());

        old[1].body_pending = false;
        old[1].html = Some("removed selected body".into());
        carry_selected_detail(&mut old, &mut fresh, Some(2));
        assert!(fresh[0].html.is_none());
        assert!(old[1].html.is_some());
    }

    #[test]
    fn refresh_without_a_shared_row_drops_the_old_rows() {
        let current = (1..=50).map(message).collect::<Vec<_>>();
        let refreshed = (101..=150).map(message).collect::<Vec<_>>();
        let (merged, retained_tail) = merge_refreshed_mail_head(
            current,
            refreshed,
            Some(MailCursor::Thread(ThreadCursor {
                last_message_at: 0,
                thread_id: 150,
            })),
            &[],
        );
        assert!(!retained_tail);
        assert_eq!(
            merged.iter().map(|row| row.id).collect::<Vec<_>>(),
            (101..=150).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_row_removed_from_the_end_does_not_return_below_older_mail() {
        // Row 50 left the mailbox, so the refresh of 50 rows reaches row 51.
        let current = (1..=50).map(message).collect::<Vec<_>>();
        let refreshed = (1..=49).chain([51]).map(message).collect::<Vec<_>>();
        let (merged, retained_tail) = merge_refreshed_mail_head(
            current,
            refreshed,
            Some(MailCursor::Thread(ThreadCursor {
                last_message_at: 0,
                thread_id: 51,
            })),
            &[],
        );
        assert!(!retained_tail);
        assert_eq!(
            merged.iter().map(|row| row.id).collect::<Vec<_>>(),
            (1..=49).chain([51]).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_short_list_keeps_the_rows_new_mail_pushes_down() {
        let current = (1..=PAGE_SIZE as i32).map(message).collect::<Vec<_>>();
        let refreshed = (101..=106)
            .chain(1..=PAGE_SIZE as i32 - 6)
            .map(message)
            .collect::<Vec<_>>();
        let (merged, retained_tail) = merge_refreshed_mail_head(
            current,
            refreshed,
            Some(MailCursor::Thread(ThreadCursor {
                last_message_at: 0,
                thread_id: PAGE_SIZE as i64 - 6,
            })),
            &[],
        );
        assert!(retained_tail);
        assert_eq!(
            merged.iter().map(|row| row.id).collect::<Vec<_>>(),
            (101..=106).chain(1..=PAGE_SIZE as i32).collect::<Vec<_>>()
        );
    }

    #[test]
    fn exhausted_head_refresh_replaces_the_old_tail() {
        let current = (1..=50).map(message).collect::<Vec<_>>();
        let refreshed = (1..=12).map(message).collect::<Vec<_>>();
        let (merged, retained_tail) = merge_refreshed_mail_head(current, refreshed, None, &[]);
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
            current,
            refreshed,
            Some(MailCursor::Thread(ThreadCursor {
                last_message_at: 0,
                thread_id: 23,
            })),
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
            current,
            refreshed,
            Some(MailCursor::Thread(ThreadCursor {
                last_message_at: 0,
                thread_id: 23,
            })),
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
    fn removed_selection_advances_to_the_next_surviving_row() {
        let order = [10, 20, 30, 40];
        let visible = |ids: &[i32]| ids.iter().copied().collect::<HashSet<_>>();

        assert_eq!(successor_selection(&order, 20, &visible(&[10, 30, 40])), Some(30));
        // Rows removed in the same batch are skipped.
        assert_eq!(successor_selection(&order, 20, &visible(&[10, 40])), Some(40));
        // The last row falls back to the nearest row above it.
        assert_eq!(successor_selection(&order, 40, &visible(&[10, 20])), Some(20));
        assert_eq!(successor_selection(&order, 20, &visible(&[])), None);
        assert_eq!(successor_selection(&order, 99, &visible(&[10])), None);
    }

    fn project_row(email: &MailMessage) -> EmailRow {
        make_rows(
            std::slice::from_ref(email),
            None,
            &HashSet::new(),
            &HashMap::new(),
            &[],
            &AccountPresentationSettings::default(),
        )
        .remove(0)
    }

    #[test]
    fn mail_rows_know_how_their_account_files_labels() {
        let mut email = message(1);
        email.account_uses_labels = true;
        let row = project_row(&email);
        assert!(row.account_uses_labels);
        email.account_uses_labels = false;
        assert!(!same_email_row(&row, &project_row(&email)));
    }

    #[test]
    fn mail_rows_offer_verification_codes() {
        let mut email = message(1);
        email.verification_code = Some("482913".into());
        let row = project_row(&email);
        assert_eq!(row.verification_code, "482913");
        assert_eq!(row.verification_code_spelled, "4 8 2 9 1 3");

        // A newer code refreshes the row in place.
        email.verification_code = Some("551203".into());
        assert!(!same_email_row(&row, &project_row(&email)));
        email.verification_code = None;
        let cleared = project_row(&email);
        assert_eq!(cleared.verification_code, "");
        assert_eq!(cleared.verification_code_spelled, "");
    }

    #[test]
    fn mail_rows_offer_the_first_file() {
        let mut email = message(1);
        email.first_attachment = Some(flectar_mail_core::models::AttachmentMeta {
            id: 9,
            filename: Some("Joel-Solano-CV.pdf".into()),
            mime_type: Some("application/pdf".into()),
            size: Some(2048),
            is_inline: false,
        });
        email.attachment_total = 3;
        let row = project_row(&email);
        assert_eq!(row.attachment.id, "9");
        assert_eq!(row.attachment.stem, "Joel-Solano-CV");
        assert_eq!(row.attachment.extension, ".pdf");
        assert_eq!(row.attachment.kind, "pdf");
        assert!(row.attachment.previewable);
        assert_eq!(row.attachment_total, 3);

        // Another file refreshes the row in place.
        email.attachment_total = 4;
        assert!(!same_email_row(&row, &project_row(&email)));
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
                owner_account_id: None,
                is_auto: false,
            },
            flectar_mail_core::models::Label {
                id: 4,
                name: "Follow up".into(),
                color: "#7c3aed".into(),
                keyword: "Follow_up".into(),
                position: 1,
                owner_account_id: None,
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
                owner_account_id: None,
                is_auto: false,
            },
            flectar_mail_core::models::Label {
                id: 4,
                name: "Follow Up".into(),
                color: "#7c3aed".into(),
                keyword: "Follow_Up".into(),
                position: 1,
                owner_account_id: None,
                is_auto: false,
            },
        ];

        let results = make_label_rows(&labels, Some(&selected), "FOLLOW");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "Follow Up");
        assert!(results[0].applied);
    }

    #[test]
    fn label_picker_only_offers_the_selected_account_and_global_labels() {
        let mut selected = message(12);
        selected.account_id = 7;
        selected.account = "Work".into();
        let label = |id, owner_account_id| flectar_mail_core::models::Label {
            id,
            name: format!("Label {id}"),
            color: "#2563eb".into(),
            keyword: format!("Label{id}"),
            position: id,
            owner_account_id,
            is_auto: false,
        };
        let labels = vec![label(1, Some(7)), label(2, Some(8)), label(3, None)];

        let results = make_label_rows(&labels, Some(&selected), "");

        assert_eq!(
            results.iter().map(|row| row.id).collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(results[0].account_name, "Work");
        assert_eq!(results[1].account_name, "All accounts");
        assert!(results[1].is_global);
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
                owner_account_id: None,
                is_auto: false,
            },
            flectar_mail_core::models::Label {
                id: 4,
                name: "Viaje".into(),
                color: "#7c3aed".into(),
                keyword: "Viaje".into(),
                position: 1,
                owner_account_id: None,
                is_auto: false,
            },
        ];

        let rows = make_rows(
            &[email],
            None,
            &HashSet::new(),
            &HashMap::new(),
            &labels,
            &AccountPresentationSettings::default(),
        );
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
            is_selectable: true,
            can_create_children: true,
            can_rename: true,
            can_delete: true,
            label: "Projects".into(),
            scope: Some(Scope::Folder(7)),
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
            owner_account_id: None,
            is_auto: false,
        }];

        let rows = make_mailbox_rows(&mailboxes, &HashMap::new(), &labels, &HashSet::new());
        assert!(rows[0].has_custom_color);
        assert_eq!(rows[0].color, slint::Color::from_rgb_u8(0x22, 0xc5, 0x5e));
        assert_eq!(
            mailbox_scope_title(Scope::Folder(7), &mailboxes, &labels),
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
            is_selectable: true,
            can_create_children: true,
            can_rename: true,
            can_delete: true,
            label: label.into(),
            scope: Some(Scope::Folder(folder_id)),
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
            owner_account_id: None,
            is_auto: false,
        }];
        assert!(project_label_rows(&labels, &[1], None, "")[0].name_has_emoji);
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
