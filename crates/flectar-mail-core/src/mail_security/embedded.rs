//! In-process OpenPGP engine for hosts without gpg/gpg-agent. Public material
//! lives in the app directory; private material is supplied by a protected
//! host store. No keyserver, subprocess, plaintext secret file, or shell is used.
use super::{
    MAX_CRYPTO_BYTES, OpenPgpBackend, OpenPgpKeyStore, OpenPgpOperation, OpenPgpOutput, error,
    fingerprint,
};
use crate::error::Result;
use pgp::{
    Cert, KeyHandle,
    cert::prelude::*,
    crypto::{KeyPair, SessionKey},
    packet::{PKESK, SKESK},
    parse::{Parse, stream::*},
    policy::StandardPolicy,
    serialize::{Serialize, stream::*},
    types::{Features, HashAlgorithm, RevocationStatus, SymmetricAlgorithm},
};
use sequoia_openpgp as pgp;
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

const MAX_KEY_BYTES: usize = 1024 * 1024;
const MAX_KEYS: usize = 2000;
type PassphraseProvider = dyn Fn(&str) -> Result<String> + Send + Sync;

/// The host must initialize this engine inside its private app directory and
/// supply a secure private-key store. Passphrase prompting is optional only
/// when all imported keys are already unlocked or newly generated in Asset.
pub struct EmbeddedOpenPgpBackend {
    public_key_dir: PathBuf,
    private_keys: Arc<dyn OpenPgpKeyStore>,
    passphrases: Option<Arc<PassphraseProvider>>,
    // Serialize certificate updates, including public/secret import merges.
    mutation: Mutex<()>,
}

impl EmbeddedOpenPgpBackend {
    pub fn new(public_key_dir: PathBuf, private_keys: Arc<dyn OpenPgpKeyStore>) -> Result<Self> {
        if !public_key_dir.is_absolute() {
            return Err(error(
                "The public key directory must be an absolute private app path",
            ));
        }
        fs::create_dir_all(&public_key_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&public_key_dir, fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            public_key_dir,
            private_keys,
            passphrases: None,
            mutation: Mutex::new(()),
        })
    }

    /// Called on a blocking crypto worker. Hosts can dispatch a secure native
    /// password dialog and block this worker until the user accepts/cancels.
    /// Passwords are never retained in the engine or account settings.
    pub fn with_passphrase_provider(mut self, provider: Arc<PassphraseProvider>) -> Self {
        self.passphrases = Some(provider);
        self
    }

    fn path(&self, fpr: &str) -> Result<PathBuf> {
        Ok(self
            .public_key_dir
            .join(format!("{}.cert", fingerprint(fpr, false)?)))
    }

    fn load(&self, fpr: &str, secret: bool) -> Result<Cert> {
        let fpr = fingerprint(fpr, false)?;
        let bytes = if secret {
            Zeroizing::new(
                self.private_keys
                    .load(&fpr)?
                    .ok_or_else(|| error("The private key is unavailable"))?,
            )
        } else {
            let file = fs::File::open(self.path(&fpr)?)
                .map_err(|_| error("The sender's public key is unavailable"))?;
            let mut bytes = Vec::new();
            file.take((MAX_KEY_BYTES + 1) as u64)
                .read_to_end(&mut bytes)?;
            Zeroizing::new(bytes)
        };
        if bytes.len() > MAX_KEY_BYTES {
            return Err(error("Stored key exceeds the 1 MiB limit"));
        }
        let cert = Cert::from_bytes(bytes.as_slice())
            .map_err(|_| error("The stored certificate is invalid"))?;
        if cert.fingerprint().to_hex() != fpr || (secret && !cert.is_tsk()) {
            return Err(error(
                "Stored certificate does not match its full fingerprint",
            ));
        }
        Ok(cert)
    }

    fn fingerprints(&self, secret: bool) -> Result<Vec<String>> {
        let mut keys = BTreeSet::new();
        if secret {
            for fpr in self.private_keys.list()? {
                keys.insert(fingerprint(&fpr, false)?);
            }
        } else {
            for entry in fs::read_dir(&self.public_key_dir)? {
                let entry = entry?;
                if !entry.file_type()?.is_file() {
                    continue;
                }
                if let Some(fpr) = entry
                    .file_name()
                    .to_str()
                    .and_then(|s| s.strip_suffix(".cert"))
                {
                    keys.insert(fingerprint(fpr, false)?);
                }
                if keys.len() > MAX_KEYS {
                    return Err(error("Too many stored certificates"));
                }
            }
        }
        if keys.len() > MAX_KEYS {
            return Err(error("Too many stored certificates"));
        }
        Ok(keys.into_iter().collect())
    }

    fn save(&self, cert: Cert) -> Result<()> {
        let fpr = cert.fingerprint().to_hex();
        let _guard = self
            .mutation
            .lock()
            .map_err(|_| error("Certificate storage is unavailable"))?;
        // A public update must retain existing secret subkeys; a secret update
        // must retain public revocations and newer self signatures.
        let existing = match self.private_keys.load(&fpr)? {
            Some(bytes) => {
                let bytes = Zeroizing::new(bytes);
                if bytes.len() > MAX_KEY_BYTES {
                    return Err(error("Stored key exceeds the 1 MiB limit"));
                }
                Some(
                    Cert::from_bytes(bytes.as_slice())
                        .map_err(|_| error("The stored private certificate is invalid"))?,
                )
            }
            None => match fs::File::open(self.path(&fpr)?) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take((MAX_KEY_BYTES + 1) as u64)
                        .read_to_end(&mut bytes)?;
                    if bytes.len() > MAX_KEY_BYTES {
                        return Err(error("Stored key exceeds the 1 MiB limit"));
                    }
                    Some(
                        Cert::from_bytes(&bytes)
                            .map_err(|_| error("The stored public certificate is invalid"))?,
                    )
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(e.into()),
            },
        };
        let cert = if let Some(existing) = existing {
            existing
                .merge_public_and_secret(cert)
                .map_err(|_| error("Certificates could not be merged"))?
        } else {
            cert
        };
        let mut public = Vec::new();
        cert.serialize(&mut public)
            .map_err(|_| error("Certificate serialization failed"))?;
        if public.len() > MAX_KEY_BYTES {
            return Err(error("Merged certificate exceeds the 1 MiB limit"));
        }
        if cert.is_tsk() {
            let mut private = Zeroizing::new(Vec::new());
            cert.as_tsk()
                .serialize(&mut *private)
                .map_err(|_| error("Private certificate serialization failed"))?;
            if private.len() > MAX_KEY_BYTES {
                return Err(error("Merged private certificate exceeds the 1 MiB limit"));
            }
            // Fail before publishing public material when secure storage fails.
            self.private_keys.store(&fpr, private.as_slice())?;
        }
        let mut temp = tempfile::NamedTempFile::new_in(&self.public_key_dir)?;
        temp.write_all(&public)?;
        temp.as_file().sync_all()?;
        temp.persist(self.path(&fpr)?)
            .map_err(|e| crate::error::CoreError::Io(e.error))?;
        Ok(())
    }

    fn check(&self, fpr: &str, email: &str, secret: bool, capability: char) -> Result<()> {
        let cert = self.load(fpr, secret)?;
        let policy = StandardPolicy::new();
        let valid = cert
            .with_policy(&policy, None)
            .map_err(|_| error("The key has no valid self-signature"))?;
        valid
            .alive()
            .map_err(|_| error("The key is expired or not yet valid"))?;
        if matches!(valid.revocation_status(), RevocationStatus::Revoked(_)) {
            return Err(error("The key is revoked"));
        }
        if !valid
            .userids()
            .filter(|uid| !matches!(uid.revocation_status(), RevocationStatus::Revoked(_)))
            .any(|uid| {
                uid.userid()
                    .email()
                    .ok()
                    .flatten()
                    .is_some_and(|uid| uid.eq_ignore_ascii_case(email))
            })
        {
            return Err(error(
                "The key does not contain a valid UID for this email address",
            ));
        }
        let usable = cert
            .keys()
            .with_policy(&policy, None)
            .supported()
            .alive()
            .revoked(false)
            .any(|key| {
                (!secret || key.key().has_secret())
                    && match capability {
                        'S' => key.key_flags().is_some_and(|flags| flags.for_signing()),
                        'E' => key.key_flags().is_some_and(|flags| {
                            flags.for_transport_encryption() || flags.for_storage_encryption()
                        }),
                        _ => false,
                    }
            });
        if !usable {
            return Err(error(
                "The key has no usable signing or encryption capability",
            ));
        }
        Ok(())
    }

    fn unlock(
        &self,
        key: pgp::packet::Key<pgp::packet::key::SecretParts, pgp::packet::key::UnspecifiedRole>,
        fpr: &str,
    ) -> Result<KeyPair> {
        let key = if key.secret().is_encrypted() {
            let callback = self.passphrases.as_ref().ok_or_else(|| {
                error("Unlock the imported private key using the platform password dialog")
            })?;
            let passphrase = Zeroizing::new(callback(fpr)?);
            key.decrypt_secret(&pgp::crypto::Password::from(passphrase.as_str()))
                .map_err(|_| error("The private key passphrase is incorrect"))?
        } else {
            key
        };
        key.into_keypair()
            .map_err(|_| error("The private key algorithm is unsupported"))
    }

    fn listing(&self, secret: bool, selected: Option<String>) -> Result<Vec<u8>> {
        let keys = if let Some(fpr) = selected {
            vec![fingerprint(&fpr, false)?]
        } else {
            self.fingerprints(secret)?
        };
        let policy = StandardPolicy::new();
        let mut output = String::new();
        for fpr in keys {
            let cert = self.load(&fpr, secret)?;
            let primary = cert.primary_key().key();
            let valid = cert.with_policy(&policy, None);
            let (validity, expires, caps) = match valid.as_ref() {
                Ok(valid) => {
                    let revoked = matches!(valid.revocation_status(), RevocationStatus::Revoked(_));
                    let alive = valid.alive().is_ok();
                    let signing = cert
                        .keys()
                        .with_policy(&policy, None)
                        .supported()
                        .alive()
                        .revoked(false)
                        .for_signing()
                        .any(|key| !secret || key.key().has_secret());
                    let encrypting = cert
                        .keys()
                        .with_policy(&policy, None)
                        .supported()
                        .alive()
                        .revoked(false)
                        .any(|key| {
                            (!secret || key.key().has_secret())
                                && key.key_flags().is_some_and(|flags| {
                                    flags.for_transport_encryption()
                                        || flags.for_storage_encryption()
                                })
                        });
                    (
                        if revoked {
                            "r"
                        } else if !alive {
                            "e"
                        } else {
                            "u"
                        },
                        valid
                            .primary_key()
                            .key_expiration_time()
                            .map(epoch)
                            .unwrap_or(0),
                        format!(
                            "c{}{}",
                            if signing { "S" } else { "" },
                            if encrypting { "E" } else { "" }
                        ),
                    )
                }
                Err(_) => ("i", 0, String::new()),
            };
            let kind = if secret { "sec" } else { "pub" };
            output.push_str(&format!(
                "{kind}:{validity}:0:{}:{}:{}:{expires}::u:::{caps}:\nfpr:::::::::{fpr}:\n",
                u8::from(primary.pk_algo()),
                primary.keyid().to_hex(),
                epoch(primary.creation_time())
            ));
            if let Ok(valid) = valid {
                for uid in valid.userids() {
                    let state = if matches!(uid.revocation_status(), RevocationStatus::Revoked(_)) {
                        "r"
                    } else {
                        validity
                    };
                    output.push_str(&format!(
                        "uid:{state}::::::::{}:\n",
                        colon_encode(uid.userid().value())
                    ));
                }
            } else {
                for uid in cert.userids() {
                    output.push_str(&format!(
                        "uid:i::::::::{}:\n",
                        colon_encode(uid.userid().value())
                    ));
                }
            }
        }
        Ok(output.into_bytes())
    }

    fn sign(&self, fpr: &str, data: &[u8]) -> Result<OpenPgpOutput> {
        let cert = self.load(fpr, true)?;
        let policy = StandardPolicy::new();
        let key = cert
            .keys()
            .secret()
            .with_policy(&policy, None)
            .supported()
            .alive()
            .revoked(false)
            .for_signing()
            .next()
            .ok_or_else(|| error("The private signing key is unavailable"))?;
        let pair = self.unlock(
            key.key().clone().role_into_unspecified(),
            &cert.fingerprint().to_hex(),
        )?;
        let mut output = Vec::new();
        let message = Message::new(&mut output);
        let message = Armorer::new(message)
            .kind(pgp::armor::Kind::Signature)
            .build()?;
        let mut signer = Signer::new(message, pair)?
            .hash_algo(HashAlgorithm::SHA256)?
            .detached()
            .build()?;
        signer.write_all(data)?;
        signer.finalize()?;
        Ok(OpenPgpOutput {
            data: output,
            status: format!(
                "[GNUPG:] SIG_CREATED D 22 8 00 0 {}\n",
                cert.fingerprint().to_hex()
            ),
        })
    }

    fn encrypt(&self, fingerprints: Vec<String>, data: &[u8]) -> Result<OpenPgpOutput> {
        let fingerprints: BTreeSet<_> = fingerprints.into_iter().collect();
        if fingerprints.is_empty() || fingerprints.len() > 501 {
            return Err(error("Invalid encryption recipient count"));
        }
        let certs: Vec<_> = fingerprints
            .iter()
            .map(|fpr| self.load(fpr, false))
            .collect::<Result<_>>()?;
        let policy = StandardPolicy::new();
        let mut recipients = Vec::new();
        for cert in &certs {
            let mut found = false;
            for key in cert
                .keys()
                .with_policy(&policy, None)
                .supported()
                .alive()
                .revoked(false)
                .filter(|key| {
                    key.key_flags().is_some_and(|flags| {
                        flags.for_transport_encryption() || flags.for_storage_encryption()
                    })
                })
            {
                // Hide every recipient, including sender and Bcc, exactly as
                // desktop --throw-keyids. Never leak bindings into ciphertext.
                recipients.push(Recipient::from(key).set_key_handle(None)?);
                found = true;
            }
            if !found {
                return Err(error("A recipient has no valid encryption key"));
            }
        }
        let mut output = Vec::new();
        let message = Message::new(&mut output);
        let message = Armorer::new(message).build()?;
        // AES/MDC is interoperable with deployed GnuPG 2.x and authenticates
        // the complete message. AEAD input is also accepted by the parser.
        let message = Encryptor::for_recipients(message, recipients).build()?;
        let mut writer = LiteralWriter::new(message).build()?;
        writer.write_all(data)?;
        writer.finalize()?;
        Ok(OpenPgpOutput {
            data: output,
            status: "[GNUPG:] END_ENCRYPTION\n".into(),
        })
    }

    fn helper(&self, decrypt: bool) -> Result<CryptoHelper<'_>> {
        let public = self
            .fingerprints(false)?
            .iter()
            .map(|fpr| self.load(fpr, false))
            .collect::<Result<_>>()?;
        let private = if decrypt {
            self.fingerprints(true)?
                .iter()
                .map(|fpr| self.load(fpr, true))
                .collect::<Result<_>>()?
        } else {
            Vec::new()
        };
        Ok(CryptoHelper {
            backend: self,
            public,
            private,
            require_signature: !decrypt,
            encrypted: false,
            checked: false,
            signatures: Vec::new(),
        })
    }

    fn decrypt(&self, data: &[u8]) -> Result<OpenPgpOutput> {
        let policy = StandardPolicy::new();
        let helper = self.helper(true)?;
        let mut reader = DecryptorBuilder::from_bytes(data)?.with_policy(&policy, None, helper)?;
        let mut plaintext = Vec::new();
        // Buffer everything until all integrity/signature checks finish. No
        // unauthenticated prefix is ever handed to the renderer or filesystem.
        reader
            .by_ref()
            .take((MAX_CRYPTO_BYTES + 1) as u64)
            .read_to_end(&mut plaintext)
            .map_err(|_| {
                error("Decryption failed or the digital signature is invalid, expired, or revoked")
            })?;
        if plaintext.len() > MAX_CRYPTO_BYTES
            || !reader.message_processed()
            || !reader.helper_ref().checked
        {
            return Err(error(
                "OpenPGP message exceeds the size limit or is incomplete",
            ));
        }
        let helper = reader.helper_ref();
        let mut status = helper.signature_status();
        if helper.encrypted {
            status.push_str("[GNUPG:] BEGIN_DECRYPTION\n[GNUPG:] DECRYPTION_INFO 2 9 0\n[GNUPG:] DECRYPTION_OKAY\n[GNUPG:] GOODMDC\n[GNUPG:] END_DECRYPTION\n");
        } else if helper.signatures.is_empty() {
            return Err(error("The message is neither encrypted nor validly signed"));
        }
        Ok(OpenPgpOutput {
            data: plaintext,
            status,
        })
    }

    fn verify(&self, signature: &[u8], data: &[u8]) -> Result<OpenPgpOutput> {
        let policy = StandardPolicy::new();
        let helper = self.helper(false)?;
        let mut verifier =
            DetachedVerifierBuilder::from_bytes(signature)?.with_policy(&policy, None, helper)?;
        verifier.verify_bytes(data).map_err(|_| error("The digital signature is invalid, expired, revoked, or its public key is unavailable"))?;
        if !verifier.helper_ref().checked {
            return Err(error("The signature was not authenticated"));
        }
        Ok(OpenPgpOutput {
            data: Vec::new(),
            status: verifier.helper_ref().signature_status(),
        })
    }
}

impl OpenPgpBackend for EmbeddedOpenPgpBackend {
    fn execute(&self, operation: OpenPgpOperation) -> Result<OpenPgpOutput> {
        match operation {
            OpenPgpOperation::Generate { email } => {
                if email.parse::<lettre::Address>().is_err() || email.starts_with('-') {
                    return Err(error("Choose a valid account email address"));
                }
                let (cert, _revocation) = CertBuilder::general_purpose(Some(email))
                    // Preserve interoperability with the installed GnuPG
                    // 2.x ecosystem while accepting RFC 9580 input as well.
                    .set_profile(pgp::Profile::RFC4880)?
                    .set_features(Features::empty().set_seipdv1())?
                    .set_cipher_suite(CipherSuite::Cv25519)
                    .set_validity_period(Duration::from_secs(2 * 365 * 24 * 60 * 60))
                    .generate()?;
                let fpr = cert.fingerprint().to_hex();
                self.save(cert)?;
                Ok(OpenPgpOutput {
                    status: format!("[GNUPG:] KEY_CREATED B {fpr}\n"),
                    ..Default::default()
                })
            }
            OpenPgpOperation::List {
                secret,
                fingerprint,
            } => Ok(OpenPgpOutput {
                data: self.listing(secret, fingerprint)?,
                ..Default::default()
            }),
            OpenPgpOperation::Import { certificate } => {
                let certificate = Zeroizing::new(certificate);
                if certificate.len() > MAX_KEY_BYTES {
                    return Err(error("Key files must be smaller than 1 MiB"));
                }
                let certs = CertParser::from_bytes(certificate.as_slice())?
                    .take(501)
                    .collect::<pgp::Result<Vec<_>>>()?;
                if certs.is_empty() || certs.len() > 500 {
                    return Err(error("Invalid key file or too many certificates"));
                }
                for cert in certs {
                    self.save(cert)?;
                }
                Ok(OpenPgpOutput::default())
            }
            OpenPgpOperation::Export { fingerprint } => {
                let cert = self.load(&fingerprint, false)?;
                let mut data = Vec::new();
                cert.armored().serialize(&mut data)?;
                Ok(OpenPgpOutput {
                    data,
                    ..Default::default()
                })
            }
            OpenPgpOperation::Check {
                fingerprint,
                email,
                secret,
                capability,
            } => {
                self.check(&fingerprint, &email, secret, capability)?;
                Ok(OpenPgpOutput::default())
            }
            OpenPgpOperation::Sign { fingerprint, data } => self.sign(&fingerprint, &data),
            OpenPgpOperation::Encrypt { fingerprints, data } => self.encrypt(fingerprints, &data),
            OpenPgpOperation::Decrypt { data } => self.decrypt(&data),
            OpenPgpOperation::Verify { signature, data } => self.verify(&signature, &data),
        }
    }
}

struct CryptoHelper<'a> {
    backend: &'a EmbeddedOpenPgpBackend,
    public: Vec<Cert>,
    private: Vec<Cert>,
    require_signature: bool,
    encrypted: bool,
    checked: bool,
    signatures: Vec<(String, String)>,
}
impl CryptoHelper<'_> {
    fn signature_status(&self) -> String {
        self.signatures
            .iter()
            .map(|(key, primary)| format!("[GNUPG:] VALIDSIG {key} 0 0 0 0 0 0 0 0 {primary}\n"))
            .collect()
    }
}
impl VerificationHelper for CryptoHelper<'_> {
    fn get_certs(&mut self, _ids: &[KeyHandle]) -> pgp::Result<Vec<Cert>> {
        // Only explicitly imported certificates; network lookup is disabled.
        Ok(self.public.clone())
    }
    fn check(&mut self, structure: MessageStructure) -> pgp::Result<()> {
        let policy = StandardPolicy::new();
        for layer in structure {
            match layer {
                MessageLayer::Encryption { .. } => self.encrypted = true,
                MessageLayer::Compression { .. } => (),
                MessageLayer::SignatureGroup { results } => {
                    if results.is_empty() {
                        return Err(anyhow::anyhow!("Empty signature group"));
                    }
                    for result in results {
                        let good = result.map_err(|_| {
                            anyhow::anyhow!("Invalid, expired, revoked, or unknown signature")
                        })?;
                        // Sequoia verifies a key at signing time. Match the
                        // application's stronger policy: reject a signing key
                        // or certificate that has since expired/been revoked.
                        let cert = good.ka.cert();
                        let current = cert.with_policy(&policy, None)?;
                        current.alive()?;
                        if matches!(current.revocation_status(), RevocationStatus::Revoked(_)) {
                            return Err(anyhow::anyhow!("Revoked signer"));
                        }
                        let key_fpr = good.ka.key().fingerprint();
                        if !cert
                            .keys()
                            .with_policy(&policy, None)
                            .supported()
                            .alive()
                            .revoked(false)
                            .for_signing()
                            .any(|key| key.key().fingerprint() == key_fpr)
                        {
                            return Err(anyhow::anyhow!("Expired or revoked signing key"));
                        }
                        self.signatures
                            .push((key_fpr.to_hex(), cert.fingerprint().to_hex()));
                    }
                }
            }
        }
        if self.require_signature && self.signatures.is_empty() {
            return Err(anyhow::anyhow!("No valid signature"));
        }
        self.checked = true;
        Ok(())
    }
}
impl DecryptionHelper for CryptoHelper<'_> {
    fn decrypt(
        &mut self,
        pkesks: &[PKESK],
        skesks: &[SKESK],
        sym_algo: Option<SymmetricAlgorithm>,
        decrypt: &mut dyn FnMut(Option<SymmetricAlgorithm>, &SessionKey) -> bool,
    ) -> pgp::Result<Option<Cert>> {
        let policy = StandardPolicy::new();
        // Prefer unlocked keys, then ask once per applicable protected key.
        // Expired private encryption keys may open historical mail, as GnuPG
        // does; signature expiry/revocation is always enforced by check().
        for encrypted in [false, true] {
            for cert in &self.private {
                for key in cert
                    .keys()
                    .secret()
                    .with_policy(&policy, None)
                    .supported()
                    .filter(|key| key.key().secret().is_encrypted() == encrypted)
                    .filter(|key| {
                        key.key_flags().is_some_and(|flags| {
                            flags.for_transport_encryption() || flags.for_storage_encryption()
                        })
                    })
                {
                    let relevant: Vec<_> = pkesks
                        .iter()
                        .filter(|pkesk| {
                            pkesk
                                .recipient()
                                .is_none_or(|id| id.aliases(key.key().key_handle()))
                        })
                        .collect();
                    if relevant.is_empty() {
                        continue;
                    }
                    let mut pair = self.backend.unlock(
                        key.key().clone().role_into_unspecified(),
                        &cert.fingerprint().to_hex(),
                    )?;
                    for pkesk in relevant {
                        if pkesk
                            .decrypt(&mut pair, sym_algo)
                            .map(|(algo, session)| decrypt(algo, &session))
                            .unwrap_or(false)
                        {
                            return Ok(Some(cert.clone()));
                        }
                    }
                }
            }
        }
        // GnuPG also opens password-encrypted armored files. Use the same
        // native prompt, with an empty fingerprint indicating a message password.
        if !skesks.is_empty() {
            let callback = self
                .backend
                .passphrases
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("The message requires a password"))?;
            let password = Zeroizing::new(callback("")?);
            let password = pgp::crypto::Password::from(password.as_str());
            for skesk in skesks {
                if let Ok((algo, session)) = skesk.decrypt(&password) {
                    if decrypt(algo, &session) {
                        return Ok(None);
                    }
                }
            }
        }
        Err(anyhow::anyhow!(
            "No private key or password could decrypt this message"
        ))
    }
}

fn epoch(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn colon_encode(bytes: &[u8]) -> String {
    let mut encoded = Vec::new();
    for &byte in bytes {
        if matches!(byte, b':' | b'\\' | b'\n' | b'\r') || byte < 0x20 || byte == 0x7f {
            encoded.extend_from_slice(format!("\\x{byte:02x}").as_bytes());
        } else {
            encoded.push(byte);
        }
    }
    String::from_utf8_lossy(&encoded).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        mail_security::{Gpg, MailSecurity, validate_listing, without_bcc},
        models::Address,
    };
    use std::collections::BTreeMap;

    /// Tests use ephemeral memory, never the developer's private key store.
    #[derive(Default)]
    struct MemoryKeys(Mutex<BTreeMap<String, Vec<u8>>>);
    impl OpenPgpKeyStore for MemoryKeys {
        fn load(&self, fpr: &str) -> Result<Option<Vec<u8>>> {
            Ok(self.0.lock().unwrap().get(fpr).cloned())
        }
        fn store(&self, fpr: &str, value: &[u8]) -> Result<()> {
            self.0
                .lock()
                .unwrap()
                .insert(fpr.to_owned(), value.to_vec());
            Ok(())
        }
        fn delete(&self, fpr: &str) -> Result<()> {
            self.0.lock().unwrap().remove(fpr);
            Ok(())
        }
        fn list(&self) -> Result<Vec<String>> {
            Ok(self.0.lock().unwrap().keys().cloned().collect())
        }
    }
    fn engine() -> (
        tempfile::TempDir,
        Arc<MemoryKeys>,
        Arc<EmbeddedOpenPgpBackend>,
        Gpg,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let keys = Arc::new(MemoryKeys::default());
        let backend =
            Arc::new(EmbeddedOpenPgpBackend::new(dir.path().to_owned(), keys.clone()).unwrap());
        let gpg = Gpg::with_backend(backend.clone());
        (dir, keys, backend, gpg)
    }
    fn policy(alice: &str, bob: &str) -> MailSecurity {
        MailSecurity {
            signing_fingerprint: alice.to_owned(),
            sign_by_default: true,
            require_encryption: true,
            recipient_keys: BTreeMap::from([("bob@example.org".into(), bob.to_owned())]),
        }
    }
    fn raw() -> Vec<u8> {
        mail_builder::MessageBuilder::new()
            .from("alice@example.org")
            .to("bob@example.org")
            .bcc("hidden@example.org")
            .subject("Visible subject")
            .text_body("Private message: 飞鸿 café")
            .attachment("text/plain", "secret.txt", b"private attachment".to_vec())
            .write_to_vec()
            .unwrap()
    }

    #[tokio::test]
    async fn embedded_mime_gnupg_interoperability_and_hidden_recipients() {
        let (_dir, keys, backend, embedded) = engine();
        let alice = embedded.generate_key("alice@example.org").await.unwrap();
        let bob = embedded.generate_key("bob@example.org").await.unwrap();
        let recipients = vec![Address {
            name: None,
            email: "bob@example.org".into(),
        }];
        let policy = policy(&alice, &bob);
        let original = raw();
        let protected = embedded
            .protect(&original, &policy, "alice@example.org", &recipients)
            .await
            .unwrap();
        let delivery = without_bcc(&protected).unwrap();
        assert!(!String::from_utf8_lossy(&delivery).contains("hidden@example.org"));
        assert!(!String::from_utf8_lossy(&delivery).contains("Private message"));
        let opened = embedded
            .open_message(&delivery, Some(&alice))
            .await
            .unwrap();
        assert!(opened.text.contains("飞鸿 café"));
        assert!(opened.status.contains("verified key"));

        // Check the actual PKESK packets, not just the visible MIME headers.
        let cipher = backend
            .execute(OpenPgpOperation::Encrypt {
                fingerprints: vec![alice.clone(), bob.clone()],
                data: original.clone(),
            })
            .unwrap();
        let packets = pgp::PacketPile::from_bytes(&cipher.data).unwrap();
        let recipients: Vec<_> = packets
            .descendants()
            .filter_map(|packet| match packet {
                pgp::Packet::PKESK(p) => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(recipients.len(), 2);
        assert!(recipients.iter().all(|p| p.recipient().is_none()));

        // A distinct GnuPG home imports only Bob's secret and Alice's public.
        let home = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(home.path(), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let external = Gpg {
            home: Some(home.path().to_owned()),
            backend: None,
        };
        external
            .import_key(&keys.load(&bob).unwrap().unwrap())
            .await
            .unwrap();
        external
            .import_key(&embedded.export_public_key(&alice).await.unwrap())
            .await
            .unwrap();
        let opened = external
            .open_message(&delivery, Some(&alice))
            .await
            .unwrap();
        assert!(opened.text.contains("飞鸿 café"));
        assert_eq!(
            opened.mime,
            embedded
                .open_message(&delivery, Some(&alice))
                .await
                .unwrap()
                .mime
        );
        external
            .import_key(&keys.load(&alice).unwrap().unwrap())
            .await
            .unwrap();
        let from_gpg = external
            .protect(
                &original,
                &policy,
                "alice@example.org",
                &[Address {
                    name: None,
                    email: "bob@example.org".into(),
                }],
            )
            .await
            .unwrap();
        assert!(
            embedded
                .open_message(&from_gpg, Some(&alice))
                .await
                .unwrap()
                .text
                .contains("飞鸿 café")
        );
        let mismatched = embedded.open_message(&delivery, Some(&bob)).await.unwrap();
        assert!(mismatched.status.contains("identity has not been verified"));
        let mut tampered = delivery;
        let end = tampered
            .windows(b"-----END PGP MESSAGE-----".len())
            .position(|w| w == b"-----END PGP MESSAGE-----")
            .unwrap();
        // Armor CRC is optional and is not cryptographic authentication. Flip
        // a real ciphertext byte before the CRC line, away from padding bits.
        let encoded_end = tampered[..end]
            .windows(2)
            .rposition(|w| w == b"\n=")
            .unwrap();
        let pos = (0..encoded_end)
            .rev()
            .filter(|&i| tampered[i].is_ascii_alphanumeric() || matches!(tampered[i], b'+' | b'/'))
            .nth(8)
            .unwrap();
        tampered[pos] = if tampered[pos] == b'A' { b'B' } else { b'A' };
        assert!(
            embedded
                .open_message(&tampered, Some(&alice))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn protected_key_import_restarts_without_plaintext_secret_files() {
        let (dir, keys, backend, embedded) = engine();
        let (cert, _) = CertBuilder::general_purpose(Some("alice@example.org"))
            .set_features(Features::empty().set_seipdv1())
            .unwrap()
            .set_password(Some("test-only-passphrase".into()))
            .generate()
            .unwrap();
        let fpr = cert.fingerprint().to_hex();
        let mut bytes = Vec::new();
        cert.as_tsk().armored().serialize(&mut bytes).unwrap();
        embedded.import_key(&bytes).await.unwrap();
        let listed = embedded.list_keys(true).await.unwrap();
        validate_listing(&listed, &fpr, "alice@example.org", 'S').unwrap();
        assert!(embedded.list_keys(false).await.unwrap().contains(&fpr));
        let public = Cert::from_bytes(&fs::read(backend.path(&fpr).unwrap()).unwrap()).unwrap();
        assert!(!public.is_tsk());
        assert!(
            Cert::from_bytes(&embedded.export_public_key(&fpr).await.unwrap())
                .unwrap()
                .is_tsk()
                == false
        );
        let sign = || OpenPgpOperation::Sign {
            fingerprint: fpr.clone(),
            data: b"mime entity".to_vec(),
        };
        assert!(backend.execute(sign()).is_err());
        let restarted = EmbeddedOpenPgpBackend::new(dir.path().to_owned(), keys)
            .unwrap()
            .with_passphrase_provider(Arc::new(|_| Ok("test-only-passphrase".into())));
        let signature = restarted.execute(sign()).unwrap();
        restarted
            .execute(OpenPgpOperation::Verify {
                signature: signature.data,
                data: b"mime entity".to_vec(),
            })
            .unwrap();
        let bad_password =
            EmbeddedOpenPgpBackend::new(dir.path().to_owned(), backend.private_keys.clone())
                .unwrap()
                .with_passphrase_provider(Arc::new(|_| Ok("incorrect".into())));
        assert!(bad_password.execute(sign()).is_err());
    }

    #[tokio::test]
    async fn embedded_policy_rejects_wrong_identity_expiry_revocation_and_missing_bcc_key() {
        let (_dir, _keys, backend, embedded) = engine();
        let alice = embedded.generate_key("alice@example.org").await.unwrap();
        let bob = embedded.generate_key("bob@example.org").await.unwrap();
        let settings = policy(&alice, &bob);
        assert!(
            embedded
                .preflight(&settings, "mallory@example.org", &[])
                .await
                .is_err()
        );
        assert!(
            embedded
                .preflight(
                    &settings,
                    "alice@example.org",
                    &[Address {
                        name: None,
                        email: "hidden@example.org".into()
                    }]
                )
                .await
                .is_err()
        );
        let old = SystemTime::now() - Duration::from_secs(48 * 60 * 60);
        let (expired, _) = CertBuilder::general_purpose(Some("expired@example.org"))
            .set_creation_time(old)
            .set_validity_period(Duration::from_secs(60))
            .generate()
            .unwrap();
        let fpr = expired.fingerprint().to_hex();
        backend.save(expired.clone()).unwrap();
        assert!(
            backend
                .check(&fpr, "expired@example.org", true, 'S')
                .is_err()
        );
        // A mathematically valid old signature is rejected after key expiry.
        let policy = StandardPolicy::new();
        let key = expired
            .keys()
            .secret()
            .with_policy(&policy, old + Duration::from_secs(30))
            .for_signing()
            .next()
            .unwrap();
        let pair = key.key().clone().into_keypair().unwrap();
        let mut signature = Vec::new();
        let mut signer = Signer::new(Message::new(&mut signature), pair)
            .unwrap()
            .creation_time(old + Duration::from_secs(30))
            .detached()
            .build()
            .unwrap();
        signer.write_all(b"old message").unwrap();
        signer.finalize().unwrap();
        assert!(backend.verify(&signature, b"old message").is_err());
        let (revoked, revocation) = CertBuilder::general_purpose(Some("revoked@example.org"))
            .generate()
            .unwrap();
        let fpr = revoked.fingerprint().to_hex();
        backend
            .save(
                revoked
                    .insert_packets(pgp::Packet::Signature(revocation))
                    .unwrap()
                    .0,
            )
            .unwrap();
        assert!(
            backend
                .check(&fpr, "revoked@example.org", true, 'S')
                .is_err()
        );
        let (_missing_dir, _missing_keys, _missing_backend, missing) = engine();
        missing
            .import_key(&embedded.export_public_key(&alice).await.unwrap())
            .await
            .unwrap();
        assert!(
            missing
                .preflight(&settings, "alice@example.org", &[])
                .await
                .is_err()
        );
    }
}
