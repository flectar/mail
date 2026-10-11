use std::collections::HashMap;
use std::path::PathBuf;

const DESKTOP_OAUTH_ENV: [&str; 3] = [
    "FLECTAR_GOOGLE_DESKTOP_CLIENT_ID",
    "FLECTAR_GOOGLE_DESKTOP_CLIENT_SECRET",
    "FLECTAR_MICROSOFT_DESKTOP_CLIENT_ID",
];

fn main() {
    // Native packaging can supply an existing stable file when the optional
    // workspace .env is absent. The host reselects this path for each build,
    // so adding/removing .env changes the tracked override rather than making
    // Cargo watch a nonexistent file forever.
    println!("cargo:rerun-if-env-changed=FLECTAR_OAUTH_BUILD_ENV_FILE");
    let workspace_env = std::env::var_os("FLECTAR_OAUTH_BUILD_ENV_FILE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.env"));
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
}
