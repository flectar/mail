import { UIAbility } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { notificationManager } from '@kit.NotificationKit';
import { BusinessError } from '@kit.BasicServicesKit';

const TAG = 'FlectarOhos';

export default class EntryAbility extends UIAbility {
  onWindowStageCreate(windowStage: window.WindowStage): void {
    windowStage.loadContent('pages/Index', (err: BusinessError) => {
      if (err.code) {
        hilog.error(0x0000, TAG, 'loadContent failed: %{public}s', JSON.stringify(err));
        return;
      }
      // Notifications are requested only after the page has loaded: asking
      // earlier (for example from the page's aboutToAppear) is rejected with
      // 1600013 and the system prompt never renders.
      this.requestNotificationPermission();
    });
  }

  // Ask the user once to allow notifications for this app. A refusal is final
  // at the system level: the prompt is not shown again and later requests
  // fail with 1600004; the user can still enable notifications in the system
  // settings for this app.
  private requestNotificationPermission(): void {
    notificationManager.isNotificationEnabled()
      .then((enabled: boolean): void => {
        hilog.info(0x0000, TAG, 'notifications enabled: %{public}s', `${enabled}`);
        if (enabled) {
          return;
        }
        notificationManager.requestEnableNotification(this.context)
          .then((): void => {
            hilog.info(0x0000, TAG, 'notification permission granted');
          })
          .catch((error: BusinessError): void => {
            hilog.error(0x0000, TAG,
              'notification permission not granted: %{public}d %{public}s',
              error.code, error.message);
          });
      })
      .catch((error: BusinessError): void => {
        hilog.error(0x0000, TAG,
          'isNotificationEnabled failed: %{public}d %{public}s', error.code, error.message);
      });
  }
}
