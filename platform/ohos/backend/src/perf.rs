//! Periodic performance telemetry: drawn-frame times plus process memory.
//!
//! Long-running (dogfooding) sessions then leave an objective trace in hilog:
//! one `perf:` line per minute with the number of drawn frames, the average and
//! worst frame time and the resident set size. The instrumentation is cheap
//! (two `Instant`s per drawn frame, one `/proc` read per minute) and stays
//! enabled.

use std::cell::Cell;
use std::time::{Duration, Instant};

const REPORT_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct FrameStats {
    /// Start of the current reporting window; `None` until the first frame.
    window_start: Cell<Option<Instant>>,
    frames: Cell<u64>,
    total: Cell<Duration>,
    worst: Cell<Duration>,
}

impl FrameStats {
    /// Record one drawn frame (skipped redraws are not frames) and emit the
    /// periodic report once a window of [`REPORT_INTERVAL`] has elapsed.
    pub fn record(&self, frame_time: Duration) {
        let start = self.window_start.get().unwrap_or_else(|| {
            let now = Instant::now();
            self.window_start.set(Some(now));
            now
        });
        self.frames.set(self.frames.get() + 1);
        self.total.set(self.total.get() + frame_time);
        if frame_time > self.worst.get() {
            self.worst.set(frame_time);
        }
        let elapsed = start.elapsed();
        if elapsed < REPORT_INTERVAL {
            return;
        }
        let frames = self.frames.get();
        let avg_ms = self.total.get().as_secs_f64() * 1000.0 / frames.max(1) as f64;
        let worst_ms = self.worst.get().as_secs_f64() * 1000.0;
        crate::log::info(&format!(
            "perf: {frames} frames in {}s, avg {avg_ms:.1} ms, worst {worst_ms:.1} ms, rss {}",
            elapsed.as_secs(),
            resident_memory(),
        ));
        self.window_start.set(Some(Instant::now()));
        self.frames.set(0);
        self.total.set(Duration::ZERO);
        self.worst.set(Duration::ZERO);
    }
}

/// Process resident set size from `/proc/self/status` (`VmRSS: 312345 kB`).
fn resident_memory() -> String {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return "?".into();
    };
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|kb| kb.parse::<u64>().ok())
        .map(|kb| format!("{:.0} MB", kb as f64 / 1024.0))
        .unwrap_or_else(|| "?".into())
}
