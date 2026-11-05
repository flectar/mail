//! TLS trust for mail connections: the operating system's certificate
//! verifier, certificates a user imported for one account, and the
//! development switch that disables verification.

use crate::error::{CoreError, Result};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use std::sync::{Arc, LazyLock};
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

/// The crypto provider of every mail connection. Several rustls backends can
/// be enabled through the dependency graph, so name one explicitly.
fn provider() -> Arc<CryptoProvider> {
    static PROVIDER: LazyLock<Arc<CryptoProvider>> = LazyLock::new(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Arc::new(rustls::crypto::ring::default_provider())
    });
    PROVIDER.clone()
}

/// The operating system's certificate verifier, which checks certificates the
/// way the system's own mail and browser apps do: Security.framework and the
/// Keychain on Apple platforms, the CryptoAPI chain engine on Windows, the
/// system TrustManager on Android, and the system CA bundle elsewhere.
/// Certificates installed by an administrator, a company network or
/// antivirus software that inspects encrypted connections are trusted as
/// they are by those apps. Built once, because on Linux it reads the CA
/// bundle from disk.
fn system_verifier() -> Arc<dyn ServerCertVerifier> {
    static VERIFIER: LazyLock<Arc<dyn ServerCertVerifier>> = LazyLock::new(|| {
        match rustls_platform_verifier::Verifier::new(provider()) {
            Ok(verifier) => Arc::new(verifier),
            Err(error) => {
                // Only a system CA bundle can be missing, on a minimal Linux
                // install without ca-certificates. Mozilla's roots keep public
                // mail servers reachable there.
                tracing::warn!(%error, "no system certificate store; trusting the bundled Mozilla roots");
                bundled_verifier()
            }
        }
    });
    VERIFIER.clone()
}

/// A verifier that trusts Mozilla's root program as bundled with the app.
fn bundled_verifier() -> Arc<dyn ServerCertVerifier> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider())
        .build()
        .expect("the bundled root store is not empty")
}

#[derive(Debug)]
struct NoVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
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
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// An account with imported certificates trusts chains that end at one of
/// its imported certificates, and everything the system trusts. Imported chains are checked by WebPKI with the imported
/// certificates as the only roots, so expiry and server names are still
/// verified.
///
/// Proton Bridge presents a self-signed certificate marked as a CA, which
/// WebPKI refuses as an end-entity certificate. That one case is accepted when
/// the presented certificate is byte-for-byte an imported one: the exact match
/// supplies the endpoint identity, and the name is checked separately.
#[derive(Debug)]
struct AccountVerifier {
    system: Arc<dyn ServerCertVerifier>,
    imported_roots: Arc<WebPkiServerVerifier>,
    imported: Vec<CertificateDer<'static>>,
}

impl AccountVerifier {
    fn is_ca_used_as_end_entity(error: &rustls::Error) -> bool {
        let rustls::Error::InvalidCertificate(CertificateError::Other(other)) = error else {
            return false;
        };
        other
            .0
            .downcast_ref::<webpki::Error>()
            .is_some_and(|error| matches!(error, webpki::Error::CaUsedAsEndEntity))
    }
}

impl ServerCertVerifier for AccountVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let imported_error = match self.imported_roots.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(verified) => return Ok(verified),
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
                return Ok(ServerCertVerified::assertion());
            }
            Err(error) => error,
        };
        self.system
            .verify_server_cert(end_entity, intermediates, server_name, ocsp_response, now)
            .map_err(|system_error| match imported_error {
                // No imported certificate is part of this chain, so the
                // system's finding is the relevant one.
                rustls::Error::InvalidCertificate(CertificateError::UnknownIssuer) => system_error,
                // The chain ends at an imported certificate but fails another
                // check, such as expiry or the server name.
                imported_error => imported_error,
            })
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.imported_roots
            .verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        self.imported_roots
            .verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.imported_roots.supported_verify_schemes()
    }
}

fn client_config(verifier: Arc<dyn ServerCertVerifier>) -> Result<Arc<rustls::ClientConfig>> {
    let config = rustls::ClientConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()
        .map_err(|e| CoreError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

fn system_connector() -> Result<TlsConnector> {
    static CONFIG: LazyLock<std::result::Result<Arc<rustls::ClientConfig>, String>> =
        LazyLock::new(|| {
            let verifier = if insecure() {
                tracing::warn!(
                    "FLECTAR_MAIL_TLS_INSECURE=1: TLS certificate verification is DISABLED"
                );
                Arc::new(NoVerify(provider()))
            } else {
                system_verifier()
            };
            client_config(verifier).map_err(|e| e.to_string())
        });
    CONFIG
        .clone()
        .map(TlsConnector::from)
        .map_err(CoreError::Tls)
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

/// The TLS client of an account: the operating system's trust, plus any
/// certificates the user imported for this account.
pub fn connector(pem: &str) -> Result<TlsConnector> {
    if pem.is_empty() {
        return system_connector();
    }
    let imported = trusted_certificates(pem)?;
    let mut roots = rustls::RootCertStore::empty();
    for cert in &imported {
        roots
            .add(cert.clone())
            .map_err(|e| CoreError::Tls(e.to_string()))?;
    }
    let imported_roots = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider())
        .build()
        .map_err(|e| CoreError::Tls(e.to_string()))?;
    let config = client_config(Arc::new(AccountVerifier {
        system: system_verifier(),
        imported_roots,
        imported,
    }))?;
    Ok(TlsConnector::from(config))
}

/// What the user can do about a failed handshake, for the end of an error
/// message.
pub fn handshake_advice(error: &std::io::Error) -> &'static str {
    let tls = error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<rustls::Error>());
    match tls {
        Some(rustls::Error::InvalidCertificate(
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. },
        )) => "Use the server name shown on its certificate.",
        Some(rustls::Error::InvalidCertificate(
            CertificateError::Expired
            | CertificateError::ExpiredContext { .. }
            | CertificateError::NotValidYet
            | CertificateError::NotValidYetContext { .. },
        )) => {
            "The server certificate is outside its validity period. Check the date and time on this device."
        }
        Some(rustls::Error::InvalidCertificate(_)) => {
            "This device does not trust the server certificate. If antivirus or network security software inspects encrypted connections, trust its certificate on this device or import it for this account. For Proton Bridge, import its exported certificate."
        }
        _ => "Check SSL/TLS versus STARTTLS for this port.",
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn failed(error: rustls::Error) -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::InvalidData, error)
    }

    fn certificate(error: CertificateError) -> std::io::Error {
        failed(rustls::Error::InvalidCertificate(error))
    }

    #[test]
    fn handshake_advice_matches_the_failure() {
        let advice = |error| handshake_advice(&error);
        assert!(advice(certificate(CertificateError::NotValidForName)).contains("server name"));
        assert!(advice(certificate(CertificateError::Expired)).contains("date and time"));
        assert!(advice(certificate(CertificateError::UnknownIssuer)).contains("antivirus"));
        // Platform verifiers report some trust failures, such as an untrusted
        // root on Apple platforms, as other certificate errors.
        let untrusted = CertificateError::Other(rustls::OtherError(Arc::new(
            std::io::Error::other("root certificate is not trusted"),
        )));
        assert!(advice(certificate(untrusted)).contains("antivirus"));
        // A plaintext reply to the TLS hello usually means the port expects
        // STARTTLS, or no TLS at all.
        let plaintext = failed(rustls::Error::InvalidMessage(
            rustls::InvalidMessage::InvalidContentType,
        ));
        assert!(advice(plaintext).contains("STARTTLS"));
        let closed = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "tls handshake eof");
        assert!(advice(closed).contains("STARTTLS"));
    }
}
