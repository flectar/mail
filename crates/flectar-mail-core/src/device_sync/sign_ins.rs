//! Passwords and sign-ins that follow the user to every device, so a device
//! restored with the recovery code, or paired, has its accounts signed in at
//! once. On unless the user turns it off, for every device of the group.
//!
//! A secret is never readable in the app database. Records carry it sealed
//! under a key derived from the group's root key, which lives in the system
//! credential store with the rest of Flectar Mail Sync's keys, so the sync
//! mirror and outbox only ever hold ciphertext; each device moves it straight
//! into its own credential store. Flectar's server sees it encrypted twice.
//!
//! What travels:
//! - an account's password, to every device;
//! - a Google or Microsoft sign-in (the refresh token with the OAuth app that
//!   issued it), to devices of the same kind only. Google wants a separate
//!   OAuth app per platform and Android signs in to Google through Play
//!   services; Microsoft's Android and iOS apps share one registration.
//!
//! Other OAuth sign-ins never travel: servers such as Fastmail and Stalwart
//! end the whole grant once one refresh token is used from two places.
//!
//! A password goes up whenever it changes here. A sign-in goes up when none
//! is shared yet, when the shared one stopped working, or when the user
//! signs in again here; otherwise each device keeps refreshing its own, so
//! providers that issue a new refresh token on every refresh cause no churn.
//! A device that receives a sign-in it cannot use marks the record stale,
//! and a device with a working one shares it instead.

use super::store;
use crate::Core;
use crate::accounts::credentials::{self, Slot};
use crate::db::repo;
use crate::db::repo::sign_ins::{Arrival, Share};
use crate::error::{CoreError, Result};
use crate::models::{AuthKind, Provider, now_ms};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use flectar_mail_sync_client::{Secrets, SyncClient};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// How long a share goes without reading the credential store again.
const RECHECK_MS: i64 = 24 * 60 * 60 * 1000;

const KEY_LABEL: &[u8] = b"flectar-mail/v1 sign-in";
const FINGERPRINT_LABEL: &[u8] = b"flectar-mail/v1 sign-in fingerprint";

/// What a record carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Password,
    Oauth,
}

impl Kind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Oauth => "oauth",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "password" => Some(Self::Password),
            "oauth" => Some(Self::Oauth),
            _ => None,
        }
    }

    fn slot(self) -> Slot {
        match self {
            Self::Password => Slot::Password,
            Self::Oauth => Slot::OAuthBundle,
        }
    }
}

/// The kind of device this is, for sign-ins that work on one kind only.
pub(crate) fn this_family() -> &'static str {
    if cfg!(target_env = "ohos") {
        "ohos"
    } else if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else {
        "desktop"
    }
}

/// The family a share of `kind` is for: a password works anywhere.
fn family_of(kind: Kind) -> &'static str {
    match kind {
        Kind::Password => "any",
        Kind::Oauth => this_family(),
    }
}

/// Whether a share for `family` works on a device of `here`.
fn works_on(kind: Kind, family: &str, provider: Provider, here: &str) -> bool {
    match kind {
        Kind::Password => true,
        Kind::Oauth => {
            family == here
                || (provider == Provider::Microsoft
                    && matches!(family, "android" | "ios")
                    && matches!(here, "android" | "ios"))
        }
    }
}

/// What the account shares, if anything: its password, or its sign-in with
/// a provider whose sign-ins may be used from several devices.
fn kind_for(auth: AuthKind, provider: Provider) -> Option<Kind> {
    match (auth, provider) {
        (AuthKind::Password, _) => Some(Kind::Password),
        (AuthKind::Oauth2, Provider::Gmail | Provider::Microsoft) => Some(Kind::Oauth),
        (AuthKind::Oauth2, Provider::Generic) => None,
    }
}

/// The name a share's record id comes from.
fn record_name(account_sync_id: &str, kind: Kind, family: &str) -> String {
    format!("signin/{account_sync_id}/{}/{family}", kind.as_str())
}

/// A secret sealed for the group, under the key of one epoch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Sealed {
    epoch: u64,
    data: String,
}

fn key(secrets: &Secrets, epoch: u64) -> Option<[u8; 32]> {
    let root = secrets.roots.get(&epoch)?;
    Some(flectar_mail_sync_crypto::hkdf_sha256(
        root.as_bytes(),
        &[KEY_LABEL, &epoch.to_be_bytes()],
    ))
}

fn newest_key(secrets: &Secrets) -> Result<(u64, [u8; 32])> {
    let epoch = *secrets
        .roots
        .keys()
        .next_back()
        .ok_or_else(|| CoreError::Other("Flectar Mail Sync has no keys yet".into()))?;
    let key = key(secrets, epoch)
        .ok_or_else(|| CoreError::Other("Flectar Mail Sync has no keys yet".into()))?;
    Ok((epoch, key))
}

/// Seal `secret` for the group, under its newest keys.
fn seal(secrets: &Secrets, secret: &[u8]) -> Result<String> {
    let (epoch, key) = newest_key(secrets)?;
    let data = flectar_mail_sync_crypto::seal_file(&key, secret)
        .map_err(|error| CoreError::Other(format!("could not seal a sign-in: {error}")))?;
    Ok(serde_json::to_string(&Sealed {
        epoch,
        data: STANDARD.encode(data),
    })?)
}

/// Open a secret sealed for the group, under whichever epoch's keys.
fn open(secrets: &Secrets, sealed: &str) -> Result<Vec<u8>> {
    let sealed: Sealed = serde_json::from_str(sealed)?;
    let key = key(secrets, sealed.epoch)
        .ok_or_else(|| CoreError::Other("a sign-in is sealed under unknown keys".into()))?;
    let data = STANDARD
        .decode(sealed.data)
        .map_err(|_| CoreError::Other("a sealed sign-in is damaged".into()))?;
    flectar_mail_sync_crypto::open_file(&key, &data)
        .map_err(|_| CoreError::Other("a sealed sign-in does not open".into()))
}

/// A keyed digest of `secret`: shows when it changes, and gives nothing away
/// without the group's keys.
fn fingerprint(secrets: &Secrets, secret: &[u8]) -> Result<Vec<u8>> {
    let (_, key) = newest_key(secrets)?;
    Ok(flectar_mail_sync_crypto::hkdf_sha256(&key, &[FINGERPRINT_LABEL, secret]).to_vec())
}

/// The record body of a share, if it has anything to say.
pub(super) fn body(share: &Share, account_sync_id: &str) -> Option<Value> {
    let mut body = json!({
        "account": account_sync_id,
        "kind": share.kind,
        "family": share.family,
    });
    if share.stale {
        body["stale"] = json!(true);
    } else {
        body["sealed"] = serde_json::from_str(share.sealed.as_deref()?).ok()?;
    }
    Some(body)
}

/// What a sign-in record says.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Record<'a> {
    pub account: &'a str,
    pub kind: Kind,
    pub family: &'a str,
    /// The sealed secret, as stored; `None` for a stale sign-in.
    pub sealed: Option<String>,
}

pub(super) fn parse(body: &Value) -> Option<Record<'_>> {
    let stale = body.get("stale").and_then(Value::as_bool) == Some(true);
    Some(Record {
        account: body.get("account")?.as_str()?,
        kind: Kind::parse(body.get("kind")?.as_str()?)?,
        family: body.get("family")?.as_str()?,
        sealed: if stale {
            None
        } else {
            let sealed: Sealed = serde_json::from_value(body.get("sealed")?.clone()).ok()?;
            Some(serde_json::to_string(&sealed).ok()?)
        },
    })
}

/// A local account that takes part.
struct Candidate {
    id: i64,
    sync_id: String,
    kind: Kind,
    needs_sign_in: bool,
}

impl Core {
    async fn sign_ins_enabled(&self) -> bool {
        self.db
            .read(|conn| Ok(repo::settings::get(conn)?.sync_sign_ins))
            .await
            .unwrap_or(false)
    }

    /// Before an upload: read the credential store for the shares that may
    /// have changed, and seal what goes up.
    pub(super) async fn share_sign_ins(&self, client: &SyncClient) {
        if !self.sign_ins_enabled().await {
            return;
        }
        let now = now_ms();
        let found = self
            .db
            .read(move |conn| {
                let sync_ids: std::collections::HashMap<i64, String> = conn
                    .prepare("SELECT id, sync_id FROM accounts WHERE sync_id IS NOT NULL")?
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()?;
                let candidates = repo::accounts::list(conn)?
                    .into_iter()
                    .filter_map(|account| {
                        Some(Candidate {
                            id: account.id,
                            sync_id: sync_ids.get(&account.id)?.clone(),
                            kind: kind_for(account.auth_kind, account.provider)?,
                            needs_sign_in: account.sync_state == "needs_reauth",
                        })
                    })
                    .collect::<Vec<_>>();
                Ok((candidates, repo::sign_ins::shares(conn)?))
            })
            .await;
        let Ok((candidates, shares)) = found else {
            return;
        };
        let mut secrets = None;
        for candidate in candidates {
            let family = family_of(candidate.kind);
            let mut share = shares
                .iter()
                .find(|share| {
                    share.account_id == candidate.id
                        && share.kind == candidate.kind.as_str()
                        && share.family == family
                })
                .cloned()
                .unwrap_or_else(|| Share {
                    account_id: candidate.id,
                    kind: candidate.kind.as_str().into(),
                    family: family.into(),
                    ..Share::default()
                });
            if !share.fresh && now - share.checked_at < RECHECK_MS {
                continue;
            }
            // A sign-in this device cannot use itself stays here.
            if candidate.kind == Kind::Oauth && candidate.needs_sign_in {
                continue;
            }
            let record_id = match share.record_id {
                Some(id) => id,
                None => match client
                    .named_record_id(&record_name(&candidate.sync_id, candidate.kind, family))
                    .await
                {
                    Ok(id) => id,
                    Err(error) => {
                        tracing::debug!(%error, "could not name a sign-in record yet");
                        return;
                    }
                },
            };
            share.record_id = Some(record_id);
            let secrets = match &secrets {
                Some(secrets) => secrets,
                None => secrets.insert(client.secrets().await),
            };
            // Nothing stored here, such as for an account waiting for its
            // sign-in, shares nothing.
            if let Ok(secret) = credentials::load_async(
                self.credentials.clone(),
                candidate.id,
                candidate.kind.slot(),
            )
            .await
            {
                let Ok(digest) = fingerprint(secrets, secret.as_bytes()) else {
                    return;
                };
                let shared = self
                    .db
                    .read(move |conn| store::mirrored_entry(conn, &record_id))
                    .await
                    .ok()
                    .flatten()
                    .and_then(|entry| parse(&entry.body).map(|record| record.sealed.is_some()));
                let share_now = match candidate.kind {
                    Kind::Password => share.fingerprint.as_deref() != Some(digest.as_slice()),
                    // None shared yet, or the shared one stopped working, or
                    // the user signed in again here.
                    Kind::Oauth => share.fresh || shared != Some(true),
                };
                if share_now {
                    match seal(secrets, secret.as_bytes()) {
                        Ok(sealed) => {
                            share.sealed = Some(sealed);
                            share.fingerprint = Some(digest);
                            share.stale = false;
                        }
                        Err(error) => {
                            tracing::warn!(%error, "could not share a sign-in");
                            continue;
                        }
                    }
                }
            }
            share.fresh = false;
            share.checked_at = now;
            let saved = share.clone();
            if let Err(error) = self
                .db
                .write(move |conn| repo::sign_ins::put_share(conn, &saved))
                .await
            {
                tracing::warn!(%error, "could not keep a sign-in share");
            }
        }
    }

    /// After a download: move the passwords and sign-ins other devices shared
    /// into this device's credential store, and start their accounts.
    pub(super) async fn install_sign_ins(&self, client: &SyncClient) {
        if !self.sign_ins_enabled().await {
            return;
        }
        let Ok(arrivals) = self.db.read(|conn| repo::sign_ins::arrivals(conn)).await else {
            return;
        };
        if arrivals.is_empty() {
            return;
        }
        let secrets = client.secrets().await;
        for arrival in arrivals {
            if let Err(error) = self.install_sign_in(&secrets, &arrival).await {
                tracing::warn!(%error, "could not take on a shared sign-in");
                continue;
            }
            let handled = arrival.clone();
            let _ = self
                .db
                .write(move |conn| repo::sign_ins::drop_arrival(conn, &handled))
                .await;
        }
    }

    async fn install_sign_in(&self, secrets: &Secrets, arrival: &Arrival) -> Result<()> {
        let account_id = arrival.account_id;
        let account = self
            .db
            .read(move |conn| repo::accounts::get(conn, account_id))
            .await?;
        let (Some(account), Some(kind)) = (account, Kind::parse(&arrival.kind)) else {
            return Ok(());
        };
        if !works_on(kind, &arrival.family, account.provider, this_family()) {
            return Ok(());
        }
        let secret = String::from_utf8(open(secrets, &arrival.sealed)?)
            .map_err(|_| CoreError::Other("a shared sign-in is damaged".into()))?;
        let digest = fingerprint(secrets, secret.as_bytes())?;
        let needs_sign_in = account.sync_state == "needs_reauth";
        let current =
            credentials::load_async(self.credentials.clone(), account_id, kind.slot()).await;
        let install = match kind {
            // The newest password is the one that works.
            Kind::Password => current.as_deref().ok() != Some(secret.as_str()),
            // A working sign-in here stays: each device refreshes its own.
            Kind::Oauth => needs_sign_in || current.is_err(),
        };
        if install {
            credentials::store_async(self.credentials.clone(), account_id, kind.slot(), secret)
                .await?;
            if kind == Kind::Oauth {
                self.tokens.invalidate(account_id).await;
                if let Err(error) = self.tokens.access_token(account_id, account.provider).await {
                    // Revoked or expired: never keep it, and say so, so a
                    // device whose sign-in works shares that one instead.
                    tracing::info!(%error, "a shared sign-in no longer works");
                    let _ = credentials::delete_async(
                        self.credentials.clone(),
                        account_id,
                        kind.slot(),
                    )
                    .await;
                    self.tokens.invalidate(account_id).await;
                    let stale = Share {
                        account_id,
                        kind: kind.as_str().into(),
                        family: arrival.family.clone(),
                        record_id: Some(arrival.record_id),
                        stale: true,
                        checked_at: now_ms(),
                        ..Share::default()
                    };
                    self.db
                        .write(move |conn| repo::sign_ins::put_share(conn, &stale))
                        .await?;
                    self.device_sync_now();
                    return Ok(());
                }
            }
        }
        // This device shares what the group has, so it never sends back an
        // older one.
        let adopted = Share {
            account_id,
            kind: kind.as_str().into(),
            family: arrival.family.clone(),
            record_id: Some(arrival.record_id),
            sealed: Some(arrival.sealed.clone()),
            fingerprint: Some(digest),
            fresh: false,
            stale: false,
            checked_at: now_ms(),
        };
        self.db
            .write(move |conn| repo::sign_ins::put_share(conn, &adopted))
            .await?;
        if install && needs_sign_in {
            self.resume_account(account_id).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flectar_mail_sync_client::DeviceIdentity;
    use std::collections::BTreeMap;

    fn secrets(epochs: &[(u64, u8)]) -> Secrets {
        let mut roots = BTreeMap::new();
        for (epoch, byte) in epochs {
            roots.insert(
                *epoch,
                flectar_mail_sync_crypto::RootKey::from_bytes([*byte; 32]),
            );
        }
        Secrets {
            identity: DeviceIdentity::generate("https://mail-sync.example", [3; 16]).unwrap(),
            roots,
        }
    }

    #[test]
    fn a_sealed_sign_in_opens_with_the_groups_keys_only() {
        let group = secrets(&[(1, 1), (2, 2)]);
        let sealed = seal(&group, b"app-password").unwrap();
        assert!(!sealed.contains("app-password"));
        assert_eq!(open(&group, &sealed).unwrap(), b"app-password");
        // Sealed under the newest epoch; an older one still opens.
        let older = seal(&secrets(&[(1, 1)]), b"older").unwrap();
        assert_eq!(open(&group, &older).unwrap(), b"older");
        // Another group's keys do not open it.
        assert!(open(&secrets(&[(2, 9)]), &sealed).is_err());
        // The fingerprint shows a change, and nothing without the keys.
        assert_eq!(
            fingerprint(&group, b"app-password").unwrap(),
            fingerprint(&group, b"app-password").unwrap()
        );
        assert_ne!(
            fingerprint(&group, b"app-password").unwrap(),
            fingerprint(&group, b"another").unwrap()
        );
        assert_ne!(
            fingerprint(&group, b"app-password").unwrap(),
            fingerprint(&secrets(&[(2, 9)]), b"app-password").unwrap()
        );
    }

    #[test]
    fn sign_ins_go_only_where_they_work() {
        assert!(works_on(
            Kind::Password,
            "any",
            Provider::Generic,
            "android"
        ));
        assert!(works_on(Kind::Oauth, "desktop", Provider::Gmail, "desktop"));
        assert!(!works_on(
            Kind::Oauth,
            "desktop",
            Provider::Gmail,
            "android"
        ));
        assert!(!works_on(Kind::Oauth, "android", Provider::Gmail, "ios"));
        assert!(works_on(Kind::Oauth, "android", Provider::Microsoft, "ios"));
        assert!(!works_on(
            Kind::Oauth,
            "desktop",
            Provider::Microsoft,
            "ios"
        ));
        // Other providers' sign-ins stay on their device.
        assert_eq!(kind_for(AuthKind::Oauth2, Provider::Generic), None);
        assert_eq!(
            kind_for(AuthKind::Oauth2, Provider::Gmail),
            Some(Kind::Oauth)
        );
        assert_eq!(
            kind_for(AuthKind::Password, Provider::Generic),
            Some(Kind::Password)
        );
    }

    #[test]
    fn records_carry_sealed_secrets_or_say_they_are_stale() {
        let share = Share {
            account_id: 1,
            kind: "oauth".into(),
            family: "desktop".into(),
            record_id: Some([4; 16]),
            sealed: Some(seal(&secrets(&[(1, 1)]), b"token").unwrap()),
            ..Share::default()
        };
        let body = body(&share, "aa").unwrap();
        let record = parse(&body).unwrap();
        assert_eq!(record.account, "aa");
        assert_eq!(record.kind, Kind::Oauth);
        assert_eq!(record.family, "desktop");
        assert_eq!(record.sealed, share.sealed);
        let stale = Share {
            stale: true,
            sealed: None,
            ..share.clone()
        };
        let body = super::body(&stale, "aa").unwrap();
        assert_eq!(body["stale"], true);
        assert_eq!(parse(&body).unwrap().sealed, None);
        // Nothing to say without a secret.
        assert!(
            super::body(
                &Share {
                    sealed: None,
                    ..share
                },
                "aa"
            )
            .is_none()
        );
    }
}
