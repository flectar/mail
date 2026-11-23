package com.flectar.mail;

import android.net.http.X509TrustManagerExtensions;
import android.os.Build;

import java.io.ByteArrayInputStream;
import java.security.GeneralSecurityException;
import java.security.KeyStore;
import java.security.cert.CertPathValidatorException;
import java.security.cert.CertPathValidatorException.BasicReason;
import java.security.cert.Certificate;
import java.security.cert.CertificateException;
import java.security.cert.CertificateExpiredException;
import java.security.cert.CertificateFactory;
import java.security.cert.CertificateNotYetValidException;
import java.security.cert.CertificateRevokedException;
import java.security.cert.X509Certificate;

import javax.net.ssl.TrustManager;
import javax.net.ssl.TrustManagerFactory;
import javax.net.ssl.X509TrustManager;

/**
 * Server certificates for platform/android/src/trust.rs, checked as Android checks them for its
 * own HTTPS connections. The default TrustManager follows the app's network security config
 * (res/xml/network_security_config.xml) and Android's blocklist, and fetches nothing online:
 * Android handles revocation itself, without online checks. From Android 16 on, it also checks a
 * stapled OCSP response offline, as for its own connections. Rust checks the server name and the
 * handshake signatures, which the TrustManager doesn't.
 */
final class SystemTrust {
    /** The authType argument, which server checks only require to be non-empty, as OkHttp's. */
    private static final String AUTH_TYPE = "RSA";
    /** How deep a refusal's causes are searched, in case a cause chain loops. */
    private static final int CAUSES = 16;

    /** The default TrustManager, made once: Android updates it when the trusted CAs change. */
    private static X509TrustManagerExtensions defaultTrust;

    private SystemTrust() {}

    /**
     * Android's verdict on {@code chain}, the server's certificate followed by the intermediates
     * it sent, DER encoded, for a connection to {@code host}, a DNS name or an IP address, with
     * the OCSP response the server stapled, or null. Null when Android trusts the chain, or else
     * "kind: Android's reason", where kind is expired, not-yet-valid, revoked, encoding,
     * untrusted or unavailable.
     */
    static String check(byte[][] chain, String host, byte[] ocsp) {
        X509TrustManagerExtensions trust;
        CertificateFactory factory;
        try {
            trust = defaultTrust();
            factory = CertificateFactory.getInstance("X.509");
        } catch (GeneralSecurityException | RuntimeException e) {
            return "unavailable: " + describe(e);
        }
        X509Certificate[] certificates = new X509Certificate[chain.length];
        for (int i = 0; i < chain.length; i++) {
            try {
                Certificate certificate =
                        factory.generateCertificate(new ByteArrayInputStream(chain[i]));
                if (!(certificate instanceof X509Certificate)) {
                    return "encoding: certificate " + i + " isn't an X.509 certificate";
                }
                certificates[i] = (X509Certificate) certificate;
            } catch (CertificateException | RuntimeException e) {
                return "encoding: " + describe(e);
            }
        }
        try {
            verify(trust, certificates, host, ocsp);
            return null;
        } catch (CertificateException e) {
            return refusal(e);
        } catch (RuntimeException e) {
            return "unavailable: " + describe(e);
        }
    }

    private static synchronized X509TrustManagerExtensions defaultTrust()
            throws GeneralSecurityException {
        if (defaultTrust == null) {
            // Without a KeyStore, the factory makes the TrustManager of the app's network
            // security config.
            TrustManagerFactory factory =
                    TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm());
            factory.init((KeyStore) null);
            for (TrustManager manager : factory.getTrustManagers()) {
                if (manager instanceof X509TrustManager) {
                    defaultTrust = new X509TrustManagerExtensions((X509TrustManager) manager);
                    break;
                }
            }
            if (defaultTrust == null) {
                throw new GeneralSecurityException("Android offers no X.509 TrustManager");
            }
        }
        return defaultTrust;
    }

    private static void verify(X509TrustManagerExtensions trust, X509Certificate[] chain,
            String host, byte[] ocsp) throws CertificateException {
        if (ocsp != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.BAKLAVA) {
            try {
                trust.checkServerTrusted(chain, ocsp, null, AUTH_TYPE, host);
                return;
            } catch (IllegalArgumentException olderConscrypt) {
                // The Conscrypt module on this device can't take the response yet. Check the
                // chain without it, as on earlier versions of Android.
            }
        }
        trust.checkServerTrusted(chain, AUTH_TYPE, host);
    }

    /**
     * The kind of a refusal, from the causes Android gives. The kind only picks the advice an
     * error message gives; Android made the decision.
     */
    private static String refusal(CertificateException error) {
        Throwable cause = error;
        for (int depth = 0; cause != null && depth < CAUSES; depth++, cause = cause.getCause()) {
            CertPathValidatorException.Reason reason = cause instanceof CertPathValidatorException
                    ? ((CertPathValidatorException) cause).getReason() : null;
            if (cause instanceof CertificateExpiredException || reason == BasicReason.EXPIRED) {
                return "expired: " + describe(error);
            }
            if (cause instanceof CertificateNotYetValidException
                    || reason == BasicReason.NOT_YET_VALID) {
                return "not-yet-valid: " + describe(error);
            }
            if (cause instanceof CertificateRevokedException || reason == BasicReason.REVOKED) {
                return "revoked: " + describe(error);
            }
        }
        return "untrusted: " + describe(error);
    }

    /** What went wrong, in the words of each exception in the chain that adds any. */
    private static String describe(Throwable error) {
        StringBuilder text = new StringBuilder();
        Throwable cause = error;
        for (int depth = 0; cause != null && depth < CAUSES; depth++, cause = cause.getCause()) {
            String message = cause.getMessage();
            Throwable next = cause.getCause();
            // An exception made from its cause alone repeats the cause's description.
            if (message == null || message.isEmpty()
                    || (next != null && message.equals(next.toString()))) {
                continue;
            }
            if (text.length() > 0) {
                text.append(": ");
            }
            text.append(message);
        }
        return text.length() > 0 ? text.toString() : error.getClass().getName();
    }
}
