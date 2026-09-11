//! One document input route for links, selection and reader controls.
use super::*;
use crate::renderer::InputModifiers;
fn banner_height(app: &AppWindow) -> f32 {
    if app.get_remote_images_blocked() {
        60.0
    } else {
        0.0
    }
}

fn repaint_reader(app: &AppWindow, renderer: &Rc<RefCell<GpuEmailRenderer>>, gpu: bool) {
    if !gpu {
        let (width, height) = email_viewport_size(app);
        let result =
            renderer
                .borrow_mut()
                .render_cpu_if_needed(width, height, app.window().scale_factor());
        match result {
            Ok(Some(frame)) => apply_cpu_frame(app, frame),
            Ok(None) => {}
            Err(error) => {
                app.global::<EmailReader>().set_notice(error.into());
                app.global::<EmailReader>().set_reader_mode(false);
                app.set_text_mode(true);
            }
        }
    }
    sync_reader_metadata(app, renderer);
    if !renderer.borrow().selection_active() {
        update_email_selection(app, renderer);
    }
    app.window().request_redraw();
}

pub(super) fn register_renderer_input_callbacks(
    app: &AppWindow,
    renderer: &Rc<RefCell<GpuEmailRenderer>>,
    gpu: bool,
) {
    let weak = app.as_weak();
    let r = renderer.clone();
    app.on_open_email_link(move |url| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        if url.starts_with('#') {
            if let Some(y) = r.borrow().fragment_y(&url) {
                let y = y + banner_height(&app);
                app.set_email_scroll_y(-y);
                app.invoke_email_scroll(y, app.get_email_viewport_height());
            }
            return;
        }
        if let Ok(parsed) = url::Url::parse(&url)
            && parsed.scheme() == "mailto"
        {
            app.invoke_open_compose();
            // Form decoding also handles UTF-8 and percent escapes in addresses.
            let address = percent_encoding::percent_decode_str(parsed.path())
                .decode_utf8_lossy()
                .into_owned();
            app.set_compose_to(address.into());
            for (key, value) in parsed.query_pairs() {
                match key.as_ref() {
                    "subject" => app.set_compose_subject(value.into_owned().into()),
                    "cc" => app.set_compose_cc(value.into_owned().into()),
                    "bcc" => app.set_compose_bcc(value.into_owned().into()),
                    "body" => app.invoke_edit_compose_body(value.into_owned().into(), 0, 0),
                    _ => {}
                }
            }
            return;
        }
        if let Err(error) = open_email_link(&url) {
            app.set_render_status(UiMessage::detail("Could not open link: {}", error));
        }
    });
    let r = renderer.clone();
    app.global::<EmailReader>()
        .on_link_at(move |x, y| r.borrow().link_at(x, y).unwrap_or_default().into());

    let weak = app.as_weak();
    let r = renderer.clone();
    let pending = Rc::new(Cell::new(false));
    app.on_email_pointer_event(move |x, y, kind, control, shift, alt, meta| {
        let changed = r.borrow_mut().handle_pointer_event(
            x,
            y,
            &kind,
            InputModifiers::new(control, shift, alt, meta),
        );
        let activation = r.borrow_mut().take_activation();
        let Some(app) = weak.upgrade() else {
            return;
        };
        if let Some(url) = activation {
            app.invoke_open_email_link(url.into());
        }
        if kind == "up" || kind == "cancel" {
            update_email_selection(&app, &r);
        }
        if changed && !pending.replace(true) {
            let weak = app.as_weak();
            let r = r.clone();
            let pending = pending.clone();
            Timer::single_shot(Duration::from_millis(16), move || {
                pending.set(false);
                if let Some(app) = weak.upgrade() {
                    repaint_reader(&app, &r, gpu);
                }
            });
        }
    });
    let weak = app.as_weak();
    let r = renderer.clone();
    app.on_email_scroll(move |y, height| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let dirty = r
            .borrow_mut()
            .set_visible_region((y - banner_height(&app)).max(0.0), height);
        if dirty {
            repaint_reader(&app, &r, gpu);
        }
    });
    let weak = app.as_weak();
    let r = renderer.clone();
    app.on_email_key_event(move |text, pressed, repeat, control, shift, alt, meta| {
        let copied = r.borrow_mut().handle_key_event(
            &text,
            pressed,
            repeat,
            InputModifiers::new(control, shift, alt, meta),
        );
        if let Some(app) = weak.upgrade() {
            if let Some(text) = copied {
                copy_text(&app, text);
            }
            if r.borrow().needs_repaint() {
                let (_, caret) = r.borrow().selection_carets();
                if caret.valid {
                    let top = (-app.get_email_scroll_y() - banner_height(&app)).max(0.0);
                    let height = app.get_email_viewport_height();
                    let y = if caret.y < top {
                        caret.y
                    } else if caret.y + caret.height > top + height {
                        caret.y + caret.height - height
                    } else {
                        top
                    };
                    app.set_email_scroll_y(-y - banner_height(&app));
                    r.borrow_mut().set_visible_region(y, height);
                }
                repaint_reader(&app, &r, gpu);
            }
        }
    });
    let weak = app.as_weak();
    let r = renderer.clone();
    app.on_select_email_text(move || {
        r.borrow_mut().select_all();
        if let Some(app) = weak.upgrade() {
            repaint_reader(&app, &r, gpu);
        }
    });
    let weak = app.as_weak();
    app.on_copy_email_status(move |status| {
        if let Some(app) = weak.upgrade() {
            match status.as_str() {
                "Copied HTML source" => {
                    app.set_render_status(UiMessage::plain("Copied HTML source"))
                }
                "Copied selected text" => {
                    app.set_render_status(UiMessage::plain("Copied selected text"))
                }
                "Copied email text" => app.set_render_status(UiMessage::plain("Copied email text")),
                "Copied link" => app.set_render_status(UiMessage::plain("Copied link")),
                _ => {}
            }
        }
    });
    let weak = app.as_weak();
    let r = renderer.clone();
    app.global::<EmailReader>()
        .on_find(move |query, direction| {
            let (index, count, y) = r.borrow_mut().find(&query, direction);
            if let Some(app) = weak.upgrade() {
                app.global::<EmailReader>()
                    .set_find_status(format!("{index} / {count}").into());
                if let Some(y) = y {
                    app.set_email_scroll_y(-y - banner_height(&app));
                    r.borrow_mut()
                        .set_visible_region(y, app.get_email_viewport_height());
                }
                repaint_reader(&app, &r, gpu);
            }
        });
    let weak = app.as_weak();
    let r = renderer.clone();
    app.global::<EmailReader>().on_command(move |command| {
        let Some(app) = weak.upgrade() else {
            return;
        };
        let reader = app.global::<EmailReader>();
        let requested_zoom = crate::preview_controls::zoom(&command, r.borrow().zoom, 0.5, 3.0);
        let command = if command.starts_with("zoom-set:") {
            "zoom-set"
        } else {
            command.as_str()
        };
        match command {
            "viewport" => repaint_reader(&app, &r, gpu),
            "zoom-set" | "zoom-in" | "zoom-out" | "zoom-reset" | "fit" => {
                let old = r.borrow().zoom;
                let zoom = match command {
                    "zoom-set" => requested_zoom,
                    "zoom-in" => old * 1.2,
                    "zoom-out" => old / 1.2,
                    "fit" => old / reader.get_width_ratio().max(1.0),
                    _ => 1.0,
                }
                .clamp(0.5, 3.0);
                r.borrow_mut().set_auto_fit(command == "fit");
                reader.set_auto_fit(command == "fit");
                r.borrow_mut().set_zoom(zoom);
                reader.set_zoom(zoom);
                reader.set_zoom_revision(reader.get_zoom_revision().wrapping_add(1));
                let banner = banner_height(&app);
                let scroll = -app.get_email_scroll_y();
                let scroll = if scroll < banner {
                    scroll
                } else {
                    banner + (scroll - banner) * zoom / old
                };
                app.set_email_scroll_y(-scroll);
                r.borrow_mut().set_visible_region(
                    (scroll - banner).max(0.0),
                    app.get_email_viewport_height(),
                );
                repaint_reader(&app, &r, gpu);
            }
            "clear-selection" => {
                r.borrow_mut().clear_selection();
                repaint_reader(&app, &r, gpu);
            }
            "copy-rich" => {
                let renderer = r.borrow();
                let text = renderer.selected_text().unwrap_or_default();
                let html = renderer.selected_html();
                if !crate::reader_clipboard::set_html(&html, &text) {
                    copy_text(&app, text);
                }
            }
            "copy-all" => copy_text(&app, r.borrow().plain_text()),
            "page-up" | "page-down" | "up" | "down" | "home" | "end" => {
                let height = app.get_email_viewport_height();
                let current = -app.get_email_scroll_y();
                let bottom = (app.get_email_content_aspect()
                    * app.get_email_viewport_width()
                    * reader.get_width_ratio()
                    + banner_height(&app)
                    - height)
                    .max(0.0);
                let y = match command {
                    "page-up" => current - height * 0.85,
                    "page-down" => current + height * 0.85,
                    "up" => current - 40.0,
                    "down" => current + 40.0,
                    "home" => 0.0,
                    _ => bottom,
                }
                .clamp(0.0, bottom);
                app.set_email_scroll_y(-y);
                r.borrow_mut()
                    .set_visible_region((y - banner_height(&app)).max(0.0), height);
                repaint_reader(&app, &r, gpu);
            }
            _ => {}
        }
    });
}
fn copy_text(app: &AppWindow, text: String) {
    app.set_clipboard_request(text.into());
    app.set_clipboard_request_id(app.get_clipboard_request_id().wrapping_add(1));
}
