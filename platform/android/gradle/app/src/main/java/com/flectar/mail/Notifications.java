package com.flectar.mail;

import android.Manifest;
import android.app.ActivityManager;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.graphics.drawable.Icon;
import android.net.Uri;
import android.os.Build;
import android.os.PowerManager;
import android.provider.Settings;

/**
 * New-mail notifications for platform/android/src/notifications.rs. Rust
 * decides what to show, in the user's language; this lays each notification
 * out, and leads its tap and buttons back to Rust.
 */
final class Notifications {
    private Notifications() {}

    /** Mail that may play the sound: from a person, with sounds on. */
    static final String NEW_MAIL = "new-mail";
    /** Every other new mail. */
    static final String QUIET_MAIL = "quiet-mail";

    /**
     * Taps and buttons carry what they ask for as a data URI, which also
     * keeps their PendingIntents apart: Android ignores extras when it
     * compares them.
     */
    static final String SCHEME = "flectar-mail";
    static final String HOST = "notification";
    /** The notification a button belongs to, taken back once it is pressed. */
    static final String EXTRA_TAG = "tag";

    private static final String PREFERENCES = "notifications";
    private static final String PERMISSION_ASKED = "permission-asked";

    /** The request a tap or button carries, or null for other intents. */
    static String request(Intent intent) {
        Uri data = intent == null ? null : intent.getData();
        if (data == null || !SCHEME.equals(data.getScheme()) || !HOST.equals(data.getHost())) {
            return null;
        }
        return data.getLastPathSegment();
    }

    private static Uri uri(String request) {
        return new Uri.Builder().scheme(SCHEME).authority(HOST).appendPath(request).build();
    }

    /** Create the two channels, or rename them in the UI's language. */
    static void nameChannels(Context context, String newMail, String quietMail) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        // An existing channel keeps the user's choices; only its name changes.
        manager.createNotificationChannel(
                new NotificationChannel(NEW_MAIL, newMail, NotificationManager.IMPORTANCE_DEFAULT));
        manager.createNotificationChannel(
                new NotificationChannel(QUIET_MAIL, quietMail, NotificationManager.IMPORTANCE_LOW));
    }

    /**
     * Show the notification {@code tag}, replacing one shown under it, about
     * {@code number} new messages. False when Android doesn't show the app's
     * notifications.
     */
    static boolean show(
            Context context,
            String tag,
            boolean sound,
            String title,
            String text,
            String details,
            String account,
            long when,
            int number,
            String open,
            String[] actionLabels,
            String[] actionRequests,
            String hiddenTitle) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (!manager.areNotificationsEnabled()) {
            return false;
        }
        String channel = sound ? NEW_MAIL : QUIET_MAIL;
        Notification.Builder builder = new Notification.Builder(context, channel)
                .setSmallIcon(R.drawable.ic_stat_mail)
                .setContentTitle(title)
                .setContentText(text)
                .setWhen(when)
                .setShowWhen(true)
                .setCategory(Notification.CATEGORY_EMAIL)
                // Launchers that show a count on the app icon add these up,
                // so a summary counts every message it sums up.
                .setNumber(number)
                // No group of its own: Android bundles four or more of the
                // app's notifications, which a group would need a summary
                // notification for.
                .setAutoCancel(true)
                .setContentIntent(opening(context, open))
                // The lock screen shows that mail came, not from whom.
                .setVisibility(Notification.VISIBILITY_PRIVATE)
                .setPublicVersion(new Notification.Builder(context, channel)
                        .setSmallIcon(R.drawable.ic_stat_mail)
                        .setContentTitle(hiddenTitle)
                        .setWhen(when)
                        .setShowWhen(true)
                        .setCategory(Notification.CATEGORY_EMAIL)
                        .build());
        if (!details.isEmpty()) {
            builder.setStyle(new Notification.BigTextStyle().bigText(details));
        }
        if (!account.isEmpty()) {
            builder.setSubText(account);
        }
        Icon icon = Icon.createWithResource(context, R.drawable.ic_stat_mail);
        for (int index = 0; index < actionLabels.length; index++) {
            builder.addAction(new Notification.Action.Builder(
                    icon, actionLabels[index], acting(context, actionRequests[index], tag)).build());
        }
        try {
            manager.notify(tag, 0, builder.build());
            return true;
        } catch (SecurityException error) {
            // The permission went away since the check above.
            return false;
        }
    }

    private static PendingIntent opening(Context context, String request) {
        Intent intent = new Intent(context, FlectarActivity.class)
                .setAction(Intent.ACTION_VIEW)
                .setData(uri(request))
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        // Straight to the activity: Android 12 and later don't let a
        // receiver start one for a notification.
        return PendingIntent.getActivity(
                context, 0, intent, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    private static PendingIntent acting(Context context, String request, String tag) {
        Intent intent = new Intent(context, NotificationActionReceiver.class)
                .setData(uri(request))
                .putExtra(EXTRA_TAG, tag);
        return PendingIntent.getBroadcast(
                context, 0, intent, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    /** Take these notifications back; tags no longer shown are ignored. */
    static void withdraw(Context context, String[] tags) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        for (String tag : tags) {
            manager.cancel(tag, 0);
        }
    }

    /** 0 when notifications show, 1 before the user was asked, 2 when they are off. */
    static int availability(Context context) {
        NotificationManager manager = context.getSystemService(NotificationManager.class);
        if (manager.areNotificationsEnabled()) {
            return 0;
        }
        boolean asked = context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .getBoolean(PERMISSION_ASKED, false);
        if (Build.VERSION.SDK_INT >= 33
                && !asked
                && context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                        != PackageManager.PERMISSION_GRANTED) {
            return 1;
        }
        return 2;
    }

    /** Android shows the permission dialog only so often; after that it is a setting. */
    static void markAsked(Context context) {
        context.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)
                .edit()
                .putBoolean(PERMISSION_ASKED, true)
                .apply();
    }

    /** The app's notification settings. */
    static void openSettings(Context context) {
        Intent intent = new Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                .putExtra(Settings.EXTRA_APP_PACKAGE, context.getPackageName())
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        context.startActivity(intent);
    }

    /** The app's page in Settings, where its background use is allowed. */
    static void openAppSettings(Context context) {
        Intent intent = new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS)
                .setData(Uri.fromParts("package", context.getPackageName(), null))
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        context.startActivity(intent);
    }

    /**
     * 0 when Android lets the app check for mail in the background, 1 when
     * its background use is restricted, 2 while Battery Saver defers it.
     */
    static int backgroundChecks(Context context) {
        if (Build.VERSION.SDK_INT >= 28
                && context.getSystemService(ActivityManager.class).isBackgroundRestricted()) {
            return 1;
        }
        return context.getSystemService(PowerManager.class).isPowerSaveMode() ? 2 : 0;
    }
}
