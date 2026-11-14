//! A list opened further down than one read goes starts with a row that
//! loads the mail before it.
use i_slint_core::accessibility::AccessibleStringProperty;
use i_slint_core::item_tree::ItemRc;
use i_slint_core::window::WindowInner;
use slint::ComponentHandle;
use slint::platform::{
    Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use std::{cell::Cell, ops::ControlFlow, rc::Rc, time::Duration};

slint::slint! {
    import { AppTheme } from "../ui/design-system.slint";
    export { MotionSettings } from "../ui/design-system.slint";
    import { MailView } from "../ui/views/mail-view.slint";

    export component EarlierMailHarness inherits Window {
        preferred-width: 1200px;
        preferred-height: 600px;
        in property <bool> windowed: true;
        in-out property <int> asked;
        MailView {
            width: parent.width;
            height: parent.height;
            available-width: parent.width;
            mail_sidebar_collapsed: true;
            show-avatars: false;
            mail_oldest_first: true;
            emails: [
                { id: 300, sender: "Newest sender", subject: "Opened from a notification", selected: true },
            ];
            list_entries: root.windowed ? [
                { is_header: true, group_key: "earlier", group_kind: "earlier" },
                { show_row: true, email_index: 0, email: { id: 300, sender: "Newest sender", subject: "Opened from a notification", selected: true } },
            ] : [
                { show_row: true, email_index: 0, email: { id: 300, sender: "Newest sender", subject: "Opened from a notification", selected: true } },
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
            load_earlier_mail => { root.asked += 1; }
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

/// The shown element labelled `label` in the window.
fn find(window: &slint::Window, label: &str) -> Option<ItemRc> {
    let root = ItemRc::new_root(WindowInner::from_pub(window).component());
    root.visit_descendants(|item| {
        if item.is_visible()
            && item
                .accessible_string_property(AccessibleStringProperty::Label)
                .as_deref()
                == Some(label)
        {
            ControlFlow::Break(item.clone())
        } else {
            ControlFlow::Continue(())
        }
    })
}

fn click(window: &slint::Window, item: &ItemRc) {
    let geometry = item.geometry();
    let middle = item.map_to_window(geometry.origin) + geometry.size.to_vector() / 2.0;
    let position = slint::LogicalPosition::new(middle.x, middle.y);
    window.dispatch_event(WindowEvent::PointerMoved { position });
    window.dispatch_event(WindowEvent::PointerPressed {
        position,
        button: PointerEventButton::Left,
    });
    window.dispatch_event(WindowEvent::PointerReleased {
        position,
        button: PointerEventButton::Left,
    });
}

#[test]
fn a_list_opened_further_down_offers_its_earlier_mail() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    let clock = Rc::new(Cell::new(Duration::ZERO));
    slint::platform::set_platform(Box::new(Headless {
        window: window.clone(),
        clock: clock.clone(),
    }))
    .unwrap();
    let ui = EarlierMailHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(1200, 600));
    ui.global::<MotionSettings>().set_enabled(false);
    ui.show().unwrap();
    let settle = || {
        clock.set(clock.get() + Duration::from_secs(1));
        slint::platform::update_timers_and_animations();
        ui.window().request_redraw();
        window.draw_if_needed(|_| {});
    };
    settle();

    let button = find(ui.window(), "Show earlier mail").expect("the list starts with the row");
    click(ui.window(), &button);
    assert_eq!(ui.get_asked(), 1);

    // A list that starts at its start has nothing before it to offer.
    ui.set_windowed(false);
    settle();
    assert!(find(ui.window(), "Show earlier mail").is_none());
}
