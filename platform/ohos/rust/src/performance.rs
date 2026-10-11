//! Optional, single UI thread performance measurements. No user data is collected.
//!
//! Enable this explicitly for diagnostics; the default is disabled. Mark only a
//! continuous gesture, animation, or benchmark with `set_active(true)`. Active
//! FPS is successful presents divided by the real time in those marked spans,
//! including slow frames and waits. Wall FPS includes the whole report window.
//! Neither metric is the inverse of rendering CPU time. Static, inactive windows
//! without frame attempts report `idle=true` and no FPS.
//!
//! The caller supplies measured phase durations and logs a returned report at
//! most every two seconds, or explicitly flushes at a measurement boundary.
//! Percentiles retain the latest 512 observations per distribution; count,
//! total, and maximum cover all observations. Frame intervals join successful
//! presents within an explicitly active span, including across report windows.
//! `set_active(false)` breaks that sequence so a deliberate idle gap is excluded.

use std::fmt;
use std::time::{Duration, Instant};

pub const REPORT_INTERVAL: Duration = Duration::from_secs(2);
pub const SAMPLE_CAPACITY: usize = 512;

#[derive(Clone, Copy, Debug)]
pub enum Phase {
    EventProcessing,
    Timers,
    SlintRender,
    NativePresent,
}

impl Phase {
    fn index(self) -> usize {
        match self {
            Self::EventProcessing => 0,
            Self::Timers => 1,
            Self::SlintRender => 2,
            Self::NativePresent => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Distribution {
    pub count: u64,
    pub retained_samples: usize,
    pub total: Duration,
    pub p50: Option<Duration>,
    pub p95: Option<Duration>,
    pub max: Option<Duration>,
}

#[derive(Debug)]
pub struct Report {
    pub wall_span: Duration,
    pub active_span: Duration,
    pub idle: bool,
    pub event_batches: u64,
    pub events: u64,
    pub frame_attempts: u64,
    pub successful_frames: u64,
    pub failed_frames: u64,
    pub active_successful_frames: u64,
    pub wall_fps: Option<f64>,
    pub active_fps: Option<f64>,
    pub event_processing: Distribution,
    pub timers: Distribution,
    pub slint_render: Distribution,
    pub native_present: Distribution,
    pub frame_work: Distribution,
    pub frame_intervals: Distribution,
}

/// Mutate this only on the UI thread. Disabled calls allocate and record nothing.
pub struct PerformanceStats {
    enabled: bool,
    window_started: Instant,
    active_started: Option<Instant>,
    active_elapsed: Duration,
    last_active_present: Option<Instant>,
    event_batches: u64,
    events: u64,
    frame_attempts: u64,
    successful_frames: u64,
    failed_frames: u64,
    active_successful_frames: u64,
    phases: [Samples; 4],
    frame_work: Samples,
    frame_intervals: Samples,
}

impl Default for PerformanceStats {
    fn default() -> Self {
        Self::new(false)
    }
}

impl PerformanceStats {
    pub fn new(enabled: bool) -> Self {
        Self::new_at(enabled, Instant::now())
    }

    fn new_at(enabled: bool, now: Instant) -> Self {
        Self {
            enabled,
            window_started: now,
            active_started: None,
            active_elapsed: Duration::ZERO,
            last_active_present: None,
            event_batches: 0,
            events: 0,
            frame_attempts: 0,
            successful_frames: 0,
            failed_frames: 0,
            active_successful_frames: 0,
            phases: std::array::from_fn(|_| Samples::new(enabled)),
            frame_work: Samples::new(enabled),
            frame_intervals: Samples::new(enabled),
        }
    }

    /// The caller can use this to avoid taking phase timestamps when disabled.
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Mark a real continuous measurement, independent of rendering duration.
    /// Repeating the current state leaves its measurement boundary unchanged.
    pub fn set_active(&mut self, active: bool) {
        if self.enabled {
            self.set_active_at(active, Instant::now());
        }
    }

    fn set_active_at(&mut self, active: bool, now: Instant) {
        if !self.enabled || active == self.active_started.is_some() {
            return;
        }
        if active {
            self.active_started = Some(now);
        } else {
            if let Some(started) = self.active_started.take() {
                self.active_elapsed = self.active_elapsed.saturating_add(since(now, started));
            }
            self.last_active_present = None;
        }
    }

    /// Record one drained queue batch, including its full processing duration.
    /// Do not also record that same batch with `Phase::EventProcessing`.
    pub fn record_event_batch(&mut self, events: usize, elapsed: Duration) {
        if !self.enabled {
            return;
        }
        self.event_batches = self.event_batches.saturating_add(1);
        self.events = self.events.saturating_add(events as u64);
        self.phases[Phase::EventProcessing.index()].record(elapsed);
    }

    pub fn record_phase(&mut self, phase: Phase, elapsed: Duration) {
        if self.enabled {
            self.phases[phase.index()].record(elapsed);
        }
    }

    /// Call after a frame attempt. `presented` must mean native present succeeded.
    /// `elapsed` is total frame work latency, and is never used to derive FPS.
    /// Successful timestamp intervals include failed attempts between successes.
    pub fn record_frame(&mut self, presented: bool, elapsed: Duration) {
        if self.enabled {
            self.record_frame_at(presented, elapsed, Instant::now());
        }
    }

    fn record_frame_at(&mut self, presented: bool, elapsed: Duration, now: Instant) {
        if !self.enabled {
            return;
        }
        self.frame_attempts = self.frame_attempts.saturating_add(1);
        self.frame_work.record(elapsed);
        if !presented {
            self.failed_frames = self.failed_frames.saturating_add(1);
            return;
        }
        self.successful_frames = self.successful_frames.saturating_add(1);
        if self.active_started.is_some() {
            self.active_successful_frames = self.active_successful_frames.saturating_add(1);
            if let Some(previous) = self.last_active_present.replace(now) {
                self.frame_intervals.record(since(now, previous));
            }
        }
    }

    /// Return a summary when at least two seconds have elapsed. No logging occurs
    /// inside this module; the caller chooses an existing diagnostic log sink.
    pub fn maybe_flush(&mut self) -> Option<Report> {
        if !self.enabled {
            return None;
        }
        self.maybe_flush_at(Instant::now())
    }

    fn maybe_flush_at(&mut self, now: Instant) -> Option<Report> {
        if !self.enabled || since(now, self.window_started) < REPORT_INTERVAL {
            return None;
        }
        self.flush_at(now)
    }

    pub fn flush(&mut self) -> Option<Report> {
        if !self.enabled {
            return None;
        }
        self.flush_at(Instant::now())
    }

    fn flush_at(&mut self, now: Instant) -> Option<Report> {
        if !self.enabled {
            return None;
        }
        let wall_span = since(now, self.window_started);
        let active_span = self.active_elapsed.saturating_add(
            self.active_started
                .map(|started| since(now, started))
                .unwrap_or_default(),
        );
        let idle = active_span.is_zero() && self.frame_attempts == 0;
        let report = Report {
            wall_span,
            active_span,
            idle,
            event_batches: self.event_batches,
            events: self.events,
            frame_attempts: self.frame_attempts,
            successful_frames: self.successful_frames,
            failed_frames: self.failed_frames,
            active_successful_frames: self.active_successful_frames,
            wall_fps: if idle {
                None
            } else {
                fps(self.successful_frames, wall_span)
            },
            active_fps: fps(self.active_successful_frames, active_span),
            event_processing: self.phases[Phase::EventProcessing.index()].summary(),
            timers: self.phases[Phase::Timers.index()].summary(),
            slint_render: self.phases[Phase::SlintRender.index()].summary(),
            native_present: self.phases[Phase::NativePresent.index()].summary(),
            frame_work: self.frame_work.summary(),
            frame_intervals: self.frame_intervals.summary(),
        };
        self.window_started = now;
        if self.active_started.is_some() {
            self.active_started = Some(now);
        }
        self.active_elapsed = Duration::ZERO;
        self.event_batches = 0;
        self.events = 0;
        self.frame_attempts = 0;
        self.successful_frames = 0;
        self.failed_frames = 0;
        self.active_successful_frames = 0;
        for samples in &mut self.phases {
            samples.clear();
        }
        self.frame_work.clear();
        self.frame_intervals.clear();
        Some(report)
    }
}

fn since(now: Instant, started: Instant) -> Duration {
    now.checked_duration_since(started).unwrap_or_default()
}

fn fps(frames: u64, span: Duration) -> Option<f64> {
    if span.is_zero() {
        None
    } else {
        Some(frames as f64 / span.as_secs_f64())
    }
}

#[derive(Default)]
struct Samples {
    values_ns: Vec<u64>,
    next: usize,
    count: u64,
    total: Duration,
    max: Option<Duration>,
}

impl Samples {
    fn new(enabled: bool) -> Self {
        Self {
            values_ns: if enabled {
                Vec::with_capacity(SAMPLE_CAPACITY)
            } else {
                Vec::new()
            },
            ..Self::default()
        }
    }

    fn record(&mut self, elapsed: Duration) {
        self.count = self.count.saturating_add(1);
        self.total = self.total.saturating_add(elapsed);
        self.max = Some(self.max.unwrap_or_default().max(elapsed));
        let value = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        if self.values_ns.len() < SAMPLE_CAPACITY {
            self.values_ns.push(value);
        } else {
            self.values_ns[self.next] = value;
            self.next = (self.next + 1) % SAMPLE_CAPACITY;
        }
    }

    fn summary(&self) -> Distribution {
        let mut sorted = self.values_ns.clone();
        sorted.sort_unstable();
        let percentile = |percent: usize| {
            if sorted.is_empty() {
                None
            } else {
                let rank = (sorted.len() * percent).div_ceil(100);
                Some(Duration::from_nanos(sorted[rank - 1]))
            }
        };
        Distribution {
            count: self.count,
            retained_samples: sorted.len(),
            total: self.total,
            p50: percentile(50),
            p95: percentile(95),
            max: self.max,
        }
    }

    fn clear(&mut self) {
        self.values_ns.clear();
        self.next = 0;
        self.count = 0;
        self.total = Duration::ZERO;
        self.max = None;
    }
}

fn optional_number(f: &mut fmt::Formatter<'_>, value: Option<f64>) -> fmt::Result {
    match value {
        Some(number) => write!(f, "{number:.3}"),
        None => f.write_str("none"),
    }
}

impl fmt::Display for Distribution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "samples={} retained={} total_ms={:.3} p50_ms=",
            self.count,
            self.retained_samples,
            self.total.as_secs_f64() * 1000.0
        )?;
        optional_number(f, self.p50.map(|value| value.as_secs_f64() * 1000.0))?;
        f.write_str(" p95_ms=")?;
        optional_number(f, self.p95.map(|value| value.as_secs_f64() * 1000.0))?;
        f.write_str(" max_ms=")?;
        optional_number(f, self.max.map(|value| value.as_secs_f64() * 1000.0))
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[feihong-perf] wall_ms={:.3} active_ms={:.3} idle={} attempts={} success={} failed={} active_success={} wall_fps=",
            self.wall_span.as_secs_f64() * 1000.0,
            self.active_span.as_secs_f64() * 1000.0,
            self.idle,
            self.frame_attempts,
            self.successful_frames,
            self.failed_frames,
            self.active_successful_frames
        )?;
        optional_number(f, self.wall_fps)?;
        f.write_str(" active_fps=")?;
        optional_number(f, self.active_fps)?;
        write!(
            f,
            " batches={} events={} event[{}] timers[{}] render[{}] present[{}] frame_work[{}] interval[{}]",
            self.event_batches,
            self.events,
            self.event_processing,
            self.timers,
            self.slint_render,
            self.native_present,
            self.frame_work,
            self.frame_intervals
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_is_default_and_records_nothing() {
        let now = Instant::now();
        let mut stats = PerformanceStats::default();
        assert!(!stats.enabled());
        stats.set_active(true);
        stats.record_event_batch(3, Duration::from_millis(20));
        stats.record_phase(Phase::SlintRender, Duration::from_millis(100));
        stats.record_frame(true, Duration::from_millis(100));
        assert!(
            stats
                .maybe_flush_at(now + Duration::from_secs(10))
                .is_none()
        );
        assert!(stats.flush().is_none());
        assert_eq!(stats.phases[0].values_ns.capacity(), 0);
        assert_eq!(stats.frame_work.count, 0);
    }

    #[test]
    fn static_window_does_not_report_low_fps() {
        let now = Instant::now();
        let mut stats = PerformanceStats::new_at(true, now);
        stats.record_event_batch(2, Duration::from_millis(1));
        stats.record_phase(Phase::Timers, Duration::from_micros(10));
        let report = stats.flush_at(now + Duration::from_secs(30)).unwrap();
        assert!(report.idle);
        assert_eq!(report.wall_fps, None);
        assert_eq!(report.active_fps, None);
        assert_eq!(report.frame_intervals.count, 0);
    }

    #[test]
    fn active_fps_includes_waits_and_differs_from_wall_fps() {
        let now = Instant::now();
        let mut stats = PerformanceStats::new_at(true, now);
        stats.set_active_at(true, now + Duration::from_secs(1));
        stats.record_frame_at(
            true,
            Duration::from_millis(1),
            now + Duration::from_millis(1100),
        );
        stats.record_frame_at(
            false,
            Duration::from_millis(200),
            now + Duration::from_millis(1300),
        );
        stats.record_frame_at(
            true,
            Duration::from_millis(1),
            now + Duration::from_millis(1800),
        );
        stats.set_active_at(false, now + Duration::from_secs(2));
        let report = stats.flush_at(now + Duration::from_secs(4)).unwrap();
        assert!(!report.idle);
        assert_eq!(report.active_span, Duration::from_secs(1));
        assert_eq!(report.active_fps, Some(2.0));
        assert_eq!(report.wall_fps, Some(0.5));
        assert_eq!(report.frame_attempts, 3);
        assert_eq!(report.successful_frames, 2);
        assert_eq!(report.failed_frames, 1);
        assert_eq!(report.frame_intervals.p95, Some(Duration::from_millis(700)));
    }

    #[test]
    fn explicit_idle_boundary_breaks_intervals_but_slow_frames_are_preserved() {
        let now = Instant::now();
        let mut stats = PerformanceStats::new_at(true, now);
        stats.set_active_at(true, now);
        stats.record_frame_at(true, Duration::ZERO, now);
        stats.record_frame_at(true, Duration::ZERO, now + Duration::from_secs(3));
        stats.set_active_at(false, now + Duration::from_secs(3));
        stats.set_active_at(true, now + Duration::from_secs(20));
        stats.record_frame_at(true, Duration::ZERO, now + Duration::from_secs(20));
        stats.record_frame_at(true, Duration::ZERO, now + Duration::from_millis(20020));
        stats.set_active_at(false, now + Duration::from_secs(21));
        let report = stats.flush_at(now + Duration::from_secs(22)).unwrap();
        assert_eq!(report.active_span, Duration::from_secs(4));
        assert_eq!(report.active_fps, Some(1.0));
        assert_eq!(report.frame_intervals.count, 2);
        assert_eq!(report.frame_intervals.max, Some(Duration::from_secs(3)));
    }

    #[test]
    fn active_without_success_is_zero_fps_not_idle() {
        let now = Instant::now();
        let mut stats = PerformanceStats::new_at(true, now);
        stats.set_active_at(true, now);
        stats.record_frame_at(false, Duration::from_secs(2), now + Duration::from_secs(2));
        let report = stats.flush_at(now + Duration::from_secs(2)).unwrap();
        assert!(!report.idle);
        assert_eq!(report.active_fps, Some(0.0));
        assert_eq!(report.wall_fps, Some(0.0));
        assert_eq!(report.frame_intervals.count, 0);
    }

    #[test]
    fn sample_cap_keeps_latest_percentiles_but_full_count_total_and_max() {
        let mut samples = Samples::new(true);
        samples.record(Duration::from_secs(10));
        for micros in 1..=1000 {
            samples.record(Duration::from_micros(micros));
        }
        let summary = samples.summary();
        assert_eq!(summary.count, 1001);
        assert_eq!(summary.retained_samples, SAMPLE_CAPACITY);
        assert_eq!(summary.p50, Some(Duration::from_micros(744)));
        assert_eq!(summary.p95, Some(Duration::from_micros(975)));
        assert_eq!(summary.max, Some(Duration::from_secs(10)));
        assert_eq!(
            summary.total,
            Duration::from_secs(10) + Duration::from_micros(500500)
        );
        samples.clear();
        assert_eq!(samples.values_ns.capacity(), SAMPLE_CAPACITY);
        assert_eq!(samples.summary().max, None);
    }

    #[test]
    fn periodic_flush_resets_counts_and_preserves_continuous_slow_interval() {
        let now = Instant::now();
        let mut stats = PerformanceStats::new_at(true, now);
        stats.set_active_at(true, now);
        stats.record_event_batch(5, Duration::from_millis(10));
        stats.record_phase(Phase::NativePresent, Duration::from_millis(20));
        stats.record_frame_at(
            true,
            Duration::from_millis(30),
            now + Duration::from_millis(100),
        );
        assert!(
            stats
                .maybe_flush_at(now + Duration::from_millis(1999))
                .is_none()
        );
        let first = stats.maybe_flush_at(now + Duration::from_secs(2)).unwrap();
        assert_eq!(first.event_batches, 1);
        assert_eq!(first.events, 5);
        assert_eq!(first.event_processing.total, Duration::from_millis(10));
        assert_eq!(first.native_present.total, Duration::from_millis(20));
        assert_eq!(first.active_fps, Some(0.5));
        stats.record_frame_at(
            true,
            Duration::from_millis(100),
            now + Duration::from_secs(3),
        );
        let second = stats.maybe_flush_at(now + Duration::from_secs(4)).unwrap();
        assert_eq!(second.event_batches, 0);
        assert_eq!(second.event_processing.count, 0);
        assert_eq!(second.successful_frames, 1);
        assert_eq!(second.active_span, Duration::from_secs(2));
        assert_eq!(
            second.frame_intervals.max,
            Some(Duration::from_millis(2900))
        );
        assert!(second.to_string().contains("active_fps=0.500"));
    }
}
