import { Configuration, ConfigurationConstant, UIAbility } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { BusinessError } from '@kit.BasicServicesKit';

const TAG = 'FlectarOhos';

export default class EntryAbility extends UIAbility {
  onBackground(): void {
    // The page releases any preview resources when this event arrives; using
    // the ability lifecycle covers background paths that onPageHide misses.
    this.context.eventHub.emit('flectarBackground');
  }

  onConfigurationUpdate(newConfig: Configuration): void {
    // The ArkUI page watches this AppStorage value and reports the new
    // light/dark preference to Slint.
    AppStorage.setOrCreate(
      'flectarColorMode',
      newConfig.colorMode ?? ConfigurationConstant.ColorMode.COLOR_MODE_NOT_SET);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    windowStage.loadContent('pages/Index', (err: BusinessError) => {
      if (err.code) {
        hilog.error(0x0000, TAG, 'loadContent failed: %{public}s', JSON.stringify(err));
        return;
      }
    });
  }

}
