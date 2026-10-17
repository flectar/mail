//! OHOS M2 demo: a tiny Slint UI loaded as a NAPI/XComponent native module.
//!
//! The ArkTS page loads `libslint_ohos_demo.so` through
//! `XComponent({ ..., libraryname: 'slint_ohos_demo' })`. The module registry
//! calls [`module_init`] on the OHOS main thread; from there the backend spawns
//! the Slint thread and the XComponent delivers surface and touch events.

#![allow(non_camel_case_types)]

use core::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use slint_ohos_backend::ffi::{self, napi_env, napi_value};

slint::include_modules!();

/// NAPI module name; must match the XComponent `libraryname` in the ArkTS page
/// (the loader resolves it to `libslint_ohos_demo.so`).
const MODULE_NAME: &[u8] = b"slint_ohos_demo\0";

static INITIALIZED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn module_init(env: napi_env, exports: napi_value) -> napi_value {
    if INITIALIZED.swap(true, Ordering::SeqCst) {
        return exports;
    }
    slint_ohos_backend::ohos_info!("demo: NAPI module init (XComponent load)");
    // Start the Slint thread first so the event channel is installed before any
    // surface callback can fire, then register the XComponent callbacks.
    if let Err(err) = slint_ohos_backend::start(run_demo) {
        slint_ohos_backend::ohos_error!("demo: failed to start Slint thread: {err}");
        return exports;
    }

    // SAFETY: `env`/`exports` are the NAPI module-init arguments; the property
    // and external lookups follow the OHOS XComponent module contract.
    unsafe {
        let mut component_obj: napi_value = core::ptr::null_mut();
        let rc = ffi::napi_get_named_property(
            env,
            exports,
            ffi::OH_NATIVE_XCOMPONENT_OBJ.as_ptr().cast(),
            &mut component_obj,
        );
        if rc != ffi::NAPI_OK {
            slint_ohos_backend::ohos_error!(
                "demo: XComponent object missing on module init ({rc})"
            );
            return exports;
        }
        let mut component: *mut c_void = core::ptr::null_mut();
        // The XComponent instance is an ArkTS object wrapping the native
        // pointer; `napi_unwrap` extracts it (the documented pattern for the
        // `libraryname` module path on current OHOS).
        let rc = ffi::napi_unwrap(env, component_obj, &mut component);
        if rc != ffi::NAPI_OK || component.is_null() {
            slint_ohos_backend::ohos_error!("demo: XComponent pointer unavailable ({rc})");
            return exports;
        }
        // SAFETY: `component` came from unwrapping the XComponent instance
        // handed to this module at load time; the loader keeps it valid for
        // the process lifetime.
        let rc = slint_ohos_backend::register_xcomponent(component);
        if rc != 0 {
            slint_ohos_backend::ohos_error!("demo: RegisterCallback failed: {rc}");
        }
    }
    exports
}

/// Runs on the Slint thread spawned by the backend.
fn run_demo() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ui = Demo::new()?;
    ui.run()?;
    Ok(())
}

// SAFETY: the NAPI runtime reads this once from the module registry; the
// `static mut` is only handed to `napi_module_register` by address and never
// mutated afterwards.
static mut MODULE: ffi::NapiModule = ffi::NapiModule {
    nm_version: 1,
    nm_flags: 0,
    nm_filename: core::ptr::null(),
    nm_register_func: Some(module_init),
    nm_modname: MODULE_NAME.as_ptr().cast(),
    nm_priv: core::ptr::null_mut(),
    reserved: [core::ptr::null_mut(); 4],
};

extern "C" fn register_module() {
    // SAFETY: `addr_of_mut!` avoids creating a reference to the mutable static.
    unsafe { ffi::napi_module_register(core::ptr::addr_of_mut!(MODULE)) };
}

/// musl calls `.init_array` entries when the library is loaded (the standard
/// NAPI module registration pattern). The XComponent load path relies on it.
#[used]
#[unsafe(link_section = ".init_array")]
static REGISTER_MODULE: extern "C" fn() = register_module;
