//! Asset Store stores short encrypted values; generations make large values
//! crash safe without putting token or OpenPGP secret material in files.
use flectar_mail::flectar_mail_core::{
    accounts::credentials::{CredentialStore, Slot},
    error::{CoreError, Result},
    mail_security::OpenPgpKeyStore,
};
use std::sync::{Mutex, OnceLock};
use zeroize::{Zeroize, Zeroizing};

const CHUNK: usize = 1000;
const MAX_VALUE: usize = 1024 * 1024;
static LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(serde::Serialize, serde::Deserialize)]
struct Manifest {
    generation: String,
    chunks: usize,
    bytes: usize,
}

#[derive(Default)]
pub struct AssetStore;
impl AssetStore {
    fn key(name: &str) -> String {
        format!("feihong:v1:{name}")
    }
    fn read_unlocked(name: &str) -> Result<Option<Vec<u8>>> {
        let Some(mut head) = native::get(&Self::key(name))? else {
            return Ok(None);
        };
        let parsed = serde_json::from_slice::<Manifest>(&head);
        head.zeroize();
        let manifest = parsed.map_err(|_| CoreError::Other("Asset manifest is corrupt".into()))?;
        if manifest.bytes > MAX_VALUE
            || manifest.chunks != manifest.bytes.div_ceil(CHUNK)
            || uuid::Uuid::parse_str(&manifest.generation).is_err()
        {
            return Err(CoreError::Other("Asset manifest bounds are invalid".into()));
        }
        let mut value = Zeroizing::new(Vec::with_capacity(manifest.bytes));
        for index in 0..manifest.chunks {
            let mut chunk = native::get(&Self::key(&format!(
                "{name}:{}:{index}",
                manifest.generation
            )))?
            .ok_or_else(|| CoreError::Other("Asset chunk is missing".into()))?;
            if chunk.len() > CHUNK {
                chunk.zeroize();
                value.zeroize();
                return Err(CoreError::Other("Asset chunk is corrupt".into()));
            }
            value.extend_from_slice(&chunk);
            chunk.zeroize();
        }
        if value.len() != manifest.bytes {
            value.zeroize();
            return Err(CoreError::Other("Asset value length is invalid".into()));
        }
        Ok(Some(std::mem::take(&mut *value)))
    }
    fn remove_generation(name: &str, manifest: &Manifest) {
        if manifest.chunks <= MAX_VALUE.div_ceil(CHUNK)
            && uuid::Uuid::parse_str(&manifest.generation).is_ok()
        {
            for index in 0..manifest.chunks {
                let _ = native::remove(&Self::key(&format!(
                    "{name}:{}:{index}",
                    manifest.generation
                )));
            }
        }
    }
    fn write_unlocked(name: &str, value: &[u8]) -> Result<()> {
        if value.len() > MAX_VALUE {
            return Err(CoreError::Other("Asset value exceeds 1 MiB".into()));
        }
        let old = native::get(&Self::key(name))?
            .and_then(|v| serde_json::from_slice::<Manifest>(&v).ok());
        let manifest = Manifest {
            generation: uuid::Uuid::new_v4().to_string(),
            chunks: value.len().div_ceil(CHUNK),
            bytes: value.len(),
        };
        let write = (|| {
            for index in 0..manifest.chunks {
                let start = (index * CHUNK).min(value.len());
                native::put(
                    &Self::key(&format!("{name}:{}:{index}", manifest.generation)),
                    &value[start..(start + CHUNK).min(value.len())],
                )?;
            }
            native::put(
                &Self::key(name),
                &serde_json::to_vec(&manifest).map_err(|e| CoreError::Other(e.to_string()))?,
            )
        })();
        if write.is_err() {
            Self::remove_generation(name, &manifest);
        } else if let Some(old) = old {
            Self::remove_generation(name, &old);
        }
        write
    }
    fn delete_unlocked(name: &str) -> Result<()> {
        let manifest = native::get(&Self::key(name))?
            .and_then(|v| serde_json::from_slice::<Manifest>(&v).ok());
        native::remove(&Self::key(name))?;
        if let Some(manifest) = manifest {
            Self::remove_generation(name, &manifest);
        }
        Ok(())
    }
    fn key_index() -> Result<Vec<String>> {
        match Self::read_unlocked("openpgp-index")? {
            None => Ok(Vec::new()),
            Some(value) => serde_json::from_slice(&value)
                .map_err(|_| CoreError::Other("OpenPGP Asset index is corrupt".into())),
        }
    }
}

impl CredentialStore for AssetStore {
    fn store(&self, account_id: i64, slot: Slot, secret: &str) -> Result<()> {
        let _guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        Self::write_unlocked(
            &format!("account:{account_id}:{}", slot.as_str()),
            secret.as_bytes(),
        )
    }
    fn load(&self, account_id: i64, slot: Slot) -> Result<String> {
        let _guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut bytes = Self::read_unlocked(&format!("account:{account_id}:{}", slot.as_str()))?
            .ok_or(CoreError::NeedsReauth)?;
        let result = String::from_utf8(bytes.clone())
            .map_err(|_| CoreError::Other("Asset credential is not UTF-8".into()));
        bytes.zeroize();
        result
    }
    fn delete(&self, account_id: i64, slot: Slot) -> Result<()> {
        let _guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        Self::delete_unlocked(&format!("account:{account_id}:{}", slot.as_str()))
    }
    fn delete_all(&self, account_id: i64) -> Result<()> {
        for slot in [
            Slot::Password,
            Slot::RefreshToken,
            Slot::AccessToken,
            Slot::OAuthClientId,
            Slot::OAuthClientSecret,
            Slot::OAuthBundle,
            Slot::AiApiKey,
            Slot::CaldavPassword,
            Slot::CarddavPassword,
        ] {
            CredentialStore::delete(self, account_id, slot)?;
        }
        Ok(())
    }
}
impl OpenPgpKeyStore for AssetStore {
    fn load(&self, fingerprint: &str) -> Result<Option<Vec<u8>>> {
        let _guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        Self::read_unlocked(&format!("openpgp:{fingerprint}"))
    }
    fn store(&self, fingerprint: &str, certificate: &[u8]) -> Result<()> {
        if fingerprint.is_empty()
            || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
            || fingerprint.len() > 64
        {
            return Err(CoreError::Other("Invalid OpenPGP fingerprint".into()));
        }
        let _guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut keys = Self::key_index()?;
        // Publish the index first; an interrupted import can be retried and never
        // leaves an unlisted private key. Readers tolerate an index without a value.
        if !keys.iter().any(|key| key == fingerprint) {
            keys.push(fingerprint.into());
            Self::write_unlocked(
                "openpgp-index",
                &serde_json::to_vec(&keys).map_err(|e| CoreError::Other(e.to_string()))?,
            )?;
        }
        Self::write_unlocked(&format!("openpgp:{fingerprint}"), certificate)
    }
    fn delete(&self, fingerprint: &str) -> Result<()> {
        let _guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        Self::delete_unlocked(&format!("openpgp:{fingerprint}"))?;
        let mut keys = Self::key_index()?;
        keys.retain(|key| key != fingerprint);
        Self::write_unlocked(
            "openpgp-index",
            &serde_json::to_vec(&keys).map_err(|e| CoreError::Other(e.to_string()))?,
        )
    }
    fn list(&self) -> Result<Vec<String>> {
        let _guard = LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut keys = Vec::new();
        for key in Self::key_index()? {
            if let Some(mut certificate) = Self::read_unlocked(&format!("openpgp:{key}"))? {
                certificate.zeroize();
                keys.push(key);
            }
        }
        Ok(keys)
    }
}

#[cfg(target_env = "ohos")]
mod native {
    use super::*;
    use std::ptr;
    const SECRET: u32 = 0x30000001;
    const ALIAS: u32 = 0x30000002;
    const ACCESS: u32 = 0x20000003;
    const SYNC: u32 = 0x20000010;
    const RETURN: u32 = 0x20000040;
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Blob {
        size: u32,
        data: *mut u8,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    union Value {
        number: u32,
        boolean: bool,
        blob: Blob,
    }
    #[repr(C)]
    struct Attr {
        tag: u32,
        value: Value,
    }
    #[repr(C)]
    struct Record {
        count: u32,
        attrs: *mut Attr,
    }
    #[repr(C)]
    struct Records {
        count: u32,
        results: *mut Record,
    }
    #[link(name = "asset_ndk.z")]
    unsafe extern "C" {
        fn OH_Asset_Add(attrs: *const Attr, count: u32) -> i32;
        fn OH_Asset_Update(
            query: *const Attr,
            count: u32,
            attrs: *const Attr,
            attrs_count: u32,
        ) -> i32;
        fn OH_Asset_Remove(query: *const Attr, count: u32) -> i32;
        fn OH_Asset_Query(query: *const Attr, count: u32, result: *mut Records) -> i32;
        fn OH_Asset_FreeResultSet(result: *mut Records);
    }
    fn blob(tag: u32, value: &[u8]) -> Attr {
        Attr {
            tag,
            value: Value {
                blob: Blob {
                    size: value.len() as u32,
                    data: value.as_ptr().cast_mut(),
                },
            },
        }
    }
    fn number(tag: u32, value: u32) -> Attr {
        Attr {
            tag,
            value: Value { number: value },
        }
    }
    fn error(code: i32) -> CoreError {
        CoreError::CredentialStoreUnavailable(format!("Asset Store returned {code}"))
    }
    pub fn get(alias: &str) -> Result<Option<Vec<u8>>> {
        let query = [blob(ALIAS, alias.as_bytes()), number(RETURN, 0)];
        let mut records = Records {
            count: 0,
            results: ptr::null_mut(),
        };
        let status = unsafe { OH_Asset_Query(query.as_ptr(), query.len() as u32, &mut records) };
        if status == 24000002 {
            return Ok(None);
        }
        if status != 0 {
            return Err(error(status));
        }
        let value = unsafe {
            if records.count == 1 && !records.results.is_null() {
                let record = &*records.results;
                if !record.attrs.is_null() {
                    std::slice::from_raw_parts(record.attrs, record.count as usize)
                        .iter()
                        .find(|a| a.tag == SECRET)
                        .and_then(|a| {
                            let value = a.value.blob;
                            (value.size <= 1024 && !value.data.is_null()).then(|| {
                                std::slice::from_raw_parts(value.data, value.size as usize).to_vec()
                            })
                        })
                } else {
                    None
                }
            } else {
                None
            }
        };
        unsafe { OH_Asset_FreeResultSet(&mut records) };
        value
            .map(Some)
            .ok_or_else(|| CoreError::Other("Asset Store returned no secret".into()))
    }
    pub fn put(alias: &str, value: &[u8]) -> Result<()> {
        let query = [blob(ALIAS, alias.as_bytes())];
        let update = [blob(SECRET, value)];
        let mut status = unsafe { OH_Asset_Update(query.as_ptr(), 1, update.as_ptr(), 1) };
        if status == 24000002 {
            // First-unlocked permits system-scheduled sync after the user has
            // unlocked once. Never sync credentials to another device.
            let attrs = [
                blob(ALIAS, alias.as_bytes()),
                blob(SECRET, value),
                number(ACCESS, 1),
                number(SYNC, 0),
            ];
            status = unsafe { OH_Asset_Add(attrs.as_ptr(), attrs.len() as u32) };
        }
        if status == 0 {
            Ok(())
        } else {
            Err(error(status))
        }
    }
    pub fn remove(alias: &str) -> Result<()> {
        let query = [blob(ALIAS, alias.as_bytes())];
        let status = unsafe { OH_Asset_Remove(query.as_ptr(), 1) };
        if status == 0 || status == 24000002 {
            Ok(())
        } else {
            Err(error(status))
        }
    }
}

#[cfg(not(target_env = "ohos"))]
mod native {
    use super::*;
    pub fn get(_: &str) -> Result<Option<Vec<u8>>> {
        Err(CoreError::CredentialStoreUnavailable(
            "Asset Store requires OHOS".into(),
        ))
    }
    pub fn put(_: &str, _: &[u8]) -> Result<()> {
        Err(CoreError::CredentialStoreUnavailable(
            "Asset Store requires OHOS".into(),
        ))
    }
    pub fn remove(_: &str) -> Result<()> {
        Err(CoreError::CredentialStoreUnavailable(
            "Asset Store requires OHOS".into(),
        ))
    }
}
