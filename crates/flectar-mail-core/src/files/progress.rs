//! Optional transfer telemetry scoped to a caller's async operation. Protocol
//! correctness never depends on an observer or a native UI being present.
use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicU64, Ordering},
};
#[derive(Default)]
pub struct Progress {
    done: AtomicU64,
    total: AtomicU64,
    phase: AtomicU8,
}
#[derive(Clone, Copy, Debug)]
pub struct Snapshot {
    pub done: u64,
    pub total: u64,
    pub phase: u8,
}
impl Progress {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            done: self.done.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
            phase: self.phase.load(Ordering::Relaxed),
        }
    }
    pub fn reset(&self) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(0, Ordering::Relaxed);
        self.phase.store(0, Ordering::Relaxed);
    }
    fn begin(&self, phase: u8, total: u64) {
        self.done.store(0, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
        self.phase.store(phase, Ordering::Relaxed);
    }
    pub(crate) fn advance(&self, bytes: u64) {
        self.done.fetch_add(bytes, Ordering::Relaxed);
    }
}
tokio::task_local! {static CURRENT:Arc<Progress>;}
pub async fn track<T>(observer: Arc<Progress>, work: impl std::future::Future<Output = T>) -> T {
    CURRENT.scope(observer, work).await
}
pub(crate) fn begin(phase: u8, total: u64) -> Option<Arc<Progress>> {
    CURRENT
        .try_with(|p| {
            p.begin(phase, total);
            p.clone()
        })
        .ok()
}
pub(crate) fn bytes() -> u64 {
    CURRENT.try_with(|p| p.snapshot().done).unwrap_or(0)
}
