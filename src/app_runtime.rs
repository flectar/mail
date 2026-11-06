//! The Tokio runtime behind one run of the app.

use std::time::Duration;
use tokio::runtime::{Handle, Runtime};

/// How long shutdown waits for blocking work that is already running, such as
/// a credential or file write. Work still running after that ends with the
/// process, so a stuck call can't keep a closed app alive.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// Owns the runtime and shuts it down when dropped.
///
/// Callbacks and controllers hold [`Handle`]s, which don't keep the runtime
/// alive, so it always shuts down here, on the UI thread, while its threads
/// are still running. With shared ownership, shutdown would happen wherever
/// the last owner was dropped. That could be a thread-local destructor during
/// process exit, which on Windows runs after every other thread has been
/// terminated, and joining the runtime's threads there aborts the process.
pub(crate) struct AppRuntime {
    runtime: Option<Runtime>,
    handle: Handle,
    grace: Duration,
}

impl AppRuntime {
    pub(crate) fn new(runtime: Runtime) -> Self {
        Self::with_grace(runtime, SHUTDOWN_GRACE)
    }

    fn with_grace(runtime: Runtime, grace: Duration) -> Self {
        Self {
            handle: runtime.handle().clone(),
            runtime: Some(runtime),
            grace,
        }
    }

    pub(crate) fn handle(&self) -> &Handle {
        &self.handle
    }
}

impl Drop for AppRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(self.grace);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    use std::time::Instant;

    fn runtime() -> Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap()
    }

    /// Records that the value holding it was dropped.
    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn dropping_the_owner_stops_tasks_spawned_through_handles() {
        let owner = AppRuntime::new(runtime());
        let handle = owner.handle().clone();
        let dropped = Arc::new(AtomicBool::new(false));
        let flag = DropFlag(dropped.clone());
        let (started_tx, started_rx) = mpsc::channel();
        handle.spawn(async move {
            let _flag = flag;
            started_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        started_rx.recv().unwrap();

        drop(owner);

        assert!(dropped.load(Ordering::SeqCst));
        // A handle that outlives the owner did not keep the runtime running.
        drop(handle);
    }

    #[test]
    fn dropping_the_owner_lets_running_blocking_work_finish() {
        let owner = AppRuntime::new(runtime());
        let finished = Arc::new(AtomicBool::new(false));
        let done = finished.clone();
        let (started_tx, started_rx) = mpsc::channel();
        owner.handle().spawn_blocking(move || {
            started_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(200));
            done.store(true, Ordering::SeqCst);
        });
        started_rx.recv().unwrap();

        drop(owner);

        assert!(finished.load(Ordering::SeqCst));
    }

    #[test]
    fn dropping_the_owner_stops_waiting_after_the_grace() {
        let owner = AppRuntime::with_grace(runtime(), Duration::from_millis(100));
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        owner.handle().spawn_blocking(move || {
            started_tx.send(()).unwrap();
            let _ = release_rx.recv();
        });
        started_rx.recv().unwrap();
        let dropping = Instant::now();

        drop(owner);

        assert!(dropping.elapsed() < Duration::from_secs(10));
        // Lets the abandoned blocking thread end.
        drop(release_tx);
    }
}
