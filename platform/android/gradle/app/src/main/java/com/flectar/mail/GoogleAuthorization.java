package com.flectar.mail;

import android.content.Context;
import android.os.Handler;
import android.os.Looper;

import com.google.android.gms.auth.api.identity.AuthorizationRequest;
import com.google.android.gms.auth.api.identity.AuthorizationResult;
import com.google.android.gms.auth.api.identity.Identity;
import com.google.android.gms.common.api.Scope;

import java.util.ArrayList;
import java.util.List;

/**
 * Google authorization without the activity, as a check for new mail in the
 * background needs: a token comes back only when no consent or account choice
 * is needed, since nobody can be asked. Signing in stays with the activity.
 */
final class GoogleAuthorization {
    private GoogleAuthorization() {}

    private static native void nativeResult(
            long requestId,
            String accessToken,
            long expiresInSeconds,
            String error);

    /** Called from Rust. Returns at once; Play Services answers later. */
    static void authorize(Context context, String[] requestedScopes, long requestId) {
        new Handler(Looper.getMainLooper()).post(() -> {
            List<Scope> scopes = new ArrayList<>(requestedScopes.length);
            for (String scope : requestedScopes) {
                scopes.add(new Scope(scope));
            }
            AuthorizationRequest request = AuthorizationRequest.builder()
                    .setRequestedScopes(scopes)
                    .build();
            try {
                Identity.getAuthorizationClient(context)
                        .authorize(request)
                        .addOnSuccessListener(result -> deliver(requestId, result))
                        .addOnFailureListener(error -> fail(requestId, error));
            } catch (RuntimeException error) {
                fail(requestId, error);
            }
        });
    }

    private static void deliver(long requestId, AuthorizationResult result) {
        if (result.hasResolution()) {
            nativeResult(requestId, "", 0, "needs_reauth:consent or account selection is required");
            return;
        }
        String token = result.getAccessToken();
        if (token == null || token.isEmpty()) {
            nativeResult(requestId, "", 0, "no access token returned");
        } else {
            // As for the activity's tokens: one hour, which Rust shortens by
            // a safety window.
            nativeResult(requestId, token, 3600, "");
        }
    }

    private static void fail(long requestId, Exception error) {
        String detail = error.getMessage();
        nativeResult(
                requestId,
                "",
                0,
                detail == null || detail.isEmpty() ? error.getClass().getSimpleName() : detail);
    }
}
