//! Mail connections trust the operating system's certificate store, as
//! antivirus software and company networks that inspect encrypted
//! connections rely on (issue #166).
//!
//! On Linux and the BSDs the system store honours SSL_CERT_FILE. The verifier
//! reads the store once per process, so each case runs in a child process
//! with its own store.
#![cfg(all(desktop, unix, not(target_vendor = "apple")))]

use flectar_mail_core::net;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const SELF_SIGNED: &str = include_str!("fixtures/tls/server.pem");
const INSPECTED: &str = include_str!("fixtures/tls/inspected-server.pem");
const PROTON_BRIDGE_CERT: &str = include_str!("fixtures/tls/proton-bridge.pem");
const KEY: &str = include_str!("fixtures/tls/server-key.pem");
const CASE: &str = "FLECTAR_SYSTEM_TRUST_CASE";

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tls")
        .join(name)
}

/// Run `case` in a child process whose system store is `store`.
fn run_with_store(case: &str, store: PathBuf) {
    let output = Command::new(std::env::current_exe().unwrap())
        .args([case, "--exact", "--nocapture", "--test-threads=1"])
        .env(CASE, case)
        .env("SSL_CERT_FILE", store)
        .env_remove("SSL_CERT_DIR")
        .env_remove("FLECTAR_MAIL_TLS_INSECURE")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{case} failed:\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Whether this process is the child running `case`.
fn is_child(case: &str) -> bool {
    std::env::var(CASE).is_ok_and(|value| value == case)
}

/// Serve one TLS connection with `certificate`, then connect to it through
/// the account TLS client with `imported` certificates.
async fn handshake(certificate: &str, imported: &str) -> std::io::Result<()> {
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![CertificateDer::from_pem_slice(certificate.as_bytes()).unwrap()],
        PrivateKeyDer::from_pem_slice(KEY.as_bytes()).unwrap(),
    )
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        if let Ok(mut tls) = acceptor.accept(tcp).await {
            let _ = tls.write_all(b"* OK ready\r\n").await;
            let _ = tls.shutdown().await;
        }
    });
    let connector = net::tls::connector(imported).unwrap();
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let result = match net::tls::handshake(&connector, tcp, "localhost").await {
        Ok(mut tls) => {
            let mut greeting = String::new();
            tls.read_to_string(&mut greeting).await.unwrap();
            assert_eq!(greeting, "* OK ready\r\n");
            Ok(())
        }
        Err(error) => Err(error),
    };
    server.await.unwrap();
    result
}

#[test]
fn system_trusted_inspection_ca_is_accepted() {
    let case = "system_trusted_inspection_ca_is_accepted";
    if !is_child(case) {
        return run_with_store(case, fixture("inspection-ca.pem"));
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        handshake(INSPECTED, "").await.unwrap();
    });
}

#[test]
fn certificates_the_system_does_not_trust_are_rejected_with_advice() {
    let case = "certificates_the_system_does_not_trust_are_rejected_with_advice";
    if !is_child(case) {
        return run_with_store(case, fixture("inspection-ca.pem"));
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let error = handshake(SELF_SIGNED, "").await.unwrap_err();
        assert!(error.to_string().contains("UnknownIssuer"), "{error}");
        let advice = net::tls::handshake_advice(&error);
        assert!(advice.contains("antivirus"), "{advice}");
    });
}

#[test]
fn imported_certificates_add_to_the_system_store() {
    let case = "imported_certificates_add_to_the_system_store";
    if !is_child(case) {
        return run_with_store(case, fixture("inspection-ca.pem"));
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        // An account that imported another certificate keeps the system's trust.
        handshake(INSPECTED, PROTON_BRIDGE_CERT).await.unwrap();
        // The imported certificate is still trusted alongside it.
        handshake(SELF_SIGNED, SELF_SIGNED).await.unwrap();
    });
}

#[test]
fn missing_system_store_falls_back_to_bundled_roots() {
    let case = "missing_system_store_falls_back_to_bundled_roots";
    if !is_child(case) {
        return run_with_store(case, fixture("does-not-exist.pem"));
    }
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        // Mozilla's roots do not include the test CA, so the chain is
        // unknown rather than the verifier failing to start.
        let error = handshake(INSPECTED, "").await.unwrap_err();
        assert!(error.to_string().contains("UnknownIssuer"), "{error}");
    });
}
