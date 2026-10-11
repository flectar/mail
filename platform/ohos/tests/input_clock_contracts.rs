//! Real production input dispatch with a deterministic monotonic Slint clock.

use crate::backend;
use slint::{
    ComponentHandle,
    platform::{Platform, PlatformError, WindowAdapter},
};
use std::{
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

struct ClockPlatform {
    owner: Rc<backend::OhosPlatform>,
    milliseconds: Arc<AtomicU64>,
}
impl Platform for ClockPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        self.owner.create_window_adapter()
    }
    fn duration_since_start(&self) -> Duration {
        Duration::from_millis(self.milliseconds.load(Ordering::SeqCst))
    }
}

slint::slint! {
    export component InputClockProbe inherits Window {
        width: 32px; height: 24px;
        callback observed();
        TouchArea {
            pointer-event(event) => {
                if event.kind != PointerEventKind.cancel { root.observed(); }
            }
        }
    }
}

fn platform() -> (
    Rc<backend::OhosPlatform>,
    mpsc::Sender<backend::Event>,
    Arc<AtomicU64>,
) {
    let (sender, receiver) = mpsc::channel();
    let owner = Rc::new(backend::OhosPlatform::new(sender.clone(), receiver));
    let milliseconds = Arc::new(AtomicU64::new(0));
    slint::platform::set_platform(Box::new(ClockPlatform {
        owner: owner.clone(),
        milliseconds: milliseconds.clone(),
    }))
    .unwrap();
    (owner, sender, milliseconds)
}

#[test]
fn queued_touch_events_observe_current_clock_before_dispatch() {
    let (owner, sender, milliseconds) = platform();
    let probe = InputClockProbe::new().unwrap();
    probe.show().unwrap();
    let observed = Arc::new(Mutex::new(Vec::new()));
    let observations = observed.clone();
    let input_clock = milliseconds.clone();
    let timer_due = Arc::new(AtomicBool::new(false));
    let timer_result = timer_due.clone();
    slint::Timer::single_shot(Duration::from_millis(40), move || {
        timer_result.store(true, Ordering::SeqCst);
    });
    probe.on_observed(move || {
        assert!(
            timer_due.load(Ordering::SeqCst),
            "due timers must run before real touch callbacks"
        );
        let tick = i_slint_core::animations::current_tick().as_millis();
        assert_eq!(
            tick,
            input_clock.load(Ordering::SeqCst),
            "each real touch callback must observe its current clock before input dispatch"
        );
        observations.lock().unwrap().push(tick);
    });
    sender
        .send(backend::Event::Viewport {
            width: 32,
            height: 24,
            density: 1.,
            insets: Default::default(),
        })
        .unwrap();
    for (time, kind, y) in [(40, 0, 8.), (60, 2, 12.), (80, 1, 16.)] {
        let clock = milliseconds.clone();
        sender
            .send(backend::Event::Task(Box::new(move || {
                clock.store(time, Ordering::SeqCst);
            })))
            .unwrap();
        sender
            .send(backend::Event::Touch {
                kind,
                id: 0,
                x: 8.,
                y,
            })
            .unwrap();
    }
    sender.send(backend::Event::Quit).unwrap();
    owner.run_event_loop().unwrap();
    // Slint refreshes its animation tick inside pointer dispatch. The due-timer
    // assertion tests the backend's additional before-input responsibility.
    // Slint can synthesize an additional hover move when a touch is released.
    // The per-callback assertion above also checks that event's clock.
    let mut times = observed.lock().unwrap().clone();
    times.dedup();
    assert_eq!(
        times,
        vec![40, 60, 80],
        "each real touch callback must observe its current clock before input dispatch"
    );
}

#[test]
fn first_queued_callback_observes_clock_after_receive() {
    let (owner, sender, milliseconds) = platform();
    milliseconds.store(120, Ordering::SeqCst);
    sender
        .send(backend::Event::Task(Box::new(|| {
            assert_eq!(
                i_slint_core::animations::current_tick().as_millis(),
                120,
                "a received event must observe the current clock before its callback"
            );
        })))
        .unwrap();
    sender.send(backend::Event::Quit).unwrap();
    owner.run_event_loop().unwrap();
}
