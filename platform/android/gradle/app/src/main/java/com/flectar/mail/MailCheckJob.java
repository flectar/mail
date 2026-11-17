package com.flectar.mail;

import android.app.job.JobParameters;
import android.app.job.JobService;
import android.content.Context;

/**
 * A check for new mail that {@link MailChecks} asked Android for. It runs on
 * a thread of its own: Rust checks every account and tells of what came, with
 * the activity's mail store if it runs, otherwise with one opened for it.
 */
public final class MailCheckJob extends JobService {
    static {
        System.loadLibrary("flectar_mail_android");
    }

    /** Returns once every account was checked, or the check was stopped. */
    private static native boolean nativeCheck(Context context, int job);

    private static native void nativeStop(int job);

    @Override
    public boolean onStartJob(JobParameters params) {
        Context application = getApplicationContext();
        int job = params.getJobId();
        new Thread(() -> {
            nativeCheck(application, job);
            jobFinished(params, false);
        }, "flectar-mail-check").start();
        return true;
    }

    /** Android wants its time back: stop, and run the check again later. */
    @Override
    public boolean onStopJob(JobParameters params) {
        nativeStop(params.getJobId());
        return true;
    }
}
