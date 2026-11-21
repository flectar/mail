//! How long to wait for an account's servers (#252). Every wait a protocol
//! sets is sized for the standard server timeout. An account set to wait
//! longer for its servers, as a slow server or connection needs, stretches
//! all of them alike.
//!
//! Connections take the account's timeout from its settings. Waits deep in a
//! protocol, such as one IMAP command's, take it from the task they run in,
//! which the account's sync, send and setup work run [`scope`]d to.

use std::future::Future;
use std::time::Duration;

/// The server timeout every wait is sized for.
pub const STANDARD: Duration = Duration::from_secs(30);

/// The server timeouts Settings offers, in seconds. The first is the
/// standard one.
pub const CHOICES: [u32; 4] = [30, 60, 120, 300];

tokio::task_local! {
    /// The server timeout of the account the current task works for.
    static ACCOUNT_TIMEOUT: Duration;
}

/// `wait`, sized for the standard server timeout, for `timeout`. Never
/// shorter than `wait`.
pub fn stretch(wait: Duration, timeout: Duration) -> Duration {
    if timeout <= STANDARD {
        return wait;
    }
    wait.mul_f64(timeout.as_secs_f64() / STANDARD.as_secs_f64())
}

/// `wait` for the account the current task works for, or as it is outside
/// an account's work.
pub fn scaled(wait: Duration) -> Duration {
    ACCOUNT_TIMEOUT
        .try_with(|timeout| stretch(wait, *timeout))
        .unwrap_or(wait)
}

/// Run `work` for an account whose server timeout is `timeout`.
pub async fn scope<F: Future>(timeout: Duration, work: F) -> F::Output {
    ACCOUNT_TIMEOUT.scope(timeout, work).await
}

/// Call `start` for an account whose server timeout is `timeout`, so the
/// tasks it [`spawn`]s work for that account.
pub fn sync_scope<R>(timeout: Duration, start: impl FnOnce() -> R) -> R {
    ACCOUNT_TIMEOUT.sync_scope(timeout, start)
}

/// `work`, made to run for the account the current task works for wherever
/// it's spawned, as on a `JoinSet`.
pub fn inherit<F: Future>(work: F) -> impl Future<Output = F::Output> {
    let timeout = ACCOUNT_TIMEOUT
        .try_with(|timeout| *timeout)
        .unwrap_or(STANDARD);
    ACCOUNT_TIMEOUT.scope(timeout, work)
}

/// Spawn `work` for the account the current task works for. A plain
/// `tokio::spawn` would start it with the standard timeout.
pub fn spawn<F>(work: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    tokio::spawn(inherit(work))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_stretch_with_the_timeout_and_never_shrink() {
        let wait = Duration::from_secs(60);
        assert_eq!(stretch(wait, STANDARD), wait);
        assert_eq!(stretch(wait, Duration::from_secs(10)), wait);
        assert_eq!(
            stretch(wait, Duration::from_secs(120)),
            Duration::from_secs(240)
        );
    }

    #[tokio::test]
    async fn an_account_scope_reaches_the_tasks_it_spawns() {
        let wait = Duration::from_secs(30);
        assert_eq!(scaled(wait), wait);
        let (inside, spawned) = scope(Duration::from_secs(300), async {
            (
                scaled(wait),
                spawn(async move { scaled(wait) }).await.unwrap(),
            )
        })
        .await;
        assert_eq!(inside, Duration::from_secs(300));
        assert_eq!(spawned, Duration::from_secs(300));
        let on_a_set = sync_scope(Duration::from_secs(60), || {
            let mut set = tokio::task::JoinSet::new();
            set.spawn(inherit(async move { scaled(wait) }));
            set
        })
        .join_next()
        .await
        .unwrap()
        .unwrap();
        assert_eq!(on_a_set, Duration::from_secs(60));
        // A plain spawn starts outside the account's scope.
        let plain = scope(Duration::from_secs(300), async {
            tokio::spawn(async move { scaled(wait) }).await.unwrap()
        })
        .await;
        assert_eq!(plain, wait);
    }
}
