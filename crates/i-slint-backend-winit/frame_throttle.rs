// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

use std::rc::{Rc, Weak};

use i_slint_core::timers::{Timer, TimerMode};

#[cfg(target_vendor = "apple")]
mod apple_display_link;

use crate::winitwindowadapter::{WindowVisibility, WinitWindowAdapter};

pub fn create_frame_throttle(
    window_adapter: Weak<WinitWindowAdapter>,
    _winit_window: &winit::window::Window,
    _is_wayland: bool,
) -> Box<dyn FrameThrottle> {
    // Winit already schedules web redraws through requestAnimationFrame.
    if cfg!(target_arch = "wasm32") || _is_wayland {
        WinitBasedFrameThrottle::create()
    } else {
        #[cfg(target_vendor = "apple")]
        if let Some(throttle) =
            apple_display_link::try_create(window_adapter.clone(), _winit_window)
        {
            return throttle;
        }
        TimerBasedFrameThrottle::create(window_adapter)
    }
}

pub trait FrameThrottle {
    fn request_throttled_redraw(&self, winit_window: &winit::window::Window);
}

struct TimerBasedFrameThrottle {
    window_adapter: Weak<WinitWindowAdapter>,
    timer: Rc<Timer>,
}

impl TimerBasedFrameThrottle {
    fn create(window_adapter: Weak<WinitWindowAdapter>) -> Box<dyn FrameThrottle> {
        Box::new(Self { window_adapter, timer: Rc::new(Timer::default()) })
    }
}

impl FrameThrottle for TimerBasedFrameThrottle {
    fn request_throttled_redraw(&self, winit_window: &winit::window::Window) {
        if self.timer.running() {
            return;
        }
        winit_window.request_redraw();
        let refresh_interval_millihertz = winit_window
            .current_monitor()
            .and_then(|monitor| monitor.refresh_rate_millihertz())
            .filter(|&millihertz| millihertz != 0)
            .unwrap_or(60000) as u64;
        let window_adapter = self.window_adapter.clone();
        let timer = Rc::downgrade(&self.timer);
        // In nanoseconds, because whole milliseconds turned 60 Hz into 62.5 Hz.
        let interval =
            std::time::Duration::from_nanos(1_000_000_000_000 / refresh_interval_millihertz);
        self.timer.start(TimerMode::Repeated, next_frame(interval), move || {
            let Some(adapter) = window_adapter.upgrade() else { return };
            if adapter.pending_redraw()
                && adapter.visibility() != WindowVisibility::Hidden
                && !adapter.is_minimized_on_screen()
                && let Some(winit_window) = adapter.winit_window()
            {
                winit_window.request_redraw();
                #[cfg(target_os = "windows")]
                if let Some(timer) = timer.upgrade() {
                    timer.set_interval(next_frame(interval));
                }
            } else {
                // Showing a hidden window draws it.
                // Windows sends no WM_PAINT to a minimized window, and repaints it on restore.
                adapter.cancel_pending_redraw();
                if let Some(timer) = timer.upgrade() {
                    timer.stop();
                }
            }
        });
    }
}

/// How long until the next frame should start.
///
/// On Windows that is the next vblank of the compositor's clock, as Chromium paces its
/// software frames, so that frames keep in step with the display instead of drifting
/// against it.
fn next_frame(interval: std::time::Duration) -> std::time::Duration {
    #[cfg(target_os = "windows")]
    if let Some(delay) = compositor_clock::until_next_vblank(interval) {
        return delay;
    }
    interval
}

#[cfg(target_os = "windows")]
mod compositor_clock {
    use std::time::Duration;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Dwm::{DWM_TIMING_INFO, DwmGetCompositionTimingInfo};
    use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

    /// The time until DWM's next vblank. `None` when DWM has no timing, or when its refresh
    /// period is more than a tenth away from `interval`, as for a window on a monitor with
    /// another refresh rate than the one DWM follows.
    pub fn until_next_vblank(interval: Duration) -> Option<Duration> {
        let mut info = DWM_TIMING_INFO {
            cbSize: core::mem::size_of::<DWM_TIMING_INFO>() as u32,
            ..Default::default()
        };
        // Since Windows 8.1 the window must be null.
        unsafe { DwmGetCompositionTimingInfo(HWND::default(), &mut info) }.ok()?;
        let (mut now, mut frequency) = (0i64, 0i64);
        unsafe { QueryPerformanceCounter(&mut now) }.ok()?;
        unsafe { QueryPerformanceFrequency(&mut frequency) }.ok()?;
        let frequency = u128::try_from(frequency).ok().filter(|&frequency| frequency > 0)?;
        let duration = |ticks: i64| {
            Duration::from_nanos((ticks.max(0) as u128 * 1_000_000_000 / frequency) as u64)
        };
        let period = i64::try_from(info.qpcRefreshPeriod).ok().filter(|&period| period > 0)?;
        let vblank = i64::try_from(info.qpcVBlank).ok()?;
        let ratio = duration(period).as_secs_f64() / interval.as_secs_f64();
        if !(0.9..1.1).contains(&ratio) {
            return None;
        }
        Some(duration(period - (now - vblank).rem_euclid(period)))
    }
}

struct WinitBasedFrameThrottle;

impl WinitBasedFrameThrottle {
    fn create() -> Box<dyn FrameThrottle> {
        Box::new(Self)
    }
}

impl FrameThrottle for WinitBasedFrameThrottle {
    fn request_throttled_redraw(&self, winit_window: &winit::window::Window) {
        winit_window.request_redraw();
    }
}
