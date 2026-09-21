//! Headless integration regression: real Slint projection and retained renderer.
use super::*;
use slint::Rgb8Pixel;
use slint::platform::{
    Platform, WindowAdapter,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
struct Headless(Rc<MinimalSoftwareWindow>, Rc<RefCell<String>>);
impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
    fn set_clipboard_text(&self, text: &str, _: slint::platform::Clipboard) {
        *self.1.borrow_mut() = text.to_owned();
    }
}

#[test]
fn automatic_selection_hydrates_and_reader_controls_are_responsive_and_keyboard_accessible() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let clipboard = Rc::new(RefCell::new(String::new()));
    slint::platform::set_platform(Box::new(Headless(window.clone(), clipboard.clone()))).unwrap();
    let app = AppWindow::new().unwrap();
    app.global::<ZoomApi>()
        .on_resolve(|action, current, min, max| {
            crate::preview_controls::zoom(&action, current, min, max)
        });
    app.set_startup_ready(true);
    app.set_startup_hydrated(true);
    app.set_connected_accounts(ModelRc::new(VecModel::from(vec![AccountRow {
        id: 1,
        ..Default::default()
    }])));
    app.set_active_view("mail".into());
    app.window().set_size(slint::PhysicalSize::new(1280, 900));
    app.show().unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (fav, _) = bounded_ui_channel();
    let (avatars, _) = bounded_ui_channel();
    let mut state = InboxState::empty(
        None,
        UiSender::new(fav, UiWake::new(app.as_weak(), |_| {})),
        None,
        UiSender::new(avatars, UiWake::new(app.as_weak(), |_| {})),
        false,
        false,
        WarmStartCacheWriter::spawn(&runtime, dir.path().join("warm.json")),
    );
    state.messages = mail::fixture_messages();
    state.messages.truncate(2);
    for (i, message) in state.messages.iter_mut().enumerate() {
        message.folder = "Inbox".into();
        message.body_pending = true;
        message.thread_id = Some(i as i64 + 1);
        message.html = Some(format!(
            "<body style='margin:0;padding:20px'><h2>Reader verification {i}</h2><p>Selectable <b>bold text</b> and <a href='https://example.com'>a working link</a>.</p><p><a style='display:inline-block;padding:16px;background:#e8ecff' href='https://example.com/shop'>Open details</a></p><img src='https://example.com/image.png' width='220' height='75' alt='A blocked image with an alternative'><p>Use Find, zoom, or the accessible Reader view.</p></body>"
        ));
    }
    state.mailboxes = fixture_mailboxes(&state.messages);
    let ids: Vec<_> = state.messages.iter().map(|m| m.id).collect();
    let renderer = state.email_renderer.clone();
    renderer
        .borrow_mut()
        .configure_resources(runtime.handle().clone(), false)
        .unwrap();
    register_renderer_input_callbacks(&app, &renderer, false);
    let requests = Rc::new(RefCell::new(Vec::new()));
    let requests2 = requests.clone();
    app.global::<EmailReader>()
        .on_ensure_body(move |id| requests2.borrow_mut().push(id));
    app.set_emails(state.email_rows.clone().into());
    app.set_sidebar_rows(Rc::clone(&state.sidebar_rows).into());
    let state = Rc::new(RefCell::new(state));
    render_current(&app, &state, &runtime).unwrap();
    assert_eq!(requests.borrow().last(), Some(&ids[0]));
    let sidebar = app.get_sidebar_rows();
    assert!(sidebar.row_count() > 0);
    render_current(&app, &state, &runtime).unwrap();
    assert_eq!(
        sidebar,
        app.get_sidebar_rows(),
        "selecting a message must not rebuild unchanged folders"
    );
    // Archive removes the current row, and render_current chooses the next.
    state.borrow_mut().messages.remove(0);
    render_current(&app, &state, &runtime).unwrap();
    assert_eq!(requests.borrow().last(), Some(&ids[1]));
    assert_eq!(app.global::<EmailReader>().get_message_id(), ids[1]);
    assert!(
        app.get_selected_plain_text()
            .contains("Reader verification 1")
    );
    let draw = |name: &str, width: u32, height: u32| {
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
        slint::platform::update_timers_and_animations();
        // Visual regression frames capture the settled finite transitions.
        // Loading shimmers use animation-tick and intentionally remain active.
        std::thread::sleep(std::time::Duration::from_millis(280));
        slint::platform::update_timers_and_animations();
        app.window().request_redraw();
        let mut pixels = vec![Rgb8Pixel::default(); (width * height) as usize];
        window.draw_if_needed(|r| {
            r.render(&mut pixels, width as usize);
        });
        let (w, h) = email_viewport_size(&app);
        if let Some(frame) = renderer
            .borrow_mut()
            .render_cpu_if_needed(w, h, 1.0)
            .unwrap()
        {
            apply_cpu_frame(&app, frame);
        }
        sync_reader_metadata(&app, &renderer);
        app.window().request_redraw();
        window.draw_if_needed(|r| {
            r.render(&mut pixels, width as usize);
        });
        let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
        std::fs::create_dir_all("tmp/blitz-fixes").unwrap();
        image::save_buffer(
            format!("tmp/blitz-fixes/{name}.png"),
            &bytes,
            width,
            height,
            image::ColorType::Rgb8,
        )
        .unwrap();
    };
    app.set_theme_mode("light".into());
    draw("message-loading", 1280, 900);
    app.global::<EmailReader>().set_body_pending(false);
    let loaded_rows = app.get_emails();
    app.set_emails(ModelRc::default());
    app.set_mail_page_loading(true);
    draw("navigation-loading", 1280, 900);
    app.set_show_avatars(false);
    draw("navigation-loading-no-avatars", 1280, 900);
    app.set_workspace_layout("minimal".into());
    draw("navigation-loading-minimal-no-avatars", 1280, 900);
    app.set_workspace_layout("default".into());
    app.set_show_avatars(true);
    app.set_mail_page_loading(false);
    app.set_emails(loaded_rows);
    draw("desktop", 1280, 900);
    let pointer = |x: f32, y: f32| {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerPressed {
                position: slint::LogicalPosition::new(x, y),
                button: slint::platform::PointerEventButton::Left,
            });
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerReleased {
                position: slint::LogicalPosition::new(x, y),
                button: slint::platform::PointerEventButton::Left,
            });
    };
    let key = |text: slint::SharedString, pressed: bool| {
        app.window().dispatch_event(if pressed {
            slint::platform::WindowEvent::KeyPressed { text }
        } else {
            slint::platform::WindowEvent::KeyReleased { text }
        });
    };
    // Icon controls must remain keyboard operable after replacing native buttons.
    pointer(721.0, 222.0);
    assert_eq!(
        app.get_email_scroll_y(),
        0.0,
        "zoom must not hide the remote-image consent banner"
    );
    let zoom_after_click = renderer.borrow().zoom;
    assert!(zoom_after_click < 1.0);
    key(slint::platform::Key::Return.into(), true);
    key(slint::platform::Key::Return.into(), false);
    assert!(renderer.borrow().zoom < zoom_after_click);
    key(slint::platform::Key::Tab.into(), true);
    key(slint::platform::Key::Tab.into(), false);
    key(slint::platform::Key::Space.into(), true);
    key(slint::platform::Key::Space.into(), false);
    assert_eq!(
        renderer.borrow().zoom,
        1.0,
        "Tab and Space must reach reset zoom"
    );
    app.global::<EmailReader>()
        .invoke_command("zoom-set:125%".into());
    assert_eq!(renderer.borrow().zoom, 1.25);
    app.global::<EmailReader>()
        .invoke_command("zoom-set:NaN".into());
    assert_eq!(renderer.borrow().zoom, 1.25);
    app.global::<EmailReader>()
        .invoke_command("zoom-set:100%".into());
    // Edit the real percentage field, then exercise buttons after its initial
    // text binding has been overwritten. Clipboard observes the displayed value.
    let shortcut = |letter: &str| {
        key(slint::platform::Key::Control.into(), true);
        key(letter.into(), true);
        key(letter.into(), false);
        key(slint::platform::Key::Control.into(), false);
    };
    let copy_zoom = || {
        pointer(820.0, 222.0);
        shortcut("a");
        shortcut("c");
        clipboard.borrow().clone()
    };
    pointer(820.0, 222.0);
    shortcut("a");
    key("21%".into(), true);
    key("21%".into(), false);
    key(slint::platform::Key::Return.into(), true);
    key(slint::platform::Key::Return.into(), false);
    draw("zoom-clamped", 1280, 900);
    assert_eq!(renderer.borrow().zoom, 0.5);
    assert_eq!(
        copy_zoom(),
        "50%",
        "typed values must show the applied clamp"
    );
    pointer(874.0, 222.0);
    draw("zoom-in", 1280, 900);
    assert_eq!(copy_zoom(), "60%");
    pointer(721.0, 222.0);
    draw("zoom-out", 1280, 900);
    assert_eq!(copy_zoom(), "50%");
    // Fit may leave zoom unchanged; it must still discard an unsubmitted draft.
    shortcut("a");
    key("21%".into(), true);
    key("21%".into(), false);
    pointer(919.0, 222.0);
    draw("zoom-fit", 1280, 900);
    assert_eq!(
        copy_zoom(),
        format!(
            "{}%",
            (app.global::<EmailReader>().get_zoom() * 100.0).round()
        )
    );
    pointer(764.0, 222.0);
    draw("zoom-reset", 1280, 900);
    assert_eq!(copy_zoom(), "100%");
    shortcut("a");
    key("NaN".into(), true);
    key("NaN".into(), false);
    key(slint::platform::Key::Return.into(), true);
    key(slint::platform::Key::Return.into(), false);
    draw("zoom-invalid", 1280, 900);
    assert_eq!(copy_zoom(), "100%");
    draw("desktop-tooltip", 1280, 900);
    pointer(750.0, 390.0);
    key(slint::platform::Key::Control.into(), true);
    key("a".into(), true);
    key("a".into(), false);
    assert!(
        app.get_has_selection(),
        "Ctrl+A must reach the rendered body"
    );
    key("c".into(), true);
    key("c".into(), false);
    key(slint::platform::Key::Control.into(), false);
    assert!(
        clipboard.borrow().contains("Reader verification"),
        "The hidden clipboard bridge must remain functional"
    );
    let activated = Rc::new(RefCell::new(String::new()));
    let target = activated.clone();
    app.on_open_email_link(move |url| *target.borrow_mut() = url.to_string());
    key(slint::platform::Key::Tab.into(), true);
    key(slint::platform::Key::Tab.into(), false);
    key(slint::platform::Key::Return.into(), true);
    key(slint::platform::Key::Return.into(), false);
    assert!(
        activated.borrow().starts_with("https://example.com"),
        "Tab must leave the document for its first actionable link"
    );
    app.global::<EmailReader>()
        .invoke_find("selectable".into(), 0);
    assert_eq!(app.global::<EmailReader>().get_find_status(), "1 / 1");
    app.global::<EmailReader>().invoke_command("zoom-in".into());
    assert!(renderer.borrow().zoom > 1.0);
    app.set_theme_mode("dark".into());
    draw("desktop-dark", 1280, 900);
    app.set_theme_mode("light".into());
    draw("phone-list", 390, 844);
    pointer(30.0, 30.0);
    draw("phone-folders", 390, 844);
    pointer(360.0, 220.0);
    draw("phone-list-closed", 390, 844);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position: slint::LogicalPosition::new(210.0, 180.0),
            button: slint::platform::PointerEventButton::Left,
        });
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position: slint::LogicalPosition::new(210.0, 180.0),
            button: slint::platform::PointerEventButton::Left,
        });
    draw("phone", 390, 844);
    draw("tablet", 768, 1024);
    draw("phone-landscape", 844, 390);
    app.global::<EmailReader>().set_reader_mode(true);
    app.global::<EmailReader>().set_dark_reader(true);
    app.set_theme_mode("dark".into());
    draw("reader-dark", 390, 844);
    app.invoke_select_email_text();
    assert!(app.get_has_selection());
    // Hydrated attachments must survive a body-rendering failure and remain
    // projected independently of the HTML/plain-text reader.
    state.borrow_mut().messages[0].attachments = vec![
        flectar_mail_core::models::AttachmentMeta {
            id: 42,
            filename: Some("Project overview.pdf".into()),
            mime_type: Some("application/pdf".into()),
            size: Some(1024),
            is_inline: false,
        },
        flectar_mail_core::models::AttachmentMeta {
            id: 43,
            filename: Some("email-logo.png".into()),
            mime_type: Some("image/png".into()),
            size: Some(512),
            is_inline: false,
        },
    ];
    render_current(&app, &state, &runtime).unwrap();
    assert_eq!(app.global::<MailAttachments>().get_rows().row_count(), 2);
    // A structural rejection while the reader is open must never leave the
    // previous message's accessible content visible over the fallback.
    state.borrow_mut().messages[0].html = Some("<span>x</span>".repeat(16_000));
    render_current(&app, &state, &runtime).unwrap();
    assert!(app.get_text_mode());
    assert!(!app.global::<EmailReader>().get_available());
    assert!(!app.get_has_selection());
    assert!(app.get_selected_text().is_empty());
    assert!(!app.global::<EmailReader>().get_reader_mode());
    assert_eq!(app.global::<EmailReader>().get_items().row_count(), 0);
    assert!(
        !app.get_selected_plain_text()
            .contains("Reader verification")
    );
    assert_eq!(app.global::<MailAttachments>().get_rows().row_count(), 2);
    let attachment_rows = app.global::<MailAttachments>().get_rows();
    let mut image_attachment = attachment_rows.row_data(1).unwrap();
    let mut thumbnail = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(96, 64);
    for (index, pixel) in thumbnail.make_mut_slice().iter_mut().enumerate() {
        *pixel = slint::Rgba8Pixel::new(24, 120 + (index % 96) as u8, 176, 255);
    }
    image_attachment.thumbnail = slint::Image::from_rgba8(thumbnail);
    image_attachment.has_thumbnail = true;
    attachment_rows.set_row_data(1, image_attachment);
    app.set_theme_mode("light".into());
    draw("mail-attachments-fallback", 1280, 900);
    draw("mail-attachments-landscape", 844, 390);
    assert!(
        app.get_email_viewport_height() >= 40.0,
        "Short windows must retain room for the body while attachments remain available in the toolbar"
    );
    app.set_text_mode(false);
    app.set_email_content_aspect(8.0);
    app.global::<EmailReader>().invoke_command("end".into());
    draw("mail-long-message-actions", 1280, 900);

    // Exercise the same percentage editor in the attachment dialog. A wide
    // bitmap verifies fit on resize and manual zoom preservation without a
    // network dependency or loading a native PDF library in this UI test.
    let attachments = app.global::<MailAttachments>();
    attachments.set_name("Project overview.pdf".into());
    let mut page_pixels = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(2400, 1600);
    for (index, pixel) in page_pixels.make_mut_slice().iter_mut().enumerate() {
        *pixel = slint::Rgba8Pixel::new(
            (index % 2400 / 10) as u8,
            (index / 2400 / 7) as u8,
            128,
            255,
        );
    }
    attachments.set_image(slint::Image::from_rgba8(page_pixels));
    attachments.set_pages(2);
    attachments.set_is_image(true);
    attachments.set_open(true);
    draw("mail-preview-dialog", 1280, 900);
    let copy_preview_zoom = || {
        pointer(280.0, 128.0);
        shortcut("a");
        shortcut("c");
        clipboard.borrow().clone()
    };
    let fitted = copy_preview_zoom();
    assert_ne!(
        fitted, "100%",
        "Wide pages should fit the dialog on opening"
    );
    shortcut("a");
    key("150%".into(), true);
    key("150%".into(), false);
    key(slint::platform::Key::Return.into(), true);
    key(slint::platform::Key::Return.into(), false);
    draw("mail-preview-zoom", 1280, 900);
    assert_eq!(copy_preview_zoom(), "150%");
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerPressed {
            position: slint::LogicalPosition::new(900.0, 500.0),
            button: slint::platform::PointerEventButton::Left,
        });
    for (x, y) in [(700.0, 450.0), (600.0, 400.0)] {
        app.window()
            .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                position: slint::LogicalPosition::new(x, y),
            });
    }
    draw("mail-preview-pan", 1280, 900);
    app.window()
        .dispatch_event(slint::platform::WindowEvent::PointerReleased {
            position: slint::LogicalPosition::new(600.0, 400.0),
            button: slint::platform::PointerEventButton::Left,
        });
    let before = image::open("tmp/blitz-fixes/mail-preview-zoom.png")
        .unwrap()
        .to_rgb8();
    let after = image::open("tmp/blitz-fixes/mail-preview-pan.png")
        .unwrap()
        .to_rgb8();
    assert_ne!(
        before.get_pixel(700, 500),
        after.get_pixel(700, 500),
        "Dragging must pan an enlarged preview"
    );
    draw("mail-preview-resize", 1200, 800);
    draw("mail-preview-restored", 1280, 900);
    assert_eq!(
        copy_preview_zoom(),
        "150%",
        "Resizing must preserve manual zoom"
    );
    pointer(378.0, 128.0);
    draw("mail-preview-fit", 1280, 900);
    assert_eq!(copy_preview_zoom(), fitted);
    let weak = app.as_weak();
    attachments.on_command(move |action, _| {
        if action == "close" {
            weak.unwrap().global::<MailAttachments>().set_open(false);
        }
    });
    key(slint::platform::Key::Escape.into(), true);
    key(slint::platform::Key::Escape.into(), false);
    assert!(
        !attachments.get_open(),
        "Escape must close a preview while its zoom editor is focused"
    );
    // High-frequency scroll input updates the final position immediately,
    // but raster work is deferred and coalesced into one timer callback.
    app.set_remote_images_blocked(false);
    app.set_text_mode(false);
    let html = format!(
        "<body>{}</body>",
        "<p style='height:110px'>Scroll integration</p>".repeat(100)
    );
    renderer.borrow_mut().set_zoom(1.0);
    renderer.borrow_mut().set_auto_fit(false);
    renderer
        .borrow_mut()
        .set_email(renderer::prepare_email_html(&html).unwrap());
    let (width, height) = email_viewport_size(&app);
    let frame = renderer
        .borrow_mut()
        .render_cpu_if_needed(width, height, 1.0)
        .unwrap()
        .unwrap();
    apply_cpu_frame(&app, frame);
    let tiles_model = app.get_email_tiles();
    let tiles_before = renderer.borrow().tile_count;
    let layouts_before = renderer.borrow().layout_count;
    for y in [600.0, 1500.0, 3000.0] {
        app.invoke_email_scroll(y, height as f32);
    }
    assert_eq!(
        renderer.borrow().tile_count,
        tiles_before,
        "input must not rasterize synchronously"
    );
    std::thread::sleep(Duration::from_millis(25));
    slint::platform::update_timers_and_animations();
    assert!(renderer.borrow().tile_count > tiles_before);
    assert!(renderer.borrow().tile_count <= tiles_before + 5);
    assert_eq!(renderer.borrow().layout_count, layouts_before);
    assert_eq!(
        app.get_email_tiles(),
        tiles_model,
        "scroll must retain Slint's tile model"
    );

    state.borrow_mut().messages.clear();
    render_current(&app, &state, &runtime).unwrap();
    assert_eq!(app.global::<MailAttachments>().get_rows().row_count(), 0);
    assert_eq!(app.global::<EmailReader>().get_message_id(), -1);
    assert!(app.global::<EmailReader>().get_notice().is_empty());
    assert!(!app.global::<EmailReader>().get_available());
    // Renderer preferences persist independently of database startup and take
    // effect only after restart. This test never initializes a GPU.
    let preferences = tempfile::tempdir().unwrap();
    let preference_path = preferences.path().join("renderer.json");
    crate::renderer_preferences::register(
        &app,
        preference_path.clone(),
        crate::renderer_preferences::RendererMode::Cpu,
        false,
        true,
    );
    let renderer_settings = app.global::<RendererSettings>();
    renderer_settings.invoke_choose("gpu".into());
    assert_eq!(
        crate::renderer_preferences::load(&preference_path),
        crate::renderer_preferences::RendererMode::Gpu
    );
    assert_eq!(renderer_settings.get_active(), "cpu");
    assert!(renderer_settings.get_restart_required());
    app.set_theme_mode("light".into());
    app.set_settings_tab("General".into());
    app.set_settings_open(true);
    draw("renderer-settings-desktop", 1280, 900);
    draw("renderer-settings-phone", 390, 844);
    renderer_settings.invoke_choose("cpu".into());
    assert!(!renderer_settings.get_restart_required());
    assert_eq!(
        crate::renderer_preferences::load(&preference_path),
        crate::renderer_preferences::RendererMode::Cpu
    );
    // A failed atomic write must retain the actual saved choice.
    crate::renderer_preferences::register(
        &app,
        preferences.path().to_owned(),
        crate::renderer_preferences::RendererMode::Cpu,
        false,
        true,
    );
    renderer_settings.invoke_choose("gpu".into());
    assert_eq!(renderer_settings.get_preferred(), "cpu");
    assert!(!renderer_settings.get_error().is_empty());
    app.set_settings_open(false);
    app.set_active_view("contacts".into());
    app.set_contacts(ModelRc::default());
    app.set_contact_loading_more(true);
    draw("contacts-loading", 1280, 900);
    draw("contacts-loading-phone", 390, 844);
    app.set_theme_mode("dark".into());
    draw("contacts-loading-dark", 1280, 900);
    app.set_theme_mode("light".into());
    app.set_contact_loading_more(false);
    app.set_active_view("files".into());
    app.global::<FilesUi>().set_rows(ModelRc::default());
    app.global::<FilesUi>().set_list_loading(true);
    draw("files-loading", 1280, 900);
    draw("files-loading-phone", 390, 844);
    app.set_theme_mode("dark".into());
    draw("files-loading-dark", 1280, 900);
    app.set_theme_mode("light".into());
    app.global::<FilesUi>().set_list_loading(false);
    app.set_active_view("mail".into());
    for view in ["calendar", "contacts", "files", "mail", "calendar", "mail"] {
        app.set_active_view(view.into());
        draw(&format!("product-switch-{view}"), 1280, 900);
        if view == "mail" {
            assert!(app.get_email_viewport_width() > 1.0);
            assert!(app.get_email_viewport_height() > 1.0);
        }
    }
    // Releasing a product must retain the source record of an edited contact.
    let contact = ContactRecord {
        id: 1,
        name: "Original name".into(),
        email: "contact@example.com".into(),
        phone: String::new(),
        company: String::new(),
        job_title: String::new(),
        website: String::new(),
        birthday: String::new(),
        postal_address: String::new(),
        notes: String::new(),
        tags: String::new(),
        is_favorite: false,
        interactions: 0,
        last_interacted: None,
        account_ids: Vec::new(),
        is_managed: false,
    };
    let directory = Rc::new(RefCell::new(ContactDirectoryState::new(
        vec![contact],
        false,
    )));
    apply_contact_directory(&app, &directory);
    app.set_contact_name("Unsaved name".into());
    assert!(!contacts::release_directory(
        &app,
        &mut directory.borrow_mut()
    ));
    app.set_active_view("contacts".into());
    draw("contact-edit-before-switch", 1280, 900);
    app.set_active_view("mail".into());
    draw("contact-edit-away", 1280, 900);
    app.set_active_view("contacts".into());
    draw("contact-edit-after-switch", 1280, 900);
    assert_eq!(app.get_contact_name(), "Unsaved name");
    assert_eq!(directory.borrow().contacts.len(), 1);
    app.set_contact_name("Original name".into());
    assert!(contacts::release_directory(
        &app,
        &mut directory.borrow_mut()
    ));
    assert!(directory.borrow().contacts.is_empty());
    app.hide().unwrap();
}
