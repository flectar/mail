//! One document input route for links, selection and reader controls.
use super::*;
use crate::renderer::InputModifiers;
fn banner_height(host: &ReaderRef) -> f32 {
    if host.ui().get_remote_images_blocked()
        && !host.ui().get_text_mode()
        && !host.ui().get_source_mode()
        && !host.email().get_reader_mode()
    {
        60.0
    } else {
        0.0
    }
}

fn repaint_reader(host: &ReaderRef, renderer: &Rc<RefCell<GpuEmailRenderer>>, gpu: bool) {
    repaint_reader_inner(host, renderer, gpu, false);
}

fn repaint_reader_inner(
    host: &ReaderRef,
    renderer: &Rc<RefCell<GpuEmailRenderer>>,
    gpu: bool,
    incremental: bool,
) {
    if !gpu {
        let (width, height) = email_viewport_size(host);
        let result = if incremental {
            renderer.borrow_mut().render_cpu_scroll_step(
                width,
                height,
                host.window().scale_factor(),
            )
        } else {
            renderer
                .borrow_mut()
                .render_cpu_if_needed(width, height, host.window().scale_factor())
        };
        match result {
            Ok(Some(frame)) => apply_cpu_frame(host, frame),
            Ok(None) => {}
            Err(error) => {
                host.email().set_notice(error.into());
                host.email().set_reader_mode(false);
                host.ui().set_text_mode(true);
            }
        }
    }
    sync_reader_metadata(host, renderer);
    if !renderer.borrow().selection_active() {
        update_email_selection(host, renderer);
    }
    host.window().request_redraw();
}

fn schedule_scroll_frame(
    host: &ReaderRef,
    renderer: &Rc<RefCell<GpuEmailRenderer>>,
    gpu: bool,
    pending: &Rc<Cell<bool>>,
) {
    if pending.replace(true) {
        return;
    }
    let reader = host.host();
    let renderer = Rc::downgrade(renderer);
    let pending = pending.clone();
    Timer::single_shot(Duration::from_millis(16), move || {
        pending.set(false);
        if let (Some(host), Some(renderer)) = (reader.upgrade(), renderer.upgrade()) {
            repaint_reader_inner(&host, &renderer, gpu, true);
            if !gpu && renderer.borrow().needs_repaint() {
                schedule_scroll_frame(&host, &renderer, gpu, &pending);
            }
        }
    });
}

pub(super) fn register_renderer_input_callbacks(
    host: &ReaderRef,
    renderer: &Rc<RefCell<GpuEmailRenderer>>,
    gpu: bool,
    opener: ExternalUrlOpener,
) {
    let reader = host.host();
    let appearance_renderer = renderer.clone();
    let appearance_pending = Rc::new(Cell::new(false));
    host.colors().on_refresh(move || {
        if appearance_pending.replace(true) {
            return;
        }
        let reader = reader.clone();
        let renderer = Rc::downgrade(&appearance_renderer);
        let pending = appearance_pending.clone();
        Timer::single_shot(Duration::ZERO, move || {
            pending.set(false);
            let (Some(host), Some(renderer)) = (reader.upgrade(), renderer.upgrade()) else {
                return;
            };
            let colors = crate::message_appearance::EmailAppearance::of(&host.colors());
            if renderer.borrow_mut().set_appearance(colors) {
                repaint_reader(&host, &renderer, gpu);
            }
            // Quotes in composers follow the main window's colors.
            if host.key() == ReaderKey::Main {
                crate::composer::message_colors_changed(host.app());
            }
        });
    });
    let reader = host.host();
    let r = renderer.clone();
    host.on_open_link(move |url| {
        let Some(host) = reader.upgrade() else {
            return;
        };
        if url.starts_with('#') {
            // The scroll below asks the renderer again, so let it go first.
            let target = r.borrow_mut().fragment_y(&url);
            if let Some(y) = target {
                let y = y + banner_height(&host);
                host.ui().set_email_scroll_y(-y);
                host.ui()
                    .invoke_email_scroll(y, host.ui().get_email_viewport_height());
            }
            return;
        }
        if flectar_mail_core::mailto::is_mailto(&url) {
            match host.key() {
                ReaderKey::Main => crate::mailto_links::open(host.app(), "message", [url.as_str()]),
                #[cfg(desktop)]
                ReaderKey::Window(_) => crate::mailto_links::open_from_window(host.app(), &url),
            }
            return;
        }
        if let Err(error) = open_email_link(&opener, &url) {
            host.app()
                .set_render_status(UiMessage::detail("Could not open link: {}", error));
        }
    });
    let r = renderer.clone();
    host.email()
        .on_link_at(move |x, y| r.borrow().link_at(x, y).unwrap_or_default().into());

    let reader = host.host();
    let r = renderer.clone();
    let pending = Rc::new(Cell::new(false));
    host.ui()
        .on_email_pointer_event(move |x, y, kind, control, shift, alt, meta| {
            let changed = r.borrow_mut().handle_pointer_event(
                x,
                y,
                &kind,
                InputModifiers::new(control, shift, alt, meta),
            );
            let activation = r.borrow_mut().take_activation();
            let Some(host) = reader.upgrade() else {
                return;
            };
            if let Some(url) = activation {
                host.open_link(url.into());
            }
            if kind == "up" || kind == "cancel" {
                update_email_selection(&host, &r);
            }
            if changed && !pending.replace(true) {
                let reader = host.host();
                let r = Rc::downgrade(&r);
                let pending = pending.clone();
                Timer::single_shot(Duration::from_millis(16), move || {
                    pending.set(false);
                    if let (Some(host), Some(r)) = (reader.upgrade(), r.upgrade()) {
                        repaint_reader(&host, &r, gpu);
                    }
                });
            }
        });
    let reader = host.host();
    let r = renderer.clone();
    let scroll_pending = Rc::new(Cell::new(false));
    host.ui().on_email_scroll(move |y, height| {
        let Some(host) = reader.upgrade() else {
            return;
        };
        let dirty = r
            .borrow_mut()
            .set_visible_region((y - banner_height(&host)).max(0.0), height);
        if dirty {
            schedule_scroll_frame(&host, &r, gpu, &scroll_pending);
        }
    });
    let reader = host.host();
    let r = renderer.clone();
    host.ui()
        .on_email_key_event(move |text, pressed, repeat, control, shift, alt, meta| {
            let copied = r.borrow_mut().handle_key_event(
                &text,
                pressed,
                repeat,
                InputModifiers::new(control, shift, alt, meta),
            );
            if let Some(host) = reader.upgrade() {
                if let Some(text) = copied {
                    copy_text(&host, text);
                }
                if r.borrow().needs_repaint() {
                    let (_, caret) = r.borrow().selection_carets();
                    if caret.valid {
                        let top = (-host.ui().get_email_scroll_y() - banner_height(&host)).max(0.0);
                        let height = host.ui().get_email_viewport_height();
                        let y = if caret.y < top {
                            caret.y
                        } else if caret.y + caret.height > top + height {
                            caret.y + caret.height - height
                        } else {
                            top
                        };
                        host.ui().set_email_scroll_y(-y - banner_height(&host));
                        r.borrow_mut().set_visible_region(y, height);
                    }
                    repaint_reader(&host, &r, gpu);
                }
            }
        });
    let reader = host.host();
    let r = renderer.clone();
    host.ui().on_select_email_text(move || {
        r.borrow_mut().select_all();
        if let Some(host) = reader.upgrade() {
            repaint_reader(&host, &r, gpu);
        }
    });
    let reader = host.host();
    host.ui().on_copy_email_status(move |status| {
        if let Some(host) = reader.upgrade() {
            match status.as_str() {
                "Copied HTML source" => host
                    .app()
                    .set_render_status(UiMessage::plain("Copied HTML source")),
                "Copied selected text" => host
                    .app()
                    .set_render_status(UiMessage::plain("Copied selected text")),
                "Copied email text" => host
                    .app()
                    .set_render_status(UiMessage::plain("Copied email text")),
                "Copied link" => host
                    .app()
                    .set_render_status(UiMessage::plain("Copied link")),
                "Copied the message source" => host
                    .app()
                    .set_render_status(UiMessage::plain("Copied the message source")),
                _ => {}
            }
        }
    });
    let reader = host.host();
    let r = renderer.clone();
    host.email().on_find(move |query, direction| {
        let (index, count, y) = r.borrow_mut().find(&query, direction);
        if let Some(host) = reader.upgrade() {
            host.email()
                .set_find_status(format!("{index} / {count}").into());
            if let Some(y) = y {
                host.ui().set_email_scroll_y(-y - banner_height(&host));
                r.borrow_mut()
                    .set_visible_region(y, host.ui().get_email_viewport_height());
            }
            repaint_reader(&host, &r, gpu);
        }
    });
    let reader = host.host();
    let r = renderer.clone();
    host.email().on_toggle_section(move |section| {
        let (Some(host), Ok(section)) = (reader.upgrade(), usize::try_from(section)) else {
            return;
        };
        let toggled = r.borrow_mut().toggle_section(section);
        if toggled {
            repaint_reader(&host, &r, gpu);
        }
    });
    let reader = host.host();
    let r = renderer.clone();
    host.email().on_command(move |command| {
        let Some(host) = reader.upgrade() else {
            return;
        };
        let reader = host.email();
        let requested_zoom = crate::preview_controls::zoom(&command, r.borrow().zoom, 0.5, 3.0);
        let command = if command.starts_with("zoom-set:") {
            "zoom-set"
        } else {
            command.as_str()
        };
        match command {
            "viewport" => repaint_reader(&host, &r, gpu),
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
                let banner = banner_height(&host);
                let scroll = -host.ui().get_email_scroll_y();
                let scroll = if scroll < banner {
                    scroll
                } else {
                    banner + (scroll - banner) * zoom / old
                };
                host.ui().set_email_scroll_y(-scroll);
                r.borrow_mut().set_visible_region(
                    (scroll - banner).max(0.0),
                    host.ui().get_email_viewport_height(),
                );
                repaint_reader(&host, &r, gpu);
            }
            "clear-selection" => {
                r.borrow_mut().clear_selection();
                repaint_reader(&host, &r, gpu);
            }
            "copy-rich" => {
                let renderer = r.borrow();
                let text = renderer.selected_text().unwrap_or_default();
                let html = renderer.selected_html();
                if !crate::reader_clipboard::set_html(&html, &text) {
                    copy_text(&host, text);
                }
            }
            "copy-all" => copy_text(&host, r.borrow().plain_text()),
            "page-up" | "page-down" | "up" | "down" | "home" | "end" => {
                let height = host.ui().get_email_viewport_height();
                let current = -host.ui().get_email_scroll_y();
                let bottom = (host.ui().get_email_scroll_document_height() - height).max(0.0);
                let y = match command {
                    "page-up" => current - height * 0.85,
                    "page-down" => current + height * 0.85,
                    "up" => current - 40.0,
                    "down" => current + 40.0,
                    "home" => 0.0,
                    _ => bottom,
                }
                .clamp(0.0, bottom);
                host.ui().set_email_scroll_y(-y);
                r.borrow_mut()
                    .set_visible_region((y - banner_height(&host)).max(0.0), height);
                repaint_reader(&host, &r, gpu);
            }
            _ => {}
        }
    });
}
pub(super) fn copy_text(host: &ReaderRef, text: String) {
    host.ui().set_clipboard_request(text.into());
    host.ui()
        .set_clipboard_request_id(host.ui().get_clipboard_request_id().wrapping_add(1));
}
