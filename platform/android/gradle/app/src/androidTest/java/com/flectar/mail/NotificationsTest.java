package com.flectar.mail;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.content.Context;
import android.content.Intent;
import android.os.SystemClock;
import android.service.notification.StatusBarNotification;

import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;

import org.junit.After;
import org.junit.Before;
import org.junit.Test;
import org.junit.runner.RunWith;

/** Lays out new-mail notifications and leads their taps and buttons back. */
@RunWith(AndroidJUnit4.class)
public final class NotificationsTest {
    private static final String TAG = "message-1-42";
    private static final String OPEN = "flectar-mail-notification:1;open;1;7;42";
    private static final String ARCHIVE = "flectar-mail-notification:1;archive;1;7;42";

    private final Context context = InstrumentationRegistry.getInstrumentation().getTargetContext();
    private final NotificationManager manager = context.getSystemService(NotificationManager.class);

    @Before
    public void nameChannels() {
        Notifications.nameChannels(context, "New mail", "New mail without sound");
    }

    @After
    public void withdraw() {
        manager.cancelAll();
    }

    private StatusBarNotification shown(String tag) throws InterruptedException {
        // The system lists a notification a moment after it is posted.
        long until = SystemClock.uptimeMillis() + 5_000;
        while (SystemClock.uptimeMillis() < until) {
            for (StatusBarNotification shown : manager.getActiveNotifications()) {
                if (tag.equals(shown.getTag())) {
                    return shown;
                }
            }
            Thread.sleep(50);
        }
        return null;
    }

    private boolean show(boolean sound, String[] labels, String[] requests) {
        return Notifications.show(
                context,
                TAG,
                sound,
                "Ana",
                "Lunch?",
                "Lunch?\nShall we meet at noon?",
                "me@example.com",
                1_700_000_000_000L,
                1,
                OPEN,
                labels,
                requests,
                "New message");
    }

    @Test
    public void theChannelsSeparateMailThatMayPlayTheSound() {
        NotificationChannel sounding = manager.getNotificationChannel(Notifications.NEW_MAIL);
        NotificationChannel quiet = manager.getNotificationChannel(Notifications.QUIET_MAIL);
        assertEquals("New mail", sounding.getName().toString());
        assertEquals(NotificationManager.IMPORTANCE_DEFAULT, sounding.getImportance());
        assertEquals("New mail without sound", quiet.getName().toString());
        assertEquals(NotificationManager.IMPORTANCE_LOW, quiet.getImportance());

        // Renaming keeps the channels.
        Notifications.nameChannels(context, "Correo nuevo", "Correo nuevo sin sonido");
        assertEquals("Correo nuevo",
                manager.getNotificationChannel(Notifications.NEW_MAIL).getName().toString());
    }

    @Test
    public void aMessageShowsWithItsButtonsAndGoesWhenWithdrawn() throws Exception {
        assertTrue(show(true, new String[] {"Mark as read", "Archive"},
                new String[] {"flectar-mail-notification:1;read;1;7;42", ARCHIVE}));
        StatusBarNotification shown = shown(TAG);
        assertNotNull(shown);
        Notification notification = shown.getNotification();
        assertEquals(Notifications.NEW_MAIL, notification.getChannelId());
        assertEquals("Ana", notification.extras.getCharSequence(Notification.EXTRA_TITLE).toString());
        assertEquals("Lunch?", notification.extras.getCharSequence(Notification.EXTRA_TEXT).toString());
        assertEquals("me@example.com",
                notification.extras.getCharSequence(Notification.EXTRA_SUB_TEXT).toString());
        // Android bundles the app's notifications itself.
        assertNull(notification.getGroup());
        assertEquals(Notification.CATEGORY_EMAIL, notification.category);
        assertEquals(1, notification.number);
        assertEquals(Notification.VISIBILITY_PRIVATE, notification.visibility);
        assertEquals("New message", notification.publicVersion.extras
                .getCharSequence(Notification.EXTRA_TITLE).toString());
        assertEquals(2, notification.actions.length);
        assertEquals("Archive", notification.actions[1].title.toString());

        Notifications.withdraw(context, new String[] {TAG, "summary-9"});
        long until = SystemClock.uptimeMillis() + 5_000;
        while (SystemClock.uptimeMillis() < until && shownNow(TAG)) {
            Thread.sleep(50);
        }
        assertFalse(shownNow(TAG));
    }

    private boolean shownNow(String tag) {
        for (StatusBarNotification shown : manager.getActiveNotifications()) {
            if (tag.equals(shown.getTag())) {
                return true;
            }
        }
        return false;
    }

    @Test
    public void aSummaryCountsEveryMessageItSumsUp() throws Exception {
        assertTrue(Notifications.show(
                context,
                "summary-1",
                false,
                "5 new messages",
                "Ana, Bo",
                "",
                "",
                1_700_000_000_000L,
                5,
                "flectar-mail-notification:1;open;1;;",
                new String[0],
                new String[0],
                "5 new messages"));
        StatusBarNotification shown = shown("summary-1");
        assertNotNull(shown);
        Notification notification = shown.getNotification();
        assertEquals(5, notification.number);
        // The lock screen still tells how many came.
        assertEquals("5 new messages", notification.publicVersion.extras
                .getCharSequence(Notification.EXTRA_TITLE).toString());
        assertNull(notification.extras.getCharSequence(Notification.EXTRA_SUB_TEXT));
        assertEquals(0, notification.actions == null ? 0 : notification.actions.length);
    }

    @Test
    public void quietMailGoesToTheQuietChannel() throws Exception {
        assertTrue(show(false, new String[0], new String[0]));
        assertEquals(Notifications.QUIET_MAIL, shown(TAG).getNotification().getChannelId());
    }

    @Test
    public void tapsAndButtonsCarryTheirRequest() {
        Intent tap = new Intent(context, FlectarActivity.class)
                .setData(new android.net.Uri.Builder()
                        .scheme(Notifications.SCHEME)
                        .authority(Notifications.HOST)
                        .appendPath(OPEN)
                        .build());
        assertEquals(OPEN, Notifications.request(tap));
        assertNull(Notifications.request(new Intent(Intent.ACTION_VIEW)));
        assertNull(Notifications.request(null));
        Intent redirect = new Intent(Intent.ACTION_VIEW,
                android.net.Uri.parse("msauth://com.flectar.mail/abc?code=1"));
        assertNull(Notifications.request(redirect));
    }

    @Test
    public void notificationsShowOnThisEmulator() {
        // API 30 grants notifications without asking.
        assertEquals(0, Notifications.availability(context));
        assertEquals(0, Notifications.backgroundChecks(context));
    }
}
