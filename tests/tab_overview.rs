//! The compact layout's tab switcher and tab overview, driven by pointer and
//! keyboard on a deterministic clock and without a display server.
use slint::platform::{
    Key, Platform, PointerEventButton, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, LogicalPosition, Model, ModelRc, VecModel};
use std::{cell::Cell, rc::Rc, time::Duration};

slint::slint! {
    import { TabOverview, TabSwitcherButton } from "../ui/components/tab-overview.slint";
    import { MotionSettings } from "../ui/design-system.slint";
    import { TabsUi } from "../ui/components/tabs-ui.slint";
    export { TabRow, TabsUi } from "../ui/components/tabs-ui.slint";

    export component TabOverviewHarness inherits Window {
        preferred-width: 390px;
        preferred-height: 844px;
        init => { MotionSettings.enabled = false; }

        // Where the mail toolbar ends it.
        TabSwitcherButton { x: 340px; y: 10px; }

        if TabsUi.overview-open : TabOverview {
            width: parent.width;
            height: parent.height;
            live-workspace: "mail";
            live-title: "Live subject";
        }
    }
}

struct Clock {
    window: Rc<MinimalSoftwareWindow>,
    now: Rc<Cell<Duration>>,
}

impl Platform for Clock {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(self.window.clone())
    }

    fn duration_since_start(&self) -> Duration {
        self.now.get()
    }
}

struct Harness {
    ui: TabOverviewHarness,
    window: Rc<MinimalSoftwareWindow>,
    now: Rc<Cell<Duration>>,
    rows: Rc<VecModel<TabRow>>,
    closed: Rc<std::cell::RefCell<Vec<i32>>>,
    created: Rc<Cell<u32>>,
}

impl Harness {
    fn new() -> Self {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
        let now = Rc::new(Cell::new(Duration::ZERO));
        slint::platform::set_platform(Box::new(Clock {
            window: window.clone(),
            now: now.clone(),
        }))
        .unwrap();
        let ui = TabOverviewHarness::new().unwrap();
        ui.window().set_size(slint::PhysicalSize::new(390, 844));
        let tab = |id: i32, workspace: &str, title: &str| TabRow {
            id,
            workspace: workspace.into(),
            title: title.into(),
            active: id == 1,
        };
        let rows = Rc::new(VecModel::from(vec![
            tab(1, "mail", "Saved subject"),
            tab(2, "mail", "Second conversation"),
            tab(3, "calendar", ""),
        ]));
        let tabs = ui.global::<TabsUi>();
        tabs.set_rows(ModelRc::from(rows.clone()));
        let closed = Rc::new(std::cell::RefCell::new(Vec::new()));
        let created = Rc::new(Cell::new(0));
        {
            let rows = rows.clone();
            tabs.on_select(move |id| {
                for index in 0..rows.row_count() {
                    let mut row = rows.row_data(index).unwrap();
                    row.active = row.id == id;
                    rows.set_row_data(index, row);
                }
            });
        }
        {
            let rows = rows.clone();
            let closed = closed.clone();
            tabs.on_close(move |id| {
                closed.borrow_mut().push(id);
                if let Some(index) = rows.iter().position(|row| row.id == id) {
                    rows.remove(index);
                }
            });
        }
        {
            let created = created.clone();
            tabs.on_create(move || created.set(created.get() + 1));
        }
        let harness = Self {
            ui,
            window,
            now,
            rows,
            closed,
            created,
        };
        harness.ui.show().unwrap();
        harness.draw();
        harness
    }

    fn tabs(&self) -> TabsUi<'_> {
        self.ui.global::<TabsUi>()
    }

    fn draw(&self) {
        self.now.set(self.now.get() + Duration::from_millis(16));
        slint::platform::update_timers_and_animations();
        self.window.draw_if_needed(|renderer| {
            let mut buffer = vec![slint::Rgb8Pixel::default(); 390 * 844];
            renderer.render(&mut buffer, 390);
        });
    }

    fn pointer(&self, event: WindowEvent) {
        self.ui.window().dispatch_event(event);
        self.draw();
    }

    fn press(&self, x: f32, y: f32) {
        let position = LogicalPosition::new(x, y);
        self.pointer(WindowEvent::PointerMoved { position });
        self.pointer(WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        });
    }

    fn release(&self, x: f32, y: f32) {
        self.pointer(WindowEvent::PointerReleased {
            position: LogicalPosition::new(x, y),
            button: PointerEventButton::Left,
        });
    }

    fn click(&self, x: f32, y: f32) {
        self.press(x, y);
        self.release(x, y);
    }

    /// Drag sideways from a point, a few pointer moves at a time.
    fn swipe(&self, x: f32, y: f32, distance: f32) {
        self.press(x, y);
        for step in 1..=8 {
            let position = LogicalPosition::new(x + distance * step as f32 / 8.0, y);
            self.pointer(WindowEvent::PointerMoved { position });
        }
        self.release(x + distance, y);
    }

    fn ids(&self) -> Vec<i32> {
        self.rows.iter().map(|row| row.id).collect()
    }

    fn active(&self) -> i32 {
        self.rows.iter().find(|row| row.active).unwrap().id
    }
}

// The overview's cards: two columns 12px apart below its 60px toolbar, 156px
// tall, with the close button in a 44px header.
const LEFT: f32 = 100.0;
const RIGHT: f32 = 290.0;
const FIRST_ROW: f32 = 170.0;

#[test]
fn the_switcher_opens_an_overview_that_selects_and_closes_tabs() {
    let harness = Harness::new();
    assert!(!harness.tabs().get_overview_open());

    harness.click(360.0, 30.0);
    assert!(
        harness.tabs().get_overview_open(),
        "the switcher opens the overview"
    );

    // Choosing a card shows its tab and returns to it.
    harness.click(RIGHT, FIRST_ROW);
    assert_eq!(harness.active(), 2);
    assert!(!harness.tabs().get_overview_open());

    // The close button closes its tab and leaves the overview open.
    harness.click(360.0, 30.0);
    let close_first = (12.0 + 177.0 - 4.0 - 20.0, 72.0 + 22.0);
    harness.click(close_first.0, close_first.1);
    assert_eq!(*harness.closed.borrow(), [1]);
    assert_eq!(harness.ids(), [2, 3]);
    assert_eq!(harness.active(), 2, "closing does not select a tab");
    assert!(harness.tabs().get_overview_open());

    // The toolbar opens a new tab and returns to it.
    harness.click(390.0 - 10.0 - 22.0, 30.0);
    assert_eq!(harness.created.get(), 1);
    assert!(!harness.tabs().get_overview_open());
}

#[test]
fn swiping_a_card_aside_closes_its_tab() {
    let harness = Harness::new();
    harness.tabs().set_overview_open(true);
    harness.draw();

    // A short swipe springs back; a long one closes the tab, either way.
    harness.swipe(LEFT, FIRST_ROW, 40.0);
    assert!(harness.closed.borrow().is_empty());
    assert_eq!(harness.active(), 1, "a swipe is not a tap");
    harness.swipe(RIGHT, FIRST_ROW, -150.0);
    assert_eq!(*harness.closed.borrow(), [2]);
    harness.swipe(LEFT, FIRST_ROW, 150.0);
    assert_eq!(*harness.closed.borrow(), [2, 1]);
    assert_eq!(harness.ids(), [3]);
}

#[test]
fn escape_returns_from_the_overview() {
    let harness = Harness::new();
    harness.tabs().set_overview_open(true);
    harness.draw();
    harness.ui.window().dispatch_event(WindowEvent::KeyPressed {
        text: Key::Escape.into(),
    });
    harness.draw();
    assert!(!harness.tabs().get_overview_open());
}
