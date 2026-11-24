//! App keys that follow the user to their other devices of the same kind:
//! the Google and Microsoft app registrations entered in Sign-in settings,
//! so a new phone or computer signs in with the user's own app without
//! typing them again. They go with passwords and sign-ins, under the same
//! switch and sealed under the same keys (sign_ins.rs).
//!
//! Google and Microsoft want a registration per kind of device (an Android
//! client is no use to a desktop app), so the keys of one kind only reach
//! devices of that kind. Keys saved on a device replace the group's; a
//! device whose keys the group lacks shares them; every other device takes
//! on what the group has. Clearing the keys clears them everywhere.

use super::sign_ins::{Sealed, fingerprint, open, seal, this_family};
use super::store;
use crate::Core;
use crate::db::repo::{self, app_keys::Share};
use crate::error::Result;
use crate::events::CoreEvent;
use crate::models::{Provider, Settings};
use flectar_mail_sync_client::{Secrets, SyncClient};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The providers whose app keys travel.
const PROVIDERS: [Provider; 2] = [Provider::Gmail, Provider::Microsoft];

/// One provider's keys as they travel. No client ID means no keys.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Keys {
    client_id: String,
    #[serde(default)]
    client_secret: String,
}

impl Keys {
    fn of(settings: &Settings, provider: Provider) -> Self {
        let (client_id, client_secret) = match provider {
            Provider::Gmail => (&settings.google_client_id, &settings.google_client_secret),
            Provider::Microsoft => (&settings.ms_client_id, &settings.ms_client_secret),
            Provider::Generic => return Self::default(),
        };
        Self {
            client_id: client_id.trim().to_owned(),
            client_secret: client_secret.trim().to_owned(),
        }
        .normalized()
    }

    /// A secret goes only with a client ID.
    fn normalized(mut self) -> Self {
        self.client_id = self.client_id.trim().to_owned();
        self.client_secret = if self.client_id.is_empty() {
            String::new()
        } else {
            self.client_secret.trim().to_owned()
        };
        self
    }

    fn put(self, settings: &mut Settings, provider: Provider) {
        let (client_id, client_secret) = match provider {
            Provider::Gmail => (
                &mut settings.google_client_id,
                &mut settings.google_client_secret,
            ),
            Provider::Microsoft => (&mut settings.ms_client_id, &mut settings.ms_client_secret),
            Provider::Generic => return,
        };
        *client_id = self.client_id;
        *client_secret = self.client_secret;
    }

    fn is_empty(&self) -> bool {
        self.client_id.is_empty()
    }

    /// Keys another device sent that this one can use: none, or a client
    /// ID of the provider's form.
    fn usable(&self, provider: Provider) -> bool {
        crate::oauth::providers::validate_custom_client_id(provider, &self.client_id).is_ok()
    }
}

/// The name a provider's record id comes from: one record per provider for
/// every device of one kind.
fn record_name(provider: Provider, family: &str) -> String {
    format!("appkeys/{}/{family}", provider.as_str())
}

/// The record body of a share, if it has keys to give.
pub(super) fn body(share: &Share) -> Option<Value> {
    let sealed: Value = serde_json::from_str(share.sealed.as_deref()?).ok()?;
    Some(json!({
        "provider": share.provider,
        "family": share.family,
        "sealed": sealed,
    }))
}

/// What an app keys record says.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Record<'a> {
    pub provider: Provider,
    pub family: &'a str,
    /// The sealed keys, as stored.
    pub sealed: String,
}

pub(super) fn parse(body: &Value) -> Option<Record<'_>> {
    let provider = Provider::parse(body.get("provider")?.as_str()?)?;
    if !PROVIDERS.contains(&provider) {
        return None;
    }
    let sealed: Sealed = serde_json::from_value(body.get("sealed")?.clone()).ok()?;
    Some(Record {
        provider,
        family: body.get("family")?.as_str()?,
        sealed: serde_json::to_string(&sealed).ok()?,
    })
}

/// Keys were saved on this device, as `next` has them: those that changed
/// go up in place of the group's. Returns whether any did. A device that
/// doesn't sync yet takes on the group's keys when it joins instead.
pub(crate) fn note_saved(conn: &Connection, previous: &Settings, next: &Settings) -> Result<bool> {
    if store::load_row(conn)?.is_none() {
        return Ok(false);
    }
    let mut changed = false;
    for provider in PROVIDERS {
        if Keys::of(previous, provider) != Keys::of(next, provider) {
            repo::app_keys::mark_fresh(conn, provider.as_str(), this_family())?;
            changed = true;
        }
    }
    Ok(changed)
}

impl Core {
    /// Before an upload: seal the keys saved here since, and keys this
    /// device has that the group lacks.
    pub(super) async fn share_app_keys(&self, client: &SyncClient) {
        let family = this_family();
        let found = self
            .db
            .read(|conn| {
                Ok((
                    repo::settings::get(conn)?,
                    repo::app_keys::all(conn)?,
                    store::adopting(conn)? != Some(false),
                ))
            })
            .await;
        let Ok((settings, shares, adopting)) = found else {
            return;
        };
        if !settings.sync_sign_ins {
            return;
        }
        let mut secrets = None;
        for provider in PROVIDERS {
            let share = shares
                .iter()
                .find(|share| share.provider == provider.as_str() && share.family == family)
                .cloned()
                .unwrap_or_default();
            let keys = Keys::of(&settings, provider);
            // Keys this device has and the group may lack. While a device
            // joins, it waits to see the group's first.
            let offered = !share.fresh
                && share.sealed.is_none()
                && share.arrived.is_none()
                && !keys.is_empty()
                && !adopting;
            if !share.fresh && !offered {
                continue;
            }
            let record_id = match share.record_id {
                Some(id) => id,
                None => match client.named_record_id(&record_name(provider, family)).await {
                    Ok(id) => id,
                    Err(error) => {
                        tracing::debug!(%error, "could not name an app keys record yet");
                        return;
                    }
                },
            };
            if offered {
                let mirrored = self
                    .db
                    .read(move |conn| store::mirrored_entry(conn, &record_id))
                    .await;
                // The group has keys already, or can't be checked now.
                if !matches!(mirrored, Ok(None)) {
                    continue;
                }
            }
            let secrets: &Secrets = match &secrets {
                Some(secrets) => secrets,
                None => secrets.insert(client.secrets().await),
            };
            let Ok(plain) = serde_json::to_vec(&keys) else {
                continue;
            };
            let Ok(digest) = fingerprint(secrets, &plain) else {
                return;
            };
            // Saved again as the group has them.
            let unchanged =
                share.sealed.is_some() && share.fingerprint.as_deref() == Some(digest.as_slice());
            let sealed = if unchanged {
                None
            } else {
                match seal(secrets, &plain) {
                    Ok(sealed) => Some(sealed),
                    Err(error) => {
                        tracing::warn!(%error, "could not share app keys");
                        continue;
                    }
                }
            };
            let saved = self
                .db
                .write(move |conn| {
                    let tx = conn.transaction()?;
                    // Saved again while they were sealed: the next round
                    // shares the newest.
                    if Keys::of(&repo::settings::get(&tx)?, provider) != keys {
                        return Ok(());
                    }
                    match &sealed {
                        Some(sealed) => repo::app_keys::shared(
                            &tx,
                            provider.as_str(),
                            family,
                            &record_id,
                            sealed,
                            &digest,
                        )?,
                        None => repo::app_keys::unmark_fresh(&tx, provider.as_str(), family)?,
                    }
                    tx.commit()?;
                    Ok(())
                })
                .await;
            if let Err(error) = saved {
                tracing::warn!(%error, "could not keep an app keys share");
            }
        }
    }

    /// After a download: put the keys other devices of this kind shared in
    /// this device's settings.
    pub(super) async fn install_app_keys(&self, client: &SyncClient) {
        let family = this_family();
        let found = self
            .db
            .read(|conn| Ok((repo::settings::get(conn)?, repo::app_keys::all(conn)?)))
            .await;
        let Ok((settings, shares)) = found else {
            return;
        };
        if !settings.sync_sign_ins {
            return;
        }
        let arrivals: Vec<Share> = shares
            .into_iter()
            .filter(|share| share.family == family && share.arrived.is_some())
            .collect();
        if arrivals.is_empty() {
            return;
        }
        let secrets = client.secrets().await;
        let mut installed = false;
        for share in arrivals {
            let (Some(provider), Some(sealed)) = (Provider::parse(&share.provider), share.arrived)
            else {
                continue;
            };
            match self.install_app_key(&secrets, provider, sealed).await {
                Ok(changed) => installed |= changed,
                Err(error) => tracing::warn!(%error, "could not take on shared app keys"),
            }
        }
        if installed {
            // Sign-in uses them at once, and the settings show them.
            if let Ok(settings) = self.get_settings().await {
                crate::apply_oauth_settings(&settings);
            }
            self.bus.emit(CoreEvent::SettingsChanged);
        }
    }

    /// Take on one provider's keys that arrived as `sealed`. Returns whether
    /// this device's settings changed.
    async fn install_app_key(
        &self,
        secrets: &Secrets,
        provider: Provider,
        sealed: String,
    ) -> Result<bool> {
        let family = this_family();
        let keys = open(secrets, &sealed)
            .ok()
            .and_then(|plain| serde_json::from_slice::<Keys>(&plain).ok())
            .map(Keys::normalized)
            .filter(|keys| keys.usable(provider));
        let Some(keys) = keys else {
            // Damaged, or not of this provider's form: this device keeps its
            // own.
            self.db
                .write(move |conn| {
                    repo::app_keys::drop_arrival(conn, provider.as_str(), family, &sealed)
                })
                .await?;
            return Ok(false);
        };
        let digest = fingerprint(secrets, &serde_json::to_vec(&keys)?)?;
        self.db
            .write(move |conn| {
                let tx = conn.transaction()?;
                let fresh = repo::app_keys::get(&tx, provider.as_str(), family)?
                    .is_some_and(|share| share.fresh);
                // Saved here meanwhile: this device's keys go up instead.
                if fresh {
                    repo::app_keys::drop_arrival(&tx, provider.as_str(), family, &sealed)?;
                    tx.commit()?;
                    return Ok(false);
                }
                let mut settings = repo::settings::get(&tx)?;
                let changed = Keys::of(&settings, provider) != keys;
                if changed {
                    keys.put(&mut settings, provider);
                    repo::settings::set(&tx, &settings)?;
                }
                repo::app_keys::settle_arrival(&tx, provider.as_str(), family, &sealed, &digest)?;
                tx.commit()?;
                Ok(changed)
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::testutil;

    fn settings_with(google: (&str, &str), microsoft: &str) -> Settings {
        Settings {
            google_client_id: google.0.into(),
            google_client_secret: google.1.into(),
            ms_client_id: microsoft.into(),
            ..Settings::default()
        }
    }

    #[test]
    fn keys_travel_as_the_settings_hold_them() {
        let settings = settings_with((" id.apps.googleusercontent.com ", " secret "), "");
        let keys = Keys::of(&settings, Provider::Gmail);
        assert_eq!(keys.client_id, "id.apps.googleusercontent.com");
        assert_eq!(keys.client_secret, "secret");
        assert!(Keys::of(&settings, Provider::Microsoft).is_empty());

        let plain = serde_json::to_value(&keys).unwrap();
        assert_eq!(
            plain,
            json!({"clientId": "id.apps.googleusercontent.com", "clientSecret": "secret"})
        );
        let mut elsewhere = Settings::default();
        serde_json::from_value::<Keys>(plain)
            .unwrap()
            .put(&mut elsewhere, Provider::Gmail);
        assert_eq!(elsewhere.google_client_id, "id.apps.googleusercontent.com");
        assert_eq!(elsewhere.google_client_secret, "secret");
    }

    #[test]
    fn a_secret_goes_only_with_a_client_id() {
        let keys = Keys::of(&settings_with(("", "left behind"), ""), Provider::Gmail);
        assert_eq!(keys, Keys::default());
    }

    #[test]
    fn only_keys_of_the_providers_form_are_taken_on() {
        let microsoft = |id: &str| Keys {
            client_id: id.into(),
            client_secret: String::new(),
        };
        assert!(microsoft("").usable(Provider::Microsoft), "cleared");
        assert!(microsoft("0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0").usable(Provider::Microsoft));
        assert!(!microsoft("not a guid").usable(Provider::Microsoft));
        assert!(!microsoft("id.apps.googleusercontent.com").usable(Provider::Generic));
    }

    #[test]
    fn records_name_their_provider_and_kind_of_device() {
        let share = Share {
            provider: "microsoft".into(),
            family: "android".into(),
            record_id: Some([1; 16]),
            sealed: Some(r#"{"epoch":2,"data":"AAAA"}"#.into()),
            ..Share::default()
        };
        let body = body(&share).unwrap();
        assert_eq!(
            parse(&body),
            Some(Record {
                provider: Provider::Microsoft,
                family: "android",
                sealed: r#"{"epoch":2,"data":"AAAA"}"#.into(),
            })
        );
        // Nothing to give yet.
        assert_eq!(
            super::body(&Share {
                sealed: None,
                ..share
            }),
            None
        );
        assert_eq!(record_name(Provider::Gmail, "ios"), "appkeys/gmail/ios");
        // Other providers' keys never travel.
        assert_eq!(
            parse(&json!({"provider": "generic", "family": "android",
                "sealed": {"epoch": 1, "data": "AAAA"}})),
            None
        );
        assert_eq!(
            parse(&json!({"provider": "gmail", "family": "android"})),
            None
        );
    }

    #[test]
    fn keys_saved_before_joining_wait_for_the_group() {
        let conn = testutil::conn();
        let previous = Settings::default();
        let next = settings_with(("id.apps.googleusercontent.com", ""), "");
        assert!(!note_saved(&conn, &previous, &next).unwrap());
        assert!(repo::app_keys::all(&conn).unwrap().is_empty());
    }
}
