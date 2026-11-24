package com.flectar.mail;

import android.content.ComponentName;
import android.content.Context;
import android.content.pm.PackageManager;
import android.os.Build;

import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;

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
        // The chosen entry comes first, so that the app never lacks one.
        List<String> changed = new ArrayList<>();
        if (!enabled(context, chosen)) changed.add(chosen);
        for (String icon : NAMES) {
            if (!icon.equals(chosen) && enabled(context, icon)) changed.add(icon);
        }
        PackageManager packages = context.getPackageManager();
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            // All at once, so a launcher never sees two entries or none.
            List<PackageManager.ComponentEnabledSetting> settings = new ArrayList<>();
            for (String icon : changed) {
                settings.add(new PackageManager.ComponentEnabledSetting(alias(context, icon),
                        state(icon.equals(chosen)), PackageManager.DONT_KILL_APP));
            }
            if (!settings.isEmpty()) packages.setComponentEnabledSettings(settings);
        } else {
            for (String icon : changed) {
                packages.setComponentEnabledSetting(alias(context, icon),
                        state(icon.equals(chosen)), PackageManager.DONT_KILL_APP);
            }
        }
    }

    private static int state(boolean enabled) {
        return enabled ? PackageManager.COMPONENT_ENABLED_STATE_ENABLED
                : PackageManager.COMPONENT_ENABLED_STATE_DISABLED;
    }
}
