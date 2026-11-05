import { AbilityConstant, Configuration, ConfigurationConstant, UIAbility } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { BusinessError } from '@kit.BasicServicesKit';
import { notificationManager } from '@kit.NotificationKit';

const TAG = 'FlectarOhos';

export default class EntryAbility extends UIAbility {
  onBackground(): void {
    // The page releases any preview resources when this event arrives; using
    // the ability lifecycle covers background paths that onPageHide misses.
    this.context.eventHub.emit('flectarBackground');
  }

  // The page forwards memory levels to Rust, which frees caches it can
  // rebuild. Every ability and stage receives each level, so only this one
  // handles them, and it returns at once: the release runs in Rust.
  onMemoryLevel(level: AbilityConstant.MemoryLevel): void {
    this.context.eventHub.emit('flectarMemoryLevel', level);
  }

  onConfigurationUpdate(newConfig: Configuration): void {
    // The ArkUI page watches this AppStorage value and reports the new
    // light/dark preference to Slint.
    AppStorage.setOrCreate(
      'flectarColorMode',
      newConfig.colorMode ?? ConfigurationConstant.ColorMode.COLOR_MODE_NOT_SET);
  }

  // Ask once for notification permission. A refusal is final at the system
  // level; the user can re-enable it in system settings later.
  private requestNotificationPermission(): void {
    notificationManager.isNotificationEnabled()
      .then((enabled: boolean): void => {
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

  onWindowStageCreate(windowStage: window.WindowStage): void {
    windowStage.loadContent('pages/Index', (err: BusinessError) => {
      if (err.code) {
        hilog.error(0x0000, TAG, 'loadContent failed: %{public}s', JSON.stringify(err));
        return;
      }
      // Notifications are requested only after the page has loaded; asking
      // earlier is rejected with 1600013 and the system prompt never renders.
      this.requestNotificationPermission();
    });
  }

}
