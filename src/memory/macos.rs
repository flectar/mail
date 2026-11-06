//! macOS reports memory pressure through a libdispatch source.

use super::{
    DISPATCH_CRITICAL, DISPATCH_NORMAL, DISPATCH_WARN, dispatch_pressure, memory_pressure,
};
use std::ffi::c_void;

unsafe extern "C" {
    // DISPATCH_SOURCE_TYPE_MEMORYPRESSURE is the address of this symbol.
    static _dispatch_source_type_memorypressure: u8;
    fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut c_void;
    fn dispatch_source_create(
        kind: *const c_void,
        handle: usize,
        mask: usize,
        queue: *mut c_void,
    ) -> *mut c_void;
    fn dispatch_set_context(object: *mut c_void, context: *mut c_void);
    fn dispatch_source_set_event_handler_f(
        source: *mut c_void,
        handler: extern "C" fn(*mut c_void),
    );
    fn dispatch_source_get_data(source: *mut c_void) -> usize;
    fn dispatch_resume(object: *mut c_void);
}

/// Runs on a dispatch queue with the source as its context.
extern "C" fn pressure_changed(source: *mut c_void) {
    // SAFETY: the context is the live source this handler is installed on.
    let flags = unsafe { dispatch_source_get_data(source) };
    if let Some(level) = dispatch_pressure(flags) {
        memory_pressure(level);
    }
}

pub(super) fn watch() {
    // SAFETY: plain libdispatch calls. The source keeps running for the life
    // of the process, so it is never released.
    unsafe {
        let queue = dispatch_get_global_queue(0, 0);
        let source = dispatch_source_create(
            (&raw const _dispatch_source_type_memorypressure).cast(),
            0,
            DISPATCH_NORMAL | DISPATCH_WARN | DISPATCH_CRITICAL,
            queue,
        );
        if source.is_null() {
            return;
        }
        dispatch_set_context(source, source);
        dispatch_source_set_event_handler_f(source, pressure_changed);
        dispatch_resume(source);
    }
}
