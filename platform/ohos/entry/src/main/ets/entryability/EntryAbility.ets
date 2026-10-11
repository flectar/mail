import { AbilityConstant, bundleManager, UIAbility, Want } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { BusinessError } from '@kit.BasicServicesKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import native from 'libfeihong.so';
import { NativeSession } from '../services/NativeSession';

export default class EntryAbility extends UIAbility {
  private smoke?: Promise<void>;
  private windowGeneration: number = 0;
  private windowAlive: boolean = false;

  onCreate(want: Want, _launch: AbilityConstant.LaunchParam): void {
    const pdfiumSmoke = want.parameters?.['feihongPdfiumSmoke'] === 'true';
    const uiFixture = want.parameters?.['feihongUiFixture'] === 'true';
    const keepScreenOn = want.parameters?.['feihongKeepScreenOn'] === 'true';
    const performance = want.parameters?.['feihongPerformance'] === 'true';
    if (!pdfiumSmoke && !uiFixture && !keepScreenOn && !performance) { return; }
    try {
      const info = bundleManager.getBundleInfoForSelfSync(bundleManager.BundleFlag.GET_BUNDLE_INFO_WITH_APPLICATION);
      if (!info.appInfo.debug) { return; }
      NativeSession.configureDebugUiFixture(uiFixture);
      NativeSession.configureDebugScreenOn(keepScreenOn);
      NativeSession.configureDebugPerformance(performance);
      if (!pdfiumSmoke) { return; }
      this.smoke = native.pdfiumSelfTest().then((result: string): void => {
        hilog.info(0xF100, 'Feihong', 'PDFium fixture: %{public}s', result);
      }).catch((): void => {
        hilog.error(0xF100, 'Feihong', 'PDFium fixture failed');
      });
    } catch (_) {
      hilog.error(0xF100, 'Feihong', 'PDFium fixture could not start');
    }
  }

  onNewWant(want: Want, _launch: AbilityConstant.LaunchParam): void {
    const keepScreenOn = want.parameters?.['feihongKeepScreenOn'];
    if (keepScreenOn === 'true' || keepScreenOn === 'false') {
      try {
        const info = bundleManager.getBundleInfoForSelfSync(bundleManager.BundleFlag.GET_BUNDLE_INFO_WITH_APPLICATION);
        if (info.appInfo.debug) { NativeSession.configureDebugScreenOn(keepScreenOn === 'true'); }
      } catch (_) { hilog.warn(0xF100, 'Feihong', 'Debug screen-on flag could not be configured'); }
    }
    if (want.parameters?.['feihongPdfiumSmoke'] === 'true') {
      // PDFium is not thread-safe; never initialize a fixture beside the retained mail UI.
      hilog.warn(0xF100, 'Feihong', 'PDFium fixture requires a cold debug launch');
    }
    if (want.parameters?.['feihongUiFixture'] === 'true') {
      hilog.warn(0xF100, 'Feihong', 'Mail UI fixture requires a cold debug launch');
    }
    if (want.parameters?.['feihongPerformance'] === 'true') {
      hilog.warn(0xF100, 'Feihong', 'Performance diagnostics require a cold debug launch');
    }
  }

  onWindowStageCreate(stage: window.WindowStage): void {
    this.windowAlive = true;
    const generation = ++this.windowGeneration;
    if (this.smoke !== undefined) {
      this.smoke.then((): void => {
        if (this.windowAlive && this.windowGeneration === generation) { this.loadWindow(stage); }
      }).catch((error: BusinessError): void => {
        hilog.error(0xF100, 'Feihong', 'Window initialization failed: %{public}d %{public}s', error.code ?? 0, error.message);
      });
    } else { this.loadWindow(stage); }
  }

  private loadWindow(stage: window.WindowStage): void {
    const viewWindow = stage.getMainWindowSync();
    NativeSession.attachWindow(viewWindow);
    stage.loadContent('pages/Index', (error: BusinessError): void => {
      if (error.code) { hilog.error(0xF100, 'Feihong', 'Window load failed: %{public}d', error.code); }
    });
  }

  onForeground(): void { NativeSession.setForeground(true); }
  onBackground(): void { NativeSession.setForeground(false); }
  onWindowStageDestroy(): void { this.windowAlive = false; ++this.windowGeneration; NativeSession.stop(); }
  onDestroy(): void { this.windowAlive = false; ++this.windowGeneration; NativeSession.stop(); }
}
