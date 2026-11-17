//! Delete actions in the reader must honor the checked mail selection.
use slint::platform::{
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, Rgb8Pixel};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};

slint::slint! {
    #[edition = "1.19"]
    import { Palette } from "std-widgets.slint";
    import { AppTheme } from "../ui/design-system.slint";
    export { MotionSettings } from "../ui/design-system.slint";
    import { MailView } from "../ui/views/mail-view.slint";

    export component MailDeletionHarness inherits Window {
        preferred-width: 1200px;
        preferred-height: 600px;
        in property <bool> compact: false;
        in property <bool> dark: false;
        in property <string> layout-mode: "default";
        in property <bool> checked: true;
        in property <bool> trash-view: false;
        in-out property <string> page: "message";
        callback single-action(string);
        callback bulk-action(string);

        init => { Palette.color-scheme = ColorScheme.light; }
        changed dark => { Palette.color-scheme = root.dark ? ColorScheme.dark : ColorScheme.light; }
        MailView {
            width: parent.width;
            height: parent.height;
            available-width: parent.width;
            compact: root.compact;
            layout-mode: root.layout-mode;
            compact-page <=> root.page;
            mail_sidebar_collapsed: true;
            show-avatars: false;
            selection-count: root.checked ? 2 : 0;
            has_selected: true;
            selected_sender: "Third sender";
            selected_subject: "Preview outside the checked selection";
            trash_view: root.trash-view;
            emails: [
                { id: 1, sender: "First sender", subject: "First checked message", checked: root.checked },
                { id: 2, sender: "Second sender", subject: "Second checked message", checked: root.checked },
                { id: 3, sender: "Third sender", subject: "Preview outside the checked selection", selected: true },
            ];
            list_entries: [
                { show_row: true, email_index: 0, email: { id: 1, sender: "First sender", subject: "First checked message", checked: root.checked } },
                { show_row: true, email_index: 1, email: { id: 2, sender: "Second sender", subject: "Second checked message", checked: root.checked } },
                { show_row: true, email_index: 2, email: { id: 3, sender: "Third sender", subject: "Preview outside the checked selection", selected: true } },
            ];
            accent: AppTheme.accent;
            accent-soft: AppTheme.accent-soft;
            avatar-bg: AppTheme.avatar-bg;
            avatar-selected-bg: AppTheme.avatar-selected-bg;
            body-text: AppTheme.body;
            border: AppTheme.border;
            border-subtle: AppTheme.border-subtle;
            control-bg: AppTheme.control-bg;
            faint-text: AppTheme.faint;
            heading: AppTheme.heading;
            muted-control-bg: AppTheme.muted-control-bg;
            muted-text: AppTheme.muted;
            secondary-text: AppTheme.secondary;
            selected-row: AppTheme.selected-row;
            surface: AppTheme.surface;
            surface-raised: AppTheme.surface-raised;
            title-color: AppTheme.title;
            message_action(action) => { root.single-action(action); }
            bulk_message_action(action) => { root.bulk-action(action); }
        }
    }
}

struct Headless {
    window: Rc<MinimalSoftwareWindow>,
    clock: Rc<Cell<Duration>>,
}

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.clock.get()
    }
}

#[test]
fn reader_deletion_uses_checked_messages_in_every_layout() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let clock = Rc::new(Cell::new(Duration::ZERO));
    slint::platform::set_platform(Box::new(Headless {
        window: window.clone(),
        clock: clock.clone(),
    }))
    .unwrap();

    for (name, compact, layout, width) in [
        ("desktop", false, "default", 1200),
        ("minimal", false, "minimal", 620),
        ("compact", true, "default", 390),
    ] {
        for dark in [false, true] {
            let ui = MailDeletionHarness::new().unwrap();
            ui.window().set_size(slint::PhysicalSize::new(width, 600));
            ui.global::<MotionSettings>().set_enabled(false);
            ui.set_compact(compact);
            ui.set_layout_mode(layout.into());
            ui.set_page("message".into());
            ui.set_dark(dark);
            let actions = Rc::new(RefCell::new(Vec::new()));
            let recorded = actions.clone();
            ui.on_single_action(move |action| {
                recorded.borrow_mut().push(("single", action.to_string()));
            });
            let recorded = actions.clone();
            ui.on_bulk_action(move |action| {
                recorded.borrow_mut().push(("bulk", action.to_string()));
            });
            ui.show().unwrap();

            let render = || {
                let mut pixels = vec![Rgb8Pixel::default(); width as usize * 600];
                for _ in 0..3 {
                    clock.set(clock.get() + Duration::from_secs(1));
                    slint::platform::update_timers_and_animations();
                    ui.window().request_redraw();
                    window.draw_if_needed(|renderer| {
                        renderer.render(&mut pixels, width as usize);
                    });
                }
                pixels
            };
            render();
            // Enter the reader after Minimal's layout initialization opens the list.
            ui.set_page("message".into());
            let pixels = render();
            if let Ok(dir) = std::env::var("FLECTAR_MAIL_TEST_SCREENSHOTS") {
                let bytes: Vec<u8> = pixels.iter().flat_map(|p| [p.r, p.g, p.b]).collect();
                image::save_buffer(
                    format!("{dir}/{name}-{}.png", if dark { "dark" } else { "light" }),
                    &bytes,
                    width,
                    600,
                    image::ColorType::Rgb8,
                )
                .unwrap();
            }
            let delete = || {
                // Trash is followed by the label and overflow controls.
                let position = slint::LogicalPosition::new(width as f32 - 130.0, 32.0);
                ui.window().dispatch_event(WindowEvent::PointerPressed {
                    position,
                    button: PointerEventButton::Left,
                });
                ui.window().dispatch_event(WindowEvent::PointerReleased {
                    position,
                    button: PointerEventButton::Left,
                });
            };

            delete();
            assert_eq!(
                actions.borrow_mut().drain(..).collect::<Vec<_>>(),
                [("bulk", "trash".into())],
                "{name}: reader trash must act on the checked messages, even when another message is open"
            );
            ui.set_trash_view(true);
            render();
            delete();
            assert_eq!(
                actions.borrow_mut().drain(..).collect::<Vec<_>>(),
                [("bulk", "delete_permanently".into())],
                "{name}: permanent deletion must use the bulk confirmation path"
            );
            ui.set_checked(false);
            render();
            delete();
            assert_eq!(
                actions.borrow_mut().drain(..).collect::<Vec<_>>(),
                [("single", "delete_permanently".into())],
                "{name}: clearing the checked selection restores the reader's single-message action"
            );
            ui.set_trash_view(false);
            render();
            delete();
            assert_eq!(
                actions.borrow_mut().drain(..).collect::<Vec<_>>(),
                [("single", "trash".into())],
                "{name}: trash without a checked selection acts on the open message"
            );
            ui.hide().unwrap();
        }
    }
}
