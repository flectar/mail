//! Marking an opened message read (Settings → General → Mark messages as
//! read). It happens as the message opens, or once it has stayed open a
//! while, as Thunderbird, Evolution and Outlook offer. Opening another
//! message first, or marking one by hand, cancels the wait, so messages the
//! user only passes over stay unread.

use std::time::Duration;

thread_local! {
    /// The wait for the open message. Starting it again replaces the wait
    /// for the message before.
    static WAIT: slint::Timer = slint::Timer::default();
}

/// Run `mark` once `delay` has passed, unless [`cancel`] or another call
/// comes first.
pub(super) fn after(delay: Duration, mark: impl FnOnce() + 'static) {
    let mut mark = Some(mark);
    WAIT.with(|wait| {
        wait.start(slint::TimerMode::SingleShot, delay, move || {
            if let Some(mark) = mark.take() {
                mark();
            }
        });
    });
}

/// Stop the wait, if one runs.
pub(super) fn cancel() {
    WAIT.with(slint::Timer::stop);
}
