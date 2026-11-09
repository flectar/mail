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
//! end the whole grant once one refresh token is used from two places. Nor
//! does a Google sign-in made through Play services, which leaves the app
//! only a marker that works on its own device.
//!
//! A password goes up whenever it changes here. A sign-in goes up when none
//! is shared yet, when the shared one stopped working, or when the user
//! signs in again here; otherwise each device keeps refreshing its own, so
//! providers that issue a new refresh token on every refresh cause no churn.
//! A device whose provider rejects a sign-in it received marks the record
//! stale, naming the sign-in that failed, and a device with a different
//! working one shares that instead. The one that failed never goes round
//! again, even when it still works where it came from, such as on a device
//! an organization's sign-in rules accept and the receiving one they don't.
//! A sign-in that cannot be checked, offline for instance, is kept: the
//! account's own retries take over.

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

/// A working sign-in the group has goes round again from a device where it
/// works once it is this old: each device refreshes its own, and Microsoft
/// ends a refresh token nobody used for 90 days.
const RESHARE_MS: i64 = 30 * 24 * 60 * 60 * 1000;

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
/// build.rs names it from the target.
pub(crate) fn this_family() -> &'static str {
    env!("FLECTAR_SIGN_IN_FAMILY")
}

/// The family a share of `kind` is for: a password works anywhere.
fn family_of(kind: Kind) -> &'static str {
    match kind {
        Kind::Password => "any",
        Kind::Oauth => this_family(),
    }
}

/// Whether this device can use a share of `kind` for `family` of the account.
pub(super) fn works_here(
    conn: &rusqlite::Connection,
    account_id: i64,
    kind: Kind,
    family: &str,
) -> Result<bool> {
    Ok(repo::accounts::get(conn, account_id)?
        .is_some_and(|account| works_on(kind, family, account.provider, this_family())))
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

/// Whether `secret`, stored in the credential store slot of `kind`, works on
/// another device at all.
fn travels(kind: Kind, secret: &str) -> bool {
    kind == Kind::Password || !crate::oauth::tokens::is_platform_managed(secret)
}

/// Whether a failed check means the provider rejected the sign-in itself,
/// rather than that it could not be reached.
fn is_rejection(error: &CoreError) -> bool {
    match error {
        CoreError::NeedsReauth => true,
        // An OAuth error the token service answered with (RFC 6749 §5.2),
        // such as invalid_client: not a page a network in between served,
        // nor a sign-in that isn't stored here.
        CoreError::Auth(message) => {
            message.starts_with("token refresh failed:") && message.contains("\"error\"")
        }
        _ => false,
    }
}

/// Whether this device's working sign-in, with fingerprint `digest`, should
/// replace what the group has: nothing yet, or a stale one that was not this
/// very sign-in, which a device already could not use.
fn sign_in_wanted(shared: Option<&Record>, digest: &[u8]) -> bool {
    match shared {
        None => true,
        Some(Record {
            sealed: Some(_), ..
        }) => false,
        Some(Record { rejected, .. }) => rejected.as_deref() != Some(digest),
    }
}

/// Whether this device shares its sign-in, with fingerprint `digest`, now:
/// a password that changed here; a sign-in the user made here again, one
/// the group wants (see [`sign_in_wanted`]), or a working one the group has
/// had for so long that its provider may end it.
fn shares_now(
    kind: Kind,
    share: &Share,
    shared: Option<&Record>,
    shared_at: Option<i64>,
    digest: &[u8],
    now: i64,
) -> bool {
    match kind {
        Kind::Password => share.fingerprint.as_deref() != Some(digest),
        Kind::Oauth => {
            share.fresh
                || sign_in_wanted(shared, digest)
                || (shared.is_some_and(|record| record.sealed.is_some())
                    && shared_at.is_some_and(|at| now - at > RESHARE_MS))
        }
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
/// without the group's keys. Keyed by the group's first keys, which every
/// device keeps, so a secret keeps its fingerprint when the keys change.
fn fingerprint(secrets: &Secrets, secret: &[u8]) -> Result<Vec<u8>> {
    let key = secrets
        .roots
        .keys()
        .next()
        .and_then(|epoch| key(secrets, *epoch))
        .ok_or_else(|| CoreError::Other("Flectar Mail Sync has no keys yet".into()))?;
    Ok(flectar_mail_sync_crypto::hkdf_sha256(&key, &[FINGERPRINT_LABEL, secret]).to_vec())
}

/// The record body of a share, if it has anything to say. Every field is
/// there, null when empty.
pub(super) fn body(share: &Share, account_sync_id: &str) -> Option<Value> {
    let (sealed, rejected) = if share.stale {
        let rejected = share
            .fingerprint
            .as_ref()
            .map(|rejected| STANDARD.encode(rejected));
        (Value::Null, json!(rejected))
    } else {
        (
            serde_json::from_str(share.sealed.as_deref()?).ok()?,
            Value::Null,
        )
    };
    Some(json!({
        "account": account_sync_id,
        "kind": share.kind,
        "family": share.family,
        "stale": share.stale,
        "rejected": rejected,
        "sealed": sealed,
    }))
}

/// What a sign-in record says.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Record<'a> {
    pub account: &'a str,
    pub kind: Kind,
    pub family: &'a str,
    /// The sealed secret, as stored; `None` for a stale sign-in.
    pub sealed: Option<String>,
    /// For a stale sign-in, the fingerprint of the one that failed.
    pub rejected: Option<Vec<u8>>,
}

pub(super) fn parse(body: &Value) -> Option<Record<'_>> {
    let stale = body.get("stale").and_then(Value::as_bool) == Some(true);
    let (sealed, rejected) = if stale {
        let rejected = body
            .get("rejected")
            .and_then(Value::as_str)
            .and_then(|digest| STANDARD.decode(digest).ok());
        (None, rejected)
    } else {
        let sealed: Sealed = serde_json::from_value(body.get("sealed")?.clone()).ok()?;
        (Some(serde_json::to_string(&sealed).ok()?), None)
    };
    Some(Record {
        account: body.get("account")?.as_str()?,
        kind: Kind::parse(body.get("kind")?.as_str()?)?,
        family: body.get("family")?.as_str()?,
        sealed,
        rejected,
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
            // sign-in, shares nothing; nor does a sign-in only this device
            // can use.
            let stored = credentials::load_async(
                self.credentials.clone(),
                candidate.id,
                candidate.kind.slot(),
            )
            .await
            .ok()
            .filter(|secret| travels(candidate.kind, secret));
            if let Some(secret) = stored {
                let Ok(digest) = fingerprint(secrets, secret.as_bytes()) else {
                    return;
                };
                let (mirrored, shared_at) = self
                    .db
                    .read(move |conn| {
                        Ok((
                            store::mirrored_entry(conn, &record_id)?,
                            store::mirrored_stamp_ms(conn, &record_id)?,
                        ))
                    })
                    .await
                    .unwrap_or_default();
                let shared = mirrored.as_ref().and_then(|entry| parse(&entry.body));
                if shares_now(
                    candidate.kind,
                    &share,
                    shared.as_ref(),
                    shared_at,
                    &digest,
                    now,
                ) {
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
        if !travels(kind, &secret) {
            return Ok(());
        }
        let digest = fingerprint(secrets, secret.as_bytes())?;
        // The very sign-in that failed here never comes back.
        let rejected = {
            let (kind, family, digest) = (kind.as_str(), arrival.family.clone(), digest.clone());
            self.db
                .read(move |conn| {
                    Ok(
                        repo::sign_ins::share(conn, account_id, kind, &family)?.is_some_and(
                            |share| {
                                share.stale
                                    && share.fingerprint.as_deref() == Some(digest.as_slice())
                            },
                        ),
                    )
                })
                .await?
        };
        if rejected {
            return Ok(());
        }
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
                match self.tokens.access_token(account_id, account.provider).await {
                    Ok(_) => {}
                    // Revoked or expired: never keep it, and say which one
                    // failed, so a device with another that works shares
                    // that instead, and this one never comes back.
                    Err(error) if is_rejection(&error) => {
                        tracing::info!(%error, "a shared sign-in does not work here");
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
                            fingerprint: Some(digest),
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
                    // Not checked, offline for instance: it worked where it
                    // came from, and the account retries like any other.
                    Err(error) => {
                        tracing::debug!(%error, "could not check a shared sign-in yet");
                    }
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
mod shares {
    use super::*;

    #[test]
    fn a_working_sign_in_goes_round_again_before_its_provider_ends_it() {
        let share = Share::default();
        let working = Record {
            account: "aa",
            kind: Kind::Oauth,
            family: "desktop",
            sealed: Some("sealed".into()),
            rejected: None,
        };
        let now = 100 * 24 * 60 * 60 * 1000;
        let recent = Some(now - 24 * 60 * 60 * 1000);
        let old = Some(now - RESHARE_MS - 1);
        assert!(!shares_now(
            Kind::Oauth,
            &share,
            Some(&working),
            recent,
            b"mine",
            now
        ));
        assert!(shares_now(
            Kind::Oauth,
            &share,
            Some(&working),
            old,
            b"mine",
            now
        ));
        // One that failed elsewhere never goes round again, however old.
        let stale = Record {
            sealed: None,
            rejected: Some(b"mine".to_vec()),
            ..working
        };
        assert!(!shares_now(
            Kind::Oauth,
            &share,
            Some(&stale),
            old,
            b"mine",
            now
        ));
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
    fn this_build_names_a_known_kind_of_device() {
        assert!(matches!(
            this_family(),
            "desktop" | "android" | "ios" | "ohos"
        ));
        #[cfg(desktop)]
        assert_eq!(this_family(), "desktop");
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
        // A stale record names the sign-in that failed.
        let failed = Share {
            fingerprint: Some(vec![7; 32]),
            ..stale.clone()
        };
        let body = super::body(&failed, "aa").unwrap();
        assert!(body["sealed"].is_null());
        assert_eq!(parse(&body).unwrap().rejected, Some(vec![7; 32]));
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

    #[test]
    fn a_sign_in_that_failed_never_goes_round_again() {
        let record = |sealed: Option<&str>, rejected: Option<Vec<u8>>| Record {
            account: "aa",
            kind: Kind::Oauth,
            family: "desktop",
            sealed: sealed.map(str::to_owned),
            rejected,
        };
        let mine = [1; 32];
        // Nothing shared yet.
        assert!(sign_in_wanted(None, &mine));
        // A shared one that works stays.
        assert!(!sign_in_wanted(Some(&record(Some("{}"), None)), &mine));
        // Another sign-in failed: this one replaces it.
        assert!(sign_in_wanted(
            Some(&record(None, Some(vec![2; 32]))),
            &mine
        ));
        assert!(sign_in_wanted(Some(&record(None, None)), &mine));
        // This very one failed elsewhere: sharing it again would go round
        // between the two devices for ever.
        assert!(!sign_in_wanted(
            Some(&record(None, Some(mine.to_vec()))),
            &mine
        ));
    }

    #[test]
    fn only_a_provider_rejection_makes_a_sign_in_stale() {
        assert!(is_rejection(&CoreError::NeedsReauth));
        assert!(is_rejection(&CoreError::Auth(
            r#"token refresh failed: {"error":"invalid_client"}"#.into()
        )));
        // A captive portal's or a proxy's page is not the provider.
        assert!(!is_rejection(&CoreError::Auth(
            "token refresh failed: <html><title>Sign in to the network</title>".into()
        )));
        assert!(!is_rejection(&CoreError::Auth(
            "no stored credential".into()
        )));
        assert!(!is_rejection(&CoreError::Offline));
        assert!(!is_rejection(&CoreError::Network(
            "OAuth token service returned 503".into()
        )));
    }

    #[test]
    fn play_services_sign_ins_stay_on_their_device() {
        let bundle = |refresh_token: &str| {
            json!({"version": 1, "refresh_token": refresh_token, "client_id": "client"}).to_string()
        };
        let marker =
            crate::oauth::tokens::platform_google_marker(&["https://mail.google.com/"]).unwrap();
        assert!(!travels(Kind::Oauth, &bundle(&marker)));
        assert!(travels(Kind::Oauth, &bundle("1//refresh-token")));
        // A password is never a bundle.
        assert!(travels(Kind::Password, &marker));
    }
}
