package com.flectar.mail;

import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertNotNull;
import static org.junit.Assert.assertNull;
import static org.junit.Assert.assertTrue;

import android.app.job.JobInfo;
import android.app.job.JobScheduler;
import android.content.Context;
import android.os.ParcelFileDescriptor;
import android.os.SystemClock;

import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;

import org.junit.After;
import org.junit.Test;
import org.junit.runner.RunWith;

/** Asks Android for checks for new mail while the user wants them. */
@RunWith(AndroidJUnit4.class)
public final class MailChecksTest {
    private final Context context = InstrumentationRegistry.getInstrumentation().getTargetContext();
    private final JobScheduler scheduler = context.getSystemService(JobScheduler.class);

    @After
    public void cancel() {
        scheduler.cancelAll();
    }

    @Test
    public void checksRunEveryFifteenMinutesWithANetworkWhileWanted() {
        MailChecks.schedule(context, true);
        JobInfo job = scheduler.getPendingJob(MailChecks.PERIODIC);
        assertNotNull(job);
        assertTrue(job.isPeriodic());
        assertEquals(15 * 60 * 1000L, job.getIntervalMillis());
        assertEquals(JobInfo.NETWORK_TYPE_ANY, job.getNetworkType());
        assertTrue(job.isPersisted());
        assertEquals(MailCheckJob.class.getName(), job.getService().getClassName());

        // Asking again leaves the scheduled check, which may be running.
        MailChecks.schedule(context, true);
        assertNotNull(scheduler.getPendingJob(MailChecks.PERIODIC));

        MailChecks.schedule(context, false);
        assertNull(scheduler.getPendingJob(MailChecks.PERIODIC));
    }

    /**
     * The check runs in a process without the activity: Rust opens the mail
     * store from the Application, checks, and closes it. A crash or a hang in
     * that path fails here.
     */
    @Test
    public void aCheckRunsAndEndsWithoutTheActivity() throws Exception {
        MailChecks.checkSoon(context);
        String command = "cmd jobscheduler run -f " + context.getPackageName() + " " + MailChecks.SOON;
        ParcelFileDescriptor output = InstrumentationRegistry.getInstrumentation()
                .getUiAutomation()
                .executeShellCommand(command);
        // The command has run once its output ends.
        try (java.io.InputStream in = new ParcelFileDescriptor.AutoCloseInputStream(output)) {
            while (in.read() != -1) {
                // Nothing to check in what it prints.
            }
        }
        long until = SystemClock.uptimeMillis() + 120_000;
        while (SystemClock.uptimeMillis() < until && scheduler.getPendingJob(MailChecks.SOON) != null) {
            Thread.sleep(250);
        }
        assertNull("the check ended", scheduler.getPendingJob(MailChecks.SOON));
    }

    @Test
    public void aButtonsActionGoesOutWithTheNextNetwork() {
        MailChecks.checkSoon(context);
        JobInfo job = scheduler.getPendingJob(MailChecks.SOON);
        assertNotNull(job);
        assertEquals(JobInfo.NETWORK_TYPE_ANY, job.getNetworkType());
        assertEquals(MailCheckJob.class.getName(), job.getService().getClassName());
    }
}
