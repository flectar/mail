//! Server certificates judged by the operating system through the platform
//! layer (`platform_trust`). On Android that is the TrustManager, which
//! follows the app's network security config and fetches nothing online.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use std::sync::{Arc, OnceLock};

/// Checks a server's certificate chain against the operating system's trust
/// store, as the system's own HTTPS connections do. Only the chain is the
/// platform's to judge. The server name and the handshake signatures are
/// checked here, as for every other verifier.
pub trait PlatformTrust: Send + Sync + std::fmt::Debug {
    /// Check `chain`, the server's certificate followed by the intermediates
    /// it sent, all DER, for a connection to `host`, a DNS name or an IP
    /// address. `ocsp` holds the OCSP response the server stapled, if any.
    fn check(
        &self,
        chain: &[&[u8]],
        host: &str,
        ocsp: Option<&[u8]>,
    ) -> std::result::Result<(), PlatformTrustError>;
}

/// Why the platform refused a certificate chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformTrustError {
    /// A certificate's validity period has ended.
    Expired,
    /// A certificate's validity period hasn't started.
    NotValidYet,
    /// The server's certificate was revoked, as its stapled OCSP response says.
    Revoked,
    /// The chain doesn't lead to a trusted root, or breaks another of the
    /// platform's rules, in the platform's own words.
    Untrusted(String),
    /// A certificate isn't valid DER.
    BadEncoding,
    /// The platform's checker couldn't run.
    Unavailable(String),
}

/// The platform's reason for refusing a chain, kept in the TLS error so that
/// logs and error messages show it.
#[derive(Debug)]
struct PlatformRefusal(String);

impl std::fmt::Display for PlatformRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PlatformRefusal {}

/// The platform layer's certificate checks, once it has installed them.
static PLATFORM_TRUST: OnceLock<Arc<dyn PlatformTrust>> = OnceLock::new();

/// Check server certificates with `trust` on every TLS connection, mail
/// protocols and HTTP alike. The platform layer calls this at startup, before
/// the first connection; connections fail until it has. On Android,
/// rustls-platform-verifier asks the system for an online revocation check,
/// which needs a revocation list fetched over plain HTTP when a certificate
/// names no OCSP responder, as Let's Encrypt's and Google's no longer do.
/// Android refuses plain HTTP, and the certificate is reported as revoked.
/// Later calls change nothing.
#[cfg(platform_trust)]
pub fn use_platform_trust(trust: Arc<dyn PlatformTrust>) {
    let _ = PLATFORM_TRUST.set(trust);
}

/// The verifier of the checks the platform layer installs.
#[cfg(platform_trust)]
pub(super) fn installed(provider: Arc<CryptoProvider>) -> Arc<dyn ServerCertVerifier> {
    Arc::new(PlatformVerifier {
        trust: None,
        provider,
    })
}

/// The rustls verifier around a [`PlatformTrust`]: the given one, or else
/// the one the platform layer installed, looked up at each handshake so that
/// clients made before it did use it too.
#[derive(Debug)]
struct PlatformVerifier {
    trust: Option<Arc<dyn PlatformTrust>>,
    provider: Arc<CryptoProvider>,
}

impl PlatformVerifier {
    fn trust(&self) -> std::result::Result<&dyn PlatformTrust, rustls::Error> {
        self.trust
            .as_ref()
            .or_else(|| PLATFORM_TRUST.get())
            .map(|trust| trust.as_ref())
            .ok_or_else(|| {
                rustls::Error::General("the system's certificate checks aren't set up".into())
            })
    }
}

impl ServerCertVerifier for PlatformVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        // The platform checks validity periods against its own clock.
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let host = match server_name {
            ServerName::DnsName(name) => name.as_ref().to_owned(),
            ServerName::IpAddress(address) => std::net::IpAddr::from(*address).to_string(),
            _ => {
                return Err(rustls::Error::General(format!(
                    "unsupported server name {server_name:?}"
                )));
            }
        };
        let chain: Vec<&[u8]> = std::iter::once(end_entity.as_ref())
            .chain(intermediates.iter().map(|certificate| certificate.as_ref()))
            .collect();
        let ocsp = (!ocsp_response.is_empty()).then_some(ocsp_response);
        self.trust()?
            .check(&chain, &host, ocsp)
            .map_err(|refusal| match refusal {
                PlatformTrustError::Expired => {
                    rustls::Error::InvalidCertificate(CertificateError::Expired)
                }
                PlatformTrustError::NotValidYet => {
                    rustls::Error::InvalidCertificate(CertificateError::NotValidYet)
                }
                PlatformTrustError::Revoked => {
                    rustls::Error::InvalidCertificate(CertificateError::Revoked)
                }
                PlatformTrustError::BadEncoding => {
                    rustls::Error::InvalidCertificate(CertificateError::BadEncoding)
                }
                PlatformTrustError::Untrusted(reason) => rustls::Error::InvalidCertificate(
                    CertificateError::Other(rustls::OtherError(Arc::new(PlatformRefusal(reason)))),
                ),
                PlatformTrustError::Unavailable(reason) => rustls::Error::General(format!(
                    "the system could not check the certificate: {reason}"
                )),
            })?;
        // The platform judged the chain. The name is checked as rustls checks
        // it for any verifier.
        let certificate = rustls::server::ParsedCertificate::try_from(end_entity)?;
        rustls::client::verify_server_name(&certificate, server_name)?;
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
            &self.provider.signature_verification_algorithms,
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
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::tls::{handshake_advice, provider, trusted_certificates};

    /// A platform that gives `answer` and remembers what it was asked.
    #[derive(Debug)]
    struct FakeTrust {
        answer: std::result::Result<(), PlatformTrustError>,
        asked: std::sync::Mutex<Vec<Question>>,
    }

    /// What the platform was asked to check.
    #[derive(Debug, PartialEq)]
    struct Question {
        chain: Vec<Vec<u8>>,
        host: String,
        ocsp: Option<Vec<u8>>,
    }

    impl PlatformTrust for FakeTrust {
        fn check(
            &self,
            chain: &[&[u8]],
            host: &str,
            ocsp: Option<&[u8]>,
        ) -> std::result::Result<(), PlatformTrustError> {
            self.asked.lock().unwrap().push(Question {
                chain: chain
                    .iter()
                    .map(|certificate| certificate.to_vec())
                    .collect(),
                host: host.to_owned(),
                ocsp: ocsp.map(<[u8]>::to_vec),
            });
            self.answer.clone()
        }
    }

    fn platform(
        answer: std::result::Result<(), PlatformTrustError>,
    ) -> (Arc<FakeTrust>, PlatformVerifier) {
        let trust = Arc::new(FakeTrust {
            answer,
            asked: Default::default(),
        });
        let verifier = PlatformVerifier {
            trust: Some(trust.clone()),
            provider: provider(),
        };
        (trust, verifier)
    }

    fn fixture(pem: &str) -> CertificateDer<'static> {
        trusted_certificates(pem).unwrap().remove(0)
    }

    const LOCALHOST: &str = include_str!("../../../tests/fixtures/tls/server.pem");
    const OTHER: &str = include_str!("../../../tests/fixtures/tls/wrong-host.pem");

    fn verify(
        verifier: &PlatformVerifier,
        host: &str,
        ocsp: &[u8],
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        verifier.verify_server_cert(
            &fixture(LOCALHOST),
            &[fixture(OTHER)],
            &ServerName::try_from(host).unwrap(),
            ocsp,
            UnixTime::now(),
        )
    }

    #[test]
    fn the_platform_judges_the_chain_and_rustls_the_name() {
        let (trust, verifier) = platform(Ok(()));
        verify(&verifier, "localhost", &[]).unwrap();
        verify(&verifier, "127.0.0.1", b"stapled").unwrap();
        let wrong_name = verify(&verifier, "mail.example.com", &[]).unwrap_err();
        assert!(
            matches!(
                wrong_name,
                rustls::Error::InvalidCertificate(
                    CertificateError::NotValidForName
                        | CertificateError::NotValidForNameContext { .. }
                )
            ),
            "{wrong_name:?}"
        );
        let asked = trust.asked.lock().unwrap();
        let chain = vec![fixture(LOCALHOST).to_vec(), fixture(OTHER).to_vec()];
        assert_eq!(
            asked[0],
            Question {
                chain: chain.clone(),
                host: "localhost".into(),
                ocsp: None,
            }
        );
        assert_eq!(
            asked[1],
            Question {
                chain,
                host: "127.0.0.1".into(),
                ocsp: Some(b"stapled".to_vec()),
            }
        );
        assert_eq!(asked[2].host, "mail.example.com");
    }

    #[test]
    fn platform_refusals_become_certificate_errors() {
        let refused = |refusal| verify(&platform(Err(refusal)).1, "localhost", &[]).unwrap_err();
        assert_eq!(
            refused(PlatformTrustError::Expired),
            rustls::Error::InvalidCertificate(CertificateError::Expired)
        );
        assert_eq!(
            refused(PlatformTrustError::NotValidYet),
            rustls::Error::InvalidCertificate(CertificateError::NotValidYet)
        );
        assert_eq!(
            refused(PlatformTrustError::Revoked),
            rustls::Error::InvalidCertificate(CertificateError::Revoked)
        );
        assert_eq!(
            refused(PlatformTrustError::BadEncoding),
            rustls::Error::InvalidCertificate(CertificateError::BadEncoding)
        );
        // The platform's own reason stays in the error, and the advice is
        // the one for an untrusted certificate.
        let untrusted = refused(PlatformTrustError::Untrusted(
            "Trust anchor for certification path not found.".into(),
        ));
        assert!(matches!(
            untrusted,
            rustls::Error::InvalidCertificate(CertificateError::Other(_))
        ));
        assert!(
            untrusted
                .to_string()
                .contains("Trust anchor for certification path not found.")
        );
        let failed = std::io::Error::new(std::io::ErrorKind::InvalidData, untrusted);
        assert!(handshake_advice(&failed).contains("antivirus"));
        let unavailable = refused(PlatformTrustError::Unavailable("no Java".into()));
        assert!(matches!(&unavailable, rustls::Error::General(text) if text.contains("no Java")));
    }

    #[test]
    fn nothing_is_trusted_before_the_platform_installs_its_checks() {
        let verifier = PlatformVerifier {
            trust: None,
            provider: provider(),
        };
        let refused = verify(&verifier, "localhost", &[]).unwrap_err();
        assert!(
            matches!(&refused, rustls::Error::General(text) if text.contains("aren't set up")),
            "{refused:?}"
        );
    }
}
