package com.flectar.mail;

import android.content.ComponentName;
import android.content.Context;
import android.content.pm.PackageManager;

import java.util.Arrays;

/** The launcher entries declared in AndroidManifest.xml, one per app icon in Settings. */
final class LauncherIcons {
    /** Settings order; only the first is enabled in the manifest. */
    static final String[] NAMES = {"orange", "graphite", "light", "blue", "green", "purple"};

    private LauncherIcons() {}

    static boolean exists(String icon) {
        return Arrays.asList(NAMES).contains(icon);
    }

    static ComponentName alias(Context context, String icon) {
        return new ComponentName(context,
                "com.flectar.mail.Icon" + Character.toUpperCase(icon.charAt(0)) + icon.substring(1));
    }

    static boolean enabled(Context context, String icon) {
        int state = context.getPackageManager().getComponentEnabledSetting(alias(context, icon));
        return state == PackageManager.COMPONENT_ENABLED_STATE_ENABLED
                || (state == PackageManager.COMPONENT_ENABLED_STATE_DEFAULT && icon.equals(NAMES[0]));
    }

    /** Show {@code chosen} in the launcher. Unchanged entries are left alone. */
    static void choose(Context context, String chosen) {
        if (!exists(chosen)) return;
        // Enable the chosen entry first so the app never lacks one.
        setEnabled(context, chosen, true);
        for (String icon : NAMES) {
            if (!icon.equals(chosen)) setEnabled(context, icon, false);
        }
    }

    private static void setEnabled(Context context, String icon, boolean enabled) {
        if (enabled(context, icon) == enabled) return;
        context.getPackageManager().setComponentEnabledSetting(alias(context, icon),
                enabled ? PackageManager.COMPONENT_ENABLED_STATE_ENABLED
                        : PackageManager.COMPONENT_ENABLED_STATE_DISABLED,
                PackageManager.DONT_KILL_APP);
    }
}
