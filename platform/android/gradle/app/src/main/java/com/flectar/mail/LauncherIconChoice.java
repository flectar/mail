package com.flectar.mail;

/**
 * The launcher icon chosen in Settings, which takes effect once the user
 * leaves the app. Android closes the task opened from a launcher entry that
 * is turned off, with any screen the app opened in it. So a screen of the
 * app's own over it, such as the document picker, isn't leaving, and neither
 * is the screen going off.
 */
final class LauncherIconChoice {
    private static volatile String pending;
    private static volatile boolean openingOwnScreen;

    private LauncherIconChoice() {}

    /** The icon chosen in Settings. Names that no entry has are ignored. */
    static void choose(String icon) {
        if (LauncherIcons.exists(icon)) pending = icon;
    }

    /** Flectar Mail is about to open a screen of its own over the app. */
    static void openingOwnScreen() {
        openingOwnScreen = true;
    }

    /** The app is in front again. */
    static void inFront() {
        openingOwnScreen = false;
    }

    /** The icon to switch to as the app stops, when that is the user leaving it. */
    static String takeOnStop(boolean screenOn) {
        if (openingOwnScreen || !screenOn) return null;
        String icon = pending;
        pending = null;
        return icon;
    }
}
