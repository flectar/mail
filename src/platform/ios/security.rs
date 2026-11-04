//! The iOS Keychain behind [`KeychainCredentialStore`](super::super::keychain::KeychainCredentialStore),
//! through Security.framework's item API.

use super::super::keychain::{Keychain, KeychainError};
use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFRetained, CFString, CFType};
use objc2_security::{
    SecCopyErrorMessageString, SecItemAdd, SecItemCopyMatching, SecItemDelete, SecItemUpdate,
    errSecSuccess, kSecAttrAccessible, kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
    kSecAttrAccount, kSecAttrService, kSecClass, kSecClassGenericPassword, kSecMatchLimit,
    kSecMatchLimitOne, kSecReturnData, kSecValueData,
};
use std::ptr::{NonNull, null_mut};

/// Generic-password items in the app's own Keychain access group.
pub(super) struct SecurityKeychain;

type Pairs<'a> = (Vec<&'a CFString>, Vec<&'a CFType>);

/// The item class and service, and the account when one item is meant.
fn query<'a>(service: &'a CFString, account: Option<&'a CFString>) -> Pairs<'a> {
    // SAFETY: Security.framework defines these constants for the process
    // lifetime.
    let (mut keys, mut values): Pairs<'a> = unsafe {
        (
            vec![kSecClass, kSecAttrService],
            vec![kSecClassGenericPassword, service],
        )
    };
    if let Some(account) = account {
        // SAFETY: as above.
        keys.push(unsafe { kSecAttrAccount });
        values.push(account);
    }
    (keys, values)
}

fn dictionary((keys, values): Pairs<'_>) -> CFRetained<CFDictionary<CFString, CFType>> {
    CFDictionary::from_slices(&keys, &values)
}

fn check(status: i32) -> Result<(), KeychainError> {
    if status == errSecSuccess {
        return Ok(());
    }
    // SAFETY: the reserved argument must be null; the function returns a
    // retained string or nothing.
    let message = unsafe { SecCopyErrorMessageString(status, null_mut()) }
        .map(|message| message.to_string())
        .unwrap_or_default();
    Err(KeychainError { status, message })
}

impl Keychain for SecurityKeychain {
    fn read(&self, service: &str, account: &str) -> Result<Vec<u8>, KeychainError> {
        let (service, account) = (CFString::from_str(service), CFString::from_str(account));
        let (mut keys, mut values) = query(&service, Some(&account));
        // SAFETY: Security.framework constants, as in `query`.
        unsafe {
            keys.extend([kSecReturnData, kSecMatchLimit]);
            values.extend([CFBoolean::new(true) as &CFType, kSecMatchLimitOne]);
        }
        let query = dictionary((keys, values));
        let mut result: *const CFType = std::ptr::null();
        // SAFETY: `query` is a valid dictionary and `result` a valid out
        // pointer. On success the result is a retained CFData we own.
        check(unsafe { SecItemCopyMatching(query.as_opaque(), &mut result) })?;
        let result = NonNull::new(result.cast_mut()).ok_or(KeychainError {
            status: errSecSuccess,
            message: "the Keychain returned no data".into(),
        })?;
        // SAFETY: SecItemCopyMatching follows the Create rule.
        let result = unsafe { CFRetained::from_raw(result) };
        result
            .downcast::<CFData>()
            .map(|data| data.to_vec())
            .map_err(|_| KeychainError {
                status: errSecSuccess,
                message: "the Keychain returned something other than data".into(),
            })
    }

    fn add(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), KeychainError> {
        let (service, account) = (CFString::from_str(service), CFString::from_str(account));
        let secret = CFData::from_bytes(secret);
        let (mut keys, mut values) = query(&service, Some(&account));
        // SAFETY: Security.framework constants, as in `query`.
        unsafe {
            keys.extend([kSecAttrAccessible, kSecValueData]);
            values.push(kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly);
        }
        values.push(&secret);
        // SAFETY: a valid attribute dictionary; no result is requested.
        check(unsafe { SecItemAdd(dictionary((keys, values)).as_opaque(), null_mut()) })
    }

    fn update(&self, service: &str, account: &str, secret: &[u8]) -> Result<(), KeychainError> {
        let (service, account) = (CFString::from_str(service), CFString::from_str(account));
        let secret = CFData::from_bytes(secret);
        let query = dictionary(query(&service, Some(&account)));
        // Rewriting the protection class moves an item that predates it.
        // SAFETY: Security.framework constants, as in `query`.
        let attributes = dictionary(unsafe {
            (
                vec![kSecAttrAccessible, kSecValueData],
                vec![kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly, &*secret],
            )
        });
        // SAFETY: both are valid dictionaries.
        check(unsafe { SecItemUpdate(query.as_opaque(), attributes.as_opaque()) })
    }

    fn delete(&self, service: &str, account: &str) -> Result<(), KeychainError> {
        let (service, account) = (CFString::from_str(service), CFString::from_str(account));
        // SAFETY: a valid query dictionary.
        check(unsafe { SecItemDelete(dictionary(query(&service, Some(&account))).as_opaque()) })
    }

    fn delete_service(&self, service: &str) -> Result<(), KeychainError> {
        let service = CFString::from_str(service);
        // On iOS, SecItemDelete removes every item the query matches.
        // SAFETY: a valid query dictionary.
        check(unsafe { SecItemDelete(dictionary(query(&service, None)).as_opaque()) })
    }
}
