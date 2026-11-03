package com.flectar.mail;

import static org.junit.Assert.assertEquals;

import androidx.test.ext.junit.runners.AndroidJUnit4;

import java.lang.reflect.InvocationTargetException;
import java.lang.reflect.Method;
import java.lang.reflect.Modifier;
import java.util.HashSet;
import java.util.Set;

import org.junit.Test;
import org.junit.runner.RunWith;

/** Exercises ART's JNI lookup against the packaged Rust library, without an account. */
@RunWith(AndroidJUnit4.class)
public final class NativeCallbacksTest {
    // No Activity is launched: no mailbox, renderer or OAuth session is started.
    // An unknown request is safely ignored by the Rust callbacks. In particular,
    // do NOT load the library in this test: that would conceal the regression.
    private static final long UNKNOWN_REQUEST = -1;

    private static Object invoke(String name, Class<?>[] types, Object... args)
            throws Throwable {
        Method method = FlectarActivity.class.getDeclaredMethod(name, types);
        method.setAccessible(true);
        try {
            return method.invoke(null, args);
        } catch (InvocationTargetException error) {
            throw error.getCause();
        }
    }

    @Test
    public void googleResultsResolve() throws Throwable {
        Class<?>[] types = {long.class, String.class, long.class, String.class};
        invoke("nativeGoogleAuthorizationResult", types,
                UNKNOWN_REQUEST, "test-token", 3600L, "");
        invoke("nativeGoogleAuthorizationResult", types,
                UNKNOWN_REQUEST, "", 0L, "authorization cancelled");
        invoke("nativeGoogleAuthorizationResult", types,
                UNKNOWN_REQUEST, "", 0L, "test authorization failure");
        invoke("nativeGoogleAuthorizationResult", types,
                UNKNOWN_REQUEST, "", 0L, "needs_reauth:consent is required");
    }

    @Test
    public void microsoftRedirectResolves() throws Throwable {
        invoke("nativeMicrosoftRedirect", new Class<?>[]{String.class},
                "msauth://com.flectar.mail/test?error=access_denied");
    }

    @Test
    public void documentCallbacksResolve() throws Throwable {
        assertEquals(Boolean.FALSE, invoke("nativeDocumentRequestActive",
                new Class<?>[]{long.class}, UNKNOWN_REQUEST));
        invoke("nativeDocumentResult",
                new Class<?>[]{long.class, String.class, String.class, String.class},
                UNKNOWN_REQUEST, "", "", "");
        invoke("nativeDocumentsResult",
                new Class<?>[]{long.class, String[].class, String[].class, String.class},
                UNKNOWN_REQUEST, new String[0], new String[0], "");
    }

    @Test
    public void pdfLifecycleCallbackResolves() throws Throwable {
        invoke("nativeSuspendPdfPreview", new Class<?>[]{});
    }

    @Test
    public void everyNativeDeclarationHasACallbackTest() {
        Set<String> declarations = new HashSet<>();
        for (Method method : FlectarActivity.class.getDeclaredMethods()) {
            if (Modifier.isNative(method.getModifiers())) {
                declarations.add(method.getName());
            }
        }
        Set<String> tested = new HashSet<>();
        tested.add("nativeGoogleAuthorizationResult");
        tested.add("nativeMicrosoftRedirect");
        tested.add("nativeDocumentRequestActive");
        tested.add("nativeDocumentResult");
        tested.add("nativeDocumentsResult");
        tested.add("nativeSuspendPdfPreview");
        assertEquals("Add a JNI invocation test when adding a native callback", tested, declarations);
    }
}
