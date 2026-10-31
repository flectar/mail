//! Keeps the Slint thread from presenting while a system window covers the app.
//!
//! A system picker opens in its own window over the XComponent without hiding
//! it, so no surface callback reports the window as covered. Frames presented
//! meanwhile queue up in the covered surface's buffer queue, and the window
//! stops responding once the picker closes. Hosts take a [`PresentationHold`]
//! for as long as such a window is open; the Slint thread waits, without
//! drawing, until the last hold is released.

use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use crate::event::BackendEvent;

/// Upper bound for one wait, in case a hold is never released.
const MAX_WAIT: Duration = Duration::from_secs(15 * 60);

/// Number of live holds; signalled when the last one is released.
static HOLDS: Mutex<usize> = Mutex::new(0);
static RELEASED: Condvar = Condvar::new();

/// Keeps the Slint thread idle until dropped. Callable from any thread.
pub struct PresentationHold(());

impl PresentationHold {
    pub fn acquire() -> Self {
        let mut holds = HOLDS.lock().unwrap_or_else(PoisonError::into_inner);
        *holds += 1;
        if *holds == 1 {
            crate::xcomponent::send(BackendEvent::Task(Box::new(wait_for_release)));
        }
        Self(())
    }
}

impl Drop for PresentationHold {
    fn drop(&mut self) {
        let mut holds = HOLDS.lock().unwrap_or_else(PoisonError::into_inner);
        *holds = holds.saturating_sub(1);
        if *holds == 0 {
            RELEASED.notify_all();
        }
    }
}

/// Runs on the Slint thread as an event-loop task.
fn wait_for_release() {
    let holds = HOLDS.lock().unwrap_or_else(PoisonError::into_inner);
    if *holds == 0 {
        return;
    }
    crate::log::info("presentation held while a system window covers the app");
    let (_holds, wait) = RELEASED
        .wait_timeout_while(holds, MAX_WAIT, |holds| *holds > 0)
        .unwrap_or_else(PoisonError::into_inner);
    if wait.timed_out() {
        crate::log::error("presentation hold was not released; resuming");
    } else {
        crate::log::info("presentation resumed");
    }
}
