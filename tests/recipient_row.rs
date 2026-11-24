use i_slint_core::accessibility::AccessibleStringProperty;
use i_slint_core::item_tree::ItemRc;
use i_slint_core::lengths::LogicalPoint;
use i_slint_core::window::WindowInner;
use slint::platform::{
    Platform, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{Model, ModelRc, Rgb8Pixel, VecModel};
use std::ops::ControlFlow;
use std::rc::Rc;

slint::slint! {
    #[edition = "1.19"]
    import { RecipientRow } from "../ui/components/composer-controls.slint";
    import { ComposerRecipients } from "../ui/components/composer-fields.slint";
    import { ComposeContact } from "../ui/models.slint";

    export component RecipientHarness inherits Window {
        preferred-width: 520px;
        preferred-height: 240px;
        in-out property <string> recipients;
        out property <length> row-height: row.height;

        public function focus-row() {
            row.focus();
        }

        row := RecipientRow {
            x: 0px;
            y: 0px;
            width: 520px;
            label: "To";
            text <=> root.recipients;
        }
    }

    // The composer's recipients, with suggestions from `search` that
    // `complete` writes in, as the composer's controller does.
    export component SuggestionHarness inherits Window {
        preferred-width: 520px;
        preferred-height: 240px;
        in-out property <string> to;
        in property <bool> open;
        in property <[ComposeContact]> found;
        callback search(string);
        callback complete(int, string) -> string;

        ComposerRecipients {
            x: 0px;
            y: 0px;
            width: 520px;
            open: root.open;
            to <=> root.to;
            contacts: root.found;
            search-contacts(query) => { root.search(query); }
            contact-selected(index) => { root.to = root.complete(index, root.to); }
        }
    }
}

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn render(window: &MinimalSoftwareWindow) {
    slint::platform::update_timers_and_animations();
    let mut buffer = vec![Rgb8Pixel::default(); 520 * 240];
    window.request_redraw();
    window.draw_if_needed(|renderer| {
        renderer.render(&mut buffer, 520);
    });
}

/// The vertical span of the editable text in window coordinates.
fn text_span(ui: &RecipientHarness, text: &str) -> (f32, f32) {
    let inner = WindowInner::from_pub(ui.window());
    let field = ItemRc::new_root(inner.component())
        .visit_descendants(|item| {
            if item
                .accessible_string_property(AccessibleStringProperty::Value)
                .as_deref()
                == Some(text)
            {
                ControlFlow::Break(item.clone())
            } else {
                ControlFlow::Continue(())
            }
        })
        .expect("the recipient field");
    let top = field.map_to_window(LogicalPoint::default()).y;
    (top, top + field.geometry().height())
}

#[test]
fn recipients_wrap_to_five_lines_then_follow_the_caret() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let ui = RecipientHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(520, 240));
    ui.show().unwrap();
    render(&window);
    assert_eq!(ui.get_row_height(), 40.0, "an empty row keeps its height");

    ui.invoke_focus_row();
    let key = |text: &str| {
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.into() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text: text.into() });
    };

    key("ada@example.com, grace@example.com");
    render(&window);
    assert_eq!(ui.get_row_height(), 40.0, "one line stays one row");

    let mut recipients = String::from("ada@example.com, grace@example.com");
    for index in 0..60 {
        let next = format!(", person{index}@example.com");
        key(&next);
        recipients.push_str(&next);
    }
    render(&window);
    let capped = ui.get_row_height();
    assert!(
        capped > 80.0 && capped < 160.0,
        "a long list grows the row to its cap: {capped}"
    );

    let (top, bottom) = text_span(&ui, &recipients);
    assert!(top < 0.0, "the start scrolled out of view: {top}");
    assert!(
        bottom <= capped,
        "the caret at the end stays inside the row: {bottom} > {capped}"
    );

    key(&slint::SharedString::from(slint::platform::Key::Return));
    assert_eq!(
        ui.get_recipients(),
        recipients,
        "Return must not insert a line break"
    );
}

#[test]
fn typing_goes_on_after_a_chosen_suggestion() {
    let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
    slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
    let ui = SuggestionHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(520, 240));
    ui.show().unwrap();

    // People whose name starts with what follows the last comma.
    let weak = ui.as_weak();
    ui.on_search(move |text| {
        let query = text
            .rsplit(',')
            .next()
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let found = [("John", "john@example.com"), ("Filip", "filip@example.com")]
            .into_iter()
            .filter(|(name, _)| !query.is_empty() && name.to_lowercase().starts_with(&query))
            .map(|(name, email)| ComposeContact {
                name: name.into(),
                email: email.into(),
                initials: name[..1].into(),
            })
            .collect::<Vec<_>>();
        weak.unwrap().set_found(ModelRc::new(VecModel::from(found)));
    });
    // The chosen contact takes the place of what was typed after the last comma.
    let weak = ui.as_weak();
    ui.on_complete(move |index, text| {
        let contact = weak.unwrap().get_found().row_data(index as usize).unwrap();
        let kept = text.rfind(',').map_or("", |comma| &text[..=comma]);
        let recipient = format!("{} <{}>, ", contact.name, contact.email);
        if kept.is_empty() {
            recipient.into()
        } else {
            format!("{kept} {recipient}").into()
        }
    });

    // Opening the composer puts the caret in To.
    ui.set_open(true);
    render(&window);
    let key = |text: &str| {
        ui.window()
            .dispatch_event(WindowEvent::KeyPressed { text: text.into() });
        ui.window()
            .dispatch_event(WindowEvent::KeyReleased { text: text.into() });
        render(&window);
    };
    let accept = slint::SharedString::from(slint::platform::Key::Return);
    key("jo");
    key(&accept);
    assert_eq!(ui.get_to(), "John <john@example.com>, ");
    key("fi");
    assert_eq!(
        ui.get_to(),
        "John <john@example.com>, fi",
        "what is typed next goes after the chosen address"
    );
    key(&accept);
    assert_eq!(
        ui.get_to(),
        "John <john@example.com>, Filip <filip@example.com>, "
    );
}
