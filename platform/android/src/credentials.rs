use base64::Engine;
use flectar_mail::flectar_mail_core::accounts::credentials::{CredentialStore, Slot};
use flectar_mail::flectar_mail_core::error::{CoreError, Result};
use jni::{
    JNIEnv, JavaVM,
    objects::{JByteArray, JObject, JValue},
};
use std::{
    collections::HashMap,
    fs::OpenOptions,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

const KEY_ALIAS: &str = "com.flectar.mail.credentials.v1";
const FORMAT_PREFIX: &str = "v1";
const MAX_CREDENTIAL_FILE_BYTES: u64 = 1024 * 1024;

/// Android Keystore backed credential persistence. The AES key is
/// non-exportable; only IV+ciphertext are stored in the app sandbox. A restore
/// to another device therefore fails closed and asks the user to authenticate.
pub(super) struct AndroidCredentialStore {
    vm: JavaVM,
    path: PathBuf,
    transaction: Mutex<()>,
}

impl AndroidCredentialStore {
    pub(super) fn new(app: &slint::android::AndroidApp, private_root: &Path) -> Result<Self> {
        // SAFETY: Android owns the VM for the process and the pointer returned
        // by AndroidApp remains valid for the process lifetime.
        let vm = unsafe { JavaVM::from_raw(app.vm_as_ptr().cast()) }
            .map_err(|error| keystore_error("opening Java VM", error))?;
        Ok(Self {
            vm,
            path: private_root.join("credentials/credentials-v1.json"),
            transaction: Mutex::new(()),
        })
    }

    fn with_env<T>(
        &self,
        operation: impl FnOnce(&mut JNIEnv<'_>) -> jni::errors::Result<T>,
    ) -> Result<T> {
        let mut env = self
            .vm
            .attach_current_thread()
            .map_err(|error| keystore_error("attaching to Android runtime", error))?;
        operation(&mut env).map_err(|error| {
            // Clear a pending Java exception so a single invalidated key does
            // not poison later JNI calls on this worker thread.
            let _ = env.exception_clear();
            keystore_error("using Android Keystore", error)
        })
    }

    fn key<'local>(env: &mut JNIEnv<'local>) -> jni::errors::Result<JObject<'local>> {
        let android_keystore = env.new_string("AndroidKeyStore")?;
        let key_store = env
            .call_static_method(
                "java/security/KeyStore",
                "getInstance",
                "(Ljava/lang/String;)Ljava/security/KeyStore;",
                &[JValue::Object(&android_keystore)],
            )?
            .l()?;
        env.call_method(
            &key_store,
            "load",
            "(Ljava/security/KeyStore$LoadStoreParameter;)V",
            &[JValue::Object(&JObject::null())],
        )?;

        let alias = env.new_string(KEY_ALIAS)?;
        let exists = env
            .call_method(
                &key_store,
                "containsAlias",
                "(Ljava/lang/String;)Z",
                &[JValue::Object(&alias)],
            )?
            .z()?;
        if !exists {
            let algorithm = env.new_string("AES")?;
            let provider = env.new_string("AndroidKeyStore")?;
            let generator = env
                .call_static_method(
                    "javax/crypto/KeyGenerator",
                    "getInstance",
                    "(Ljava/lang/String;Ljava/lang/String;)Ljavax/crypto/KeyGenerator;",
                    &[JValue::Object(&algorithm), JValue::Object(&provider)],
                )?
                .l()?;
            let builder = env.new_object(
                "android/security/keystore/KeyGenParameterSpec$Builder",
                "(Ljava/lang/String;I)V",
                &[JValue::Object(&alias), JValue::Int(1 | 2)],
            )?;

            let gcm = env.new_string("GCM")?;
            let block_modes = env.new_object_array(1, "java/lang/String", JObject::null())?;
            env.set_object_array_element(&block_modes, 0, &gcm)?;
            env.call_method(
                &builder,
                "setBlockModes",
                "([Ljava/lang/String;)Landroid/security/keystore/KeyGenParameterSpec$Builder;",
                &[JValue::Object(&block_modes)],
            )?;

            let no_padding = env.new_string("NoPadding")?;
            let paddings = env.new_object_array(1, "java/lang/String", JObject::null())?;
            env.set_object_array_element(&paddings, 0, &no_padding)?;
            env.call_method(
                &builder,
                "setEncryptionPaddings",
                "([Ljava/lang/String;)Landroid/security/keystore/KeyGenParameterSpec$Builder;",
                &[JValue::Object(&paddings)],
            )?;
            env.call_method(
                &builder,
                "setKeySize",
                "(I)Landroid/security/keystore/KeyGenParameterSpec$Builder;",
                &[JValue::Int(256)],
            )?;
            let spec = env
                .call_method(
                    &builder,
                    "build",
                    "()Landroid/security/keystore/KeyGenParameterSpec;",
                    &[],
                )?
                .l()?;
            env.call_method(
                &generator,
                "init",
                "(Ljava/security/spec/AlgorithmParameterSpec;)V",
                &[JValue::Object(&spec)],
            )?;
            env.call_method(&generator, "generateKey", "()Ljavax/crypto/SecretKey;", &[])?;
        }

        env.call_method(
            key_store,
            "getKey",
            "(Ljava/lang/String;[C)Ljava/security/Key;",
            &[JValue::Object(&alias), JValue::Object(&JObject::null())],
        )?
        .l()
    }

    fn encrypt(&self, plaintext: &[u8]) -> Result<String> {
        self.with_env(|env| {
            let key = Self::key(env)?;
            let transformation = env.new_string("AES/GCM/NoPadding")?;
            let cipher = env
                .call_static_method(
                    "javax/crypto/Cipher",
                    "getInstance",
                    "(Ljava/lang/String;)Ljavax/crypto/Cipher;",
                    &[JValue::Object(&transformation)],
                )?
                .l()?;
            env.call_method(
                &cipher,
                "init",
                "(ILjava/security/Key;)V",
                &[JValue::Int(1), JValue::Object(&key)],
            )?;
            let iv = env.call_method(&cipher, "getIV", "()[B", &[])?.l()?;
            let input = env.byte_array_from_slice(plaintext)?;
            let ciphertext = env
                .call_method(cipher, "doFinal", "([B)[B", &[JValue::Object(&input)])?
                .l()?;
            let iv = env.convert_byte_array(JByteArray::from(iv))?;
            let ciphertext = env.convert_byte_array(JByteArray::from(ciphertext))?;
            Ok(format!(
                "{FORMAT_PREFIX}.{}.{}",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(iv),
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(ciphertext)
            ))
        })
    }

    fn decrypt(&self, encoded: &str) -> Result<Vec<u8>> {
        let mut fields = encoded.split('.');
        if fields.next() != Some(FORMAT_PREFIX) {
            return Err(CoreError::Auth(
                "stored credentials use an unsupported format; sign in again".into(),
            ));
        }
        let iv = fields
            .next()
            .and_then(|value| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(value)
                    .ok()
            })
            .ok_or_else(|| {
                CoreError::Auth("stored credentials are damaged; sign in again".into())
            })?;
        let ciphertext = fields
            .next()
            .and_then(|value| {
                base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(value)
                    .ok()
            })
            .filter(|_| fields.next().is_none())
            .ok_or_else(|| {
                CoreError::Auth("stored credentials are damaged; sign in again".into())
            })?;

        self.with_env(|env| {
            let key = Self::key(env)?;
            let transformation = env.new_string("AES/GCM/NoPadding")?;
            let cipher = env
                .call_static_method(
                    "javax/crypto/Cipher",
                    "getInstance",
                    "(Ljava/lang/String;)Ljavax/crypto/Cipher;",
                    &[JValue::Object(&transformation)],
                )?
                .l()?;
            let iv = env.byte_array_from_slice(&iv)?;
            let params = env.new_object(
                "javax/crypto/spec/GCMParameterSpec",
                "(I[B)V",
                &[JValue::Int(128), JValue::Object(&iv)],
            )?;
            env.call_method(
                &cipher,
                "init",
                "(ILjava/security/Key;Ljava/security/spec/AlgorithmParameterSpec;)V",
                &[
                    JValue::Int(2),
                    JValue::Object(&key),
                    JValue::Object(&params),
                ],
            )?;
            let input = env.byte_array_from_slice(&ciphertext)?;
            let plaintext = env
                .call_method(cipher, "doFinal", "([B)[B", &[JValue::Object(&input)])?
                .l()?;
            env.convert_byte_array(JByteArray::from(plaintext))
        })
        .map_err(|_| {
            CoreError::Auth(
                "secure credentials are unavailable on this device; sign in again".into(),
            )
        })
    }

    fn read_map(&self) -> Result<HashMap<String, String>> {
        match std::fs::File::open(&self.path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_CREDENTIAL_FILE_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(CoreError::from)?;
                if bytes.len() as u64 > MAX_CREDENTIAL_FILE_BYTES {
                    return Err(CoreError::Auth(
                        "stored credentials are damaged; sign in again".into(),
                    ));
                }
                serde_json::from_slice(&bytes).map_err(|_| {
                    CoreError::Auth("stored credentials are damaged; sign in again".into())
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(HashMap::new()),
            Err(error) => Err(error.into()),
        }
    }

    fn write_map(&self, values: &HashMap<String, String>) -> Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| CoreError::Other("credential path has no parent".into()))?;
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join(".credentials-v1.json.tmp");
        let bytes = serde_json::to_vec(values)?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &self.path)?;
        Ok(())
    }

    fn slot_key(account_id: i64, slot: Slot) -> String {
        format!("{account_id}:{}", slot.as_str())
    }
}

impl CredentialStore for AndroidCredentialStore {
    fn store(&self, account_id: i64, slot: Slot, secret: &str) -> Result<()> {
        let _transaction = self.transaction.lock().map_err(|_| {
            CoreError::Other("Android credential transaction lock is unavailable".into())
        })?;
        let encrypted = self.encrypt(secret.as_bytes())?;
        let mut values = self.read_map()?;
        values.insert(Self::slot_key(account_id, slot), encrypted);
        self.write_map(&values)
    }

    fn load(&self, account_id: i64, slot: Slot) -> Result<String> {
        let _transaction = self.transaction.lock().map_err(|_| {
            CoreError::Other("Android credential transaction lock is unavailable".into())
        })?;
        let values = self.read_map()?;
        let encoded = values
            .get(&Self::slot_key(account_id, slot))
            .ok_or_else(|| CoreError::Auth("no stored credential".into()))?;
        String::from_utf8(self.decrypt(encoded)?)
            .map_err(|_| CoreError::Auth("stored credentials are damaged; sign in again".into()))
    }

    fn delete(&self, account_id: i64, slot: Slot) -> Result<()> {
        let _transaction = self.transaction.lock().map_err(|_| {
            CoreError::Other("Android credential transaction lock is unavailable".into())
        })?;
        let mut values = self.read_map()?;
        values.remove(&Self::slot_key(account_id, slot));
        self.write_map(&values)
    }

    fn delete_all(&self, account_id: i64) -> Result<()> {
        let _transaction = self.transaction.lock().map_err(|_| {
            CoreError::Other("Android credential transaction lock is unavailable".into())
        })?;
        let mut values = self.read_map()?;
        let prefix = format!("{account_id}:");
        values.retain(|key, _| !key.starts_with(&prefix));
        self.write_map(&values)
    }
}

fn keystore_error(context: &str, error: impl std::fmt::Display) -> CoreError {
    CoreError::Keyring(format!("{context}: {error}"))
}
