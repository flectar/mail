//! The Move to picker: arrows steer the list past its headings while the
//! search field keeps the typing, and a choice or a dismissal closes it.
use slint::platform::{
    Key, Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, ModelRc, Rgb8Pixel, VecModel};
use std::{cell::RefCell, rc::Rc};

slint::slint! {
    import { MoveDialog, MovePickerUi } from "../ui/dialogs/move-dialog.slint";
    export { MovePickerUi, MoveTargetRow } from "../ui/dialogs/move-dialog.slint";

    export component MoveDialogHarness inherits Window {
        width: 600px;
        height: 560px;
        if MovePickerUi.open : MoveDialog {
            width: parent.width;
            height: parent.height;
        }
    }
}

struct Headless(Rc<MinimalSoftwareWindow>);

impl Platform for Headless {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.0.clone())
    }
}

fn row(key: i32, kind: &str, label: &str) -> MoveTargetRow {
    MoveTargetRow {
        key,
        kind: kind.into(),
        label: label.into(),
        ..Default::default()
    }
}

/// Recent folders, then every folder, as Rust lists them.
fn rows() -> Vec<MoveTargetRow> {
    vec![
        row(-1, "recent", ""),
        row(4, "folder", "Receipts"),
        row(-1, "folders", ""),
        row(0, "archive", "Archive"),
        row(1, "spam", "Spam"),
        row(4, "folder", "Receipts"),
        row(5, "folder", "Travel"),
    ]
}

struct Picker {
    ui: MoveDialogHarness,
    window: Rc<MinimalSoftwareWindow>,
    chosen: Rc<RefCell<Vec<i32>>>,
    searches: Rc<RefCell<Vec<String>>>,
}

impl Picker {
    fn new() -> Self {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        slint::platform::set_platform(Box::new(Headless(window.clone()))).unwrap();
        let ui = MoveDialogHarness::new().unwrap();
        let chosen = Rc::new(RefCell::new(Vec::new()));
        let searches = Rc::new(RefCell::new(Vec::new()));
        let picker = ui.global::<MovePickerUi>();
        let record = chosen.clone();
        picker.on_choose(move |key| record.borrow_mut().push(key));
        let record = searches.clone();
        picker.on_search(move |query| record.borrow_mut().push(query.into()));
        picker.set_rows(ModelRc::new(VecModel::from(rows())));
        ui.show().unwrap();
        Self {
            ui,
            window,
            chosen,
            searches,
        }
    }

    /// Opens the picker and lets it build, as a frame would.
    fn open(&self) {
        self.ui.global::<MovePickerUi>().set_open(true);
        self.window.request_redraw();
        let mut pixels = vec![Rgb8Pixel::default(); 600 * 560];
        self.window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, 600);
        });
    }

    fn is_open(&self) -> bool {
        self.ui.global::<MovePickerUi>().get_open()
    }

    fn press(&self, key: impl Into<slint::SharedString>) {
        let text = key.into();
        let window = self.ui.window();
        window.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
        window.dispatch_event(WindowEvent::KeyReleased { text });
    }

    fn click(&self, x: f32, y: f32) {
        let position = slint::LogicalPosition::new(x, y);
        let button = PointerEventButton::Left;
        let window = self.ui.window();
        window.dispatch_event(WindowEvent::PointerMoved { position });
        window.dispatch_event(WindowEvent::PointerPressed { position, button });
        window.dispatch_event(WindowEvent::PointerReleased { position, button });
    }

    fn chosen(&self) -> Vec<i32> {
        std::mem::take(&mut *self.chosen.borrow_mut())
    }
}

/// The card is 440px wide and centered; seven rows of 34px under 124px of
/// title, search field and padding make it 362px tall. Its list starts
/// 108px down, after the 12px padding, the 40px title row, the 36px field
/// and two 10px gaps, and the rows 4px further.
const CARD_TOP: f32 = (560.0 - 362.0) / 2.0;
fn row_center(index: usize) -> f32 {
    CARD_TOP + 108.0 + 4.0 + 34.0 * index as f32 + 17.0
}

#[test]
fn arrows_skip_headings_and_return_moves_to_the_highlighted_folder() {
    let picker = Picker::new();

    picker.open();
    picker.press(Key::Return);
    assert_eq!(picker.chosen(), [4], "the first folder starts highlighted");
    assert!(!picker.is_open(), "a choice closes the picker");

    picker.open();
    picker.press(Key::DownArrow);
    picker.press(Key::Return);
    assert_eq!(picker.chosen(), [0], "down passes the All folders heading");

    picker.open();
    let (down, up) = (Key::DownArrow, Key::UpArrow);
    for key in [down, down, up, up, up] {
        picker.press(key);
    }
    picker.press(Key::Return);
    assert_eq!(
        picker.chosen(),
        [4],
        "up passes the heading back and stops at the first folder"
    );

    picker.open();
    picker.press(Key::PageDown);
    picker.press(Key::PageDown);
    picker.press(Key::Return);
    assert_eq!(picker.chosen(), [5], "page down stops at the last folder");

    picker.open();
    picker.press("t");
    assert_eq!(
        *picker.searches.borrow(),
        ["t"],
        "letters reach the search field"
    );
    assert!(picker.is_open());
    assert!(picker.chosen().is_empty());
}

#[test]
fn escape_close_and_clicks_outside_dismiss_without_moving() {
    let picker = Picker::new();

    picker.open();
    picker.press(Key::Escape);
    assert!(!picker.is_open(), "escape closes the picker");

    picker.open();
    picker.click(100.0, CARD_TOP + 30.0);
    assert!(picker.is_open(), "a click on the card itself keeps it open");
    picker.click(20.0, 20.0);
    assert!(!picker.is_open(), "a click outside the card closes it");

    picker.open();
    // The close button ends the title row, inside the card's 12px padding.
    picker.click(300.0 + 220.0 - 12.0 - 20.0, CARD_TOP + 12.0 + 20.0);
    assert!(!picker.is_open(), "the close button closes the picker");
    assert!(picker.chosen().is_empty(), "dismissing moves nothing");

    picker.open();
    picker.click(300.0, row_center(4));
    assert_eq!(picker.chosen(), [1], "a click moves to the folder clicked");
    assert!(!picker.is_open());
    picker.open();
    picker.click(300.0, row_center(2));
    assert!(picker.is_open(), "a heading is not a folder to move to");
    assert!(picker.chosen().is_empty());
}
