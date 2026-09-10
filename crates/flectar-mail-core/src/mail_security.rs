//! OpenPGP/MIME via the installed GnuPG 2.x engine. Private keys and passphrases
//! stay with gpg-agent; account settings contain only public fingerprints.
use crate::{
    Core,
    db::{Db, repo},
    error::{CoreError, Result},
    models::Address,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf, process::Stdio, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MAX_CRYPTO_BYTES: usize = 100 * 1024 * 1024;
const MAX_STATUS_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MailSecurity {
    pub signing_fingerprint: String,
    pub sign_by_default: bool,
    pub require_encryption: bool,
    /// Explicitly verified email -> full primary fingerprint bindings.
    pub recipient_keys: BTreeMap<String, String>,
}
impl MailSecurity {
    pub fn enabled(&self) -> bool {
        self.sign_by_default || self.require_encryption
    }
    pub fn validate(&mut self) -> Result<()> {
        self.signing_fingerprint = fingerprint(&self.signing_fingerprint, !self.enabled())?;
        if self.recipient_keys.len() > 500 {
            return Err(error("Too many recipient keys"));
        }
        let mut keys = BTreeMap::new();
        for (email, key) in &self.recipient_keys {
            let email = email.trim().to_lowercase();
            if email.parse::<lettre::Address>().is_err() {
                return Err(error("Invalid recipient email address"));
            }
            if keys.insert(email, fingerprint(key, false)?).is_some() {
                return Err(error("Duplicate recipient address"));
            }
        }
        self.recipient_keys = keys;
        Ok(())
    }
}
fn error(message: &str) -> CoreError {
    CoreError::Other(format!("OpenPGP: {message}"))
}
fn fingerprint(value: &str, allow_empty: bool) -> Result<String> {
    let value: String = value.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if (allow_empty && value.is_empty())
        || (matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        Ok(value.to_ascii_uppercase())
    } else {
        Err(error("Use the full 40- or 64-digit key fingerprint"))
    }
}

#[derive(Clone, Default)]
pub struct Gpg {
    // Test engines use isolated keyrings. Production uses the user's GnuPG home.
    home: Option<PathBuf>,
}
impl Gpg {
    async fn run(&self, args: &[String], input: Option<&[u8]>) -> Result<(Vec<u8>, String)> {
        if cfg!(any(target_os = "android", target_os = "ios")) {
            return Err(error("OpenPGP requires desktop GnuPG"));
        }
        if input.is_some_and(|data| data.len() > MAX_CRYPTO_BYTES) {
            return Err(error("Message exceeds the 100 MiB OpenPGP limit"));
        }
        let status_file = tempfile::NamedTempFile::new()?;
        let mut cmd = tokio::process::Command::new("gpg");
        cmd.args([
            "--no-options",
            "--batch",
            "--yes",
            "--no-auto-key-retrieve",
            "--auto-key-locate",
            "clear",
            "--status-file",
        ]);
        cmd.arg(status_file.path());
        if let Some(home) = &self.home {
            cmd.arg("--homedir").arg(home);
        }
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|_| error("Install GnuPG 2.x and make gpg available on PATH"))?;
        let mut stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let task = async {
            let write = async {
                if let Some(input) = input {
                    stdin.write_all(input).await?;
                }
                drop(stdin);
                Ok::<_, std::io::Error>(())
            };
            let read = async {
                let mut data = Vec::new();
                stdout
                    .take((MAX_CRYPTO_BYTES + 1) as u64)
                    .read_to_end(&mut data)
                    .await?;
                Ok::<_, std::io::Error>(data)
            };
            let status = async {
                let mut data = Vec::new();
                stderr.take(1024 * 1024).read_to_end(&mut data).await?;
                Ok::<_, std::io::Error>(data)
            };
            let (_, data, _diagnostics) = tokio::try_join!(write, read, status)?;
            if data.len() > MAX_CRYPTO_BYTES {
                return Err(error("OpenPGP output exceeds the size limit"));
            }
            let exit = child.wait().await?;
            let mut status_bytes = Vec::new();
            tokio::fs::File::open(status_file.path())
                .await?
                .take((MAX_STATUS_BYTES + 1) as u64)
                .read_to_end(&mut status_bytes)
                .await?;
            if status_bytes.len() > MAX_STATUS_BYTES {
                return Err(error("GnuPG status exceeds the size limit"));
            }
            let status = String::from_utf8_lossy(&status_bytes).into_owned();
            // Expired/revoked signatures can accompany a successful GnuPG
            // exit. Never downgrade a failed signature to unsigned plaintext.
            if invalid_signature(&status) {
                return Err(error(
                    "The digital signature is invalid, expired, or revoked",
                ));
            }
            // GnuPG 2.2 can exit 2 after trying another anonymous recipient's
            // key even though this recipient's decryption succeeded. Accept
            // only the complete authenticated-decryption status sequence.
            let partial_recipient_success = args == ["--decrypt"]
                && status.contains("[GNUPG:] NO_SECKEY ")
                && authenticated_decryption(&status)
                && ![
                    "FAILURE",
                    "ERROR",
                    "BADSIG",
                    "ERRSIG",
                    "EXPKEYSIG",
                    "REVKEYSIG",
                    "EXPSIG",
                ]
                .iter()
                .any(|tag| status.contains(&format!("[GNUPG:] {tag} ")));
            if !exit.success() && !partial_recipient_success {
                // Do not expose arbitrary GPG diagnostics (UIDs, filenames, user data).
                return Err(error(if status.contains("NO_SECKEY") {
                    "The private key is unavailable"
                } else if status.contains("NO_PUBKEY") {
                    "The sender's public key is unavailable"
                } else if status.contains("BAD_PASSPHRASE") || status.contains("MISSING_PASSPHRASE")
                {
                    "Unlock the private key using GnuPG's pinentry"
                } else {
                    "GnuPG could not complete the operation. Check key validity and unlock the private key using pinentry"
                }));
            }
            Ok((data, status))
        };
        tokio::time::timeout(Duration::from_secs(120), task)
            .await
            .map_err(|_| error("GnuPG timed out; no message was sent"))?
    }

    /// GnuPG chooses its supported default key algorithms. Its pinentry asks
    /// for the new passphrase; no secret enters our process or settings.
    pub async fn generate_key(&self, email: &str) -> Result<String> {
        if email.parse::<lettre::Address>().is_err() || email.starts_with('-') {
            return Err(error("Choose a valid account email address"));
        }
        let (_, status) = self
            .run(
                &[
                    "--pinentry-mode".into(),
                    "ask".into(),
                    "--quick-generate-key".into(),
                    email.to_owned(),
                    "default".into(),
                    "default".into(),
                    "2y".into(),
                ],
                None,
            )
            .await?;
        let key = status
            .lines()
            .find_map(|s| s.strip_prefix("[GNUPG:] KEY_CREATED "))
            .and_then(|s| s.split_whitespace().nth(1))
            .ok_or_else(|| error("GnuPG did not confirm key creation"))?;
        fingerprint(key, false)
    }

    pub async fn list_keys(&self, secret: bool) -> Result<String> {
        let (data, _) = self
            .run(
                &[
                    "--with-colons".into(),
                    "--fixed-list-mode".into(),
                    if secret {
                        "--list-secret-keys"
                    } else {
                        "--list-keys"
                    }
                    .into(),
                ],
                None,
            )
            .await?;
        Ok(String::from_utf8_lossy(&data).into_owned())
    }

    pub async fn import_key(&self, data: &[u8]) -> Result<()> {
        if data.len() > 1024 * 1024 {
            return Err(error("Key files must be smaller than 1 MiB"));
        }
        self.run(&["--import".into()], Some(data)).await?;
        Ok(())
    }

    pub async fn export_public_key(&self, key: &str) -> Result<Vec<u8>> {
        let key = fingerprint(key, false)?;
        let (data, _) = self
            .run(&["--armor".into(), "--export".into(), key], None)
            .await?;
        if data.is_empty() {
            return Err(error("Public key not found"));
        }
        Ok(data)
    }

    /// Validate exact primary fingerprints, non-expired/revoked keys, UID email
    /// binding, and usable signing/encryption capability. GPG chooses subkeys.
    async fn check_key(
        &self,
        key: &str,
        email: &str,
        secret: bool,
        capability: char,
    ) -> Result<()> {
        let key = fingerprint(key, false)?;
        let (data, _) = self
            .run(
                &[
                    "--with-colons".into(),
                    "--fixed-list-mode".into(),
                    if secret {
                        "--list-secret-keys"
                    } else {
                        "--list-keys"
                    }
                    .into(),
                    key.clone(),
                ],
                None,
            )
            .await?;
        let listing = String::from_utf8_lossy(&data);
        validate_listing(&listing, &key, email, capability)
    }

    pub async fn preflight(
        &self,
        policy: &MailSecurity,
        sender: &str,
        recipients: &[Address],
    ) -> Result<()> {
        let mut checked = policy.clone();
        checked.validate()?;
        if !checked.enabled() {
            return Ok(());
        }
        if checked.sign_by_default {
            self.check_key(&checked.signing_fingerprint, sender, true, 'S')
                .await?;
        }
        if checked.require_encryption {
            self.check_key(&checked.signing_fingerprint, sender, true, 'E')
                .await?;
            for recipient in recipients {
                if recipient.email.eq_ignore_ascii_case(sender) {
                    continue;
                }
                let key = checked
                    .recipient_keys
                    .get(&recipient.email.to_lowercase())
                    .ok_or_else(|| {
                        error(&format!(
                            "Verify a public key for {} before sending. Encryption is required",
                            recipient.email
                        ))
                    })?;
                self.check_key(key, &recipient.email, false, 'E').await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn protect(
        &self,
        raw: &[u8],
        policy: &MailSecurity,
        sender: &str,
        recipients: &[Address],
    ) -> Result<Vec<u8>> {
        if !policy.enabled() {
            return Ok(raw.to_vec());
        }
        let mut normalized = policy.clone();
        normalized.validate()?;
        let policy = &normalized;
        self.preflight(policy, sender, recipients).await?;
        let (headers, mut entity) = split_entity(raw)?;
        if policy.sign_by_default {
            let (signature, status) = self
                .run(
                    &[
                        "--armor".into(),
                        "--digest-algo".into(),
                        "SHA256".into(),
                        "--local-user".into(),
                        policy.signing_fingerprint.clone(),
                        "--detach-sign".into(),
                    ],
                    Some(&entity),
                )
                .await?;
            if !status.contains("[GNUPG:] SIG_CREATED ") {
                return Err(error("GnuPG did not confirm the signature"));
            }
            entity = multipart("signed", "application/pgp-signature", Some("pgp-sha256"), &entity,
                &format!("Content-Type: application/pgp-signature; name=\"signature.asc\"\r\nContent-Disposition: attachment; filename=\"signature.asc\"\r\n\r\n{}", String::from_utf8_lossy(&signature)).into_bytes());
        }
        if policy.require_encryption {
            // Trust is explicit in our email/fingerprint bindings, never inferred
            // from a matching UID. Hidden recipients prevent Bcc key-id leaks.
            let mut args = vec![
                "--armor".into(),
                "--trust-model".into(),
                "always".into(),
                "--throw-keyids".into(),
                "--recipient".into(),
                policy.signing_fingerprint.clone(),
            ];
            for recipient in recipients {
                if let Some(key) = policy.recipient_keys.get(&recipient.email.to_lowercase()) {
                    args.extend(["--recipient".into(), key.clone()]);
                }
            }
            args.push("--encrypt".into());
            let (ciphertext, status) = self.run(&args, Some(&entity)).await?;
            if !status.contains("[GNUPG:] END_ENCRYPTION") {
                return Err(error("GnuPG did not confirm encryption"));
            }
            entity = multipart("encrypted", "application/pgp-encrypted", None,
                b"Content-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n",
                &format!("Content-Type: application/octet-stream; name=\"encrypted.asc\"\r\nContent-Disposition: inline; filename=\"encrypted.asc\"\r\n\r\n{}", String::from_utf8_lossy(&ciphertext)).into_bytes());
        }
        Ok([headers, entity].concat())
    }
}

fn validate_listing(listing: &str, key: &str, email: &str, capability: char) -> Result<()> {
    let mut primary = false;
    let mut exact = false;
    let mut valid = false;
    let mut uid = false;
    for line in listing.lines() {
        let fields: Vec<_> = line.split(':').collect();
        if fields.len() < 10 {
            continue;
        }
        match fields[0] {
            "pub" | "sec" => {
                primary = true;
                valid = !matches!(fields[1], "r" | "e" | "d" | "i")
                    && fields
                        .get(11)
                        .is_some_and(|value| value.contains(capability) && !value.contains('D'))
                    && fields[6].parse::<i64>().map_or(true, |expiry| {
                        expiry == 0 || expiry > chrono::Utc::now().timestamp()
                    });
            }
            "sub" | "ssb" => primary = false,
            "fpr" if primary => {
                exact = fields[9] == key;
                primary = false;
            }
            "uid" if !matches!(fields[1], "r" | "e" | "i") => {
                let value = decode_colon(fields[9]);
                if value.contains(['\r', '\n', '\0']) {
                    continue;
                }
                let parsed = mail_parser::MessageParser::default()
                    .parse(&format!("From: {value}\r\n\r\n").into_bytes())
                    .and_then(|m| {
                        m.from()
                            .and_then(|a| a.first())
                            .and_then(|a| a.address())
                            .map(str::to_owned)
                    });
                uid |= parsed.is_some_and(|address| address.eq_ignore_ascii_case(email));
            }
            _ => {}
        }
    }
    if exact && valid && uid {
        Ok(())
    } else {
        Err(error(
            "Key must be valid, match the email address, and support the requested operation",
        ))
    }
}

pub fn decode_colon(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"\\x")
            && i + 3 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 2..i + 4])
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            result.push(byte);
            i += 4;
            continue;
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).into_owned()
}

/// Split transport headers from the complete canonical MIME entity.
fn split_entity(raw: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let at = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| error("Malformed MIME headers"))?;
    let header = std::str::from_utf8(&raw[..at]).map_err(|_| error("Invalid MIME headers"))?;
    let mut outer = String::new();
    let mut content = String::new();
    let mut is_content = false;
    for line in header.split("\r\n") {
        if !line.starts_with([' ', '\t']) {
            is_content = line.to_ascii_lowercase().starts_with("content-");
        }
        let target = if is_content { &mut content } else { &mut outer };
        target.push_str(line);
        target.push_str("\r\n");
    }
    content.push_str("\r\n");
    let entity = [content.as_bytes(), &raw[at + 4..]].concat();
    Ok((outer.into_bytes(), entity))
}
fn multipart(
    kind: &str,
    protocol: &str,
    micalg: Option<&str>,
    first: &[u8],
    second: &[u8],
) -> Vec<u8> {
    let boundary = format!("flectar-{:032x}", rand::random::<u128>());
    let alg = micalg
        .map(|s| format!("; micalg=\"{s}\""))
        .unwrap_or_default();
    let mut out = format!("Content-Type: multipart/{kind}; protocol=\"{protocol}\"{alg}; boundary=\"{boundary}\"\r\n\r\n--{boundary}\r\n").into_bytes();
    out.extend(first);
    out.extend(format!("\r\n--{boundary}\r\n").as_bytes());
    out.extend(second);
    out.extend(format!("\r\n--{boundary}--\r\n").as_bytes());
    out
}

/// Every transport calls this immediately before uploading MIME. Queue-time
/// policy survives setting changes and application restarts without downgrade.
pub async fn protect_draft(
    db: &Db,
    account_id: i64,
    draft_id: i64,
    raw: Vec<u8>,
    recipients: Vec<Address>,
) -> Result<Vec<u8>> {
    let (policy, sender) = db.read(move |conn| {
        let config = repo::accounts::get_config(conn, account_id)?.ok_or_else(|| error("Account not found"))?;
        use rusqlite::OptionalExtension;
        let payload: Option<String> = conn.query_row("SELECT payload FROM pending_actions WHERE message_id=?1 AND kind='send' AND state IN ('pending','inflight') ORDER BY id DESC LIMIT 1", [draft_id], |r| r.get(0)).optional()?;
        let policy = if let Some(payload) = payload {
            let payload: serde_json::Value = serde_json::from_str(&payload)?;
            match payload.get("mailSecurity") { Some(value) => serde_json::from_value(value.clone())?, None => config.settings.security }
        } else { config.settings.security };
        Ok((policy, config.email))
    }).await?;
    Gpg::default()
        .protect(&raw, &policy, &sender, &recipients)
        .await
}

impl Core {
    pub async fn set_mail_security(&self, account_id: i64, mut policy: MailSecurity) -> Result<()> {
        policy.validate()?;
        let config = self
            .db
            .read(move |conn| repo::accounts::get_config(conn, account_id))
            .await?
            .ok_or_else(|| error("Account not found"))?;
        Gpg::default()
            .preflight(&policy, &config.email, &[])
            .await?;
        self.db
            .write(move |conn| {
                let mut config = repo::accounts::get_config(conn, account_id)?
                    .ok_or_else(|| error("Account not found"))?;
                config.settings.security = policy;
                repo::accounts::set_settings(conn, account_id, &config.settings)?;
                Ok(())
            })
            .await
    }
}

pub fn without_bcc(raw: &[u8]) -> Result<Vec<u8>> {
    let at = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| error("Malformed message"))?;
    let mut result = Vec::new();
    let mut skip = false;
    for line in raw[..at].split(|b| *b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if !line.starts_with(b" ") && !line.starts_with(b"\t") {
            skip = line
                .get(..4)
                .is_some_and(|v| v.eq_ignore_ascii_case(b"bcc:"));
        }
        if !skip {
            result.extend(line);
            result.extend(b"\r\n");
        }
    }
    result.extend(b"\r\n");
    result.extend(&raw[at + 4..]);
    Ok(result)
}

pub struct OpenedMessage {
    pub text: String,
    pub status: String,
    /// Authenticated inner MIME, retained only in memory until explicit export.
    pub mime: Vec<u8>,
}
impl Gpg {
    pub async fn open_message(
        &self,
        raw: &[u8],
        expected_fingerprint: Option<&str>,
    ) -> Result<OpenedMessage> {
        use mail_parser::MimeHeaders;
        if raw.len() > MAX_CRYPTO_BYTES {
            return Err(error("Message exceeds the 100 MiB OpenPGP limit"));
        }
        let mut entity = raw.to_vec();
        let mut encrypted = false;
        let mut signature = None;
        for _ in 0..8 {
            if entity.starts_with(b"-----BEGIN PGP MESSAGE-----") {
                let (data, status) = self.run(&["--decrypt".into()], Some(&entity)).await?;
                if !authenticated_decryption(&status) {
                    return Err(error(
                        "Message integrity could not be verified; legacy unauthenticated encryption is not accepted",
                    ));
                }
                if let Some(fpr) = valid_signature(&status) {
                    signature = Some(fpr);
                }
                encrypted = true;
                entity = data;
                continue;
            }
            let parsed = mail_parser::MessageParser::default()
                .parse(entity.as_slice())
                .ok_or_else(|| error("Malformed OpenPGP MIME message"))?;
            let root = parsed
                .parts
                .first()
                .ok_or_else(|| error("Missing MIME part"))?;
            let kind = root
                .content_type()
                .and_then(|ct| ct.subtype())
                .unwrap_or_default();
            let protocol = root
                .content_type()
                .and_then(|ct| ct.attribute("protocol"))
                .unwrap_or_default();
            if kind.eq_ignore_ascii_case("encrypted")
                && protocol.eq_ignore_ascii_case("application/pgp-encrypted")
            {
                let parts = root
                    .sub_parts()
                    .filter(|p| p.len() == 2)
                    .ok_or_else(|| error("Encrypted MIME must have two parts"))?;
                let control = &parsed.parts[parts[0] as usize];
                if !String::from_utf8_lossy(control.contents())
                    .lines()
                    .any(|line| line.trim() == "Version: 1")
                {
                    return Err(error("Unsupported encrypted MIME version"));
                }
                entity = parsed.parts[parts[1] as usize].contents().to_vec();
                continue;
            }
            if kind.eq_ignore_ascii_case("signed")
                && protocol.eq_ignore_ascii_case("application/pgp-signature")
            {
                let parts = root
                    .sub_parts()
                    .filter(|p| p.len() == 2)
                    .ok_or_else(|| error("Signed MIME must have two parts"))?;
                let signed = &parsed.parts[parts[0] as usize];
                let data = &entity[signed.offset_header as usize..signed.offset_end as usize];
                let sig = &parsed.parts[parts[1] as usize];
                let file = tempfile::NamedTempFile::new()?;
                tokio::fs::write(file.path(), sig.contents()).await?;
                let (_, status) = self
                    .run(
                        &[
                            "--verify".into(),
                            file.path().to_string_lossy().into_owned(),
                            "-".into(),
                        ],
                        Some(data),
                    )
                    .await?;
                signature = valid_signature(&status);
                if signature.is_none() {
                    return Err(error("The digital signature could not be verified"));
                }
                entity = data.to_vec();
                break;
            }
            break;
        }
        if !encrypted && signature.is_none() {
            return Err(error(
                "No supported OpenPGP/MIME content found. For signed mail, download the original .eml and open it from OpenPGP settings",
            ));
        }
        let parsed = mail_parser::MessageParser::default()
            .parse(entity.as_slice())
            .ok_or_else(|| error("Decrypted MIME could not be parsed"))?;
        let text = parsed
            .body_text(0)
            .map(|s| s.into_owned())
            .unwrap_or_default();
        let attachment_count = parsed.attachments().count();
        let protection = if encrypted {
            "Decrypted; integrity verified."
        } else {
            "Not encrypted."
        };
        let signature_status = match signature {
            Some(fpr)
                if expected_fingerprint
                    .is_some_and(|expected| expected.eq_ignore_ascii_case(&fpr)) =>
            {
                format!("Valid signature from the verified key {fpr}.")
            }
            Some(fpr) => format!(
                "Valid signature from {fpr}. The sender's identity has not been verified against this key."
            ),
            None => "No verified digital signature; sender identity is unverified.".to_owned(),
        };
        Ok(OpenedMessage {
            text,
            status: format!(
                "{protection} {signature_status} {attachment_count} attachment(s); export the decrypted message to open them. Outer subject and addresses are not authenticated by this signature."
            ),
            mime: entity,
        })
    }
}
fn invalid_signature(status: &str) -> bool {
    status.lines().any(|s| {
        ["BADSIG", "ERRSIG", "EXPKEYSIG", "REVKEYSIG", "EXPSIG"]
            .iter()
            .any(|bad| {
                s.strip_prefix("[GNUPG:] ")
                    .and_then(|s| s.split_whitespace().next())
                    == Some(*bad)
            })
    })
}
fn valid_signature(status: &str) -> Option<String> {
    if invalid_signature(status) {
        return None;
    }
    status
        .lines()
        .find_map(|s| s.strip_prefix("[GNUPG:] VALIDSIG "))
        .and_then(|s| {
            let fields: Vec<_> = s.split_whitespace().collect();
            fingerprint(fields.get(9).or_else(|| fields.first()).copied()?, false).ok()
        })
}
impl Core {
    pub async fn open_openpgp_message(&self, message_id: i64) -> Result<OpenedMessage> {
        let (detail, path, expected) = self
            .db
            .read(move |conn| {
                let detail = repo::messages::detail(conn, message_id)?;
                let path = repo::messages::get_row(conn, message_id)?.and_then(|r| r.raw_path);
                let config = repo::accounts::get_config(conn, detail.account_id)?
                    .ok_or_else(|| error("Account not found"))?;
                let expected = config
                    .settings
                    .security
                    .recipient_keys
                    .get(&detail.from.email.to_lowercase())
                    .cloned();
                Ok((detail, path, expected))
            })
            .await?;
        let raw = if let Some(path) = path {
            crate::file_io::read(path, MAX_CRYPTO_BYTES, "OpenPGP message").await?
        } else if let Some(attachment) = detail.attachments.iter().find(|a| {
            a.filename
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case("encrypted.asc"))
        }) {
            let path = self.get_attachment(attachment.id).await?;
            crate::file_io::read(path, MAX_CRYPTO_BYTES, "OpenPGP payload").await?
        } else {
            return Err(error(
                "Original MIME is unavailable. Download the original .eml from your provider and use Open message file in OpenPGP settings",
            ));
        };
        Gpg::default().open_message(&raw, expected.as_deref()).await
    }
}

fn authenticated_decryption(status: &str) -> bool {
    let has = |tag: &str| status.lines().any(|line| line == format!("[GNUPG:] {tag}"));
    let aead = status
        .lines()
        .filter_map(|s| s.strip_prefix("[GNUPG:] DECRYPTION_INFO "))
        .any(|s| s.split_whitespace().nth(2).is_some_and(|v| v != "0"));
    has("DECRYPTION_OKAY")
        && has("END_DECRYPTION")
        && (has("GOODMDC") || aead)
        && !has("BADMDC")
        && !has("DECRYPTION_FAILED")
}

#[cfg(test)]
mod tests {
    use super::*;
    use mail_parser::MimeHeaders;
    #[test]
    fn validates_fingerprints_and_bcc_folding() {
        assert!(!authenticated_decryption(
            "[GNUPG:] DECRYPTION_OKAY\n[GNUPG:] END_DECRYPTION\n"
        ));
        assert!(!authenticated_decryption(
            "[GNUPG:] DECRYPTION_OKAY\n[GNUPG:] GOODMDC\n[GNUPG:] END_DECRYPTION\n[GNUPG:] BADMDC\n"
        ));
        assert!(fingerprint("DEADBEEF", false).is_err());
        assert!(fingerprint(&"a".repeat(40), false).is_ok());
        assert!(fingerprint(&format!("--{}", "a".repeat(38)), false).is_err());
        let raw = b"From: a@example.org\r\nBcc: hidden@example.org,\r\n another@example.org\r\nSubject: x\r\nContent-Type: text/plain\r\n\r\nBcc: body text stays\r\n";
        let stripped = String::from_utf8(without_bcc(raw).unwrap()).unwrap();
        assert!(!stripped.contains("hidden@example.org"));
        assert!(!stripped.contains("another@example.org"));
        assert!(stripped.contains("Bcc: body text stays"));
        let (header, entity) = split_entity(raw).unwrap();
        assert!(!String::from_utf8_lossy(&header).contains("Content-Type"));
        assert!(entity.starts_with(b"Content-Type: text/plain\r\n\r\n"));
    }
    #[test]
    fn rejects_revoked_expired_wrong_identity_and_untrusted_bindings() {
        let fpr = "A".repeat(40);
        let listing = format!(
            "pub:u:255:22:KEY:0:0::u:::scESC:\nfpr:::::::::{fpr}:\nuid:u::::::::Alice <alice@example.org>:\n"
        );
        assert!(validate_listing(&listing, &fpr, "alice@example.org", 'E').is_ok());
        assert!(validate_listing(&listing, &fpr, "mallory@example.org", 'E').is_err());
        assert!(
            validate_listing(
                &listing.replace("pub:u", "pub:r"),
                &fpr,
                "alice@example.org",
                'E'
            )
            .is_err()
        );
        assert!(
            validate_listing(
                &listing.replace(":KEY:0:0:", ":KEY:0:1:"),
                &fpr,
                "alice@example.org",
                'E'
            )
            .is_err()
        );
        assert!(
            valid_signature(&format!(
                "[GNUPG:] REVKEYSIG key uid\n[GNUPG:] VALIDSIG {fpr} 0 0 0 0 0 0 0 0 {fpr}\n"
            ))
            .is_none()
        );
    }
    async fn generate(gpg: &Gpg, uid: &str) -> String {
        let args: Vec<String> = [
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-generate-key",
            uid,
            "ed25519",
            "sign",
            "1d",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let (_, status) = gpg.run(&args, None).await.unwrap();
        let fpr = status
            .lines()
            .find_map(|l| l.strip_prefix("[GNUPG:] KEY_CREATED "))
            .and_then(|s| s.split_whitespace().nth(1))
            .unwrap()
            .to_owned();
        let args: Vec<String> = [
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-add-key",
            &fpr,
            "cv25519",
            "encrypt",
            "1d",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        gpg.run(&args, None).await.unwrap();
        fpr
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn generation_uses_pinentry_and_creates_signing_and_encryption_keys() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        // Test-only pinentry implements the Assuan dialog protocol. The app
        // still uses its normal production command, with no passphrase args.
        let pinentry = home.path().join("test-pinentry");
        std::fs::write(&pinentry, "#!/bin/sh\nprintf 'OK ready\\n'\nwhile IFS= read -r line; do\n case \"$line\" in\n GETPIN*) printf 'D test-only-passphrase\\nOK\\n' ;;\n BYE*) printf 'OK\\n'; exit 0 ;;\n *) printf 'OK\\n' ;;\n esac\ndone\n").unwrap();
        std::fs::set_permissions(&pinentry, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(
            home.path().join("gpg-agent.conf"),
            format!("pinentry-program {}\n", pinentry.display()),
        )
        .unwrap();
        let gpg = Gpg {
            home: Some(home.path().to_owned()),
        };
        let key = gpg.generate_key("generated@example.org").await.unwrap();
        gpg.check_key(&key, "generated@example.org", true, 'S')
            .await
            .unwrap();
        gpg.check_key(&key, "generated@example.org", true, 'E')
            .await
            .unwrap();
        assert!(
            gpg.export_public_key(&key)
                .await
                .unwrap()
                .starts_with(b"-----BEGIN PGP PUBLIC KEY BLOCK-----")
        );
    }

    /// Requires GnuPG, and never touches the developer's normal keyring.
    #[tokio::test]
    async fn openpgp_mime_roundtrip_and_tampering() {
        let home = tempfile::tempdir().unwrap();
        let gpg = Gpg {
            home: Some(home.path().to_owned()),
        };
        let alice = generate(&gpg, "Alice <alice@example.org>").await;
        let bob = generate(&gpg, "Bob <bob@example.org>").await;
        let recipients = vec![Address {
            name: None,
            email: "bob@example.org".into(),
        }];
        let raw = mail_builder::MessageBuilder::new()
            .from("alice@example.org")
            .to("bob@example.org")
            .subject("Visible subject")
            .text_body("Private message: café")
            .attachment("text/plain", "secret.txt", b"private attachment".to_vec())
            .write_to_vec()
            .unwrap();
        let mut policy = MailSecurity {
            signing_fingerprint: alice.clone(),
            sign_by_default: true,
            require_encryption: true,
            recipient_keys: BTreeMap::from([("bob@example.org".into(), bob.clone())]),
        };
        let protected = gpg
            .protect(&raw, &policy, "alice@example.org", &recipients)
            .await
            .unwrap();
        let wire = String::from_utf8_lossy(&protected);
        assert!(wire.contains("multipart/encrypted"));
        assert!(wire.contains("Visible subject"));
        assert!(!wire.contains("Private message"));
        assert!(!wire.contains("private attachment"));
        let mut corrupted = protected.clone();
        let armor = b"-----BEGIN PGP MESSAGE-----";
        let start = corrupted
            .windows(armor.len())
            .position(|w| w == armor)
            .unwrap()
            + armor.len();
        let pos = (start..corrupted.len())
            .find(|&i| corrupted[i].is_ascii_alphanumeric())
            .unwrap();
        corrupted[pos] = if corrupted[pos] == b'A' { b'B' } else { b'A' };
        assert!(gpg.open_message(&corrupted, Some(&alice)).await.is_err());
        let opened = gpg.open_message(&protected, Some(&alice)).await.unwrap();
        assert!(opened.text.contains("café"));
        assert!(opened.status.contains("verified key"));
        let parsed = mail_parser::MessageParser::default()
            .parse(opened.mime.as_slice())
            .unwrap();
        let attachment = parsed.attachments().next().unwrap();
        assert_eq!(attachment.attachment_name(), Some("secret.txt"));
        assert_eq!(attachment.contents(), b"private attachment");
        // An independently imported recipient secret key can decrypt too.
        let (secret, _) = gpg
            .run(
                &["--armor".into(), "--export-secret-keys".into(), bob],
                None,
            )
            .await
            .unwrap();
        let other_home = tempfile::tempdir().unwrap();
        let receiver = Gpg {
            home: Some(other_home.path().to_owned()),
        };
        receiver.import_key(&secret).await.unwrap();
        receiver
            .import_key(&gpg.export_public_key(&alice).await.unwrap())
            .await
            .unwrap();
        assert!(
            receiver
                .open_message(&protected, None)
                .await
                .unwrap()
                .status
                .contains("has not been verified")
        );
        policy.require_encryption = false;
        let signed = gpg
            .protect(&raw, &policy, "alice@example.org", &recipients)
            .await
            .unwrap();
        assert!(gpg.open_message(&signed, Some(&alice)).await.is_ok());
        let tampered = String::from_utf8(signed)
            .unwrap()
            .replace("Private message", "Altered message");
        assert!(
            gpg.open_message(tampered.as_bytes(), Some(&alice))
                .await
                .is_err()
        );
        policy.require_encryption = true;
        policy.recipient_keys.clear();
        assert!(
            gpg.protect(&raw, &policy, "alice@example.org", &recipients)
                .await
                .is_err()
        );
        policy
            .recipient_keys
            .insert("bob@example.org".into(), alice);
        assert!(
            gpg.protect(&raw, &policy, "alice@example.org", &recipients)
                .await
                .is_err()
        );
        assert!(gpg.open_message(&raw, None).await.is_err());
    }

    #[tokio::test]
    async fn expired_signer_never_releases_successfully_decrypted_plaintext() {
        let home = tempfile::tempdir().unwrap();
        let gpg = Gpg {
            home: Some(home.path().to_owned()),
        };
        let key = generate(&gpg, "Alice <alice@example.org>").await;
        let (encrypted, _) = gpg
            .run(
                &[
                    "--armor".into(),
                    "--trust-model".into(),
                    "always".into(),
                    "--local-user".into(),
                    key.clone(),
                    "--recipient".into(),
                    key,
                    "--sign".into(),
                    "--encrypt".into(),
                ],
                Some(b"Content-Type: text/plain\r\n\r\nPrivate message"),
            )
            .await
            .unwrap();
        // The test key expires after one day. GnuPG still decrypts at this
        // later time and reports EXPKEYSIG rather than a decryption failure.
        let future = (chrono::Utc::now().timestamp() + 2 * 24 * 60 * 60).to_string();
        let error = gpg
            .run(
                &["--faked-system-time".into(), future, "--decrypt".into()],
                Some(&encrypted),
            )
            .await
            .expect_err("an expired signature must fail closed");
        assert!(
            error
                .to_string()
                .contains("signature is invalid, expired, or revoked"),
            "{error}"
        );
    }
}
