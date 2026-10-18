//! System clipboard access through the OHOS pasteboard NDK.
//!
//! Writes are permission-free; reads require `ohos.permission.READ_PASTEBOARD`
//! (see the module manifest). Failures are logged with their status code and
//! reported as "no text" so the UI keeps working without clipboard access.
//! Text content is never logged.

use core::ffi::{CStr, c_int};
use std::ffi::CString;

use crate::ffi;

/// Read the current clipboard text, if any.
pub fn read_text() -> Option<String> {
    // SAFETY: the pasteboard handle is created and destroyed inside this call,
    // and the returned UDMF data is destroyed after it has been copied out.
    unsafe {
        let board = ffi::OH_Pasteboard_Create();
        if board.is_null() {
            crate::log::error("clipboard: OH_Pasteboard_Create failed");
            return None;
        }
        let mut status: c_int = 0;
        let data = ffi::OH_Pasteboard_GetData(board, &mut status);
        let mut text = None;
        if data.is_null() {
            // 0 == PASTEBOARD_ErrCode::ERR_OK with empty data; other codes mean
            // denial (e.g. 201: missing READ_PASTEBOARD).
            if status != 0 {
                crate::log::error(&format!("clipboard: read failed (status {status})"));
            }
        } else {
            let plain = ffi::OH_UdsPlainText_Create();
            if !plain.is_null() {
                if ffi::OH_UdmfData_GetPrimaryPlainText(data, plain) == 0 {
                    let content = ffi::OH_UdsPlainText_GetContent(plain);
                    if !content.is_null() {
                        let value = CStr::from_ptr(content).to_string_lossy().into_owned();
                        crate::log::info(&format!("clipboard: read {} bytes", value.len()));
                        text = Some(value);
                    }
                }
                ffi::OH_UdsPlainText_Destroy(plain);
            }
            ffi::OH_UdmfData_Destroy(data);
        }
        ffi::OH_Pasteboard_Destroy(board);
        text
    }
}

/// Replace the clipboard with `text`.
pub fn write_text(text: &str) {
    let Ok(content) = CString::new(text) else {
        crate::log::error("clipboard: text contains an interior NUL");
        return;
    };
    // SAFETY: every object is created and destroyed within this call; the
    // record copies the plain-text structure when it is added.
    unsafe {
        let board = ffi::OH_Pasteboard_Create();
        if board.is_null() {
            crate::log::error("clipboard: OH_Pasteboard_Create failed");
            return;
        }
        let data = ffi::OH_UdmfData_Create();
        let record = ffi::OH_UdmfRecord_Create();
        let plain = ffi::OH_UdsPlainText_Create();
        if !data.is_null() && !record.is_null() && !plain.is_null() {
            let staged = ffi::OH_UdsPlainText_SetContent(plain, content.as_ptr()) == 0
                && ffi::OH_UdmfRecord_AddPlainText(record, plain) == 0
                && ffi::OH_UdmfData_AddRecord(data, record) == 0;
            if staged {
                let rc = ffi::OH_Pasteboard_SetData(board, data);
                if rc == 0 {
                    crate::log::info(&format!("clipboard: wrote {} bytes", text.len()));
                } else {
                    crate::log::error(&format!("clipboard: write failed (status {rc})"));
                }
            } else {
                crate::log::error("clipboard: could not stage the plain-text record");
            }
        }
        if !plain.is_null() {
            ffi::OH_UdsPlainText_Destroy(plain);
        }
        if !record.is_null() {
            ffi::OH_UdmfRecord_Destroy(record);
        }
        if !data.is_null() {
            ffi::OH_UdmfData_Destroy(data);
        }
        ffi::OH_Pasteboard_Destroy(board);
    }
}
