use serde::{Deserialize, Serialize};

use crate::core::{set::SetError, RequestParams};

#[derive(Debug, Clone, Serialize)]
pub struct SieveScriptValidateRequest {
    #[serde(rename = "accountId")]
    account_id: String,

    #[serde(rename = "blobId")]
    blob_id: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct SieveScriptValidateResponse {
    #[serde(rename = "accountId")]
    account_id: String,

    error: Option<SetError<String>>,
}

impl SieveScriptValidateRequest {
    pub fn new(params: RequestParams, blob_id: impl Into<String>) -> Self {
        SieveScriptValidateRequest {
            account_id: params.account_id,
            blob_id: blob_id.into(),
        }
    }
}

impl SieveScriptValidateResponse {
    pub fn unwrap_error(self) -> crate::Result<()> {
        match self.error {
            Some(err) => Err(err.into()),
            None => Ok(()),
        }
    }
}
