import { common } from '@kit.AbilityKit';
import { UIContext, window } from '@kit.ArkUI';
import { workScheduler } from '@kit.BackgroundTasksKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { i18n } from '@kit.LocalizationKit';
import native from 'libfeihong.so';
import { ServiceBridge } from './ServiceBridge';

interface StartOptions {
  filesDir: string;
  cacheDir: string;
  tempDir: string;
  databaseDir: string;
  foreground: boolean;
  headless: boolean;
  systemLanguage: string;
  debugUiFixture: boolean;
  performanceDiagnostics: boolean;
}

export function sessionOptions(context: common.Context, headless: boolean, debugUiFixture: boolean = false): string {
  const options: StartOptions = {
    filesDir: context.filesDir,
    cacheDir: context.cacheDir,
    tempDir: context.tempDir,
    databaseDir: context.databaseDir,
    foreground: !headless,
    headless: headless,
    systemLanguage: i18n.System.getSystemLanguage(),
    debugUiFixture: !headless && debugUiFixture,
    performanceDiagnostics: !headless && NativeSession.performanceDiagnosticsEnabled()
  };
  return JSON.stringify(options);
}

export class NativeSession {
  private static services?: ServiceBridge;
  private static started: boolean = false;
  private static foreground: boolean = true;
  private static debugUiFixture: boolean = false;
  private static debugScreenOn: boolean = false;
  private static debugPerformance: boolean = false;
  private static mainWindow?: window.Window;
  private static surfaceX: number = 0;
  private static surfaceY: number = 0;
  private static density: number = 1;
  private static rectListener: (event: window.RectChangeOptions) => void = (event: window.RectChangeOptions): void => {
    NativeSession.publishInputGeometry(event.rect);
  };

  static configureDebugUiFixture(value: boolean): void { NativeSession.debugUiFixture = value; }
  static configureDebugPerformance(value: boolean): void { NativeSession.debugPerformance = value; }
  static performanceDiagnosticsEnabled(): boolean { return NativeSession.debugPerformance; }

  static configureDebugScreenOn(value: boolean): void {
    // Reset the old window flag before disabling its owner, including onNewWant.
    if (!value) { NativeSession.keepDebugScreenOn(false); }
    NativeSession.debugScreenOn = value;
    NativeSession.keepDebugScreenOn(NativeSession.foreground);
  }

  static attachWindow(viewWindow: window.Window): void {
    NativeSession.detachWindow();
    NativeSession.mainWindow = viewWindow;
    try { viewWindow.on('windowRectChange', NativeSession.rectListener); }
    catch (_) { hilog.warn(0xF100, 'Feihong', 'Window position listener could not be attached'); }
    NativeSession.keepDebugScreenOn(NativeSession.foreground);
    NativeSession.publishInputGeometry();
  }

  static updateSurfaceArea(area: Area, density: number): void {
    // ArkUI globalPosition is relative to this window and uses vp.
    NativeSession.surfaceX = Number(area.globalPosition.x ?? 0);
    NativeSession.surfaceY = Number(area.globalPosition.y ?? 0);
    NativeSession.density = density;
    NativeSession.publishInputGeometry();
  }

  private static publishInputGeometry(rect?: window.Rect): void {
    const viewWindow = NativeSession.mainWindow;
    if (viewWindow === undefined) { return; }
    try {
      const properties = viewWindow.getWindowProperties();
      const windowRect = rect ?? properties.windowRect;
      // Window rectangles and the native IME cursor use physical screen pixels.
      native.setInputGeometry(properties.id,
        windowRect.left + NativeSession.surfaceX * NativeSession.density,
        windowRect.top + NativeSession.surfaceY * NativeSession.density);
    } catch (_) { hilog.warn(0xF100, 'Feihong', 'Input window geometry could not be refreshed'); }
  }

  private static detachWindow(): void {
    const viewWindow = NativeSession.mainWindow;
    NativeSession.keepDebugScreenOn(false);
    NativeSession.mainWindow = undefined;
    if (viewWindow !== undefined) {
      try { viewWindow.off('windowRectChange', NativeSession.rectListener); }
      catch (_) { /* A destroyed window no longer owns the listener. */ }
    }
    NativeSession.surfaceX = 0;
    NativeSession.surfaceY = 0;
  }

  private static keepDebugScreenOn(enabled: boolean): void {
    const viewWindow = NativeSession.mainWindow;
    if ((!NativeSession.debugUiFixture && !NativeSession.debugScreenOn) || viewWindow === undefined) { return; }
    // Debug-only drawing or explicitly requested live-account testing. This is
    // an app-window flag; it never changes the system timeout or mail profile.
    viewWindow.setWindowKeepScreenOn(enabled).catch((): void => {
      hilog.warn(0xF100, 'Feihong', 'Debug screen-on flag could not be updated');
    });
  }

  static start(context: common.UIAbilityContext, uiContext: UIContext): void {
    if (NativeSession.started) { return; }
    NativeSession.services?.dispose();
    NativeSession.services = new ServiceBridge(context, uiContext);
    NativeSession.services.setForeground(NativeSession.foreground);
    NativeSession.services.install();
    native.start(sessionOptions(context, false, NativeSession.debugUiFixture));
    native.setForeground(NativeSession.foreground);
    NativeSession.started = true;
    if (!NativeSession.debugUiFixture) { NativeSession.scheduleSync(); }
  }

  static installBackgroundServices(context: common.Context): void {
    if (NativeSession.started) { return; }
    NativeSession.services?.dispose();
    NativeSession.services = new ServiceBridge(context);
    NativeSession.services.install();
  }

  static stop(): void {
    NativeSession.detachWindow();
    NativeSession.services?.dispose();
    NativeSession.services = undefined;
    if (NativeSession.started) { native.stop(); }
    native.setInputGeometry(0, 0, 0);
    NativeSession.started = false;
  }

  static setForeground(value: boolean): void {
    NativeSession.foreground = value;
    NativeSession.services?.setForeground(value);
    native.setForeground(value);
    NativeSession.keepDebugScreenOn(value);
    if (value) { NativeSession.publishInputGeometry(); }
  }

  private static scheduleSync(): void {
    // Actual delivery is controlled by HarmonyOS and application activity.
    const work: workScheduler.WorkInfo = {
      workId: 1,
      bundleName: 'moe.kiwi.FlectarMail',
      abilityName: 'MailSyncAbility',
      isPersisted: true,
      networkType: workScheduler.NetworkType.NETWORK_TYPE_ANY,
      isRepeat: true,
      repeatCycleTime: 2 * 60 * 60 * 1000
    };
    try { workScheduler.startWork(work); }
    catch (_) { hilog.warn(0xF100, 'Feihong', 'Deferred mail sync could not be scheduled'); }
  }
}
