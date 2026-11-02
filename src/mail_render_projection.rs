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
            count: account_messages().count().to_string(),
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
                count: account_messages()
                    .filter(|message| scope_matches(message, scope))
                    .count()
                    .to_string(),
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
    app.global::<MessageColors>().set_original(false);
    app.global::<AccountMailPreferences>().invoke_close_reader();
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
    app.set_selected_label(email.label.clone().into());
    app.set_selected_is_draft(email.folder == "Drafts" || email.label == "DRAFT");
    app.set_selected_initials(email.initials.clone().into());
    app.set_selected_starred(email.starred);
    app.set_selected_unread(email.unread);
    app.set_selected_sender_verification(email.sender_verification.clone().into());
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
    let html = email.html.as_deref().unwrap_or(&fallback_html);
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
    app.set_selected_source(html.into());
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
    reader.set_width_ratio(
        (renderer.layout_width / app.get_email_viewport_width().max(1.0)).max(1.0),
    );
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
