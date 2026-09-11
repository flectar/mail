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
    app.global::<EmailReader>().set_body_pending(false);
    let draw = |name: &str, width: u32, height: u32| {
        slint::platform::update_timers_and_animations();
        app.window()
            .set_size(slint::PhysicalSize::new(width, height));
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
    let loaded_rows = app.get_emails();
    app.set_emails(ModelRc::default());
    app.set_mail_page_loading(true);
    draw("navigation-loading", 1280, 900);
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
    state.borrow_mut().messages[0].attachments = vec![flectar_mail_core::models::AttachmentMeta {
        id: 42,
        filename: Some("Project overview.pdf".into()),
        mime_type: Some("application/pdf".into()),
        size: Some(1024),
        is_inline: false,
    }];
    render_current(&app, &state, &runtime).unwrap();
    assert_eq!(app.global::<MailAttachments>().get_rows().row_count(), 1);
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
    assert_eq!(app.global::<MailAttachments>().get_rows().row_count(), 1);
    app.set_theme_mode("light".into());
    draw("mail-attachments-fallback", 1280, 900);
    draw("mail-attachments-landscape", 844, 390);
    assert!(
        app.get_email_viewport_height() >= 40.0,
        "Short windows must retain room for the body while attachments remain available in the toolbar"
    );

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
    state.borrow_mut().messages.clear();
    render_current(&app, &state, &runtime).unwrap();
    assert_eq!(app.global::<MailAttachments>().get_rows().row_count(), 0);
    assert_eq!(app.global::<EmailReader>().get_message_id(), -1);
    assert!(app.global::<EmailReader>().get_notice().is_empty());
    assert!(!app.global::<EmailReader>().get_available());
    app.hide().unwrap();
}
