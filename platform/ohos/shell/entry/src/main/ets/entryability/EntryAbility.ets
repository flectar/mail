import { AbilityConstant, Configuration, ConfigurationConstant, UIAbility } from '@kit.AbilityKit';
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

  onWindowStageCreate(windowStage: window.WindowStage): void {
    windowStage.loadContent('pages/Index', (err: BusinessError) => {
      if (err.code) {
        hilog.error(0x0000, TAG, 'loadContent failed: %{public}s', JSON.stringify(err));
      }
    });
  }

}
