//! Real production event loop and renderer, with deterministic host-only native callbacks.

use crate::{backend, framebuffer};

pub(crate) mod hooks {
    use super::*;
    use i_slint_core::lengths::PhysicalEdges;
    use std::sync::{Mutex, mpsc::Sender};
    use std::time::Duration;
    #[derive(Debug, Copy, Clone)]
    pub enum IdentityChange {
        SurfaceGeneration,
        EqualAreaResize,
        ScaleFactor,
    }
    #[derive(Debug, Copy, Clone)]
    pub enum Mode {
        DuringPresent,
        FastFrame,
        RequestFailure,
        SurfaceReplacement,
        FramebufferIdentity(IdentityChange),
    }
    struct Scenario {
        mode: Mode,
        sender: Sender<backend::Event>,
        requests: usize,
        presentations: usize,
        pending: bool,
        timer_checked_gate: bool,
        generations: Vec<u64>,
        trace: Vec<&'static str>,
    }
    static STATE: Mutex<Option<Scenario>> = Mutex::new(None);
    fn viewport(sender: &Sender<backend::Event>) {
        sender
            .send(backend::Event::Viewport {
                width: 32,
                height: 24,
                density: 1.,
                insets: PhysicalEdges::default(),
            })
            .unwrap();
    }
    pub fn begin(mode: Mode, sender: Sender<backend::Event>) {
        *STATE.lock().unwrap() = Some(Scenario {
            mode,
            sender,
            requests: 0,
            presentations: 0,
            pending: false,
            timer_checked_gate: false,
            generations: vec![],
            trace: vec![],
        });
    }
    pub fn request_frame() -> Option<bool> {
        let mut guard = STATE.lock().unwrap();
        let s = guard.as_mut()?;
        s.requests += 1;
        s.trace.push("request");
        if matches!(s.mode, Mode::RequestFailure) {
            return Some(false);
        }
        assert!(
            !s.pending,
            "a later draw must consume the prior callback before requesting again"
        );
        s.pending = true;
        Some(true)
    }
    fn callback() {
        let mut guard = STATE.lock().unwrap();
        let s = guard.as_mut().unwrap();
        s.pending = false;
        s.trace.push("vsync");
        s.sender.send(backend::Event::Frame).unwrap();
    }
    pub fn present(
        generation: u64,
        pixels: &[framebuffer::OpaqueRgba],
        width: u32,
        height: u32,
        damage: &slint::platform::software_renderer::PhysicalRegion,
    ) -> Option<bool> {
        let mut guard = STATE.lock().unwrap();
        let s = guard.as_mut()?;
        let expected_size = match (s.mode, s.presentations) {
            (Mode::FramebufferIdentity(IdentityChange::EqualAreaResize), 3) => (24, 32),
            _ => (32, 24),
        };
        assert_eq!((width, height), expected_size);
        assert_eq!(pixels.len(), 768);
        s.presentations += 1;
        s.generations.push(generation);
        s.trace.push("present");
        assert_eq!(
            s.requests, s.presentations,
            "request must precede every presentation, including the first"
        );
        assert!(
            framebuffer::OpaqueRgba::as_bytes(pixels)
                .chunks_exact(4)
                .all(|p| p[3] == 255)
        );
        if let Mode::FramebufferIdentity(change) = s.mode {
            match s.presentations {
                3 => assert_eq!(
                    damage.bounding_box_size(),
                    slint::PhysicalSize::new(0, 0),
                    "warm retained buffer must have no damage before identity changes"
                ),
                4 => {
                    assert_eq!(
                        damage.bounding_box_origin(),
                        slint::PhysicalPosition::new(0, 0)
                    );
                    assert_eq!(
                        damage.bounding_box_size(),
                        slint::PhysicalSize::new(width, height),
                        "a changed framebuffer identity must force a complete source render"
                    );
                    let last = &framebuffer::OpaqueRgba::as_bytes(pixels)[pixels.len() * 4 - 4..];
                    assert_eq!(last, &[0x23, 0x45, 0x67, 255]);
                    s.sender.send(backend::Event::Quit).unwrap();
                    return Some(true);
                }
                _ => {}
            }
            let sender = s.sender.clone();
            let frame = s.presentations;
            drop(guard);
            if frame == 3 {
                match change {
                    IdentityChange::SurfaceGeneration => sender
                        .send(backend::Event::Surface {
                            generation: 2,
                            available: true,
                        })
                        .unwrap(),
                    IdentityChange::EqualAreaResize | IdentityChange::ScaleFactor => sender
                        .send(backend::Event::Viewport {
                            width: if matches!(change, IdentityChange::EqualAreaResize) {
                                24
                            } else {
                                32
                            },
                            height: if matches!(change, IdentityChange::EqualAreaResize) {
                                32
                            } else {
                                24
                            },
                            density: if matches!(change, IdentityChange::ScaleFactor) {
                                2.
                            } else {
                                1.
                            },
                            insets: PhysicalEdges::default(),
                        })
                        .unwrap(),
                }
            } else {
                viewport(&sender);
            }
            callback();
            return Some(true);
        }
        if s.presentations == 2 {
            s.sender.send(backend::Event::Quit).unwrap();
            return Some(true);
        }
        let sender = s.sender.clone();
        let mode = s.mode;
        drop(guard);
        match mode {
            Mode::DuringPresent => {
                // Deterministic callback while native presentation is still in progress.
                callback();
                viewport(&sender);
            }
            Mode::FastFrame => {
                viewport(&sender);
                sender
                    .send(backend::Event::Task(Box::new(move || {
                        // A redraw is already queued, but the pending VSync must gate it.
                        slint::Timer::single_shot(Duration::from_millis(1), move || {
                            {
                                let mut guard = STATE.lock().unwrap();
                                let s = guard.as_mut().unwrap();
                                assert_eq!(
                                    s.presentations, 1,
                                    "fast redraw cannot outrun pending VSync"
                                );
                                assert!(s.pending);
                                s.timer_checked_gate = true;
                                s.trace.push("timer-gate");
                            }
                            callback();
                        });
                    })))
                    .unwrap();
            }
            Mode::RequestFailure => viewport(&sender),
            Mode::SurfaceReplacement => {
                // A destroyed/recreated native surface rejects old-generation present.
                sender
                    .send(backend::Event::Surface {
                        generation: 2,
                        available: false,
                    })
                    .unwrap();
                sender
                    .send(backend::Event::Surface {
                        generation: 3,
                        available: true,
                    })
                    .unwrap();
                viewport(&sender);
                callback();
                return Some(false);
            }
            Mode::FramebufferIdentity(_) => unreachable!(),
        }
        Some(true)
    }
    pub fn finish() {
        let s = STATE.lock().unwrap().take().unwrap();
        let expected = if matches!(s.mode, Mode::FramebufferIdentity(_)) {
            4
        } else {
            2
        };
        assert_eq!((s.requests, s.presentations), (expected, expected));
        match s.mode {
            Mode::FastFrame => assert!(s.timer_checked_gate),
            Mode::SurfaceReplacement => assert_eq!(s.generations, vec![1, 3]),
            Mode::RequestFailure => assert!(!s.trace.contains(&"vsync")),
            Mode::DuringPresent => {
                assert_eq!(s.trace[..4], ["request", "present", "vsync", "request"])
            }
            Mode::FramebufferIdentity(IdentityChange::SurfaceGeneration) => {
                assert_eq!(s.generations, vec![1, 1, 1, 2]);
            }
            Mode::FramebufferIdentity(_) => assert_eq!(s.generations, vec![1, 1, 1, 1]),
        }
        println!(
            "pacing {:?}: requests={} presents={} generations={:?} trace={:?}",
            s.mode, s.requests, s.presentations, s.generations, s.trace
        );
    }
}
use slint::{
    ComponentHandle,
    platform::{Platform, PlatformError, WindowAdapter},
};
use std::{rc::Rc, sync::mpsc};
struct DelegatingPlatform(Rc<backend::OhosPlatform>);
impl Platform for DelegatingPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        self.0.create_window_adapter()
    }
}
slint::slint! {
    export component PacingProbe inherits Window {
        width: 32px; height: 24px;
        background: #234567;
        Rectangle { x: 2px; y: 3px; width: 12px; height: 15px; background: #ff335566; border-radius: 4px; }
    }
}
fn check(mode: hooks::Mode) {
    let (sender, receiver) = mpsc::channel();
    let owner = Rc::new(backend::OhosPlatform::new(sender.clone(), receiver));
    slint::platform::set_platform(Box::new(DelegatingPlatform(owner.clone()))).unwrap();
    let probe = PacingProbe::new().unwrap();
    probe.show().unwrap();
    sender
        .send(backend::Event::Viewport {
            width: 32,
            height: 24,
            density: 1.,
            insets: i_slint_core::lengths::PhysicalEdges::default(),
        })
        .unwrap();
    sender
        .send(backend::Event::Surface {
            generation: 1,
            available: true,
        })
        .unwrap();
    hooks::begin(mode, sender);
    owner.run_event_loop().unwrap();
    hooks::finish();
}
#[test]
fn early_vsync_during_present_unblocks_next_dirty_frame() {
    check(hooks::Mode::DuringPresent);
}
#[test]
fn early_vsync_fast_frame_waits_and_timers_continue() {
    check(hooks::Mode::FastFrame);
}
#[test]
fn early_vsync_request_failure_does_not_gate_redraw() {
    check(hooks::Mode::RequestFailure);
}
#[test]
fn early_vsync_rejected_old_generation_recovers_new_surface() {
    check(hooks::Mode::SurfaceReplacement);
}
#[test]
fn framebuffer_generation_change_repaints_warm_source_completely() {
    check(hooks::Mode::FramebufferIdentity(
        hooks::IdentityChange::SurfaceGeneration,
    ));
}
#[test]
fn framebuffer_equal_area_resize_repaints_source_with_new_row_layout() {
    check(hooks::Mode::FramebufferIdentity(
        hooks::IdentityChange::EqualAreaResize,
    ));
}
#[test]
fn framebuffer_scale_change_repaints_source_completely() {
    check(hooks::Mode::FramebufferIdentity(
        hooks::IdentityChange::ScaleFactor,
    ));
}
