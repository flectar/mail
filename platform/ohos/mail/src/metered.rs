//! Whether HarmonyOS's default network is metered (#253), read through
//! Network Kit's native API: mobile data, or a network the user marked as
//! metered. The call needs `ohos.permission.GET_NETWORK_INFO`, which the
//! system grants at install.
//!
//! `libnet_connection.so` is a system library; the SDK ships a link-time stub.
//! The shared app asks again every quarter minute, since Network Kit tells of
//! changes only to callbacks.

use std::sync::atomic::{AtomicI32, Ordering};

#[link(name = "net_connection", kind = "dylib")]
unsafe extern "C" {
    /// `OH_NetConn_IsDefaultNetMetered`: 0 on success, 201 without the
    /// permission.
    fn OH_NetConn_IsDefaultNetMetered(is_metered: *mut i32) -> i32;
}

/// The last error code logged, so a failing call is told of once, not at
/// every read.
static LOGGED: AtomicI32 = AtomicI32::new(0);

/// Install the metered reader. Call before `flectar_mail::run`, which starts
/// the watch.
pub(super) fn install() {
    flectar_mail::platform::configure_metered(metered);
}

/// Whether the default network is metered. A read that fails counts as not
/// metered, so downloads never wait on an answer the system couldn't give.
fn metered() -> bool {
    match default_net_metered() {
        Ok(metered) => {
            LOGGED.store(0, Ordering::Relaxed);
            metered
        }
        Err(code) => {
            if LOGGED.swap(code, Ordering::Relaxed) != code {
                tracing::warn!(
                    code,
                    "could not ask HarmonyOS whether the network is metered"
                );
            }
            false
        }
    }
}

/// Network Kit's answer, or the call's error code.
fn default_net_metered() -> Result<bool, i32> {
    let mut metered = 0;
    // SAFETY: `metered` is a live, writable `int32_t`.
    let rc = unsafe { OH_NetConn_IsDefaultNetMetered(&mut metered) };
    if rc == 0 { Ok(metered != 0) } else { Err(rc) }
}
