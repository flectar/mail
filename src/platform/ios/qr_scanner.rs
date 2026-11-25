//! Scanning a QR code with the camera, to pair devices in Flectar Mail Sync
//! (platform/ios/QrScanner.m). iOS asks for camera access the first time.
use crate::{QrScannerHost, ScanError, ScanFuture, ScanResult, ScanTexts};
use std::ffi::{CStr, CString, c_char, c_int};
use std::sync::{Mutex, MutexGuard, PoisonError};
use tokio::sync::oneshot;

/// The scan waiting for the scanner's answer.
static PENDING: Mutex<Option<oneshot::Sender<ScanResult>>> = Mutex::new(None);

// What a scan ended with, as QrScanner.m numbers it.
const CODE: c_int = 0;
const CLOSED: c_int = 1;
const DENIED: c_int = 2;

unsafe extern "C" {
    fn flectar_qr_can_scan() -> bool;
    fn flectar_qr_scan(
        hint: *const c_char,
        close: *const c_char,
        callback: extern "C" fn(*const c_char, c_int),
    );
}

pub(super) struct IosQrScanner;

impl QrScannerHost for IosQrScanner {
    fn available(&self) -> bool {
        // SAFETY: only asks AVFoundation whether there is a camera.
        unsafe { flectar_qr_can_scan() }
    }

    fn scan(&self, texts: ScanTexts) -> ScanFuture {
        let (sender, receiver) = oneshot::channel();
        // An earlier scan nobody answered ends as closed.
        pending().replace(sender);
        let hint = CString::new(texts.hint).unwrap_or_default();
        let close = CString::new(texts.close).unwrap_or_default();
        // SAFETY: the scanner copies both strings before returning.
        unsafe { flectar_qr_scan(hint.as_ptr(), close.as_ptr(), scanned) };
        Box::pin(async move { receiver.await.unwrap_or(Ok(None)) })
    }
}

fn pending() -> MutexGuard<'static, Option<oneshot::Sender<ScanResult>>> {
    PENDING.lock().unwrap_or_else(PoisonError::into_inner)
}

extern "C" fn scanned(text: *const c_char, outcome: c_int) {
    let Some(sender) = pending().take() else {
        return;
    };
    // SAFETY: QrScanner.m passes a NUL-terminated string or null.
    let text = (!text.is_null()).then(|| {
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    });
    let _ = sender.send(match outcome {
        CODE => Ok(Some(text.unwrap_or_default())),
        CLOSED => Ok(None),
        DENIED => Err(ScanError::CameraDenied),
        _ => Err(ScanError::Unavailable(
            text.unwrap_or_else(|| "the camera didn't start".to_owned()),
        )),
    });
}
