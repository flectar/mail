import { WorkSchedulerExtensionAbility, workScheduler } from '@kit.BackgroundTasksKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import native from 'libfeihong.so';
import { NativeSession, sessionOptions } from '../services/NativeSession';

export default class MailSyncAbility extends WorkSchedulerExtensionAbility {
  private running: boolean = false;

  onWorkStart(work: workScheduler.WorkInfo): void {
    if (this.running) { return; }
    this.running = true;
    NativeSession.installBackgroundServices(this.context);
    native.runDeferredSync(sessionOptions(this.context, true)).catch(() => {
      hilog.warn(0xF100, 'Feihong', 'Deferred mail sync failed');
    }).finally(() => {
      this.running = false;
      try { workScheduler.stopWork(work, false); }
      catch (_) { hilog.warn(0xF100, 'Feihong', 'Deferred mail sync completion was not accepted'); }
    });
  }

  onWorkStop(_work: workScheduler.WorkInfo): void {
    if (this.running) { native.cancelDeferredSync(); }
    this.running = false;
  }
}
