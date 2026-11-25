//! Scanning a QR code with the device's camera, to pair it with another
//! device in Flectar Mail Sync.
//!
//! Each phone scans with what its platform has: Google's code scanner on
//! Android and Scan Kit on HarmonyOS, which need no camera permission of the
//! app, and the camera on iOS. Computers show QR codes but don't scan them.

/// Why a scan ended without a code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanError {
    /// The person turned down camera access, or it is off in the system's
    /// settings.
    CameraDenied,
    /// The scanner couldn't start. The text is the platform's reason, for the
    /// log.
    Unavailable(String),
}

/// What a scan read: the code's text, or `None` when the person closed the
/// scanner.
pub type ScanResult = Result<Option<String>, ScanError>;

/// A scan in progress, which ends with what it read.
pub type ScanFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ScanResult> + Send>>;

/// The words on a scanner the app draws itself, in the app's language.
/// Scanners the system draws have their own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanTexts {
    /// What to point the camera at.
    pub hint: String,
    /// The button that closes the scanner.
    pub close: String,
}

/// The platform's QR code scanner.
pub trait QrScannerHost: Send + Sync + 'static {
    /// Whether this device can scan, such as an Android phone with Google
    /// Play services.
    fn available(&self) -> bool;

    /// Open the scanner.
    fn scan(&self, texts: ScanTexts) -> ScanFuture;
}

/// Whether `text` is a Flectar Mail Sync pairing code, which both ways of
/// pairing share. Any other code never reaches the sync server.
pub(crate) fn is_pairing_code(text: &str) -> bool {
    text.starts_with("flectar-mail-sync:pair?")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_pairing_codes_count() {
        assert!(is_pairing_code(
            "flectar-mail-sync:pair?v=1&server=https%3A%2F%2Fmail-sync.flectar.com&slot=AAAA&secret=BBBB"
        ));
        assert!(!is_pairing_code(
            "https://example.com/?flectar-mail-sync:pair?"
        ));
        assert!(!is_pairing_code("FLECTAR-MAIL-SYNC:pair?v=1"));
        assert!(!is_pairing_code("flectar-mail-sync:recover?v=1"));
        assert!(!is_pairing_code(""));
    }
}
