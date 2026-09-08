//! Coalesced, event-driven delivery from worker threads to Slint's UI thread.

use crate::AppWindow;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::mpsc;

const UI_UPDATE_QUEUE_CAPACITY: usize = 16;

/// Create a bounded worker-to-UI queue. The Slint wake coalescer normally
/// drains updates on the next event-loop turn; the bound prevents a suspended
/// or blocked window from retaining an unlimited number of materialized
/// models, images, or task results.
pub(crate) fn bounded_ui_channel<T>() -> (mpsc::Sender<T>, mpsc::Receiver<T>) {
    mpsc::channel(UI_UPDATE_QUEUE_CAPACITY)
}

#[derive(Clone)]
pub(crate) struct UiWake {
    app: slint::Weak<AppWindow>,
    pending: Arc<AtomicBool>,
    invoke: fn(&AppWindow),
}

impl UiWake {
    pub(crate) fn new(app: slint::Weak<AppWindow>, invoke: fn(&AppWindow)) -> Self {
        Self {
            app,
            pending: Arc::new(AtomicBool::new(false)),
            invoke,
        }
    }

    pub(crate) fn wake(&self) {
        if self.pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let app = self.app.clone();
        let pending = Arc::clone(&self.pending);
        let invoke = self.invoke;
        if slint::invoke_from_event_loop(move || {
            // Clear before draining. A producer racing with the drain schedules
            // one more pass, so no channel item can become stranded.
            pending.store(false, Ordering::Release);
            if let Some(app) = app.upgrade() {
                invoke(&app);
            }
        })
        .is_err()
        {
            self.pending.store(false, Ordering::Release);
        }
    }
}

pub(crate) struct UiSender<T> {
    sender: mpsc::Sender<T>,
    wake: UiWake,
}

impl<T> Clone for UiSender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            wake: self.wake.clone(),
        }
    }
}

impl<T> UiSender<T> {
    pub(crate) fn new(sender: mpsc::Sender<T>, wake: UiWake) -> Self {
        Self { sender, wake }
    }

    pub(crate) async fn send(&self, update: T) -> Result<(), mpsc::error::SendError<T>> {
        self.sender.send(update).await?;
        self.wake.wake();
        Ok(())
    }
}

/// Enqueue UI-triggered work without blocking, deduplicating outstanding IDs.
/// The caller removes IDs when completions arrive; rejection releases them here.
pub(crate) fn enqueue_once<T>(
    sender: &mpsc::Sender<T>,
    pending: &std::cell::RefCell<std::collections::HashSet<i64>>,
    id: i64,
    request: T,
) -> Result<(), ()> {
    if !pending.borrow_mut().insert(id) {
        return Ok(());
    }
    if sender.try_send(request).is_err() {
        pending.borrow_mut().remove(&id);
        return Err(());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{UI_UPDATE_QUEUE_CAPACITY, bounded_ui_channel};
    use tokio::sync::mpsc::error::TrySendError;

    #[test]
    fn queued_work_deduplicates_and_full_queues_release_retry_ids() {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        let pending = Default::default();
        assert!(super::enqueue_once(&sender, &pending, 1, "first").is_ok());
        assert!(super::enqueue_once(&sender, &pending, 1, "duplicate").is_ok());
        assert!(super::enqueue_once(&sender, &pending, 2, "next").is_err());
        assert!(!pending.borrow().contains(&2));
        assert_eq!(receiver.try_recv().unwrap(), "first");
        assert!(super::enqueue_once(&sender, &pending, 2, "retry").is_ok());
        assert_eq!(receiver.try_recv().unwrap(), "retry");
    }

    #[test]
    fn ui_update_queue_has_a_hard_capacity() {
        let (sender, _receiver) = bounded_ui_channel();
        for update in 0..UI_UPDATE_QUEUE_CAPACITY {
            sender.try_send(update).expect("queue has advertised room");
        }
        assert!(matches!(
            sender.try_send(UI_UPDATE_QUEUE_CAPACITY),
            Err(TrySendError::Full(UI_UPDATE_QUEUE_CAPACITY))
        ));
    }
}
