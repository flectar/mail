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
import android.util.Log;

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
    static {
        // NativeActivity loads the library for ANativeActivity_onCreate, but
        // Java's native-method lookup also needs it registered with this class
        // loader. Load before any OAuth, document or lifecycle callback runs.
        System.loadLibrary("flectar_mail_android");
    }

    private static native void nativeSuspendPdfPreview();

    @Override
    protected void onStop() {
        applyLauncherIcon();
        nativeSuspendPdfPreview();
        super.onStop();
    }

    private volatile String pendingLauncherIcon;

    /**
     * Called from Rust with the icon chosen in Settings. Switching launcher
     * entries can close a visible app on some launchers, so the switch waits
     * until the app leaves the foreground.
     */
    public void setLauncherIcon(String icon) {
        if (LauncherIcons.exists(icon)) pendingLauncherIcon = icon;
    }

    private void applyLauncherIcon() {
        String chosen = pendingLauncherIcon;
        pendingLauncherIcon = null;
        if (chosen == null) return;
        try {
            LauncherIcons.choose(this, chosen);
        } catch (RuntimeException error) {
            Log.e("flectar-mail", "could not change the launcher icon", error);
        }
    }

    private static final int FILE_DOCUMENT_REQUEST = 0xF11E;
    // One document is copied for the Files pane, whose transfers stop here.
    // Several are bounded by the free space alone, which a copy never takes
    // below the reserve.
    private static final long SINGLE_DOCUMENT_LIMIT = 512L * 1024 * 1024;
    private static final long FREE_SPACE_RESERVE = 64L * 1024 * 1024;
    private long pendingDocumentRequest = -1;
    private String pendingDocumentExport = "";
    // Above zero, the request takes up to this many documents.
    private int pendingDocumentLimit = 0;
    // Imported copies take generated names. Each result carries the name the
    // provider reports alongside, which Rust validates before using it.
    private static native void nativeDocumentResult(long requestId, String path, String name, String error);
    private static native void nativeDocumentsResult(long requestId, String[] paths, String[] names, String error);
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
            if(root.equals(file.getParentFile()) && file.isFile())file.delete();
        } catch(java.io.IOException ignored) { }
    }
    public void chooseDocument(long requestId, String exportPath, String name) {
        runOnUiThread(() -> {
            if (!nativeDocumentRequestActive(requestId)) return;
            if (pendingDocumentRequest >= 0) {nativeDocumentResult(requestId, "", "", "Another document picker is open."); return;}
            pendingDocumentRequest = requestId;
            pendingDocumentExport = exportPath;
            pendingDocumentLimit = 0;
            Intent intent = new Intent(exportPath.isEmpty() ? Intent.ACTION_OPEN_DOCUMENT : Intent.ACTION_CREATE_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType(exportPath.isEmpty() ? "*/*" : "application/octet-stream");
            if (!exportPath.isEmpty()) intent.putExtra(Intent.EXTRA_TITLE, name);
            try {startActivityForResult(intent, FILE_DOCUMENT_REQUEST);}
            catch (RuntimeException error) {pendingDocumentRequest=-1;pendingDocumentExport="";nativeDocumentResult(requestId,"","","Could not open the system document picker.");}
        });
    }

    /** Several documents for one action, as many as {@code limit}. */
    public void chooseDocuments(long requestId, int limit) {
        runOnUiThread(() -> {
            if (!nativeDocumentRequestActive(requestId)) return;
            if (pendingDocumentRequest >= 0) {nativeDocumentsResult(requestId, new String[0], new String[0], "Another document picker is open."); return;}
            pendingDocumentRequest = requestId;
            pendingDocumentExport = "";
            pendingDocumentLimit = Math.max(1, limit);
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            intent.setType("*/*");
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
            try {startActivityForResult(intent, FILE_DOCUMENT_REQUEST);}
            catch (RuntimeException error) {pendingDocumentRequest=-1;pendingDocumentLimit=0;nativeDocumentsResult(requestId,new String[0],new String[0],"Could not open the system document picker.");}
        });
    }

    private static void deliverDocuments(long id,int limit,List<String> paths,List<String> names,String error) {
        if(limit>0)nativeDocumentsResult(id,paths.toArray(new String[0]),names.toArray(new String[0]),error);
        else nativeDocumentResult(id,paths.isEmpty()?"":paths.get(0),names.isEmpty()?"":names.get(0),error);
    }
    private static void deliverDocuments(long id,int limit,String error) {
        deliverDocuments(id,limit,new ArrayList<>(),new ArrayList<>(),error);
    }

    private void finishDocument(int resultCode, Intent data) {
        final long id=pendingDocumentRequest;
        final String exportPath=pendingDocumentExport;
        final int limit=pendingDocumentLimit;
        pendingDocumentRequest=-1;pendingDocumentExport="";pendingDocumentLimit=0;
        if(id<0)return;
        final List<Uri> uris=new ArrayList<>();
        if(resultCode==RESULT_OK&&data!=null) {
            android.content.ClipData clip=exportPath.isEmpty()?data.getClipData():null;
            if(clip!=null){for(int i=0;i<clip.getItemCount();i++){Uri uri=clip.getItemAt(i).getUri();if(uri!=null)uris.add(uri);}}
            else if(data.getData()!=null)uris.add(data.getData());
        }
        if(uris.isEmpty()){deliverDocuments(id,limit,"");return;}
        if(uris.size()>Math.max(1,limit)){deliverDocuments(id,limit,"Choose at most "+Math.max(1,limit)+" files.");return;}
        if(!exportPath.isEmpty()){exportDocument(id,exportPath,uris.get(0));return;}
        new Thread(() -> {
            List<java.io.File> copies=new ArrayList<>();
            try {
                long maxBytes=limit>0?Long.MAX_VALUE:SINGLE_DOCUMENT_LIMIT;
                long known=0;
                for(Uri uri:uris)known+=Math.max(0,documentSize(uri));
                if(known>maxBytes)throw new java.io.IOException("File exceeds the transfer limit");
                if(known+FREE_SPACE_RESERVE>getCacheDir().getUsableSpace())throw new java.io.IOException("There is not enough free space on this device.");
                java.io.File dir=new java.io.File(getCacheDir(),"file-imports");
                if(!dir.mkdirs()&&!dir.isDirectory())throw new java.io.IOException("Cannot create import directory");
                List<String> paths=new ArrayList<>();
                List<String> names=new ArrayList<>();
                for(Uri uri:uris) {
                    if(!nativeDocumentRequestActive(id))throw new java.io.IOException("Document transfer cancelled");
                    // The provider's name never names a file here.
                    java.io.File imported=new java.io.File(dir,java.util.UUID.randomUUID().toString());
                    copies.add(imported);
                    String name=documentName(uri);
                    try(java.io.InputStream in=getContentResolver().openInputStream(uri);java.io.OutputStream out=new java.io.FileOutputStream(imported)) {copyDocument(id,in,out,maxBytes,true);}
                    paths.add(imported.getAbsolutePath());
                    names.add(name);
                }
                deliverDocuments(id,limit,paths,names,"");
            } catch(Exception error) {
                for(java.io.File copy:copies)copy.delete();
                deliverDocuments(id,limit,"The document could not be transferred: "+error.getMessage());
            }
        },"flectar-documents").start();
    }
    private void exportDocument(long id,String exportPath,Uri uri) {
        new Thread(() -> {
            try {
                if (!nativeDocumentRequestActive(id)) throw new java.io.IOException("Document transfer cancelled");
                java.io.File source=new java.io.File(exportPath).getCanonicalFile();
                // Only private application paths supplied by our Rust host.
                if(!source.toPath().startsWith(getCacheDir().getCanonicalFile().toPath()) && !source.toPath().startsWith(getFilesDir().getCanonicalFile().toPath()))throw new java.io.IOException("Invalid export source");
                try(java.io.InputStream in=new java.io.FileInputStream(source);java.io.OutputStream out=getContentResolver().openOutputStream(uri,"w")){copyDocument(id,in,out,Long.MAX_VALUE,false);}
                nativeDocumentResult(id,exportPath,"","");
            } catch(Exception error) {
                try {android.provider.DocumentsContract.deleteDocument(getContentResolver(),uri);} catch(Exception ignored) {}
                nativeDocumentResult(id,"","","The document could not be transferred: "+error.getMessage());
            }
        },"flectar-documents").start();
    }
    /** The name the provider reports, or an empty one when it reports none. */
    private String documentName(Uri uri) {
        try(android.database.Cursor cursor=getContentResolver().query(uri,new String[]{android.provider.OpenableColumns.DISPLAY_NAME},null,null,null)) {
            if(cursor!=null&&cursor.moveToFirst()&&!cursor.isNull(0))return cursor.getString(0);
        } catch(RuntimeException ignored) { }
        return "";
    }
    /** The size the provider reports, or -1 when it reports none. */
    private long documentSize(Uri uri) {
        try(android.database.Cursor cursor=getContentResolver().query(uri,new String[]{android.provider.OpenableColumns.SIZE},null,null,null)) {
            if(cursor!=null&&cursor.moveToFirst()&&!cursor.isNull(0))return cursor.getLong(0);
        } catch(RuntimeException ignored) { }
        return -1;
    }
    private void copyDocument(long id,java.io.InputStream in,java.io.OutputStream out,long maxBytes,boolean importing) throws java.io.IOException {
        if(in==null||out==null)throw new java.io.IOException("Document provider did not open a stream");
        byte[] buffer=new byte[65536];long total=0;long checked=0;int count;
        while((count=in.read(buffer))!=-1){
            if(!nativeDocumentRequestActive(id))throw new java.io.IOException("Document transfer cancelled");
            total+=count;
            if(total>maxBytes)throw new java.io.IOException("File exceeds the transfer limit");
            // Providers may report no size, or a wrong one.
            if(importing&&total-checked>=16L*1024*1024){checked=total;if(getCacheDir().getUsableSpace()<FREE_SPACE_RESERVE)throw new java.io.IOException("There is not enough free space on this device.");}
            out.write(buffer,0,count);
        }
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
