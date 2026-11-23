package com.flectar.mail;

import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;
import static org.junit.Assume.assumeNoException;

import android.util.Base64;

import androidx.test.ext.junit.runners.AndroidJUnit4;

import java.io.IOException;
import java.net.InetSocketAddress;
import java.security.cert.Certificate;
import java.util.Collections;

import javax.net.ssl.SNIHostName;
import javax.net.ssl.SSLParameters;
import javax.net.ssl.SSLSocket;
import javax.net.ssl.SSLSocketFactory;

import org.junit.Test;
import org.junit.runner.RunWith;

/**
 * Checks server certificates with Android's TrustManager, as the app's TLS connections do through
 * platform/android/src/trust.rs.
 */
@RunWith(AndroidJUnit4.class)
public final class SystemTrustTest {
    /**
     * The self-signed certificate of the core's TLS tests
     * (crates/flectar-mail-core/tests/fixtures/tls/server.pem), valid until 2126.
     */
    private static final String SELF_SIGNED =
            "MIIDJDCCAgygAwIBAgIUZKDDvOME4AolHtU3h+AWlKS8pP8wDQYJKoZIhvcNAQELBQAwFDESMBAG"
            + "A1UEAwwJbG9jYWxob3N0MCAXDTI2MDkxMDAyMjg1OVoYDzIxMjYwODE3MDIyODU5WjAUMRIwEAYD"
            + "VQQDDAlsb2NhbGhvc3QwggEiMA0GCSqGSIb3DQEBAQUAA4IBDwAwggEKAoIBAQDwAhdQ/bT7CpDa"
            + "8RBkOwdywgJCSeaZU24WDCB7J7+78uHgJoYIpmZ6tpnlcByHNuETSIviiOcjhMr1ON4fEfOYQxjF"
            + "/VfoQDmrlvVyDi7nTd8IzlvG0DlCneaNlzBqnAJ5xfHds565pxCGkBDS1nTKPxvLiNhXcm18C3Vc"
            + "VjDD7acS+DDrHpAuwhzv5OgPlLgQP/2VuHI9ba26q7FXajv5fhzp7DvKjNc8fxA29ZvbnfBHw/oB"
            + "iTFEe8NA0ol0AukB2EKogh009tAYptxGvKpcHR16ExXPOrfX1CYgp+NcenVvPk7FmucPTv77TfmB"
            + "TIUblPSfnYNgi9KpcGVDWDoXAgMBAAGjbDBqMB0GA1UdDgQWBBRBzAPAlB5114l5/msgsCRp5oro"
            + "qjAfBgNVHSMEGDAWgBRBzAPAlB5114l5/msgsCRp5oroqjAaBgNVHREEEzARgglsb2NhbGhvc3SH"
            + "BH8AAAEwDAYDVR0TAQH/BAIwADANBgkqhkiG9w0BAQsFAAOCAQEA6vWV6G9gi0DoviHLpQ+uSmvt"
            + "inPDh8q6Sr9b0hEvYHMR9ixd2biGO+JEAXOwfnSrAYuu6Fg+Hxszk0J1if1lL+SZNRVJYfWtyUaX"
            + "QxO40Ss7IAiCU66XCoBnXqZAvWUei10IvOZrMScl5bZt2R+ZCdSRsGghwr8K7I8aHIstPmz12ZHH"
            + "CLAracPe/tCL18DNhgUDvQUg06GO8+N9DP3rDaY2XrHOD3O1c2wY/Wa6N9qYgZC2qwXa1FfS1tGL"
            + "C/UXo2dGAKNrRy4nl0zVKThH0KxwiF2vsRou3JB4BhY/Wt/+quGsrwFEWwcGk5um4fqdc3hEoT1z"
            + "/af2hawz/Jd3JA==";
    private static final int TIMEOUT_MS = 15_000;

    @Test
    public void malformedCertificatesAreRefused() {
        String verdict = SystemTrust.check(new byte[][] {{0}}, "example.test", null);
        assertTrue(verdict, verdict.startsWith("encoding: "));
    }

    @Test
    public void certificatesWithoutTrustedRootsAreRefused() {
        byte[] certificate = Base64.decode(SELF_SIGNED, Base64.DEFAULT);
        String verdict = SystemTrust.check(new byte[][] {certificate}, "localhost", null);
        assertTrue(verdict, verdict.startsWith("untrusted: "));
    }

    /**
     * Let's Encrypt's certificates, the sync server's, and Google's, Gmail's, name no OCSP
     * responder. The verifier before SystemTrust refused them as revoked.
     */
    @Test
    public void publicServersWithoutOcspRespondersAreTrusted() throws Exception {
        assertTrusted("mail-sync.flectar.com", 443);
        assertTrusted("imap.gmail.com", 993);
    }

    private static void assertTrusted(String host, int port) throws Exception {
        String verdict = SystemTrust.check(chainOf(host, port), host, null);
        assertNull(host + ": " + verdict, verdict);
    }

    /**
     * The certificates {@code host} sends on {@code port}, server's first, DER encoded. Skips the
     * test where the emulator can't reach the server.
     */
    private static byte[][] chainOf(String host, int port) throws Exception {
        SSLSocketFactory factory = (SSLSocketFactory) SSLSocketFactory.getDefault();
        try (SSLSocket socket = (SSLSocket) factory.createSocket()) {
            SSLParameters parameters = socket.getSSLParameters();
            parameters.setServerNames(Collections.singletonList(new SNIHostName(host)));
            socket.setSSLParameters(parameters);
            socket.connect(new InetSocketAddress(host, port), TIMEOUT_MS);
            socket.setSoTimeout(TIMEOUT_MS);
            socket.startHandshake();
            Certificate[] certificates = socket.getSession().getPeerCertificates();
            byte[][] chain = new byte[certificates.length][];
            for (int i = 0; i < certificates.length; i++) {
                chain[i] = certificates[i].getEncoded();
            }
            return chain;
        } catch (IOException e) {
            assumeNoException("can't reach " + host, e);
            throw e;
        }
    }
}
