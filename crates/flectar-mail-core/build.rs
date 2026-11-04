use cfg_aliases::cfg_aliases;
use std::collections::HashMap;
use std::path::PathBuf;

const DESKTOP_OAUTH_ENV: [&str; 3] = [
    "FLECTAR_GOOGLE_DESKTOP_CLIENT_ID",
    "FLECTAR_GOOGLE_DESKTOP_CLIENT_SECRET",
    "FLECTAR_MICROSOFT_DESKTOP_CLIENT_ID",
];

/// Mobile OAuth registrations are client types of one platform, so each
/// platform bundles its own: the name the core reads, and the build
/// variable that supplies it on Android (also used by OpenHarmony) and on
/// iOS.
const MOBILE_OAUTH_ENV: [(&str, &str, &str); 2] = [
    (
        "FLECTAR_MOBILE_GOOGLE_CLIENT_ID",
        "FLECTAR_GOOGLE_ANDROID_CLIENT_ID",
        "FLECTAR_GOOGLE_IOS_CLIENT_ID",
    ),
    (
        "FLECTAR_MOBILE_MICROSOFT_CLIENT_ID",
        "FLECTAR_MICROSOFT_ANDROID_CLIENT_ID",
        "FLECTAR_MICROSOFT_IOS_CLIENT_ID",
    ),
];

/// The build variable that supplies `name` when building for `target_os`.
fn mobile_oauth_source(target_os: &str, name: &str) -> Option<&'static str> {
    let &(_, android, ios) = MOBILE_OAUTH_ENV.iter().find(|(core, ..)| *core == name)?;
    Some(if target_os == "ios" { ios } else { android })
}

fn main() {
    // Keep `mobile` identical to the application's build.rs; see
    // platform/README.md.
    cfg_aliases! {
        mobile: { any(target_os = "android", target_os = "ios", target_env = "ohos") },
        desktop: { not(mobile) },
        // rustix exposes renameat2/renameatx_np with RENAME_NOREPLACE here.
        rename_noreplace: { any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios") },
        // keyring stores secrets through the freedesktop Secret Service.
        // OpenHarmony reports target_os = "linux" but has no Secret Service.
        secret_service: { all(desktop, any(target_os = "linux", target_os = "freebsd", target_os = "openbsd"), not(target_env = "ohos")) },
    }

    let workspace_env = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.env");
    println!("cargo:rerun-if-changed={}", workspace_env.display());

    let file_values = std::fs::read_to_string(&workspace_env)
        .ok()
        .map(|contents| parse_env(&contents))
        .unwrap_or_default();

    for key in DESKTOP_OAUTH_ENV {
        println!("cargo:rerun-if-env-changed={key}");
        let value = std::env::var(key)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| file_values.get(key).cloned());
        if let Some(value) = value {
            // Make the selected value available to option_env! in the core so
            // packaged binaries retain their desktop OAuth registration.
            println!("cargo:rustc-env={key}={value}");
        }
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    for (name, ..) in MOBILE_OAUTH_ENV {
        let Some(source) = mobile_oauth_source(&target_os, name) else {
            continue;
        };
        println!("cargo:rerun-if-env-changed={source}");
        // Error messages name the variable to set for this platform.
        println!("cargo:rustc-env={name}_SOURCE={source}");
        if let Some(value) = std::env::var(source)
            .ok()
            .filter(|value| !value.trim().is_empty())
        {
            println!("cargo:rustc-env={name}={value}");
        }
    }
}

fn parse_env(contents: &str) -> HashMap<String, String> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            if !DESKTOP_OAUTH_ENV.contains(&key) {
                return None;
            }
            let value = value.trim();
            let value = if value.len() >= 2
                && ((value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\'')))
            {
                &value[1..value.len() - 1]
            } else {
                value
            };
            (!value.is_empty()).then(|| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_parser_only_accepts_supported_non_empty_values() {
        let values = parse_env(
            r#"
                # comment
                export FLECTAR_GOOGLE_DESKTOP_CLIENT_ID="google-id"
                FLECTAR_GOOGLE_DESKTOP_CLIENT_SECRET='google-secret'
                FLECTAR_MICROSOFT_DESKTOP_CLIENT_ID=ms-id
                UNRELATED=value
            "#,
        );
        assert_eq!(values["FLECTAR_GOOGLE_DESKTOP_CLIENT_ID"], "google-id");
        assert_eq!(
            values["FLECTAR_GOOGLE_DESKTOP_CLIENT_SECRET"],
            "google-secret"
        );
        assert_eq!(values["FLECTAR_MICROSOFT_DESKTOP_CLIENT_ID"], "ms-id");
        assert!(!values.contains_key("UNRELATED"));
    }

    #[test]
    fn mobile_builds_bundle_their_own_platform_registrations() {
        for (target_os, google, microsoft) in [
            (
                "ios",
                "FLECTAR_GOOGLE_IOS_CLIENT_ID",
                "FLECTAR_MICROSOFT_IOS_CLIENT_ID",
            ),
            (
                "android",
                "FLECTAR_GOOGLE_ANDROID_CLIENT_ID",
                "FLECTAR_MICROSOFT_ANDROID_CLIENT_ID",
            ),
            // OpenHarmony reports target_os = "linux".
            (
                "linux",
                "FLECTAR_GOOGLE_ANDROID_CLIENT_ID",
                "FLECTAR_MICROSOFT_ANDROID_CLIENT_ID",
            ),
        ] {
            assert_eq!(
                mobile_oauth_source(target_os, "FLECTAR_MOBILE_GOOGLE_CLIENT_ID"),
                Some(google)
            );
            assert_eq!(
                mobile_oauth_source(target_os, "FLECTAR_MOBILE_MICROSOFT_CLIENT_ID"),
                Some(microsoft)
            );
        }
        assert_eq!(mobile_oauth_source("ios", "UNRELATED"), None);
    }
}
