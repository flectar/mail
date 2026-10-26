//! `slint::platform::Platform` implementation and the Slint-side event loop.
//!
//! The loop runs on the application thread (spawned by [`crate::start`]) and is
//! driven by the same mpsc channel that receives surface and touch events from
//! the OHOS main thread.

use core::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use slint::PhysicalSize;
use slint::platform::{EventLoopProxy, Platform, PlatformError, WindowAdapter};

use crate::event::BackendEvent;
use crate::present::{PresentOutcome, Presenter, SurfaceGate};
use crate::window::OhosWindow;

/// Lower bound of the wake-up cadence while animations are running (~60 Hz).
const ANIMATION_TICK: Duration = Duration::from_millis(16);
/// Idle poll interval. Bounded so Slint `Timer`s fire with acceptable latency
/// without burning a core. (A vsync-driven loop can replace this later.)
const IDLE_TICK: Duration = Duration::from_millis(100);

/// Backend platform state. Lives on the application thread inside Slint's
/// thread-local context.
pub struct OhosPlatform {
    start_time: Instant,
    event_tx: Sender<BackendEvent>,
    event_rx: RefCell<Option<Receiver<BackendEvent>>>,
    window: RefCell<Option<Rc<OhosWindow>>>,
    gate: Arc<SurfaceGate>,
    quit: Arc<AtomicBool>,
    visible: Arc<AtomicBool>,
    /// Drawn-frame times + RSS, reported to hilog once a minute.
    perf: crate::perf::FrameStats,
}

impl OhosPlatform {
    pub fn new(
        event_tx: Sender<BackendEvent>,
        event_rx: Receiver<BackendEvent>,
        gate: Arc<SurfaceGate>,
        quit: Arc<AtomicBool>,
        visible: Arc<AtomicBool>,
    ) -> Self {
        Self {
            start_time: Instant::now(),
            event_tx,
            event_rx: RefCell::new(Some(event_rx)),
            window: RefCell::new(None),
            gate,
            quit,
            visible,
            perf: crate::perf::FrameStats::default(),
        }
    }

    fn run_loop(&self) -> Result<(), PlatformError> {
        let Some(rx) = self.event_rx.borrow_mut().take() else {
            return Err(PlatformError::NoEventLoopProvider);
        };

        let mut presenter = Presenter::new(self.visible.clone());
        let mut surface_size: Option<PhysicalSize> = None;
        let mut active_surface_epoch: Option<u64> = None;
        let mut next_frame = Instant::now();

        loop {
            let now = Instant::now();
            let mut timeout = if self.quit.load(Ordering::Relaxed) {
                Duration::ZERO
            } else if self
                .window
                .borrow()
                .as_ref()
                .is_some_and(|w| w.window().has_active_animations())
            {
                ANIMATION_TICK
            } else {
                IDLE_TICK
            };
            // Throttle redraws to the animation cadence: Slint may request a
            // redraw for every frame (e.g. through `animation-tick()`), and
            // without this the loop would spin at unbounded fps.
            if now < next_frame {
                timeout = timeout.min(next_frame - now);
            }

            match rx.recv_timeout(timeout) {
                Ok(event) => self.handle_event(
                    event,
                    &mut presenter,
                    &mut surface_size,
                    &mut active_surface_epoch,
                ),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            // Drain the queue before drawing so bursts coalesce into one frame.
            while let Ok(event) = rx.try_recv() {
                self.handle_event(
                    event,
                    &mut presenter,
                    &mut surface_size,
                    &mut active_surface_epoch,
                );
            }

            slint::platform::update_timers_and_animations();

            if self.quit.load(Ordering::Relaxed) {
                break;
            }

            let now = Instant::now();
            if now >= next_frame {
                self.draw(&mut presenter, surface_size);
                next_frame = now + ANIMATION_TICK;
            }
        }

        Ok(())
    }

    fn handle_event(
        &self,
        event: BackendEvent,
        presenter: &mut Presenter,
        surface_size: &mut Option<PhysicalSize>,
        active_surface_epoch: &mut Option<u64>,
    ) {
        match event {
            BackendEvent::Surface {
                epoch,
                window,
                width,
                height,
            } => {
                // A Surface event can sit in the queue while OnSurfaceDestroyed
                // times out and the compositor releases the window. Never
                // attach a pointer whose epoch was invalidated by that
                // teardown.
                if !self.gate.is_surface_epoch(epoch) {
                    crate::log::info(&format!(
                        "platform: ignoring stale surface epoch {epoch} ({width}x{height})"
                    ));
                    return;
                }
                *active_surface_epoch = Some(epoch);
                presenter.set_surface(window.0);
                let size = PhysicalSize::new(width, height);
                *surface_size = Some(size);
                // Slint works in logical pixels; the surface size is physical.
                // The ArkUI `vp` scale (display density) maps between them.
                let density = crate::display::default_density().unwrap_or(1.0);
                if let Some(w) = self.window.borrow().as_ref() {
                    w.dispatch_scale_factor(density);
                    w.adapt_size(size);
                    // Fresh surface: repaint everything before partial updates
                    // resume (software), or (re)attach the GPU surface. The
                    // gate stays locked across attach so a teardown cannot
                    // invalidate the pointer mid-call.
                    w.invalidate_frame();
                    if self
                        .gate
                        .with_surface(epoch, || w.attach_surface(window.0, size))
                        .is_none()
                    {
                        crate::log::info(&format!(
                            "platform: surface epoch {epoch} torn down before attach"
                        ));
                        *active_surface_epoch = None;
                        *surface_size = None;
                        presenter.clear();
                        return;
                    }
                    w.request_redraw();
                }
                crate::log::info(&format!(
                    "platform: surface {width}x{height} epoch {epoch} density {density}"
                ));
            }
            BackendEvent::SurfaceDestroyed { epoch } => {
                // Ignore a stale destroy if a newer surface has already been
                // accepted; its own event will clear that generation. A destroy
                // with no epoch had no live surface at callback time and must
                // not clear a surface that was created afterwards.
                let matches_active = epoch.is_some() && epoch == *active_surface_epoch;
                let had_no_surface = epoch.is_none() && active_surface_epoch.is_none();
                if matches_active || had_no_surface {
                    presenter.clear();
                    *surface_size = None;
                    *active_surface_epoch = None;
                    if let Some(w) = self.window.borrow().as_ref() {
                        w.detach_surface();
                    }
                } else {
                    crate::log::info(&format!(
                        "platform: ignoring stale surface destroy {epoch:?}"
                    ));
                }
                // The gate itself decides whether this acknowledgement still
                // belongs to the current generation.
                if let Some(epoch) = epoch {
                    self.gate.acked(epoch);
                }
            }
            BackendEvent::Touch { x, y, phase } => {
                if let Some(w) = self.window.borrow().as_ref() {
                    w.dispatch_touch(x, y, phase);
                }
            }
            BackendEvent::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                if let Some(w) = self.window.borrow().as_ref() {
                    w.dispatch_scroll(x, y, delta_x, delta_y);
                }
            }
            BackendEvent::ColorScheme(dark) => {
                if let Some(w) = self.window.borrow().as_ref() {
                    let scheme = if dark {
                        i_slint_core::items::ColorScheme::Dark
                    } else {
                        i_slint_core::items::ColorScheme::Light
                    };
                    i_slint_core::window::WindowInner::from_pub(w.window())
                        .context()
                        .set_color_scheme(scheme);
                    w.request_redraw();
                }
            }
            BackendEvent::Mouse {
                x,
                y,
                action,
                button,
            } => {
                if let Some(w) = self.window.borrow().as_ref() {
                    w.dispatch_mouse(x, y, action, button);
                }
            }
            BackendEvent::Key { text, down, repeat } => {
                if let Some(w) = self.window.borrow().as_ref() {
                    w.dispatch_key(text, down, repeat);
                }
            }
            BackendEvent::SurfaceHidden => {
                // The gate already blocks drawing; keep the retained frame so
                // the window can repaint immediately when it comes back.
                crate::log::info("surface hidden; render loop paused");
            }
            BackendEvent::SurfaceShown => {
                if let Some(w) = self.window.borrow().as_ref() {
                    w.invalidate_frame();
                    // Invalidate only resets the renderer's partial-repaint
                    // cache; it does not schedule a frame.
                    w.request_redraw();
                }
                crate::log::info("surface shown; repaint requested");
            }
            BackendEvent::Task(task) => task(),
            BackendEvent::Wake => {}
        }
    }

    fn draw(&self, presenter: &mut Presenter, surface_size: Option<PhysicalSize>) {
        if self.gate.is_destroying() {
            return;
        }
        // Keep `needs_redraw` set while hidden: the retained frame is skipped
        // by the presenter, and the next SurfaceShown must schedule a fresh
        // frame instead of finding the request already consumed.
        if !self.visible.load(Ordering::Relaxed) {
            return;
        }
        let Some(size) = surface_size else { return };
        let Some(window) = self.window.borrow().clone() else {
            return;
        };
        let started = Instant::now();
        let (drawn, outcome) = window.draw_if_needed(size, |renderer| {
            presenter.present(renderer, size.width, size.height)
        });
        if drawn {
            self.perf.record(started.elapsed());
        }
        if let Some(PresentOutcome::Failed(code)) = outcome {
            // The frame was dropped; request another so a transient buffer
            // queue failure does not leave stale pixels on screen.
            crate::log::error(&format!("present failed: {code}"));
            window.request_redraw();
        }
    }
}

impl Platform for OhosPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let window = OhosWindow::new(self.event_tx.clone());
        self.window.replace(Some(window.clone()));
        Ok(window)
    }

    fn run_event_loop(&self) -> Result<(), PlatformError> {
        self.run_loop()
    }

    fn new_event_loop_proxy(&self) -> Option<Box<dyn EventLoopProxy>> {
        Some(Box::new(OhosEventLoopProxy {
            tx: self.event_tx.clone(),
            quit: self.quit.clone(),
        }))
    }

    fn duration_since_start(&self) -> Duration {
        self.start_time.elapsed()
    }

    /// OpenHarmony has one system clipboard and no selection clipboard. Slint
    /// copies every selection to the latter, password fields included, so it
    /// must not reach the system clipboard.
    fn set_clipboard_text(&self, text: &str, clipboard: slint::platform::Clipboard) {
        if clipboard == slint::platform::Clipboard::DefaultClipboard {
            crate::clipboard::write_text(text);
        }
    }

    fn clipboard_text(&self, clipboard: slint::platform::Clipboard) -> Option<String> {
        if clipboard == slint::platform::Clipboard::DefaultClipboard {
            crate::clipboard::read_text()
        } else {
            None
        }
    }
}

/// Proxy used by `slint::invoke_from_event_loop` and `slint::quit_event_loop`.
struct OhosEventLoopProxy {
    tx: Sender<BackendEvent>,
    quit: Arc<AtomicBool>,
}

impl EventLoopProxy for OhosEventLoopProxy {
    fn quit_event_loop(&self) -> Result<(), slint::EventLoopError> {
        self.quit.store(true, Ordering::Relaxed);
        self.tx
            .send(BackendEvent::Wake)
            .map_err(|_| slint::EventLoopError::EventLoopTerminated)
    }

    fn invoke_from_event_loop(
        &self,
        event: Box<dyn FnOnce() + Send>,
    ) -> Result<(), slint::EventLoopError> {
        self.tx
            .send(BackendEvent::Task(event))
            .map_err(|_| slint::EventLoopError::EventLoopTerminated)
    }
}
