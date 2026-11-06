//! Hand freed memory back to the operating system.
//!
//! Allocators keep freed pages for later allocations, so resident memory stays
//! at the peak of the last burst of work. The app returns those pages only at
//! explicit idle boundaries (a finished sync, leaving a product, the window
//! going to the tray), never on a periodic timer.

/// Return already-free allocator pages to the operating system. Live
/// allocations are untouched, so this is safe while the window is visible.
pub(crate) fn release_free_heap() {
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
