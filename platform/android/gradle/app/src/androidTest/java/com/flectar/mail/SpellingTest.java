package com.flectar.mail;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertTrue;
import static org.junit.Assume.assumeTrue;

import android.content.Context;
import android.os.Build;
import android.view.textservice.TextServicesManager;

import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;

import java.util.Arrays;
import java.util.concurrent.TimeoutException;

import org.junit.After;
import org.junit.Test;
import org.junit.runner.RunWith;

/**
 * Checks spelling with the device's own spell checker, as messages being written are. The
 * checks need one that knows English, as Android's own keyboard does on the emulator's default
 * images. Gboard on the Google APIs images answers late and passes every word, so there they're
 * skipped.
 */
@RunWith(AndroidJUnit4.class)
public final class SpellingTest {
    /** Whether the device's spell checker knows English, asked once. */
    private static Boolean knowsEnglish;

    private final Context context = InstrumentationRegistry.getInstrumentation().getTargetContext();
    private final Spelling spelling = new Spelling(context);

    /** Skip a test that needs a spell checker that knows English, on a device without one. */
    private void needsEnglish() throws Exception {
        if (knowsEnglish == null) {
            int[] found;
            try {
                found = spelling.check("teh");
            } catch (TimeoutException e) {
                found = null;
            }
            knowsEnglish = Arrays.equals(new int[] {0, 3}, found);
        }
        assumeTrue("the device's spell checker doesn't know English", knowsEnglish);
    }

    @After
    public void close() {
        spelling.close();
    }

    /** The words of {@code text} that {@code ranges} cover. */
    private static String[] words(String text, int[] ranges) {
        String[] words = new String[ranges.length / 2];
        for (int i = 0; i < words.length; i++) {
            words[i] = text.substring(ranges[2 * i], ranges[2 * i + 1]);
        }
        return words;
    }

    @Test
    public void availabilityFollowsTheSystemsSwitch() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            TextServicesManager manager = context.getSystemService(TextServicesManager.class);
            assertEquals(manager.isSpellCheckerEnabled(), Spelling.available(context));
        } else {
            assertTrue(Spelling.available(context));
        }
    }

    @Test
    public void misspelledWordsComeBackWithTheirPlaces() throws Exception {
        needsEnglish();
        // UTF-16 places past an accented letter and an emoji.
        String text = "Café 😀 the cat sat on teh mat. We recieve it.";
        int[] found = spelling.check(text);
        assertNotNull("the device checks no spelling", found);
        assertArrayEquals(Arrays.toString(words(text, found)),
                new String[] {"teh", "recieve"}, words(text, found));
    }

    @Test
    public void aFullStopAtTheEndIsNoTypo() throws Exception {
        needsEnglish();
        assertArrayEquals(new int[0], spelling.check("We will meet tomorrow."));
    }

    @Test
    public void misspelledWordsGetCorrections() throws Exception {
        needsEnglish();
        String[] suggestions = spelling.suggest("recieve");
        assertTrue(Arrays.toString(suggestions), Arrays.asList(suggestions).contains("receive"));
    }

    @Test
    public void aClosedSessionStartsAgain() throws Exception {
        needsEnglish();
        assertNotNull(spelling.check("teh"));
        spelling.close();
        int[] found = spelling.check("teh");
        assertNotNull(found);
        assertArrayEquals(new int[] {0, 3}, found);
    }
}
