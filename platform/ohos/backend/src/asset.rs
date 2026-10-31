//! OpenHarmony Asset Store FFI (secret storage).
//!
//! `libasset_ndk.z.so` — see `asset/asset_api.h` and `asset/asset_type.h` in
//! the OHOS NDK. Only the non-persistent attributes are used (no
//! `ASSET_TAG_IS_PERSISTENT`), which requires no per-app permission.

use core::ptr::null_mut;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

// Tag type bases (`Asset_TagType`).
const ASSET_TYPE_NUMBER: u32 = 0x2 << 28;
const ASSET_TYPE_BYTES: u32 = 0x3 << 28;

// Attribute tags (`Asset_Tag`).
const TAG_SECRET: u32 = ASSET_TYPE_BYTES | 0x01;
const TAG_ALIAS: u32 = ASSET_TYPE_BYTES | 0x02;
const TAG_ACCESSIBILITY: u32 = ASSET_TYPE_NUMBER | 0x03;
const TAG_AUTH_TYPE: u32 = ASSET_TYPE_NUMBER | 0x05;
const TAG_SYNC_TYPE: u32 = ASSET_TYPE_NUMBER | 0x10;
const TAG_RETURN_TYPE: u32 = ASSET_TYPE_NUMBER | 0x40;
const TAG_CONFLICT_RESOLUTION: u32 = ASSET_TYPE_NUMBER | 0x44;

const ACCESSIBILITY_DEVICE_FIRST_UNLOCKED: u32 = 1;
const AUTH_TYPE_NONE: u32 = 0;
const SYNC_TYPE_NEVER: u32 = 0;
const RETURN_ALL: u32 = 0;
const CONFLICT_OVERWRITE: u32 = 0;

const ASSET_SUCCESS: i32 = 0;
const ASSET_NOT_FOUND: i32 = 24000002;
const ASSET_CRYPTO_ERROR: i32 = 24000009;

/// The service rejects empty secrets, but callers store an empty string to
/// clear one (a disconnected calendar, a server without a password). It is
/// kept as a single NUL, which no secret contains.
const EMPTY_SECRET: &str = "\0";

/// Asset Store rejects secrets longer than 1024 bytes. Longer credentials
/// (an OAuth bundle or a large API token) are split at UTF-8 boundaries and
/// stored under generation-suffixed aliases; the main alias keeps a small
/// marker that is written last.
const MAX_CHUNK_BYTES: usize = 1000;
const MAX_CHUNKS: usize = 64;
const CHUNK_MARKER: &str = "\0chunked:";

static CHUNK_GENERATION: AtomicU64 = AtomicU64::new(0);

#[repr(C)]
#[derive(Clone, Copy)]
struct AssetBlob {
    size: u32,
    data: *mut u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
union AssetValue {
    boolean: bool,
    u32_value: u32,
    blob: AssetBlob,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AssetAttr {
    tag: u32,
    value: AssetValue,
}

#[repr(C)]
struct AssetResult {
    count: u32,
    attrs: *mut AssetAttr,
}

#[repr(C)]
struct AssetResultSet {
    count: u32,
    results: *mut AssetResult,
}

#[link(name = "asset_ndk.z", kind = "dylib")]
unsafe extern "C" {
    fn OH_Asset_Add(attributes: *const AssetAttr, attrCnt: u32) -> i32;
    fn OH_Asset_Remove(query: *const AssetAttr, queryCnt: u32) -> i32;
    fn OH_Asset_Query(
        query: *const AssetAttr,
        queryCnt: u32,
        resultSet: *mut AssetResultSet,
    ) -> i32;
    fn OH_Asset_ParseAttr(result: *const AssetResult, tag: u32) -> *mut AssetAttr;
    fn OH_Asset_FreeBlob(blob: *mut AssetBlob);
    fn OH_Asset_FreeResultSet(resultSet: *mut AssetResultSet);
}

fn bytes_attr(tag: u32, bytes: &[u8]) -> AssetAttr {
    AssetAttr {
        tag,
        value: AssetValue {
            blob: AssetBlob {
                size: bytes.len() as u32,
                data: bytes.as_ptr() as *mut u8,
            },
        },
    }
}

fn number_attr(tag: u32, value: u32) -> AssetAttr {
    AssetAttr {
        tag,
        value: AssetValue { u32_value: value },
    }
}

/// Adds `secret` under `alias`, overwriting an existing entry.
fn add(alias: &str, secret: &str) -> Result<(), i32> {
    let alias_bytes = alias.as_bytes();
    let secret_bytes = secret.as_bytes();
    let attrs = [
        bytes_attr(TAG_ALIAS, alias_bytes),
        bytes_attr(TAG_SECRET, secret_bytes),
        number_attr(TAG_ACCESSIBILITY, ACCESSIBILITY_DEVICE_FIRST_UNLOCKED),
        number_attr(TAG_AUTH_TYPE, AUTH_TYPE_NONE),
        number_attr(TAG_SYNC_TYPE, SYNC_TYPE_NEVER),
        number_attr(TAG_CONFLICT_RESOLUTION, CONFLICT_OVERWRITE),
    ];
    // SAFETY: the attrs array borrows `alias_bytes`/`secret_bytes` for the
    // duration of this synchronous call; the service copies them.
    let rc = unsafe { OH_Asset_Add(attrs.as_ptr(), attrs.len() as u32) };
    if rc == ASSET_SUCCESS { Ok(()) } else { Err(rc) }
}

/// Stores (or overwrites) one short secret under `alias`.
fn put_single(alias: &str, secret: &str) -> Result<(), String> {
    let secret = if secret.is_empty() {
        EMPTY_SECRET
    } else {
        secret
    };
    let failed = match add(alias, secret) {
        Ok(()) => return Ok(()),
        Err(rc) => rc,
    };
    // Reinstalling under a different signing identity leaves rows the asset
    // service can no longer decrypt: overwriting one fails with
    // `ASSET_CRYPTO_ERROR`, while queries report it as missing. Do not delete
    // blindly: a readable row must survive a failed overwrite. Only the
    // confirmed-unreadable stale case is recoverable by deleting it.
    if failed != ASSET_CRYPTO_ERROR {
        return Err(format!("asset add failed: {failed}"));
    }
    match get_single(alias) {
        Ok(Some(_)) => {
            return Err(format!(
                "asset add failed: {failed}; the existing entry is readable, refusing to delete it"
            ));
        }
        Ok(None) => {}
        Err(error) => {
            return Err(format!(
                "asset add failed: {failed}; could not verify the existing entry: {error}"
            ));
        }
    }
    let alias_bytes = alias.as_bytes();
    let query = [bytes_attr(TAG_ALIAS, alias_bytes)];
    // SAFETY: the query borrows `alias_bytes` for this synchronous call.
    let rc = unsafe { OH_Asset_Remove(query.as_ptr(), query.len() as u32) };
    if rc != ASSET_SUCCESS && rc != ASSET_NOT_FOUND {
        return Err(format!("asset remove failed: {rc}"));
    }
    crate::log::info(&format!(
        "asset: removed unreadable entry {alias} (crypto error {failed}) and retried"
    ));
    add(alias, secret).map_err(|rc| format!("asset add failed: {rc}"))
}

/// Loads one short secret stored under `alias`. `Ok(None)` when absent.
fn get_single(alias: &str) -> Result<Option<String>, String> {
    let alias_bytes = alias.as_bytes();
    let query = [
        bytes_attr(TAG_ALIAS, alias_bytes),
        number_attr(TAG_RETURN_TYPE, RETURN_ALL),
    ];
    let mut result_set = AssetResultSet {
        count: 0,
        results: null_mut(),
    };
    // SAFETY: synchronous call; the query borrows `alias_bytes`; the service
    // fills `result_set` (freed below).
    let rc = unsafe { OH_Asset_Query(query.as_ptr(), query.len() as u32, &mut result_set) };
    if rc == ASSET_NOT_FOUND {
        return Ok(None);
    }
    if rc != ASSET_SUCCESS {
        return Err(format!("asset query failed: {rc}"));
    }
    if result_set.count == 0 || result_set.results.is_null() {
        // SAFETY: nothing was allocated for an empty result.
        unsafe { OH_Asset_FreeResultSet(&mut result_set) };
        return Ok(None);
    }

    let mut value = None;
    // SAFETY: the service returned at least one result; `ParseAttr` returns a
    // pointer into that result and the blob is copied before being freed.
    unsafe {
        let attr = OH_Asset_ParseAttr(result_set.results, TAG_SECRET);
        if !attr.is_null() {
            let blob: &AssetBlob = &(*attr).value.blob;
            if !blob.data.is_null() && blob.size > 0 {
                let bytes = core::slice::from_raw_parts(blob.data, blob.size as usize);
                value = Some(String::from_utf8_lossy(bytes).into_owned());
            }
            OH_Asset_FreeBlob(&mut (*attr).value.blob as *mut AssetBlob);
        }
        OH_Asset_FreeResultSet(&mut result_set);
    }
    Ok(value.map(|secret| {
        if secret == EMPTY_SECRET {
            String::new()
        } else {
            secret
        }
    }))
}

/// Removes one short secret stored under `alias`. Missing entries succeed
/// (idempotent delete).
fn remove_single(alias: &str) -> Result<(), String> {
    let alias_bytes = alias.as_bytes();
    let query = [bytes_attr(TAG_ALIAS, alias_bytes)];
    // SAFETY: synchronous call; the query borrows `alias_bytes`.
    let rc = unsafe { OH_Asset_Remove(query.as_ptr(), query.len() as u32) };
    if rc == ASSET_SUCCESS || rc == ASSET_NOT_FOUND {
        Ok(())
    } else {
        Err(format!("asset remove failed: {rc}"))
    }
}

/// Splits a secret at UTF-8 boundaries into chunks no longer than `max`
/// bytes. A chunk always contains at least one character.
fn split_secret(secret: &str, max: usize) -> Vec<String> {
    let max = max.max(1);
    let mut chunks = Vec::new();
    let mut current = String::new();
    for ch in secret.chars() {
        if !current.is_empty() && current.len() + ch.len_utf8() > max {
            chunks.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn parse_chunk_marker(value: &str) -> Option<(String, usize)> {
    let rest = value.strip_prefix(CHUNK_MARKER)?;
    let (generation, count) = rest.split_once(':')?;
    let count = count.parse().ok()?;
    if generation.is_empty() || count == 0 || count > MAX_CHUNKS {
        return None;
    }
    Some((generation.to_owned(), count))
}

fn chunk_alias(alias: &str, generation: &str, index: usize) -> String {
    format!("{alias}:chunk:{generation}:{index}")
}

fn new_generation() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let sequence = CHUNK_GENERATION.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}{sequence:x}")
}

fn remove_chunks(alias: &str, generation: &str, count: usize) {
    for index in 0..count {
        let _ = remove_single(&chunk_alias(alias, generation, index));
    }
}

/// Stores (or overwrites) `secret` under `alias`, chunking values the Asset
/// Store would reject for exceeding its 1024-byte limit.
pub fn put(alias: &str, secret: &str) -> Result<(), String> {
    let secret = if secret.is_empty() {
        EMPTY_SECRET
    } else {
        secret
    };
    let old = get_single(alias).ok().flatten();
    let old_chunks = old.as_deref().and_then(parse_chunk_marker);

    if secret.len() <= MAX_CHUNK_BYTES {
        put_single(alias, secret)?;
        if let Some((generation, count)) = old_chunks {
            remove_chunks(alias, &generation, count);
        }
        return Ok(());
    }

    let chunks = split_secret(secret, MAX_CHUNK_BYTES);
    if chunks.len() > MAX_CHUNKS {
        return Err(format!(
            "asset secret is too large ({} chunks; limit {MAX_CHUNKS})",
            chunks.len()
        ));
    }
    let generation = new_generation();
    for (index, chunk) in chunks.iter().enumerate() {
        let chunk_key = chunk_alias(alias, &generation, index);
        if let Err(error) = put_single(&chunk_key, chunk) {
            for done in 0..index {
                let _ = remove_single(&chunk_alias(alias, &generation, done));
            }
            return Err(error);
        }
    }
    // Publish the marker only after every chunk is durable. A failed marker
    // write leaves the previous value readable and cleans the new chunks.
    let marker = format!("{CHUNK_MARKER}{generation}:{}", chunks.len());
    if let Err(error) = put_single(alias, &marker) {
        remove_chunks(alias, &generation, chunks.len());
        return Err(error);
    }
    if let Some((old_generation, old_count)) = old_chunks {
        remove_chunks(alias, &old_generation, old_count);
    }
    Ok(())
}

/// Loads the secret stored under `alias`, reassembling a chunked value.
pub fn get(alias: &str) -> Result<Option<String>, String> {
    let Some(value) = get_single(alias)? else {
        return Ok(None);
    };
    let Some((generation, count)) = parse_chunk_marker(&value) else {
        return Ok(Some(value));
    };
    let mut secret = String::new();
    for index in 0..count {
        let Some(chunk) = get_single(&chunk_alias(alias, &generation, index))? else {
            return Err(format!(
                "asset chunk {index}/{count} is missing for {alias}"
            ));
        };
        secret.push_str(&chunk);
    }
    Ok(Some(secret))
}

/// Removes the secret stored under `alias`, including any chunks.
pub fn remove(alias: &str) -> Result<(), String> {
    if let Ok(Some(value)) = get_single(alias)
        && let Some((generation, count)) = parse_chunk_marker(&value)
    {
        remove_chunks(alias, &generation, count);
    }
    remove_single(alias)
}

/// One `put`/`get`/`remove` cycle; returns `Ok` when the service works.
pub fn self_test() -> Result<(), String> {
    const ALIAS: &str = "flectar:asset-self-test";
    let probe = "flectar-asset-probe-v1";
    put(ALIAS, probe)?;
    let loaded = get(ALIAS)?;
    let remove_result = remove(ALIAS);
    match (loaded.as_deref(), remove_result) {
        (Some(value), Ok(())) if value == probe => {}
        (Some(_), Ok(())) => return Err("asset self-test returned different data".into()),
        (_, Err(error)) => return Err(error),
        (None, Ok(())) => return Err("asset self-test read back nothing".into()),
    }

    // Exercise the chunked path with a value above the Asset Store limit.
    const LONG_ALIAS: &str = "flectar:asset-self-test-long";
    let long_probe = "long-secret-".repeat(200);
    put(LONG_ALIAS, &long_probe)?;
    let loaded = get(LONG_ALIAS)?;
    let remove_result = remove(LONG_ALIAS);
    match (loaded.as_deref(), remove_result) {
        (Some(value), Ok(())) if value == long_probe => Ok(()),
        (Some(_), Ok(())) => Err("chunked asset self-test returned different data".into()),
        (_, Err(error)) => Err(error),
        (None, Ok(())) => Err("chunked asset self-test read back nothing".into()),
    }
}

#[cfg(test)]
mod chunk_tests {
    use super::*;

    #[test]
    fn chunks_round_trip_at_utf8_boundaries() {
        let secret = "é".repeat(1500);
        let chunks = split_secret(&secret, MAX_CHUNK_BYTES);
        assert!(chunks.len() > 1);
        assert!(chunks.iter().all(|chunk| chunk.len() <= MAX_CHUNK_BYTES));
        assert_eq!(chunks.concat(), secret);
    }

    #[test]
    fn marker_round_trips() {
        let marker = format!("{CHUNK_MARKER}abc:3");
        assert_eq!(parse_chunk_marker(&marker), Some(("abc".to_owned(), 3)));
        assert_eq!(parse_chunk_marker("ordinary"), None);
        assert_eq!(parse_chunk_marker(&format!("{CHUNK_MARKER}abc:0")), None);
    }
}
