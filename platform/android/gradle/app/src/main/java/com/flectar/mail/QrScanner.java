package com.flectar.mail;

import android.app.Activity;

import com.google.android.gms.common.ConnectionResult;
import com.google.android.gms.common.GoogleApiAvailability;
import com.google.mlkit.vision.barcode.common.Barcode;
import com.google.mlkit.vision.codescanner.GmsBarcodeScannerOptions;
import com.google.mlkit.vision.codescanner.GmsBarcodeScanning;

/**
 * Scans a QR code with Google's code scanner, to pair devices in Flectar Mail
 * Sync. The scanner runs in Google Play services, so the app needs no camera
 * permission, and the images stay on the phone.
 */
final class QrScanner {
    private QrScanner() {}

    /** The code's text; neither when the person closed the scanner. */
    private static native void nativeResult(String text, String error);

    /** Called from Rust: whether Google Play services can run the scanner. */
    static boolean available(Activity activity) {
        return GoogleApiAvailability.getInstance().isGooglePlayServicesAvailable(activity)
                == ConnectionResult.SUCCESS;
    }

    /** Called from Rust. Returns at once; the scanner answers later. */
    static void scan(Activity activity) {
        activity.runOnUiThread(() -> {
            GmsBarcodeScannerOptions options = new GmsBarcodeScannerOptions.Builder()
                    .setBarcodeFormats(Barcode.FORMAT_QR_CODE)
                    .build();
            try {
                GmsBarcodeScanning.getClient(activity, options)
                        .startScan()
                        .addOnSuccessListener(barcode -> nativeResult(text(barcode), null))
                        .addOnCanceledListener(() -> nativeResult(null, null))
                        .addOnFailureListener(error -> nativeResult(null, describe(error)));
            } catch (RuntimeException error) {
                nativeResult(null, describe(error));
            }
        });
    }

    /** A code that isn't text reads as empty, which Rust turns down. */
    private static String text(Barcode barcode) {
        String value = barcode.getRawValue();
        return value == null ? "" : value;
    }

    private static String describe(Exception error) {
        String message = error.getMessage();
        return message == null || message.isEmpty() ? error.getClass().getName() : message;
    }
}
