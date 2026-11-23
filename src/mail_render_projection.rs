//! Slint projection for mailbox fixtures and rendered message frames.

use super::*;

/// Account headings and standard mailboxes for fixture rows. Fixtures carry
/// folder names rather than folder ids, so custom folders are not projected.
#[cfg(test)]
pub(super) fn fixture_mailboxes(messages: &[MailMessage]) -> Vec<MailboxEntry> {
    let mut accounts: Vec<(i64, &str)> = Vec::new();
    for message in messages {
        if !accounts.iter().any(|(id, _)| *id == message.account_id) {
            accounts.push((message.account_id, &message.account));
        }
    }

    let mut entries = Vec::new();
    for (account_id, account) in accounts {
        let account_messages = || {
            messages
                .iter()
                .filter(move |message| message.account_id == account_id)
        };
        entries.push(MailboxEntry {
            account_id,
            folder_id: -1,
            parent_folder_id: -1,
            depth: 0,
            has_children: false,
            is_standard: false,
            is_selectable: false,
            can_create_children: true,
            can_rename: false,
            can_delete: false,
            label: account.to_owned(),
            scope: None,
            context: account.to_owned(),
            detail: "Account folders".to_owned(),
            avatar: avatar_initials(account),
            is_account: true,
            // A collapsed account adds up its folders' badges.
            count: 0,
        });

        for mailbox in StandardMailbox::ALL {
            let scope = Scope::Account {
                account_id,
                mailbox,
            };
            entries.push(MailboxEntry {
                account_id,
                folder_id: -1,
                parent_folder_id: -1,
                depth: 0,
                has_children: false,
                is_standard: true,
                is_selectable: true,
                can_create_children: false,
                can_rename: false,
                can_delete: false,
                label: mailbox.label().to_owned(),
                scope: Some(scope),
                context: account.to_owned(),
                detail: String::new(),
                avatar: String::new(),
                is_account: false,
                count: u32::try_from(
                    account_messages()
                        .filter(|message| scope_matches(message, scope))
                        .count(),
                )
                .unwrap_or(u32::MAX),
            });
        }
    }
    entries
}

pub(super) fn avatar_initials(label: &str) -> String {
    let mut words = label
        .split_whitespace()
        .filter_map(|word| word.chars().next());
    let Some(first) = words.next() else {
        return "?".to_owned();
    };
    let second = words.next();
    second.map_or_else(
        || first.to_uppercase().collect(),
        |second| format!("{}{}", first.to_uppercase(), second.to_uppercase()),
    )
}

pub(super) fn update_email_selection(
    app: &AppWindow,
    email_renderer: &Rc<RefCell<GpuEmailRenderer>>,
) {
    let renderer = email_renderer.borrow();
    let selected_text = renderer.selected_text().unwrap_or_default();
    if app.get_selected_text().as_str() != selected_text {
        crate::reader_clipboard::set_primary(&selected_text);
    }
    app.set_selected_text(selected_text.into());
    app.set_has_selection(renderer.has_selection());
}

pub(super) fn email_viewport_size(app: &AppWindow) -> (u32, u32) {
    let width = app.get_email_viewport_width().max(1.0).ceil() as u32;
    let height = app.get_email_viewport_height().max(1.0).ceil() as u32;
    // Before Slint's first layout pass these output properties can still be
    // zero. Use the fallback canvas only for that initial frame.
    if width <= 1 || height <= 1 {
        (520, 900)
    } else {
        (width, height)
    }
}

/// Remove every actionable part of the previous body on empty/error transitions.
/// Source and fallback text are owned by the caller and remain readable.
pub(super) fn clear_reader_projection(app: &AppWindow) {
    let send_later = app.global::<SendLaterUi>();
    send_later.set_open_send_time(Default::default());
    send_later.set_open_send_error(Default::default());
    send_later.set_open_send_uncertain(false);
    let reminder = app.global::<ReminderUi>();
    reminder.set_open_remind_time(Default::default());
    reminder.set_open_due(false);
    reminder.set_can_remind(false);
    app.global::<MessageColors>().set_original(false);
    app.global::<AccountMailPreferences>().invoke_close_reader();
    app.global::<Unsubscribe>().set_available(false);
    let block = app.global::<BlockSender>();
    block.set_available(false);
    block.set_blocked(false);
    crate::invitation::clear(app);
    crate::read_receipts::clear(app);
    let reader = app.global::<EmailReader>();
    reader.set_available(false);
    reader.set_body_pending(false);
    reader.set_reader_mode(false);
    reader.set_find_open(false);
    reader.set_find_status("".into());
    reader.set_destination("".into());
    reader.set_context_link("".into());
    reader.set_width_ratio(1.0);
    reader.set_failed_images(0);
    reader.set_items(ModelRc::default());
    reader.set_images(ModelRc::default());
    reader.set_selection_start(Default::default());
    reader.set_selection_end(Default::default());
    app.set_email_tiles(ModelRc::default());
    app.set_email_links(ModelRc::default());
    app.set_selected_text("".into());
    app.set_has_selection(false);
}

/// A name and address as one mailbox for the clipboard, as a header writes it
/// (RFC 5322 section 3.4): a name that isn't plain words is quoted, so it
/// pastes back as one recipient. Non-ASCII names stay as they are (RFC 6532).
pub(super) fn mailbox_text(name: &str, address: &str) -> String {
    let address = address.trim();
    let name = name.chars().filter(|c| !c.is_control()).collect::<String>();
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() || name.eq_ignore_ascii_case(address) {
        return address.to_owned();
    }
    // `atext` from RFC 5322 section 3.2.3, plus RFC 6532's UTF-8.
    let plain_word =
        |c: char| c.is_ascii_alphanumeric() || !c.is_ascii() || "!#$%&'*+-/=?^_`{|}~".contains(c);
    if name.chars().all(|c| c == ' ' || plain_word(c)) {
        format!("{name} <{address}>")
    } else {
        let quoted = name.replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{quoted}\" <{address}>")
    }
}

/// Who a message is from and to, each with their address, and when it was
/// sent, as the reader's details list them.
pub(super) fn message_details(email: &MailMessage) -> MessageDetails {
    let line = |name: Option<&str>, address: &str| -> slint::SharedString {
        match name
            .map(str::trim)
            .filter(|name| !name.is_empty() && !name.eq_ignore_ascii_case(address))
        {
            Some(name) => format!("{name} <{address}>").into(),
            None => address.into(),
        }
    };
    let lines = |people: &[flectar_mail_core::models::Address]| {
        ModelRc::new(VecModel::from(
            people
                .iter()
                .map(|person| line(person.name.as_deref(), &person.email))
                .collect::<Vec<_>>(),
        ))
    };
    let date = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(email.date_ms)
        .filter(|_| email.date_ms > 0)
        .map(|date| {
            date_format::current().date_time(
                date_format::DateStyle::WeekdayMedium,
                date.with_timezone(&Local).naive_local(),
            )
        })
        .unwrap_or_default();
    MessageDetails {
        from: line(Some(&email.sender), &email.address),
        to: lines(&email.to_people),
        cc: lines(&email.cc_people),
        date: date.into(),
    }
}

pub(super) fn apply_email(
    app: &AppWindow,
    email: MailMessage,
    email_renderer: &Rc<RefCell<GpuEmailRenderer>>,
    use_wgpu: bool,
    allow_remote_images: bool,
) -> Result<(), String> {
    app.set_selected_sender(email.sender.clone().into());
    app.set_selected_address(email.address.clone().into());
    app.set_selected_subject(email.subject.clone().into());
    app.set_selected_time(message_time(email.date_ms, &Local::now()).into());
    app.set_selected_to(email.to.clone().into());
    app.set_selected_details(message_details(&email));
    app.set_selected_label(email.label.clone().into());
    app.set_selected_is_draft(
        email.folder == "Drafts" || email.send_at.is_some() || email.label == "DRAFT",
    );
    // In the Scheduled view: when the draft goes out, or why it did not.
    let send_later = app.global::<SendLaterUi>();
    send_later.set_open_send_time(
        email
            .send_at
            .map(|at| mail_view_model::upcoming_time(at, &Local::now()))
            .unwrap_or_default()
            .into(),
    );
    send_later.set_open_send_error(email.send_error.clone().unwrap_or_default().into());
    send_later.set_open_send_uncertain(email.send_uncertain);
    app.set_selected_initials(email.initials.clone().into());
    app.set_selected_starred(email.starred);
    app.set_selected_unread(email.unread);
    app.set_selected_sender_verification(email.sender_verification.clone().into());
    app.global::<Unsubscribe>()
        .set_available(email.can_unsubscribe);
    let block = app.global::<BlockSender>();
    block.set_available(email.can_block);
    block.set_blocked(email.sender_blocked);
    crate::invitation::project(app, &email);
    crate::read_receipts::project(app, &email);
    crate::translation::project(app, &email);
    crate::ai_features::project(app, &email);
    let reader = app.global::<EmailReader>();
    reader.set_body_pending(email.body_pending);
    reader.set_authored_text(email.text.clone().unwrap_or_default().into());
    let same_message = reader.get_message_id() == email.id;
    let mut scroll = if same_message {
        app.get_email_scroll_y()
    } else {
        0.0
    };
    if !same_message {
        app.global::<MessageColors>().set_original(false);
        app.global::<AccountMailPreferences>().invoke_close_reader();
        reader.set_auto_fit(true);
        email_renderer.borrow_mut().set_auto_fit(true);
        reader.set_use_authored_text(false);
        reader.set_message_id(email.id);
        reader.set_query("".into());
        reader.set_find_status("".into());
        reader.set_notice("".into());
        reader.set_destination("".into());
    }
    crate::attachment_controller::project(app, &email, same_message);
    app.set_email_scroll_y(scroll);

    let preview = display_preview(&email.preview);
    let fallback_html = format!(
        "<html><body style=\"font-family:Arial,sans-serif;padding:32px;line-height:1.6;color:#303348\"><p>{}</p></body></html>",
        preview
    );
    // A message the user translated shows its translation (#240).
    let translation = crate::translation::shown_html(i64::from(email.id));
    let html = translation
        .as_deref()
        .or(email.html.as_deref())
        .unwrap_or(&fallback_html);
    email_renderer
        .borrow_mut()
        .set_appearance(crate::message_appearance::EmailAppearance::from_app(app));
    use std::hash::{Hash, Hasher};
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    html.hash(&mut hash);
    let key = (hash.finish(), allow_remote_images);
    if same_message && email_renderer.borrow().loaded_key == Some(key) {
        return Ok(());
    }
    let blocked = renderer::has_remote_images(html) && !allow_remote_images;
    if same_message && app.get_remote_images_blocked() != blocked {
        scroll = (scroll + if blocked { -60.0 } else { 60.0 }).min(0.0);
    }
    // The source is the message's own, translated or not.
    app.set_selected_source(email.html.as_deref().unwrap_or(&fallback_html).into());
    app.set_remote_images_blocked(blocked);
    app.set_email_scroll_y(scroll);
    // Store readable recovery content before entering any renderer code.
    app.set_selected_plain_text(crate::email_document::fallback(html).into());
    let (viewport_width, viewport_height) = email_viewport_size(app);
    let scale = app.window().scale_factor();
    email_renderer.borrow_mut().preparation_viewport = (
        (viewport_width as f32 * scale).ceil() as u32,
        (viewport_height as f32 * scale).ceil() as u32,
        scale,
    );
    let bookmark = if same_message {
        email_renderer.borrow().selection_bookmark()
    } else {
        None
    };
    // Drop the old DOM, decoded images and tile model before allocating the
    // replacement. Selection offsets above are small and can be restored.
    email_renderer.borrow_mut().clear();
    app.set_email_tiles(ModelRc::default());
    app.set_email_links(ModelRc::default());
    reader.set_items(ModelRc::default());
    reader.set_images(ModelRc::default());
    let prepare_result = email_renderer
        .borrow()
        .prepare_email_html(html, allow_remote_images);
    let prepared = match prepare_result {
        Ok(prepared) => prepared,
        Err(error) => {
            email_renderer.borrow_mut().clear();
            clear_reader_projection(app);
            reader.set_notice(error.into());
            app.set_text_mode(true);
            return Ok(());
        }
    };
    reader.set_available(true);
    // Publish readable text independently of the raster surface.
    let plain_text = prepared.plain_text.clone();

    if !use_wgpu {
        // Release the previous tile model before allocating the first
        // viewport-sized tile set for the replacement message. Rendering is
        // synchronous here, so no intermediate frame is exposed.
        app.set_email_tiles(ModelRc::new(VecModel::default()));
        let mut renderer = email_renderer.borrow_mut();
        renderer.set_email(prepared);
        renderer.restore_selection(bookmark);
        renderer.loaded_key = Some(key);
        renderer.set_visible_region(-scroll, app.get_email_viewport_height());
        let (width, height) = email_viewport_size(app);
        let rendered =
            match renderer.render_cpu_if_needed(width, height, app.window().scale_factor()) {
                Ok(Some(frame)) => frame,
                Ok(None) => return Ok(()),
                Err(error) => {
                    clear_reader_projection(app);
                    reader.set_notice(error.into());
                    app.set_text_mode(true);
                    return Ok(());
                }
            };
        reader.set_zoom(renderer.zoom);
        reader.set_width_ratio((renderer.layout_width / width.max(1) as f32).max(1.0));
        reader.set_items(ModelRc::new(VecModel::from(renderer.reader_items())));
        reader.set_images(ModelRc::new(VecModel::from(renderer.image_placeholders())));
        reader.set_failed_images(renderer.failed_images());
        reader.set_notice(renderer.notice.clone().unwrap_or_default().into());
        drop(renderer);
        app.set_selected_plain_text(plain_text.into());
        update_email_selection(app, email_renderer);
        apply_cpu_frame(app, rendered);
        return Ok(());
    }

    let links = prepared.links.clone();
    email_renderer.borrow_mut().set_email(prepared);
    email_renderer.borrow_mut().restore_selection(bookmark);
    email_renderer.borrow_mut().loaded_key = Some(key);
    email_renderer
        .borrow_mut()
        .set_visible_region(-scroll, app.get_email_viewport_height());
    app.set_email_tiles(ModelRc::new(VecModel::default()));
    app.set_email_links(ModelRc::new(VecModel::from(
        links.into_iter().map(slint_email_link).collect::<Vec<_>>(),
    )));
    app.set_selected_plain_text(plain_text.into());
    update_email_selection(app, email_renderer);
    app.set_render_status(UiMessage::plain("Message ready."));
    Ok(())
}

pub(super) fn slint_email_link(link: renderer::EmailLink) -> EmailLink {
    EmailLink {
        x: link.x,
        y: link.y,
        width: link.width,
        height: link.height,
        url: link.url.into(),
        name: link.name.into(),
    }
}

pub(super) fn slint_email_tile(tile: renderer::RenderedEmailTile) -> EmailTile {
    EmailTile {
        image: tile.image,
        y: tile.y,
        height: tile.height,
    }
}

#[cfg(feature = "gpu-renderer")]
pub(super) fn apply_gpu_frame(app: &AppWindow, rendered: RenderedEmail) {
    apply_cpu_frame(app, rendered);
}

pub(super) fn apply_cpu_frame(app: &AppWindow, rendered: RenderedEmail) {
    let start = renderer::render_timings_enabled().then(std::time::Instant::now);
    app.set_email_content_aspect(rendered.height as f32 / rendered.width.max(1) as f32);
    let tiles: Vec<_> = rendered.tiles.into_iter().map(slint_email_tile).collect();
    let retained = app.get_email_tiles();
    if let Some(model) = retained.as_any().downcast_ref::<VecModel<EmailTile>>() {
        // Keep Slint image items (and their software-renderer caches) alive
        // when scrolling only adds or removes tiles at the viewport edges.
        crate::reconcile_model_rows_by(model, tiles, |tile| tile.y.to_bits(), PartialEq::eq);
    } else {
        app.set_email_tiles(ModelRc::new(VecModel::from(tiles)));
    }
    let links = rendered
        .links
        .into_iter()
        .map(slint_email_link)
        .collect::<Vec<_>>();
    if app.get_email_links().iter().collect::<Vec<_>>() != links {
        app.set_email_links(ModelRc::new(VecModel::from(links)));
    }
    app.set_render_status(UiMessage::plain("Message ready."));
    if let Some(start) = start {
        eprintln!(
            "email Slint model update: {:.2}ms",
            start.elapsed().as_secs_f64() * 1000.0
        );
    }
}

/// Show only the tiles the renderer still holds after it released memory.
pub(super) fn retain_email_tiles(app: &AppWindow, tiles: Vec<renderer::RenderedEmailTile>) {
    let tiles: Vec<_> = tiles.into_iter().map(slint_email_tile).collect();
    let retained = app.get_email_tiles();
    if let Some(model) = retained.as_any().downcast_ref::<VecModel<EmailTile>>() {
        crate::reconcile_model_rows_by(model, tiles, |tile| tile.y.to_bits(), PartialEq::eq);
    } else {
        app.set_email_tiles(ModelRc::new(VecModel::from(tiles)));
    }
}

pub(super) fn open_email_link(opener: &ExternalUrlOpener, url: &str) -> Result<(), String> {
    let Some((scheme, _)) = url.trim().split_once(':') else {
        return Err("only absolute links can be opened".into());
    };
    match scheme.to_ascii_lowercase().as_str() {
        "http" | "https" | "mailto" | "tel" => {
            opener.open_external(url.trim())?;
            Ok(())
        }
        _ => Err("unsupported link type".into()),
    }
}

pub(super) fn sync_reader_metadata(app: &AppWindow, renderer: &Rc<RefCell<GpuEmailRenderer>>) {
    let renderer = renderer.borrow();
    let reader = app.global::<EmailReader>();
    reader.set_zoom(renderer.zoom);
    reader.set_auto_fit(renderer.auto_fit);
    reader.set_available(renderer.has_document());
    if !renderer.has_document() {
        clear_reader_projection(app);
        if let Some(notice) = &renderer.notice {
            reader.set_notice(notice.clone().into());
            app.set_text_mode(true);
        }
        return;
    }
    // Against the width the tiles were painted for, as `apply_email` does: a
    // viewport the window hasn't reported yet would scale them up by
    // hundreds, showing only their first pixel.
    let (width, _) = email_viewport_size(app);
    reader.set_width_ratio((renderer.layout_width / width as f32).max(1.0));
    let (start, end) = renderer.selection_carets();
    reader.set_selection_start(start);
    reader.set_selection_end(end);
    if reader.get_revision() == renderer.metadata_revision as i32 {
        return;
    }
    reader.set_revision(renderer.metadata_revision as i32);
    reader.set_failed_images(renderer.failed_images());
    reader.set_items(ModelRc::new(VecModel::from(renderer.reader_items())));
    reader.set_images(ModelRc::new(VecModel::from(renderer.image_placeholders())));
    if let Some(notice) = &renderer.notice {
        reader.set_notice(notice.clone().into());
    }
    let plain = renderer.plain_text();
    if !plain.is_empty() {
        app.set_selected_plain_text(plain.into());
    }
}

#[cfg(test)]
mod tests {
    use super::mailbox_text;

    #[test]
    fn copied_mailboxes_quote_names_that_are_not_plain_words() {
        assert_eq!(
            mailbox_text("Ada Lovelace", "ada@example.com"),
            "Ada Lovelace <ada@example.com>"
        );
        assert_eq!(
            mailbox_text("Doe, Jane", "jane@example.com"),
            "\"Doe, Jane\" <jane@example.com>"
        );
        assert_eq!(
            mailbox_text("J. Doe", "j@example.com"),
            "\"J. Doe\" <j@example.com>"
        );
        assert_eq!(
            mailbox_text("Say \"hi\" \\o/", "x@example.com"),
            "\"Say \\\"hi\\\" \\\\o/\" <x@example.com>"
        );
        assert_eq!(
            mailbox_text("José Müller", "jose@example.com"),
            "José Müller <jose@example.com>"
        );
        assert_eq!(
            mailbox_text("O'Brien", "ob@example.com"),
            "O'Brien <ob@example.com>"
        );
    }

    #[test]
    fn copied_mailboxes_without_a_name_are_the_address() {
        assert_eq!(mailbox_text("", "ada@example.com"), "ada@example.com");
        assert_eq!(mailbox_text("  ", "ada@example.com"), "ada@example.com");
        assert_eq!(
            mailbox_text("ADA@example.com", "ada@example.com"),
            "ada@example.com"
        );
        assert_eq!(
            mailbox_text("Ada\r\n Lovelace", "ada@example.com"),
            "Ada Lovelace <ada@example.com>"
        );
    }
}
