//! Exercise the actual shared Slint controller with deterministic time, without
//! a display server. No copies of the pagination algorithm live in these tests.
use slint::platform::{Platform, WindowAdapter};
use std::{cell::Cell, rc::Rc, time::Duration};

slint::slint! {
    import { InfiniteScrollController } from "../ui/components/infinite-scroll-controller.slint";
    export component PaginationHarness inherits Window {
        in-out property <bool> active: true;
        in-out property <bool> loading: false;
        in-out property <bool> more: true;
        in-out property <int> rows: 100;
        in-out property <int> revision: 0;
        in-out property <length> content: 8200px;
        in-out property <length> position: 0px;
        in-out property <length> viewport: 600px;
        callback requested;
        public function scroll() { pagination.user-scrolled(); }
        public function reset() { pagination.reset(); }
        pagination := InfiniteScrollController {
            active: root.active; loading: root.loading; can-load-more: root.more;
            row-count: root.rows; revision: root.revision;
            content-height: root.content; viewport-y: root.position;
            visible-height: root.viewport;
            load-more => { root.requested(); }
        }
    }
}
struct Clock(Rc<Cell<Duration>>);
impl Platform for Clock {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
        Ok(
            slint::platform::software_renderer::MinimalSoftwareWindow::new(
                slint::platform::software_renderer::RepaintBufferType::ReusedBuffer,
            ),
        )
    }
    fn duration_since_start(&self) -> Duration {
        self.0.get()
    }
}

#[test]
fn shared_workspace_pagination_handles_geometry_retries_and_inactive_views() {
    let clock = Rc::new(Cell::new(Duration::ZERO));
    slint::platform::set_platform(Box::new(Clock(clock.clone()))).unwrap();
    let ui = PaginationHarness::new().unwrap();
    let requests = Rc::new(Cell::new(0));
    let counted = requests.clone();
    ui.on_requested(move || counted.set(counted.get() + 1));
    let settle = || {
        for _ in 0..3 {
            clock.set(clock.get() + Duration::from_millis(2));
            slint::platform::update_timers_and_animations();
        }
    };
    settle();
    assert_eq!(requests.get(), 0, "do not load while far from the tail");
    ui.set_position(-7400.);
    ui.invoke_scroll();
    assert_eq!(requests.get(), 1, "prefetch before reaching the bottom");
    ui.set_loading(true);
    ui.invoke_scroll();
    settle();
    assert_eq!(requests.get(), 1, "never duplicate an in-flight request");
    // A successful page commits before its new ListView geometry settles.
    ui.set_rows(200);
    ui.set_revision(1);
    ui.set_loading(false);
    settle();
    assert_eq!(requests.get(), 1, "wait for the committed row geometry");
    ui.set_content(16400.);
    settle();
    assert_eq!(requests.get(), 1);
    ui.set_position(-15600.);
    ui.invoke_scroll();
    assert_eq!(requests.get(), 2);
    // An error does not increment revision. Time alone must not retry.
    ui.set_loading(true);
    settle();
    ui.set_loading(false);
    settle();
    assert_eq!(requests.get(), 2);
    ui.invoke_scroll();
    assert_eq!(requests.get(), 3, "a new gesture retries an error");
    ui.set_active(false);
    ui.set_content(100.);
    ui.set_rows(1);
    ui.set_position(0.);
    ui.invoke_reset();
    settle();
    assert_eq!(requests.get(), 3, "hidden workspaces do not page");
    ui.set_active(true);
    settle();
    assert_eq!(
        requests.get(),
        4,
        "short pages automatically fill the viewport"
    );
    ui.set_more(false);
    ui.invoke_reset();
    ui.invoke_scroll();
    settle();
    assert_eq!(requests.get(), 4, "stop at the last page");
}
