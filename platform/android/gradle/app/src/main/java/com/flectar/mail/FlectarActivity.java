package com.flectar.mail;

import java.io.File;
import android.app.NativeActivity;
import android.content.Intent;
import android.content.IntentSender;
import android.content.pm.PackageInfo;
import android.content.pm.PackageManager;
import android.content.pm.Signature;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.util.Base64;

import com.google.android.gms.auth.api.identity.AuthorizationRequest;
import com.google.android.gms.auth.api.identity.AuthorizationResult;
import com.google.android.gms.auth.api.identity.Identity;
import com.google.android.gms.common.api.ApiException;
import com.google.android.gms.common.api.Scope;

import java.util.ArrayList;
import java.util.List;
import java.security.MessageDigest;
import java.security.NoSuchAlgorithmException;

/** NativeActivity host plus the supported Google AuthorizationClient bridge. */
public final class FlectarActivity extends NativeActivity {
    private static native void nativeSuspendPdfPreview();

    @Override
    protected void onStop() {
        nativeSuspendPdfPreview();
        super.onStop();
    }

    @Override
    public void onTrimMemory(int level) {
        super.onTrimMemory(level);
        if (level >= android.content.ComponentCallbacks2.TRIM_MEMORY_RUNNING_LOW) {
            nativeSuspendPdfPreview();
        }
    }

    private static final int FILE_DOCUMENT_REQUEST = 0xF11E;
    private long pendingDocumentRequest = -1;
    private String pendingDocumentExport = "";
    private static native void nativeDocumentResult(long requestId, String path, String error);
    private static native boolean nativeDocumentRequestActive(long requestId);

    public void cancelDocumentRequest(long requestId) {
        runOnUiThread(() -> {
            if (pendingDocumentRequest == requestId) {
                // Retain the request ID until its result arrives. Reusing the
                // activity result code earlier could misroute a late result.
                finishActivity(FILE_DOCUMENT_REQUEST);
            }
        });
    }

    public void releaseImportedDocument(String path) {
        try {
            File root=new File(getCacheDir(),"file-imports").getCanonicalFile();
            File file=new File(path).getCanonicalFile();
            File folder=file.getParentFile();
            if(folder!=null && root.equals(folder.getParentFile()) && file.isFile()) {
                if(file.delete())folder.delete();
            }
        } catch(java.io.IOException ignored) { }
    }
    public void chooseDocument(long requestId, String exportPath, String name) {
        runOnUiThread(() -> {
            if (!nativeDocumentRequestActive(requestId)) return;
            if (pendingDocumentRequest >= 0) {nativeDocumentResult(requestId, "", "Another document picker is open."); return;}
            pendingDocumentRequest = requestId;
            pendingDocumentExport = exportPath;
            Intent intent = new Intent(exportPath.isEmpty() ? Intent.ACTION_OPEN_DOCUMENT : Intent.ACTION_CREATE_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType(exportPath.isEmpty() ? "*/*" : "application/octet-stream");
            if (!exportPath.isEmpty()) intent.putExtra(Intent.EXTRA_TITLE, name);
            try {startActivityForResult(intent, FILE_DOCUMENT_REQUEST);}
            catch (RuntimeException error) {pendingDocumentRequest=-1;pendingDocumentExport="";nativeDocumentResult(requestId,"","Could not open the system document picker.");}
        });
    }

    private void finishDocument(int resultCode, Intent data) {
        final long id=pendingDocumentRequest;
        final String exportPath=pendingDocumentExport;
        pendingDocumentRequest=-1;pendingDocumentExport="";
        if(id<0)return;
        if(resultCode!=RESULT_OK||data==null||data.getData()==null){nativeDocumentResult(id,"","");return;}
        final Uri uri=data.getData();
        new Thread(() -> {
            java.io.File imported=null;
            java.io.File importDirectory=null;
            try {
                if (!nativeDocumentRequestActive(id)) throw new java.io.IOException("Document transfer cancelled");
                if(exportPath.isEmpty()) {
                    String name="upload";
                    try(android.database.Cursor cursor=getContentResolver().query(uri,new String[]{android.provider.OpenableColumns.DISPLAY_NAME},null,null,null)) {
                        if(cursor!=null&&cursor.moveToFirst())name=cursor.getString(0);
                    }
                    if(name==null||name.isEmpty()||name.equals(".")||name.equals("..")||name.contains("/")||name.contains("\\"))name="upload";
                    java.io.File dir=new java.io.File(getCacheDir(),"file-imports/"+java.util.UUID.randomUUID());
                    importDirectory=dir;
                    if(!dir.mkdirs())throw new java.io.IOException("Cannot create import directory");
                    imported=new java.io.File(dir,name);
                    try(java.io.InputStream in=getContentResolver().openInputStream(uri);java.io.OutputStream out=new java.io.FileOutputStream(imported)) {copyDocument(id,in,out);}
                    nativeDocumentResult(id,imported.getAbsolutePath(),"");
                } else {
                    java.io.File source=new java.io.File(exportPath).getCanonicalFile();
                    // Only private application paths supplied by our Rust host.
                    if(!source.toPath().startsWith(getCacheDir().getCanonicalFile().toPath()) && !source.toPath().startsWith(getFilesDir().getCanonicalFile().toPath()))throw new java.io.IOException("Invalid export source");
                    try(java.io.InputStream in=new java.io.FileInputStream(source);java.io.OutputStream out=getContentResolver().openOutputStream(uri,"w")){copyDocument(id,in,out);}
                    nativeDocumentResult(id,exportPath,"");
                }
            } catch(Exception error) {
                if(imported!=null)imported.delete();
                if(importDirectory!=null)importDirectory.delete();
                if(!exportPath.isEmpty()) {try {android.provider.DocumentsContract.deleteDocument(getContentResolver(),uri);} catch(Exception ignored) {}}
                nativeDocumentResult(id,"","The document could not be transferred: "+error.getMessage());
            }
        },"flectar-documents").start();
    }
    private static void copyDocument(long id,java.io.InputStream in,java.io.OutputStream out) throws java.io.IOException {
        if(in==null||out==null)throw new java.io.IOException("Document provider did not open a stream");
        byte[] buffer=new byte[65536];long total=0;int count;
        while((count=in.read(buffer))!=-1){if(!nativeDocumentRequestActive(id))throw new java.io.IOException("Document transfer cancelled");total+=count;if(total>512L*1024*1024)throw new java.io.IOException("File exceeds the transfer limit");out.write(buffer,0,count);}
        if(!nativeDocumentRequestActive(id))throw new java.io.IOException("Document transfer cancelled");
        out.flush();
    }

    private static final int GOOGLE_AUTHORIZATION_REQUEST = 0xF1EC;
    private long pendingGoogleRequest = -1;

    private static native void nativeGoogleAuthorizationResult(
            long requestId,
            String accessToken,
            long expiresInSeconds,
            String error);

    private static native void nativeMicrosoftRedirect(String redirectUri);

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        if (savedInstanceState != null) {
            pendingDocumentRequest=savedInstanceState.getLong("pendingDocumentRequest",-1);
            pendingDocumentExport=savedInstanceState.getString("pendingDocumentExport","");
            pendingGoogleRequest = savedInstanceState.getLong("pendingGoogleRequest", -1);
        }
    }

    @Override
    protected void onSaveInstanceState(Bundle state) {
        state.putLong("pendingDocumentRequest",pendingDocumentRequest);
        state.putString("pendingDocumentExport",pendingDocumentExport);
        state.putLong("pendingGoogleRequest", pendingGoogleRequest);
        super.onSaveInstanceState(state);
    }

    /** Entra binds Android redirects to the certificate of the installed APK. */
    public String microsoftRedirectUri() {
        try {
            Signature signature;
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                PackageInfo info = getPackageManager().getPackageInfo(
                        getPackageName(), PackageManager.GET_SIGNING_CERTIFICATES);
                Signature[] signers = info.signingInfo.getApkContentsSigners();
                signature = signers[0];
            } else {
                @SuppressWarnings("deprecation")
                PackageInfo info = getPackageManager().getPackageInfo(
                        getPackageName(), PackageManager.GET_SIGNATURES);
                @SuppressWarnings("deprecation")
                Signature legacySignature = info.signatures[0];
                signature = legacySignature;
            }
            byte[] digest = MessageDigest.getInstance("SHA-1").digest(signature.toByteArray());
            String hash = Base64.encodeToString(digest, Base64.NO_WRAP);
            return "msauth://" + getPackageName() + "/" + Uri.encode(hash);
        } catch (PackageManager.NameNotFoundException | NoSuchAlgorithmException
                 | NullPointerException | ArrayIndexOutOfBoundsException error) {
            return null;
        }
    }

    @Override
    protected void onNewIntent(Intent intent) {
        super.onNewIntent(intent);
        setIntent(intent);
        if (intent.getData() != null
                && "msauth".equals(intent.getData().getScheme())
                && "com.flectar.mail".equals(intent.getData().getHost())) {
            nativeMicrosoftRedirect(intent.getData().toString());
        }
    }

    /** Called from Rust. Must return quickly; Play Services completes asynchronously. */
    public void authorizeGoogle(String[] requestedScopes, long requestId, boolean interactive) {
        runOnUiThread(() -> {
            List<Scope> scopes = new ArrayList<>(requestedScopes.length);
            for (String scope : requestedScopes) {
                scopes.add(new Scope(scope));
            }
            AuthorizationRequest request = AuthorizationRequest.builder()
                    .setRequestedScopes(scopes)
                    .build();
            Identity.getAuthorizationClient(this)
                    .authorize(request)
                    .addOnSuccessListener(result -> handleAuthorization(result, requestId, interactive))
                    .addOnFailureListener(error -> deliverFailure(requestId, error));
        });
    }

    private void handleAuthorization(
            AuthorizationResult result,
            long requestId,
            boolean interactive) {
        if (!result.hasResolution()) {
            deliverSuccess(requestId, result);
            return;
        }
        if (!interactive) {
            nativeGoogleAuthorizationResult(
                    requestId, "", 0, "needs_reauth:consent or account selection is required");
            return;
        }
        pendingGoogleRequest = requestId;
        try {
            startIntentSenderForResult(
                    result.getPendingIntent().getIntentSender(),
                    GOOGLE_AUTHORIZATION_REQUEST,
                    null,
                    0,
                    0,
                    0);
        } catch (IntentSender.SendIntentException error) {
            pendingGoogleRequest = -1;
            deliverFailure(requestId, error);
        }
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode == FILE_DOCUMENT_REQUEST) {finishDocument(resultCode,data);return;}
        if (requestCode != GOOGLE_AUTHORIZATION_REQUEST) {
            return;
        }
        long requestId = pendingGoogleRequest;
        pendingGoogleRequest = -1;
        if (requestId < 0 || data == null) {
            if (requestId >= 0) {
                nativeGoogleAuthorizationResult(requestId, "", 0, "authorization cancelled");
            }
            return;
        }
        try {
            AuthorizationResult result = Identity.getAuthorizationClient(this)
                    .getAuthorizationResultFromIntent(data);
            deliverSuccess(requestId, result);
        } catch (ApiException error) {
            deliverFailure(requestId, error);
        }
    }

    private static void deliverSuccess(long requestId, AuthorizationResult result) {
        String token = result.getAccessToken();
        if (token == null || token.isEmpty()) {
            nativeGoogleAuthorizationResult(requestId, "", 0, "no access token returned");
        } else {
            // Google access tokens are currently one hour. Rust subtracts a
            // safety window and asks AuthorizationClient for another token.
            nativeGoogleAuthorizationResult(requestId, token, 3600, "");
        }
    }

    private static void deliverFailure(long requestId, Exception error) {
        String detail = error.getMessage();
        nativeGoogleAuthorizationResult(
                requestId,
                "",
                0,
                detail == null || detail.isEmpty() ? error.getClass().getSimpleName() : detail);
    }
}
