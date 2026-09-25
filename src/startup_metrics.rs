//! Opt-in startup phase markers used by the repeatable benchmark harness.
//!
//! Normal launches pay only one environment lookup and keep no timers. Set
//! `FLECTAR_STARTUP_METRICS=1` to emit JSON events on stderr. Benchmark-only
//! snapshots force a real Slint render, making the reported frame milestones
//! comparable across runs without changing production rendering behavior.

use crate::AppWindow;
use serde_json::{Map, Value, json};
use slint::ComponentHandle;
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const METRIC_PREFIX: &str = "FLECTAR_STARTUP_METRIC ";

#[derive(Clone)]
pub(crate) struct StartupMetrics {
    inner: Option<Arc<StartupMetricsInner>>,
}

struct StartupMetricsInner {
    started: Instant,
    emitted_once: Mutex<HashSet<&'static str>>,
}

impl StartupMetrics {
    pub(crate) fn from_environment() -> Self {
        let enabled = std::env::var("FLECTAR_STARTUP_METRICS")
            .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "yes"));
        let metrics = Self {
            inner: enabled.then(|| {
                Arc::new(StartupMetricsInner {
                    started: Instant::now(),
                    emitted_once: Mutex::new(HashSet::new()),
                })
            }),
        };
        metrics.emit(
            "run_started",
            json!({
                "package_version": env!("CARGO_PKG_VERSION"),
                "target_os": std::env::consts::OS,
                "target_arch": std::env::consts::ARCH,
                "pid": std::process::id(),
            }),
        );
        metrics
    }

    pub(crate) fn enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub(crate) fn emit(&self, event: &'static str, details: Value) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        let mut payload = match details {
            Value::Object(values) => values,
            Value::Null => Map::new(),
            value => Map::from_iter([("details".to_owned(), value)]),
        };
        payload.insert("event".to_owned(), Value::String(event.to_owned()));
        payload.insert(
            "elapsed_ms".to_owned(),
            json!(inner.started.elapsed().as_secs_f64() * 1_000.0),
        );
        eprintln!("{METRIC_PREFIX}{}", Value::Object(payload));
    }

    pub(crate) fn emit_once(&self, event: &'static str, details: Value) {
        let Some(inner) = self.inner.as_ref() else {
            return;
        };
        if !inner
            .emitted_once
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(event)
        {
            return;
        }
        self.emit(event, details);
    }

    /// Render the current window state on the next event-loop turn and record
    /// the completed snapshot. This is enabled only by the benchmark harness.
    pub(crate) fn schedule_rendered_frame(&self, app: slint::Weak<AppWindow>, event: &'static str) {
        if !self.enabled() {
            return;
        }
        let metrics = self.clone();
        slint::Timer::single_shot(Duration::ZERO, move || {
            let Some(app) = app.upgrade() else {
                return;
            };
            match app.window().take_snapshot() {
                Ok(frame) => {
                    if let Some(directory) = std::env::var_os("FLECTAR_BENCHMARK_SCREENSHOT_DIR") {
                        let directory = std::path::PathBuf::from(directory);
                        let result = std::fs::create_dir_all(&directory).and_then(|_| {
                            image::save_buffer(
                                directory.join(format!("{event}.png")),
                                frame.as_bytes(),
                                frame.width(),
                                frame.height(),
                                image::ColorType::Rgba8,
                            )
                            .map_err(std::io::Error::other)
                        });
                        if let Err(error) = result {
                            eprintln!("benchmark screenshot: {error}");
                        }
                    }
                    metrics.emit_once(
                        event,
                        json!({
                            "width": frame.width(),
                            "height": frame.height(),
                        }),
                    )
                }
                Err(error) => metrics.emit_once(
                    event,
                    json!({
                        "render_error": error.to_string(),
                    }),
                ),
            }
        });
    }

    pub(crate) fn schedule_benchmark_exit(&self, app: slint::Weak<AppWindow>) {
        if !self.enabled() {
            return;
        }
        // Opt-in lifecycle measurement uses exactly the production tray
        // release/restore paths, without requiring a desktop tray host.
        if let Some(interval) = std::env::var("FLECTAR_BENCHMARK_TRAY_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| (1000..=30_000).contains(n))
        {
            let weak = app.clone();
            let metrics = self.clone();
            slint::Timer::single_shot(Duration::from_millis(interval), move || {
                if let Some(app) = weak.upgrade() {
                    crate::window_controller::prepare_for_tray(&app);
                    match app.hide() {
                        Ok(()) => metrics.emit("tray_hidden", json!({})),
                        Err(error) => {
                            metrics.emit("tray_error", json!({"error": error.to_string()}))
                        }
                    }
                }
            });
            let metrics = self.clone();
            slint::Timer::single_shot(Duration::from_millis(interval * 2), move || {
                if let Some(app) = app.upgrade() {
                    crate::window_controller::show_from_tray(&app);
                    metrics.schedule_rendered_frame(app.as_weak(), "tray_restored_frame");
                }
            });
        }
        let Some(delay_ms) = std::env::var("FLECTAR_BENCHMARK_EXIT_AFTER_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .filter(|delay| (250..=120_000).contains(delay))
        else {
            return;
        };
        let metrics = self.clone();
        slint::Timer::single_shot(Duration::from_millis(delay_ms), move || {
            metrics.emit_once("benchmark_exit", json!({ "delay_ms": delay_ms }));
            let _ = slint::quit_event_loop();
        });
    }
}
