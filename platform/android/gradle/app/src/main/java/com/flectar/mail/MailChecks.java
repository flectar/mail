package com.flectar.mail;

import android.app.job.JobInfo;
import android.app.job.JobScheduler;
import android.content.ComponentName;
import android.content.Context;
import android.os.Build;

/**
 * When Android runs {@link MailCheckJob}: every 15 minutes, the shortest
 * period it allows, while the user wants what a check brings, and once soon
 * after a notification's button saved an action. It runs them only with a
 * network, and less often for an app the user seldom opens.
 */
final class MailChecks {
    private MailChecks() {}

    static final int PERIODIC = 1;
    static final int SOON = 2;
    private static final long INTERVAL = 15 * 60 * 1000L;

    /** Called from Rust whenever the user's settings change what a check brings. */
    static void schedule(Context context, boolean wanted) {
        JobScheduler scheduler = context.getSystemService(JobScheduler.class);
        if (!wanted) {
            scheduler.cancel(PERIODIC);
            return;
        }
        // Scheduling again would stop a check in progress.
        if (scheduler.getPendingJob(PERIODIC) != null) {
            return;
        }
        scheduler.schedule(new JobInfo.Builder(PERIODIC, new ComponentName(context, MailCheckJob.class))
                .setPeriodic(INTERVAL)
                .setRequiredNetworkType(JobInfo.NETWORK_TYPE_ANY)
                // Kept across restarts of the device.
                .setPersisted(true)
                .build());
    }

    /** One check as soon as there is a network. */
    static void checkSoon(Context context) {
        JobInfo.Builder builder = new JobInfo.Builder(SOON, new ComponentName(context, MailCheckJob.class))
                .setRequiredNetworkType(JobInfo.NETWORK_TYPE_ANY);
        if (Build.VERSION.SDK_INT >= 31) {
            builder.setExpedited(true);
        }
        context.getSystemService(JobScheduler.class).schedule(builder.build());
    }
}
