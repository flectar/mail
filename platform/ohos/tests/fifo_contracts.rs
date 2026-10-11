//! Exercise the production owner loop: batching must preserve FIFO and yield to timers.

use crate::backend;
use slint::platform::{Platform, PlatformError, WindowAdapter};
use std::{
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};
struct DelegatingPlatform(Rc<backend::OhosPlatform>);
impl Platform for DelegatingPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        self.0.create_window_adapter()
    }
}
fn check_fifo(count: usize, slow: bool) {
    let (sender, receiver) = mpsc::channel();
    let owner = Rc::new(backend::OhosPlatform::new(sender.clone(), receiver));
    slint::platform::set_platform(Box::new(DelegatingPlatform(owner.clone()))).unwrap();
    let order = Arc::new(Mutex::new(Vec::new()));
    let timer_boundary = Arc::new(Mutex::new(None));
    for index in 0..count {
        let order = order.clone();
        let boundary = timer_boundary.clone();
        sender
            .send(backend::Event::Task(Box::new(move || {
                order.lock().unwrap().push(index);
                if index == 0 {
                    let order = order.clone();
                    slint::Timer::single_shot(Duration::ZERO, move || {
                        *boundary.lock().unwrap() = Some(order.lock().unwrap().len());
                    });
                }
                if slow {
                    std::thread::sleep(Duration::from_millis(2));
                }
            })))
            .unwrap();
    }
    sender.send(backend::Event::Quit).unwrap();
    owner.run_event_loop().unwrap();
    assert_eq!(
        &*order.lock().unwrap(),
        &(0..count).collect::<Vec<_>>(),
        "production FIFO must not drop or reorder events across batches"
    );
    let boundary = timer_boundary
        .lock()
        .unwrap()
        .expect("timers must receive a turn before all pending tasks are drained");
    assert!(
        boundary > 0 && boundary < count,
        "batch did not yield while tasks remained"
    );
    assert!(boundary <= 128, "event-count cap not observed");
    if slow {
        assert!(
            boundary <= 3,
            "4ms cap should yield after at most 3 two-ms tasks"
        );
    }
    println!(
        "FIFO contract: all {count} events executed in order; timer turn after {boundary}; slow={slow}"
    );
}
#[test]
fn production_event_count_batch_yields_without_losing_fifo() {
    check_fifo(400, false);
}
#[test]
fn production_event_time_batch_yields_without_losing_fifo() {
    check_fifo(12, true);
}
