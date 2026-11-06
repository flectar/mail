//! Hand freed memory back to the operating system.
//!
//! Allocators keep freed pages for later allocations, so resident memory stays
//! at the peak of the last burst of work. The app returns those pages only at
//! explicit idle boundaries (a finished sync, leaving a product, the window
//! going to the tray) and when the operating system reports memory pressure,
//! never on a periodic timer.

use std::{
    cell::RefCell,
    sync::{Mutex, PoisonError},
    time::{Duration, Instant},
};

#[cfg(linux_desktop)]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

/// How hard the operating system asks for memory back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum MemoryPressure {
    /// Free what is cheap to rebuild: caches and pixels that are off screen.
    Moderate,
    /// Free everything that isn't on screen, and purge the allocator as far
    /// as it goes.
    Critical,
}

/// Repeats at the same level within this time are merged. Systems repeat a
/// signal while pressure lasts (low-memory-monitor and GLib every 15 s), and
/// some report several levels for one event.
const REPEAT_INTERVAL: Duration = Duration::from_secs(15);

/// How long overscan stays off after a signal. Linux, Android and iOS never
/// report that pressure is over.
pub(crate) const PRESSURE_WINDOW: Duration = Duration::from_secs(20);

static LAST_SIGNAL: Mutex<Option<(Instant, MemoryPressure)>> = Mutex::new(None);

/// What the UI thread frees under pressure.
type PressureRelease = Box<dyn Fn(MemoryPressure)>;

thread_local! {
    static RELEASE: RefCell<Option<PressureRelease>> = const { RefCell::new(None) };
}

/// Handle a signal unless the same or a stronger one was handled recently.
fn should_handle(
    last: Option<(Instant, MemoryPressure)>,
    now: Instant,
    level: MemoryPressure,
) -> bool {
    last.is_none_or(|(at, previous)| {
        level > previous || now.saturating_duration_since(at) >= REPEAT_INTERVAL
    })
}

/// Register, on the UI thread, how the app frees memory under pressure.
pub(crate) fn on_memory_pressure(release: impl Fn(MemoryPressure) + 'static) {
    RELEASE.with_borrow_mut(|current| *current = Some(Box::new(release)));
}

/// Report memory pressure from the operating system. Callable from any
/// thread; the work runs on the UI thread and the runtime, so platform
/// callbacks return at once.
pub(crate) fn memory_pressure(level: MemoryPressure) {
    {
        let mut last = LAST_SIGNAL.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if !should_handle(*last, now, level) {
            return;
        }
        *last = Some((now, level));
    }
    let _ = slint::invoke_from_event_loop(move || {
        RELEASE.with_borrow(|release| {
            if let Some(release) = release {
                release(level);
            }
        });
    });
}

/// libdispatch's `DISPATCH_MEMORYPRESSURE_*` flags.
#[cfg(any(test, target_os = "macos"))]
const DISPATCH_NORMAL: usize = 0x01;
#[cfg(any(test, target_os = "macos"))]
const DISPATCH_WARN: usize = 0x02;
#[cfg(any(test, target_os = "macos"))]
const DISPATCH_CRITICAL: usize = 0x04;

/// libdispatch merges the flags of events it hasn't delivered yet, so the
/// strongest level present wins. `NORMAL` alone means pressure is over.
#[cfg(any(test, target_os = "macos"))]
fn dispatch_pressure(flags: usize) -> Option<MemoryPressure> {
    if flags & DISPATCH_CRITICAL != 0 {
        Some(MemoryPressure::Critical)
    } else if flags & DISPATCH_WARN != 0 {
        Some(MemoryPressure::Moderate)
    } else {
        None
    }
}

/// Start listening to the system's memory-pressure signal. Android and
/// OpenHarmony forward theirs from the activity or ability as they arrive.
pub(crate) fn watch_pressure(runtime: &tokio::runtime::Handle) {
    #[cfg(linux_desktop)]
    linux::watch(runtime);
    #[cfg(not(linux_desktop))]
    let _ = runtime;
    #[cfg(target_os = "macos")]
    macos::watch();
    #[cfg(target_os = "windows")]
    windows::watch();
    #[cfg(mobile)]
    crate::platform::watch_memory_pressure();
}

/// Return already-free allocator pages to the operating system. Live
/// allocations are untouched, so this is safe while the window is visible.
pub(crate) fn release_free_heap() {
    release_heap(MemoryPressure::Moderate);
}

/// Return free allocator pages, as far as `level` asks.
pub(crate) fn release_heap(level: MemoryPressure) {
    #[cfg(mobile)]
    crate::platform::purge_allocator(level == MemoryPressure::Critical);
    #[cfg(not(mobile))]
    let _ = level;
    #[cfg(glibc)]
    // SAFETY: malloc_trim only releases free heap pages; no pointers change.
    unsafe {
        libc::malloc_trim(0);
    }
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn malloc_zone_pressure_relief(zone: *mut std::ffi::c_void, goal: usize) -> usize;
        }
        // SAFETY: a null zone asks every malloc zone to release free pages.
        unsafe {
            malloc_zone_pressure_relief(std::ptr::null_mut(), 0);
        }
    }
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::System::{
            Memory::{HeapOptimizeResources, HeapSetInformation},
            SystemServices::{
                HEAP_OPTIMIZE_RESOURCES_CURRENT_VERSION, HEAP_OPTIMIZE_RESOURCES_INFORMATION,
            },
        };
        let information = HEAP_OPTIMIZE_RESOURCES_INFORMATION {
            Version: HEAP_OPTIMIZE_RESOURCES_CURRENT_VERSION,
            Flags: 0,
        };
        // SAFETY: a null heap handle asks every heap of this process to
        // decommit its free cached memory. The structure outlives the call
        // and its size is passed with it.
        unsafe {
            HeapSetInformation(
                std::ptr::null_mut(),
                HeapOptimizeResources,
                (&raw const information).cast(),
                size_of::<HEAP_OPTIMIZE_RESOURCES_INFORMATION>(),
            );
        }
    }
}

/// Move this process's pages out of its working set, which is what Task
/// Manager reports. Only for a window that is hidden or stays minimized:
/// pages in use fault back in on the next interaction.
#[cfg(target_os = "windows")]
pub(crate) fn trim_working_set() {
    use windows_sys::Win32::System::{
        Memory::SetProcessWorkingSetSizeEx, Threading::GetCurrentProcess,
    };
    // SAFETY: this acts on the current process's own working set only.
    // Trimmed pages go to the standby list and fault back cheaply.
    unsafe {
        SetProcessWorkingSetSizeEx(GetCurrentProcess(), usize::MAX, usize::MAX, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DISPATCH_CRITICAL, DISPATCH_NORMAL, DISPATCH_WARN, MemoryPressure, REPEAT_INTERVAL,
        dispatch_pressure, should_handle,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn repeats_are_merged_unless_stronger() {
        let start = Instant::now();
        let moderate = Some((start, MemoryPressure::Moderate));
        assert!(should_handle(None, start, MemoryPressure::Moderate));
        assert!(!should_handle(
            moderate,
            start + Duration::from_secs(1),
            MemoryPressure::Moderate
        ));
        assert!(should_handle(
            moderate,
            start + Duration::from_secs(1),
            MemoryPressure::Critical
        ));
        assert!(should_handle(
            moderate,
            start + REPEAT_INTERVAL,
            MemoryPressure::Moderate
        ));
        let critical = Some((start, MemoryPressure::Critical));
        assert!(!should_handle(
            critical,
            start + Duration::from_secs(5),
            MemoryPressure::Moderate
        ));
        assert!(should_handle(
            critical,
            start + REPEAT_INTERVAL,
            MemoryPressure::Moderate
        ));
    }

    #[test]
    fn dispatch_flags_take_the_strongest_level() {
        assert_eq!(dispatch_pressure(DISPATCH_NORMAL), None);
        assert_eq!(
            dispatch_pressure(DISPATCH_WARN),
            Some(MemoryPressure::Moderate)
        );
        assert_eq!(
            dispatch_pressure(DISPATCH_WARN | DISPATCH_NORMAL),
            Some(MemoryPressure::Moderate)
        );
        assert_eq!(
            dispatch_pressure(DISPATCH_CRITICAL | DISPATCH_WARN),
            Some(MemoryPressure::Critical)
        );
    }
}
