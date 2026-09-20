use flectar_mail_core::{
    Core,
    accounts::credentials::{CredentialStore, CredentialStoreHandle, Slot},
    config::Paths,
    error::{CoreError, Result},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct MemoryCredentials(Mutex<HashMap<(i64, Slot), String>>);

impl CredentialStore for MemoryCredentials {
    fn store(&self, account_id: i64, slot: Slot, secret: &str) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .insert((account_id, slot), secret.into());
        Ok(())
    }

    fn load(&self, account_id: i64, slot: Slot) -> Result<String> {
        self.0
            .lock()
            .unwrap()
            .get(&(account_id, slot))
            .cloned()
            .ok_or_else(|| CoreError::Auth("no stored credential".into()))
    }

    fn delete(&self, account_id: i64, slot: Slot) -> Result<()> {
        self.0.lock().unwrap().remove(&(account_id, slot));
        Ok(())
    }

    fn delete_all(&self, account_id: i64) -> Result<()> {
        self.0
            .lock()
            .unwrap()
            .retain(|(id, _), _| *id != account_id);
        Ok(())
    }
}

async fn seeded_core() -> (tempfile::TempDir, Core) {
    let temp = tempfile::tempdir().unwrap();
    let credentials: CredentialStoreHandle = Arc::new(MemoryCredentials::default());
    let core = Core::start_mail_ui_with_credentials(Paths::for_tests(temp.path()), credentials)
        .await
        .unwrap();
    core.db
        .write(|conn| {
            conn.execute_batch(
                "INSERT INTO accounts
                    (id,email,provider,auth_kind,username,imap_host,imap_port,
                     smtp_host,smtp_port,created_at)
                 VALUES
                    (1,'alice@example.org','imap','password','alice',
                     'imap.example.org',993,'smtp.example.org',465,0),
                    (2,'bob@example.org','imap','password','bob',
                     'imap.example.org',993,'smtp.example.org',465,0)",
            )?;
            Ok(())
        })
        .await
        .unwrap();
    (temp, core)
}

#[tokio::test]
async fn profiles_are_validated_persisted_and_assign_each_account_once() {
    let (_temp, core) = seeded_core().await;

    let (_, work) = core
        .save_mail_profile(None, " Work ".into(), "#3b82f6".into())
        .await
        .unwrap();
    let (_, personal) = core
        .save_mail_profile(None, "Personal".into(), "#DB2777".into())
        .await
        .unwrap();
    assert_eq!(work.name, "Work");
    assert_eq!(work.color, "#3B82F6");
    assert!(
        core.save_mail_profile(None, "work".into(), "#000000".into())
            .await
            .is_err()
    );
    assert!(
        core.save_mail_profile(None, "Invalid".into(), "blue".into())
            .await
            .is_err()
    );

    core.assign_account_profile(1, Some(work.id.clone()))
        .await
        .unwrap();
    let (_, renamed_work) = core
        .save_mail_profile(Some(work.id.clone()), "Office".into(), "#0891B2".into())
        .await
        .unwrap();
    assert_eq!(renamed_work.account_ids, vec![1]);
    assert_eq!(renamed_work.name, "Office");
    let settings = core
        .assign_account_profile(1, Some(personal.id.clone()))
        .await
        .unwrap();
    assert!(
        settings
            .mail_profiles
            .iter()
            .find(|profile| profile.id == work.id)
            .unwrap()
            .account_ids
            .is_empty()
    );
    assert_eq!(
        settings
            .mail_profiles
            .iter()
            .find(|profile| profile.id == personal.id)
            .unwrap()
            .account_ids,
        vec![1]
    );

    let settings = core
        .set_account_color(1, Some("#f97316".into()))
        .await
        .unwrap();
    assert_eq!(
        settings.account_colors.get("1").map(String::as_str),
        Some("#F97316")
    );
    assert!(
        settings
            .mail_profiles
            .iter()
            .all(|profile| !profile.account_ids.contains(&1))
    );
    let settings = core.set_account_color(1, None).await.unwrap();
    assert!(!settings.account_colors.contains_key("1"));
    assert!(
        core.set_account_color(99, Some("#000000".into()))
            .await
            .is_err()
    );
    assert!(
        core.set_account_color(1, Some("orange".into()))
            .await
            .is_err()
    );

    assert!(
        core.assign_account_profile(2, Some("missing".into()))
            .await
            .is_err()
    );

    let settings = core.delete_mail_profile(personal.id).await.unwrap();
    assert_eq!(settings.mail_profiles.len(), 1);
    assert!(
        core.list_accounts()
            .await
            .unwrap()
            .iter()
            .any(|a| a.id == 1)
    );
}

#[tokio::test]
async fn removing_an_account_cleans_local_profile_membership() {
    let (_temp, core) = seeded_core().await;
    let (_, profile) = core
        .save_mail_profile(None, "Work".into(), "#0D9488".into())
        .await
        .unwrap();
    core.assign_account_profile(1, Some(profile.id.clone()))
        .await
        .unwrap();

    core.remove_account(1).await.unwrap();

    let settings = core.get_settings().await.unwrap();
    assert!(
        settings
            .mail_profiles
            .iter()
            .find(|candidate| candidate.id == profile.id)
            .unwrap()
            .account_ids
            .is_empty()
    );
    assert!(
        core.list_accounts()
            .await
            .unwrap()
            .iter()
            .all(|a| a.id != 1)
    );
}
