//! Pulling a touch list down from its top to check for new mail.
use i_slint_core::input::TouchPhase;
use i_slint_core::lengths::LogicalPoint;
use i_slint_core::platform::InternalEvent;
use slint::platform::{
    Platform, WindowAdapter, WindowEvent,
    software_renderer::{MinimalSoftwareWindow, RepaintBufferType},
};
use slint::{ComponentHandle, ModelRc, VecModel};
use std::{cell::Cell, rc::Rc, time::Duration};

slint::slint! {
    #[edition = "1.19"]
    import { ListView } from "std-widgets.slint";
    import { EmailRow, MailListEntry } from "../ui/models.slint";
    import { PullToRefresh } from "../ui/components/pull-to-refresh.slint";
    import { MailListPane } from "../ui/views/mail-list-pane.slint";
    import { MotionSettings } from "../ui/design-system.slint";

    export component PullHarness inherits Window {
        width: 360px;
        height: 640px;
        in-out property <bool> enabled: true;
        in-out property <bool> refreshing: false;
        // Whether a refresh starts when asked, as a check for new mail does.
        in-out property <bool> starts-refresh: true;
        in-out property <int> refreshes: 0;
        in-out property <int> taps: 0;
        in-out property <length> scroll <=> list.content-y;
        out property <bool> shown: pull.shown;
        out property <length> travel: pull.travel;
        init => { MotionSettings.enabled = false; }

        pull := PullToRefresh {
            enabled: root.enabled;
            at-top: list.content-y >= -1px;
            refreshing: root.refreshing;
            refresh => {
                root.refreshes += 1;
                if root.starts-refresh {
                    root.refreshing = true;
                }
            }

            list := ListView {
                for index in 40 : TouchArea {
                    height: 60px;
                    clicked => { root.taps += 1; }
                }
            }
        }
    }

    export component MailListHarness inherits Window {
        width: 397px;
        height: 640px;
        in-out property <[EmailRow]> emails: [];
        in-out property <[MailListEntry]> entries: [];
        in-out property <bool> compact: true;
        in-out property <string> query: "";
        in-out property <bool> syncing: false;
        in-out property <int> checks: 0;
        in-out property <int> all-checks: 0;
        init => { MotionSettings.enabled = false; }

        MailListPane {
            width: parent.width;
            height: parent.height;
            compact: root.compact;
            emails: root.emails;
            list_entries: root.entries;
            search_query: root.query;
            sync_in_progress: root.syncing;
            surface: #18191a;
            heading: white;
            body-text: white;
            secondary-text: #cccccc;
            muted-text: #999999;
            border-subtle: #303030;
            accent: #5ca3ff;
            check_mail_in_view => {
                root.checks += 1;
                root.syncing = true;
            }
            sync_mail => { root.all-checks += 1; }
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

/// A headless window with a clock the test moves, and a finger on it.
struct Screen {
    window: Rc<MinimalSoftwareWindow>,
    clock: Rc<Cell<Duration>>,
}

impl Screen {
    fn new() -> Self {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        let clock = Rc::new(Cell::new(Duration::ZERO));
        slint::platform::set_platform(Box::new(Headless {
            window: window.clone(),
            clock: clock.clone(),
        }))
        .unwrap();
        Self { window, clock }
    }

    fn advance(&self, ms: u64) {
        self.clock.set(self.clock.get() + Duration::from_millis(ms));
        slint::platform::update_timers_and_animations();
        let size = self.window.size();
        let mut pixels = vec![slint::Rgb8Pixel::default(); (size.width * size.height) as usize];
        self.window.draw_if_needed(|renderer| {
            renderer.render(&mut pixels, size.width as usize);
        });
    }

    fn touch(&self, y: f32, phase: TouchPhase) {
        self.window
            .dispatch_event(WindowEvent::internal(InternalEvent::Touch {
                id: 0,
                position: LogicalPoint::new(180.0, y),
                phase,
                event_time: None,
                history: Default::default(),
            }));
    }

    /// A finger that lands at `y`, rests for `hold` ms, moves `distance`
    /// down in 20px steps, and lifts.
    fn drag(&self, y: f32, hold: u64, distance: f32) {
        self.touch(y, TouchPhase::Started);
        self.advance(hold);
        let mut moved = 0.0;
        while moved < distance {
            moved = f32::min(moved + 20.0, distance);
            self.touch(y + moved, TouchPhase::Moved);
            self.advance(16);
        }
        self.touch(y + distance, TouchPhase::Ended);
        self.advance(16);
    }
}

fn pull_harness() -> (Screen, PullHarness) {
    let screen = Screen::new();
    let ui = PullHarness::new().unwrap();
    ui.window().set_size(slint::PhysicalSize::new(360, 640));
    ui.show().unwrap();
    screen.advance(0);
    (screen, ui)
}

#[test]
fn pulling_down_from_the_top_refreshes_and_waits_for_the_refresh() {
    let (screen, ui) = pull_harness();
    screen.touch(100.0, TouchPhase::Started);
    screen.advance(16);
    // The pull takes over once the finger has moved, and counts from there.
    screen.touch(120.0, TouchPhase::Moved);
    screen.advance(16);
    assert!(ui.get_shown(), "the spinner comes out");
    assert_eq!(ui.get_travel(), 0.0);
    screen.touch(220.0, TouchPhase::Moved);
    screen.advance(16);
    assert_eq!(ui.get_travel(), 50.0, "the spinner moves half as far");
    screen.touch(300.0, TouchPhase::Moved);
    screen.advance(16);
    assert_eq!(ui.get_travel(), 77.0, "and a quarter as far past 64px");
    screen.touch(600.0, TouchPhase::Moved);
    screen.advance(16);
    assert_eq!(ui.get_travel(), 96.0, "up to 96px");
    assert_eq!(ui.get_scroll(), 0.0, "the list stays put");
    screen.touch(600.0, TouchPhase::Ended);
    screen.advance(16);
    assert_eq!(ui.get_refreshes(), 1);
    assert_eq!(ui.get_taps(), 0, "a pull is not a tap");
    assert!(ui.get_shown(), "the spinner waits for the refresh");

    // Another pull while it waits does nothing.
    screen.drag(100.0, 16, 300.0);
    assert_eq!(ui.get_refreshes(), 1);

    ui.set_refreshing(false);
    screen.advance(16);
    assert!(!ui.get_shown(), "and leaves when it ends");
}

#[test]
fn a_short_pull_springs_back_without_refreshing() {
    let (screen, ui) = pull_harness();
    screen.drag(100.0, 16, 120.0);
    assert_eq!(ui.get_refreshes(), 0);
    assert!(!ui.get_shown());
    assert_eq!(ui.get_taps(), 0);
}

#[test]
fn a_finger_that_rests_on_a_row_first_still_pulls() {
    let (screen, ui) = pull_harness();
    // Past the hold, the row and the list have the press too.
    screen.drag(100.0, 300, 300.0);
    assert_eq!(ui.get_refreshes(), 1);
    assert_eq!(ui.get_taps(), 0, "the row loses the press to the pull");
}

#[test]
fn a_scrolled_list_scrolls_instead() {
    let (screen, ui) = pull_harness();
    ui.set_scroll(-600.0);
    screen.advance(16);
    screen.drag(100.0, 16, 300.0);
    assert_eq!(ui.get_refreshes(), 0);
    assert!(!ui.get_shown());
    assert!(ui.get_scroll() > -600.0, "the list scrolled up");
}

#[test]
fn taps_still_reach_the_rows() {
    let (screen, ui) = pull_harness();
    for hold in [30, 300] {
        screen.touch(130.0, TouchPhase::Started);
        screen.advance(hold);
        screen.touch(130.0, TouchPhase::Ended);
        screen.advance(16);
    }
    assert_eq!(ui.get_taps(), 2);
    assert_eq!(ui.get_refreshes(), 0);
}

#[test]
fn a_pull_waits_only_for_a_refresh_that_runs() {
    let (screen, ui) = pull_harness();
    // Nothing to refresh: the spinner goes straight back.
    ui.set_starts_refresh(false);
    screen.drag(100.0, 16, 300.0);
    assert_eq!(ui.get_refreshes(), 1);
    assert!(!ui.get_shown());

    // A refresh already running, from the toolbar: the spinner waits for it.
    ui.set_refreshing(true);
    screen.drag(100.0, 16, 300.0);
    assert_eq!(ui.get_refreshes(), 2);
    assert!(ui.get_shown());
    ui.set_refreshing(false);
    screen.advance(16);
    assert!(!ui.get_shown());
}

#[test]
fn a_disabled_list_does_not_pull() {
    let (screen, ui) = pull_harness();
    ui.set_enabled(false);
    screen.drag(100.0, 16, 300.0);
    assert_eq!(ui.get_refreshes(), 0);
    assert!(!ui.get_shown());
}

fn mail_list(compact: bool) -> (Screen, MailListHarness) {
    let screen = Screen::new();
    let ui = MailListHarness::new().unwrap();
    let emails = (0..30)
        .map(|id| EmailRow {
            id,
            subject: format!("Message {id}").into(),
            sender: "Alex".into(),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    let entries = emails
        .iter()
        .enumerate()
        .map(|(index, email)| MailListEntry {
            show_row: true,
            email_index: index as i32,
            email: email.clone(),
            ..Default::default()
        })
        .collect::<Vec<_>>();
    ui.set_emails(ModelRc::new(VecModel::from(emails)));
    ui.set_entries(ModelRc::new(VecModel::from(entries)));
    ui.set_compact(compact);
    ui.window().set_size(slint::PhysicalSize::new(397, 640));
    ui.show().unwrap();
    screen.advance(0);
    (screen, ui)
}

#[test]
fn phones_pull_the_mail_list_to_check_the_accounts_in_view() {
    let (screen, ui) = mail_list(true);
    screen.drag(200.0, 16, 300.0);
    assert_eq!(ui.get_checks(), 1, "as F5 does");
    assert_eq!(ui.get_all_checks(), 0);

    // Not while searching, where the list shows what was found.
    ui.set_syncing(false);
    ui.set_query("invoice".into());
    screen.advance(16);
    screen.drag(200.0, 16, 300.0);
    assert_eq!(ui.get_checks(), 1);
}

#[test]
fn desktop_layouts_keep_drags_for_the_mouse() {
    let (screen, ui) = mail_list(false);
    screen.drag(200.0, 16, 300.0);
    assert_eq!(ui.get_checks(), 0);
}
