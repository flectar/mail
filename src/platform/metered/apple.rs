//! A metered connection on macOS and iOS (#253): Network.framework's path
//! monitor, which counts a path as expensive on cellular data or a phone's
//! hotspot, and as constrained in Low Data Mode. Either one is metered, as
//! Apple's own downloads treat them.

use block2::RcBlock;
use std::ffi::{c_char, c_void};

type PathMonitor = *mut c_void;
type Path = *mut c_void;
type Queue = *mut c_void;

#[link(name = "Network", kind = "framework")]
unsafe extern "C" {
    fn nw_path_monitor_create() -> PathMonitor;
    fn nw_path_monitor_set_queue(monitor: PathMonitor, queue: Queue);
    fn nw_path_monitor_set_update_handler(
        monitor: PathMonitor,
        handler: &block2::Block<dyn Fn(Path)>,
    );
    fn nw_path_monitor_start(monitor: PathMonitor);
    fn nw_path_is_expensive(path: Path) -> bool;
    fn nw_path_is_constrained(path: Path) -> bool;
}

unsafe extern "C" {
    fn dispatch_queue_create(label: *const c_char, attributes: *mut c_void) -> Queue;
}

pub(super) fn watch(changed: impl Fn(bool) + Send + Sync + 'static) {
    // SAFETY: Network.framework and libdispatch are thread safe; the monitor
    // copies the handler block, and the monitor and its queue are kept for
    // the life of the app, as watching never stops.
    unsafe {
        let monitor = nw_path_monitor_create();
        if monitor.is_null() {
            return;
        }
        let queue =
            dispatch_queue_create(c"com.flectar.mail.metered".as_ptr(), std::ptr::null_mut());
        let handler = RcBlock::new(move |path: Path| {
            changed(nw_path_is_expensive(path) || nw_path_is_constrained(path));
        });
        nw_path_monitor_set_update_handler(monitor, &handler);
        nw_path_monitor_set_queue(monitor, queue);
        nw_path_monitor_start(monitor);
    }
}
