//! Windows signals low physical memory through a memory resource notification
//! object, which stays signaled while memory is low.

use super::{MemoryPressure, REPEAT_INTERVAL, memory_pressure};
use windows_sys::Win32::{
    Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT},
    System::{
        Memory::{
            CreateMemoryResourceNotification, HighMemoryResourceNotification,
            LowMemoryResourceNotification,
        },
        Threading::{INFINITE, WaitForSingleObject},
    },
};

pub(super) fn watch() {
    // SAFETY: creating the notification objects has no preconditions. The
    // handles stay open for the life of the process.
    let (low, high) = unsafe {
        (
            CreateMemoryResourceNotification(LowMemoryResourceNotification),
            CreateMemoryResourceNotification(HighMemoryResourceNotification),
        )
    };
    if low.is_null() || high.is_null() {
        return;
    }
    // Raw handles aren't Send; the thread only waits on them.
    let (low, high) = (low as usize, high as usize);
    let _ = std::thread::Builder::new()
        .name("memory-pressure".into())
        .spawn(move || watch_episodes(low as HANDLE, high as HANDLE));
}

/// Report each low-memory episode once, and again every few seconds while it
/// lasts. Waiting for memory to be plentiful before watching for the next
/// episode keeps a level that hovers near the threshold from repeating.
fn watch_episodes(low: HANDLE, high: HANDLE) {
    let repeat = REPEAT_INTERVAL.as_millis() as u32;
    // SAFETY: both handles stay valid for the life of the process.
    unsafe {
        while WaitForSingleObject(low, INFINITE) == WAIT_OBJECT_0 {
            loop {
                memory_pressure(MemoryPressure::Critical);
                match WaitForSingleObject(high, repeat) {
                    WAIT_OBJECT_0 => break,
                    WAIT_TIMEOUT if WaitForSingleObject(low, 0) == WAIT_OBJECT_0 => {}
                    WAIT_TIMEOUT => {
                        if WaitForSingleObject(high, INFINITE) != WAIT_OBJECT_0 {
                            return;
                        }
                        break;
                    }
                    _ => return,
                }
            }
        }
    }
}
