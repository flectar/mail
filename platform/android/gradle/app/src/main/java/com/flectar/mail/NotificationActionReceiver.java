package com.flectar.mail;

import android.app.NotificationManager;
import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/**
 * The buttons on new-mail notifications. Android may have ended the app's
 * process since the notification went up; Rust then opens the mail store
 * without the activity.
 */
public final class NotificationActionReceiver extends BroadcastReceiver {
    static {
        System.loadLibrary("flectar_mail_android");
    }

    /**
     * Carry out {@code request} on its message. Returns once the action is
     * saved, which takes well under the time Android gives a receiver.
     */
    private static native void nativeRespond(Context context, String request);

    @Override
    public void onReceive(Context context, Intent intent) {
        String request = Notifications.request(intent);
        if (request == null) {
            return;
        }
        // The notification goes at once; the mail follows in Rust.
        String tag = intent.getStringExtra(Notifications.EXTRA_TAG);
        if (tag != null) {
            context.getSystemService(NotificationManager.class).cancel(tag, 0);
        }
        Context application = context.getApplicationContext();
        PendingResult pending = goAsync();
        new Thread(() -> {
            try {
                nativeRespond(application, request);
            } finally {
                // The saved action reaches the server with the next check,
                // which this asks for as soon as there is a network.
                MailChecks.checkSoon(application);
                pending.finish();
            }
        }, "flectar-mail-action").start();
    }
}
