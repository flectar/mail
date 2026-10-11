//! The production Slint scheduler, driven by a deterministic animation clock.
use slint::platform::{Platform, PlatformError, WindowAdapter};
use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use std::{cell::Cell, rc::Rc, time::Duration};

slint::slint! {
    import { ViewportRenderScheduler } from "../../../ui/components/viewport-render-scheduler.slint";
    export component ViewportProbe inherits Window {
        width: 64px; height: 64px;
        in-out property <length> viewport-width: 100px;
        in-out property <length> viewport-height: 100px;
        in-out property <bool> available: true;
        in-out property <bool> debounce: true;
        callback commit();
        ViewportRenderScheduler {
            viewport-width: root.viewport-width;
            viewport-height: root.viewport-height;
            available: root.available;
            debounce: root.debounce;
            commit => { root.commit(); }
        }
    }
}

struct ClockPlatform {
    clock: Rc<Cell<Duration>>,
    window: Rc<MinimalSoftwareWindow>,
}
impl Platform for ClockPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(self.window.clone())
    }
    fn duration_since_start(&self) -> Duration { self.clock.get() }
}
fn probe() -> (ViewportProbe, Rc<Cell<Duration>>, Rc<Cell<usize>>) {
    let clock = Rc::new(Cell::new(Duration::ZERO));
    slint::platform::set_platform(Box::new(ClockPlatform {
        clock: clock.clone(),
        window: MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer),
    })).unwrap();
    let probe = ViewportProbe::new().unwrap();
    let commits = Rc::new(Cell::new(0));
    let count = commits.clone();
    probe.on_commit(move || count.set(count.get() + 1));
    slint::platform::update_timers_and_animations();
    (probe, clock, commits)
}
fn advance(clock: &Cell<Duration>, millis: u64) {
    clock.set(Duration::from_millis(millis));
    slint::platform::update_timers_and_animations();
}

#[test]
fn continuous_resize_commits_once_after_the_final_geometry() {
    let (probe, clock, commits) = probe();
    for tick in 1..=14 {
        advance(&clock, tick * 16);
        probe.set_viewport_width(100. + tick as f32);
        probe.set_viewport_height(100. + tick as f32);
        slint::platform::update_timers_and_animations();
        assert_eq!(commits.get(), 0, "HTML must not reflow during continuous geometry changes");
    }
    advance(&clock, 273);
    assert_eq!(commits.get(), 0);
    advance(&clock, 274);
    assert_eq!(commits.get(), 1);
    advance(&clock, 600);
    assert_eq!(commits.get(), 1, "settled content must stay idle");
}

#[test]
fn unavailable_content_does_not_render_and_next_resize_is_not_lost() {
    let (probe, clock, commits) = probe();
    probe.set_available(false);
    probe.set_viewport_width(120.);
    slint::platform::update_timers_and_animations();
    advance(&clock, 50);
    assert_eq!(commits.get(), 0);
    probe.set_available(true);
    probe.set_viewport_height(130.);
    slint::platform::update_timers_and_animations();
    advance(&clock, 100);
    assert_eq!(commits.get(), 1);
}

#[test]
fn other_backends_keep_periodic_viewport_updates() {
    let (probe, clock, commits) = probe();
    probe.set_debounce(false);
    for tick in 1..=14 {
        advance(&clock, tick * 16);
        probe.set_viewport_width(100. + tick as f32);
        slint::platform::update_timers_and_animations();
    }
    assert!(commits.get() >= 2, "non-OHOS backends retain the existing throttle");
}
