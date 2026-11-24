package com.flectar.mail;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import android.app.NativeActivity;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ActivityInfo;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;

import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

import org.junit.After;
import org.junit.Test;
import org.junit.runner.RunWith;

/** Switches the launcher entries for the app icons chosen in Settings. */
@RunWith(AndroidJUnit4.class)
public final class LauncherIconsTest {
    private final Context context = InstrumentationRegistry.getInstrumentation().getTargetContext();

    private List<String> launcherEntries() {
        Intent launcher = new Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
                .setPackage(context.getPackageName());
        List<String> entries = new ArrayList<>();
        for (ResolveInfo info : context.getPackageManager().queryIntentActivities(launcher, 0)) {
            entries.add(info.activityInfo.name);
        }
        return entries;
    }

    private void assertOnly(String icon) {
        for (String name : LauncherIcons.NAMES) {
            assertEquals(name, name.equals(icon), LauncherIcons.enabled(context, name));
        }
        assertEquals(Collections.singletonList(LauncherIcons.alias(context, icon).getClassName()),
                launcherEntries());
    }

    @After
    public void restoreDefault() {
        LauncherIcons.choose(context, "orange");
    }

    /**
     * A screen the app opens over itself, such as the document picker, isn't
     * leaving it. Android closes the task opened from a launcher entry that
     * is turned off, picker and all, so the icon changes once the user leaves.
     */
    @Test
    public void theChosenIconWaitsUntilTheUserLeavesTheApp() {
        LauncherIconChoice.inFront();
        LauncherIconChoice.takeOnStop(true);
        LauncherIconChoice.choose("graphite");
        LauncherIconChoice.openingOwnScreen();
        assertNull("not under the app's own screen", LauncherIconChoice.takeOnStop(true));
        LauncherIconChoice.inFront();
        assertNull("not as the screen goes off", LauncherIconChoice.takeOnStop(false));
        assertEquals("graphite", LauncherIconChoice.takeOnStop(true));
        assertNull("only once", LauncherIconChoice.takeOnStop(true));
        LauncherIconChoice.choose("future");
        assertNull("not for icons without an entry", LauncherIconChoice.takeOnStop(true));
    }

    @Test
    public void everyIconHasALauncherEntry() {
        assertOnly("orange");
        for (String icon : LauncherIcons.NAMES) {
            LauncherIcons.choose(context, icon);
            assertOnly(icon);
        }
    }

    @Test
    public void everyLauncherEntryNamesTheNativeLibrary() throws Exception {
        // NativeActivity loads the library named by the component that started
        // it, which is the alias when the launcher opens the app.
        PackageManager packages = context.getPackageManager();
        for (String icon : LauncherIcons.NAMES) {
            ActivityInfo info = packages.getActivityInfo(LauncherIcons.alias(context, icon),
                    PackageManager.GET_META_DATA | PackageManager.MATCH_DISABLED_COMPONENTS);
            assertEquals(icon, "flectar_mail_android", info.metaData == null ? null
                    : info.metaData.getString(NativeActivity.META_DATA_LIB_NAME));
        }
    }

    @Test
    public void unknownIconsAreIgnored() {
        LauncherIcons.choose(context, "graphite");
        LauncherIcons.choose(context, "future");
        assertOnly("graphite");
        assertTrue(LauncherIcons.exists("purple"));
    }
}
