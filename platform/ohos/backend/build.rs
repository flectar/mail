//! Supplies the OHOS native library search path for the `#[link]` attributes in
//! `ffi.rs` (libace_napi.z.so, libace_ndk.z.so, libnative_window.so,
//! libhilog_ndk.z.so, ...).
//!
//! The SDK location is discovered from the environment so the repository stays
//! free of machine-specific absolute paths. Library locations are only needed
//! for the link step; at run time the app linker namespace resolves the same
//! sonames from the system partitions.

fn main() {
    let mut candidates = Vec::new();
    if let Ok(sdk_native) = std::env::var("OHOS_SDK_NATIVE") {
        candidates.push(format!("{sdk_native}/sysroot/usr/lib/aarch64-linux-ohos"));
    }
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(format!(
            "{home}/.harmonybrew/opt/ohos-sdk-native/sysroot/usr/lib/aarch64-linux-ohos"
        ));
        // The versioned Cellar path as a fallback for hosts without `opt` links.
        candidates.push(format!(
            "{home}/.harmonybrew/Cellar/ohos-sdk-native/26.0.0.18/sysroot/usr/lib/aarch64-linux-ohos"
        ));
    }

    for candidate in candidates {
        if std::path::Path::new(&candidate).is_dir() {
            println!("cargo:rustc-link-search=native={candidate}");
        }
    }

    println!("cargo:rerun-if-env-changed=OHOS_SDK_NATIVE");
}
