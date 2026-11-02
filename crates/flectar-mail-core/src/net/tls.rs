//! TLS trust for mail connections: the system roots, certificates a user
//! imported for one account, and the development switch that disables
//! verification.

use crate::error::{CoreError, Result};
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

/// Dev/self-hosted escape hatch: FLECTAR_MAIL_TLS_INSECURE=1 disables certificate
/// verification (e.g. self-signed Dovecot). Off by default; never set it for
/// real accounts.
pub fn insecure() -> bool {
    std::env::var("FLECTAR_MAIL_TLS_INSECURE")
        .map(|v| v == "1")
        .unwrap_or(false)
}

#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Verify normal PKI chains first, then allow an imported certificate to act
/// as an explicitly pinned endpoint when it is the exact CA-marked certificate
/// presented by Proton Bridge. The narrow fallback preserves expiry and name
/// validation while the exact DER match supplies the endpoint identity check.
#[derive(Debug)]
struct ImportedCertificateVerifier {
    standard: Arc<rustls::client::WebPkiServerVerifier>,
    imported: Vec<rustls::pki_types::CertificateDer<'static>>,
}

impl ImportedCertificateVerifier {
    fn is_ca_used_as_end_entity(error: &rustls::Error) -> bool {
        let rustls::Error::InvalidCertificate(rustls::CertificateError::Other(other)) = error
        else {
            return false;
        };
        other
            .0
            .downcast_ref::<webpki::Error>()
            .is_some_and(|error| matches!(error, webpki::Error::CaUsedAsEndEntity))
    }
}

impl rustls::client::danger::ServerCertVerifier for ImportedCertificateVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        server_name: &rustls::pki_types::ServerName<'_>,
        ocsp_response: &[u8],
        now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        match self.standard.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(verified) => Ok(verified),
            Err(error)
                if Self::is_ca_used_as_end_entity(&error)
                    && self
                        .imported
                        .iter()
                        .any(|certificate| certificate.as_ref() == end_entity.as_ref()) =>
            {
                // WebPKI checked validity before reporting CaUsedAsEndEntity.
                // Check the name separately because chain validation failed
                // before the standard verifier reached its name check. rustls
                // verifies proof of possession in verify_tls{12,13}_signature.
                let certificate = rustls::server::ParsedCertificate::try_from(end_entity)?;
                rustls::client::verify_server_name(&certificate, server_name)?;
                Ok(rustls::client::danger::ServerCertVerified::assertion())
            }
            Err(error) => Err(error),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.standard.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.standard.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.standard.supported_verify_schemes()
    }
}

fn system_connector() -> TlsConnector {
    static CONFIG: once_cell::sync::Lazy<Arc<rustls::ClientConfig>> =
        once_cell::sync::Lazy::new(|| {
            // Multiple rustls backends may be enabled via the dependency
            // graph; pick ring explicitly so there's always a default.
            let _ = rustls::crypto::ring::default_provider().install_default();
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let mut config = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            if insecure() {
                tracing::warn!(
                    "FLECTAR_MAIL_TLS_INSECURE=1: TLS certificate verification is DISABLED"
                );
                let provider = rustls::crypto::CryptoProvider::get_default()
                    .cloned()
                    .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));
                config
                    .dangerous()
                    .set_certificate_verifier(Arc::new(NoVerify(provider)));
            }
            Arc::new(config)
        });
    TlsConnector::from(CONFIG.clone())
}

/// Parse a bounded PEM bundle and reject private keys or non-certificate data.
pub fn trusted_certificates(pem: &str) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>> {
    use rustls::pki_types::pem::PemObject;
    if pem.is_empty() {
        return Ok(Vec::new());
    }
    if pem.len() > 256 * 1024 || pem.contains("PRIVATE KEY") {
        return Err(CoreError::Tls(
            "Import a public PEM certificate, not a private key (maximum 256 KiB).".into(),
        ));
    }
    let certificates = rustls::pki_types::CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| CoreError::Tls("The imported file is not a valid PEM certificate.".into()))?;
    if certificates.is_empty() {
        return Err(CoreError::Tls(
            "No certificates found in the PEM file.".into(),
        ));
    }
    let mut roots = rustls::RootCertStore::empty();
    for cert in &certificates {
        roots
            .add(cert.clone())
            .map_err(|_| CoreError::Tls("The imported certificate is invalid.".into()))?;
    }
    Ok(certificates)
}

/// The TLS client of an account: the system roots, plus any certificates the
/// user imported for this account.
pub fn connector(pem: &str) -> Result<TlsConnector> {
    if pem.is_empty() {
        return Ok(system_connector());
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let imported = trusted_certificates(pem)?;
    for cert in &imported {
        roots
            .add(cert.clone())
            .map_err(|e| CoreError::Tls(e.to_string()))?;
    }
    let verifier = rustls::client::WebPkiServerVerifier::builder_with_provider(
        Arc::new(roots),
        provider.clone(),
    )
    .build()
    .map_err(|e| CoreError::Tls(e.to_string()))?;
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| CoreError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(ImportedCertificateVerifier {
            standard: verifier,
            imported,
        }))
        .with_no_client_auth();
    Ok(TlsConnector::from(Arc::new(config)))
}

/// Complete a TLS handshake on `tcp`, verifying the certificate for `host`.
pub async fn handshake(
    connector: &TlsConnector,
    tcp: TcpStream,
    host: &str,
) -> std::io::Result<TlsStream<TcpStream>> {
    let server_name = rustls::pki_types::ServerName::try_from(host.to_owned()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("invalid server name: {host}"),
        )
    })?;
    connector.connect(server_name, tcp).await
}
