//! Shared HTTPS configuration for sandboxed native hosts.
//! HarmonyOS has no desktop certificate discovery paths. Its HTTP clients use
//! the same Mozilla trust roots and explicit ring provider as IMAP/SMTP.

/// Start a client builder, retaining platform trust behavior on desktop and
/// supplying a complete, verified TLS configuration on HarmonyOS.
pub fn client_builder() -> reqwest::ClientBuilder {
    let builder = reqwest::Client::builder();
    #[cfg(target_env = "ohos")]
    {
        static CONFIG: std::sync::LazyLock<rustls::ClientConfig> = std::sync::LazyLock::new(|| {
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("ring supports the standard TLS protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth()
        });
        builder.tls_backend_preconfigured(CONFIG.clone())
    }
    #[cfg(not(target_env = "ohos"))]
    {
        builder
    }
}

/// Equivalent to reqwest::Client::new(), with the native-host TLS roots.
pub fn client() -> reqwest::Client {
    client_builder()
        .build()
        .expect("default HTTPS client configuration is valid")
}
