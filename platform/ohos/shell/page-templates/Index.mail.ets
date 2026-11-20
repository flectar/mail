import { abilityAccessCtrl, common, ConfigurationConstant, PermissionRequestResult, Permissions, Want, wantAgent } from '@kit.AbilityKit';
import { accessibility } from '@kit.AccessibilityKit';
import { BusinessError, deviceInfo, pasteboard } from '@kit.BasicServicesKit';
import { fileIo, fileUri, picker, statfs } from '@kit.CoreFileKit';
import { inputMethod } from '@kit.IMEKit';
import { i18n } from '@kit.LocalizationKit';
import { notificationManager } from '@kit.NotificationKit';
import { preferences } from '@kit.ArkData';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';
import { util } from '@kit.ArkTS';

const LOG_TAG = 'FlectarOhos';
const PRIVACY_STORE = 'flectar_ohos';
const PRIVACY_KEY = 'privacy_accepted';
// The system reports "not enabled" both before the first prompt and after a
// refusal, so the page remembers that the prompt has already been shown.
const NOTIFICATION_ASKED_KEY = 'notification_asked';
// Lucide outline paths (ISC), drawn through Shape/Path so the gate uses the
// default palette in light and dark.
const CONSENT_SHIELD_PATH = 'M20 13c0 5-3.5 7.5-7.66 8.95a1 1 0 0 1-.67-.01C7.5 20.5 4 18 4 13V6a1 1 0 0 1 1-1c2 0 4.5-1.2 6.24-2.72a1.17 1.17 0 0 1 1.52 0C14.51 3.81 17 5 19 5a1 1 0 0 1 1 1z';
const CONSENT_X_PATH = 'M18 6 L6 18 M6 6 L18 18';
const CONSENT_CHECK_PATH = 'M20 6 L9 17 L5 12';
// The XComponent, as both its native id and its component id.
const SURFACE_ID = 'flectar_mail_surface';
// One document is copied for the Files pane, whose transfers stop here.
// Several are bounded by the free space alone, which a copy never takes
// below the reserve. The same limits apply on Android and iOS.
const SINGLE_DOCUMENT_LIMIT = 512 * 1024 * 1024;
const FREE_SPACE_RESERVE = 64 * 1024 * 1024;
// Services the native module invokes from Rust; the closures run on the ArkTS
// UI thread through a thread-safe function. `pickOpen`/`pickSave` are
// request/response: the page reports the outcome through `submitBridgeResult`.
interface FlectarBridge {
  openUrl: (url: string) => void;
  pickOpen: (requestId: number) => void;
  pickOpenMany: (requestId: number, limit: number) => void;
  pickSave: (requestId: number, sourcePath: string, suggestedName: string) => void;
  showIme: (password: boolean) => void;
  imeUpdateCursor: (left: number, top: number, width: number, height: number) => void;
  hideIme: () => void;
  requestPasteboard: () => void;
  notify: (sender: string, subject: string, threadId: number, messageId: number) => void;
  withdrawPrivacyConsent: () => void;
  requestNotificationPermission: () => void;
  setAppPreferredLanguage: (language: string) => void;
}

interface ImeCursor {
  left: number;
  top: number;
  width: number;
  height: number;
}

interface StagedFile {
  path: string;
  name: string;
}

// The exports of the native XComponent module (`onLoad` hands them over).
interface FlectarNativeModule {
  setBridge?: (bridge: FlectarBridge) => void;
  submitBridgeResult?: (requestId: number, path: string, name: string, error: string) => boolean;
  pickerClosed?: (requestId: number) => void;
  imeCommit?: (text: string) => void;
  imeDelete?: (count: number) => void;
  imeFunction?: (key: number) => void;
  setColorScheme?: (dark: boolean) => void;
  suspendPreview?: () => void;
  memoryLevel?: (level: number) => void;
  reduceMotion?: (reduced: boolean) => void;
  setClipboardText?: (text: string) => void;
  setNotificationAvailability?: (state: number) => void;
}

@Entry
@Component
struct Index {
  private nativeModule: FlectarNativeModule | undefined = undefined;
  @StorageLink('flectarColorMode') @Watch('onColorModeChanged') colorMode: number =
    ConfigurationConstant.ColorMode.COLOR_MODE_NOT_SET;
  private imeAttached: boolean = false;
  private imeAttachPending: boolean = false;
  private imePassword: boolean | undefined = undefined;
  private pendingImePassword: boolean | undefined = undefined;
  private observedWindow: window.Window | undefined = undefined;
  // Moving the window moves the surface on the screen without a layout pass.
  private windowRectHandler = (_options: window.RectChangeOptions): void => {
    this.applyImeCursor();
  };
  private appBackgroundHandler: () => void = (): void => {
    this.nativeModule?.suspendPreview?.();
  };
  private memoryLevelHandler: (level: number) => void = (level: number): void => {
    this.nativeModule?.memoryLevel?.(level);
  };
  // Animation reduction in the accessibility settings, which apps can read
  // from API 23.
  private reduceMotionHandler: (reduced: boolean) => void = (reduced: boolean): void => {
    this.nativeModule?.reduceMotion?.(reduced);
  };
  private lastImeCursor: string = '';
  private pendingImeCursor: ImeCursor | undefined = undefined;
  private imeCursorRetries: number = 0;
  private appliedColorMode: number = -2;
  @State private privacyChecked: boolean = false;
  @State private privacyAccepted: boolean = false;
  private notificationAsked: boolean = false;
  private pasteboardPermissionRequested: boolean = false;
  private systemPasteboard: pasteboard.SystemPasteboard | undefined = undefined;
  private clipboardUpdateHandler: () => void = (): void => {
    this.refreshClipboard();
  };
  aboutToAppear(): void {
    this.probeResources();
    this.loadPrivacyConsent();
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      AppStorage.setOrCreate('flectarColorMode', context.config.colorMode ?? -1);
      context.eventHub.on('flectarBackground', this.appBackgroundHandler);
      context.eventHub.on('flectarMemoryLevel', this.memoryLevelHandler);
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'could not read the system color mode: %{public}s',
        (error as BusinessError).message);
    }
    if (deviceInfo.sdkApiVersion >= 23) {
      try {
        accessibility.onAnimationReduceStateChange(this.reduceMotionHandler);
      } catch (error) {
        hilog.error(0x0000, LOG_TAG, 'could not follow animation reduction: %{public}s',
          (error as BusinessError).message);
      }
    }
  }

  onColorModeChanged(): void {
    this.applySystemColorScheme(this.colorMode);
  }

  onPageShow(): void {
    this.applySystemColorScheme(this.colorMode);
    this.applyReduceMotion();
    this.observeWindow();
    this.refreshClipboard();
    this.refreshNotificationAvailability();
  }

  onPageHide(): void {
    this.nativeModule?.suspendPreview?.();
  }

  // The IME controller is process-scoped; its listeners capture this page, so
  // they are released when the page goes away (they are also re-registered
  // idempotently on every attach).
  aboutToDisappear(): void {
    try {
      const controller = inputMethod.getController();
      controller.off('insertText');
      controller.off('deleteLeft');
      controller.off('sendFunctionKey');
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'IME listener cleanup failed: %{public}s',
        (error as BusinessError).message);
    }
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      context.eventHub.off('flectarBackground', this.appBackgroundHandler);
      context.eventHub.off('flectarMemoryLevel', this.memoryLevelHandler);
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'background listener cleanup failed: %{public}s',
        (error as BusinessError).message);
    }
    if (deviceInfo.sdkApiVersion >= 23) {
      try {
        accessibility.offAnimationReduceStateChange(this.reduceMotionHandler);
      } catch (error) {
        hilog.error(0x0000, LOG_TAG, 'animation reduction listener cleanup failed: %{public}s',
          (error as BusinessError).message);
      }
    }
    try {
      this.systemPasteboard?.off('update', this.clipboardUpdateHandler);
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'clipboard listener cleanup failed: %{public}s',
        (error as BusinessError).message);
    }
    this.systemPasteboard = undefined;
    this.releaseWindowObserver();
    this.imeAttached = false;
    this.imeAttachPending = false;
    this.imePassword = undefined;
    this.pendingImePassword = undefined;
    this.lastImeCursor = '';
  }

  build(): void {
    if (!this.privacyChecked) {
      Column() {
        LoadingProgress()
          .width(48)
          .height(48)
          .color(this.consentAccent())
      }
      .width('100%')
      .height('100%')
      .justifyContent(FlexAlign.Center)
      .backgroundColor(this.consentPageBackground())
    } else if (!this.privacyAccepted) {
      this.privacyConsentView()
    } else {
      Column() {
        XComponent({
          id: SURFACE_ID,
          type: XComponentType.SURFACE,
          libraryname: 'flectar_mail_ohos',
        })
          .id(SURFACE_ID)
          .width('100%')
          .height('100%')
          // Maximizing, restoring and split screen lay the surface out again.
          .onAreaChange((): void => {
            this.applyImeCursor();
          })
          .focusable(true)
          .defaultFocus(true)
          .onLoad((context?: object): void => {
            if (context) {
              this.installBridge(context);
            }
          })
      }
      .width('100%')
      .height('100%')
    }
  }

  private consentDarkMode(): boolean {
    return this.colorMode === ConfigurationConstant.ColorMode.COLOR_MODE_DARK;
  }

  private consentPageBackground(): string {
    return this.consentDarkMode() ? '#111213' : '#f2f2f0';
  }

  private consentSurface(): string {
    return this.consentDarkMode() ? '#18191a' : '#ffffff';
  }

  private consentTextColor(): string {
    return this.consentDarkMode() ? '#f3f3f2' : '#202120';
  }

  private consentSecondaryColor(): string {
    return this.consentDarkMode() ? '#a0a19e' : '#72736f';
  }

  private consentBorderColor(): string {
    return this.consentDarkMode() ? '#292a2b' : '#d9d9d6';
  }

  private consentBorderSubtleColor(): string {
    return this.consentDarkMode() ? '#232425' : '#e6e6e2';
  }

  private consentAccent(): string {
    return this.consentDarkMode() ? '#5ca3ff' : '#0969da';
  }

  private consentAccentSoft(): string {
    return this.consentDarkMode() ? '#26364a' : '#dcecff';
  }

  private consentPrimary(): string {
    return '#0969da';
  }

  @Builder
  consentShieldIcon(): void {
    Shape() {
      Path({ width: 24, height: 24, commands: CONSENT_SHIELD_PATH })
        .fill(Color.Transparent)
        .stroke(this.consentAccent())
        .strokeWidth(2)
        .strokeLineCap(LineCapStyle.Round)
        .strokeLineJoin(LineJoinStyle.Round)
    }
    .viewPort({ x: 0, y: 0, width: 24, height: 24 })
    .width(18)
    .height(18)
  }

  @Builder
  consentDeclineIcon(): void {
    Shape() {
      Path({ width: 24, height: 24, commands: CONSENT_X_PATH })
        .fill(Color.Transparent)
        .stroke(this.consentSecondaryColor())
        .strokeWidth(2)
        .strokeLineCap(LineCapStyle.Round)
        .strokeLineJoin(LineJoinStyle.Round)
    }
    .viewPort({ x: 0, y: 0, width: 24, height: 24 })
    .width(18)
    .height(18)
  }

  @Builder
  consentAgreeIcon(): void {
    Shape() {
      Path({ width: 24, height: 24, commands: CONSENT_CHECK_PATH })
        .fill(Color.Transparent)
        .stroke('#ffffff')
        .strokeWidth(2)
        .strokeLineCap(LineCapStyle.Round)
        .strokeLineJoin(LineJoinStyle.Round)
    }
    .viewPort({ x: 0, y: 0, width: 24, height: 24 })
    .width(18)
    .height(18)
  }

  @Builder
  consentBulletIcon(): void {
    Shape() {
      Path({ width: 24, height: 24, commands: CONSENT_CHECK_PATH })
        .fill(Color.Transparent)
        .stroke(this.consentAccent())
        .strokeWidth(2)
        .strokeLineCap(LineCapStyle.Round)
        .strokeLineJoin(LineJoinStyle.Round)
    }
    .viewPort({ x: 0, y: 0, width: 24, height: 24 })
    .width(14)
    .height(14)
  }

  @Builder
  consentBullet(text: ResourceStr): void {
    Row({ space: 12 }) {
      Stack() {
        this.consentBulletIcon()
      }
      .width(24)
      .height(24)
      .borderRadius(12)
      .backgroundColor(this.consentAccentSoft())

      Text(text)
        .fontSize(14)
        .lineHeight(21)
        .fontColor(this.consentSecondaryColor())
        .layoutWeight(1)
    }
    .width('100%')
    .alignItems(VerticalAlign.Top)
  }

  @Builder
  privacyConsentView(): void {
    Column() {
      Scroll() {
        Row() {
          Column({ space: 0 }) {
            Image($r('app.media.app_icon'))
              .width(76)
              .height(76)
              .borderRadius(18)
              .margin({ bottom: 18 })

            Text($r('app.string.privacy_consent_title'))
              .fontSize(28)
              .fontWeight(700)
              .fontColor(this.consentTextColor())
              .textAlign(TextAlign.Center)
              .width('100%')

            Text($r('app.string.privacy_consent_intro'))
              .fontSize(15)
              .lineHeight(23)
              .fontColor(this.consentSecondaryColor())
              .textAlign(TextAlign.Center)
              .width('100%')
              .margin({ top: 8, bottom: 24 })

            Column({ space: 14 }) {
              this.consentBullet($r('app.string.privacy_consent_bullet_local'))
              this.consentBullet($r('app.string.privacy_consent_bullet_clipboard'))
              this.consentBullet($r('app.string.privacy_consent_bullet_withdraw'))
            }
            .width('100%')
            .padding(20)
            .backgroundColor(this.consentSurface())
            .borderRadius(12)
            .borderWidth(1)
            .borderColor(this.consentBorderSubtleColor())

            Button() {
              Row({ space: 8 }) {
                this.consentShieldIcon()
                Text($r('app.string.privacy_consent_view_policy'))
                  .fontSize(14)
                  .fontWeight(500)
                  .fontColor(this.consentAccent())
                  .textAlign(TextAlign.Center)
                  .maxLines(2)
                  .textOverflow({ overflow: TextOverflow.Ellipsis })
              }
              .width('100%')
              .justifyContent(FlexAlign.Center)
            }
            .width('100%')
            .height(46)
            .padding({ left: 12, right: 12 })
            .type(ButtonType.Normal)
            .backgroundColor(this.consentAccentSoft())
            .borderRadius(8)
            .margin({ top: 20 })
            .onClick((): void => {
              this.openPrivacyPolicy();
            })

            Row({ space: 8 }) {
              Button() {
                Row({ space: 6 }) {
                  this.consentDeclineIcon()
                  Text($r('app.string.privacy_consent_decline'))
                    .fontSize(14)
                    .fontWeight(500)
                    .fontColor(this.consentSecondaryColor())
                    .textAlign(TextAlign.Center)
                    .maxLines(2)
                    .textOverflow({ overflow: TextOverflow.Ellipsis })
                }
                .width('100%')
                .justifyContent(FlexAlign.Center)
              }
              .layoutWeight(1)
              .height(50)
              .padding({ left: 8, right: 8 })
              .type(ButtonType.Normal)
              .backgroundColor(this.consentSurface())
              .borderRadius(8)
              .borderWidth(1)
              .borderColor(this.consentBorderColor())
              .onClick((): void => {
                this.declinePrivacy();
              })

              Button() {
                Row({ space: 6 }) {
                  this.consentAgreeIcon()
                  Text($r('app.string.privacy_consent_agree'))
                    .fontSize(14)
                    .fontWeight(600)
                    .fontColor('#ffffff')
                    .textAlign(TextAlign.Center)
                    .maxLines(2)
                    .textOverflow({ overflow: TextOverflow.Ellipsis })
                }
                .width('100%')
                .justifyContent(FlexAlign.Center)
              }
              .layoutWeight(1)
              .height(50)
              .padding({ left: 8, right: 8 })
              .type(ButtonType.Normal)
              .backgroundColor(this.consentPrimary())
              .borderRadius(8)
              .onClick((): void => {
                this.acceptPrivacy();
              })
            }
            .width('100%')
            .margin({ top: 10 })
          }
          .width('100%')
          .constraintSize({ maxWidth: 560 })
          .alignItems(HorizontalAlign.Center)
          .padding({ left: 24, right: 24, top: 36, bottom: 28 })
        }
        .width('100%')
        .justifyContent(FlexAlign.Center)
      }
      .width('100%')
      .layoutWeight(1)
      .scrollBar(BarState.Off)
    }
    .width('100%')
    .height('100%')
    .backgroundColor(this.consentPageBackground())
    .alignItems(HorizontalAlign.Center)
  }

  private privacyLanguage(): string {
    try {
      // HarmonyOS returns BCP-47 tags ("zh-Hant-HK"); normalize the separator
      // so the Traditional checks also match plain region tags.
      const language = i18n.System.getAppPreferredLanguage()
        .toLowerCase()
        .split('-')
        .join('_');
      // Traditional Chinese falls back to English in the app; keep the policy
      // link on the same language as the consent text.
      if (language.includes('hant') || language.startsWith('zh_tw')
        || language.startsWith('zh_hk') || language.startsWith('zh_mo')) {
        return 'en';
      }
      if (language.startsWith('zh')) {
        return 'zh_Hans';
      }
      if (language.startsWith('tr')) {
        return 'tr';
      }
      if (language.startsWith('es')) {
        return 'es';
      }
      if (language.startsWith('ru')) {
        return 'ru';
      }
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'could not read the app language: %{public}s',
        (error as BusinessError).message);
    }
    return 'en';
  }

  private privacyPolicyUrl(): string {
    const language = this.privacyLanguage();
    if (language === 'zh_Hans') {
      return 'https://flectar.com/zh-hans/mail/privacy/';
    }
    if (language === 'tr') {
      return 'https://flectar.com/tr/mail/privacy/';
    }
    if (language === 'es') {
      return 'https://flectar.com/es/mail/privacy/';
    }
    if (language === 'ru') {
      return 'https://flectar.com/ru/mail/privacy/';
    }
    return 'https://flectar.com/mail/privacy/';
  }

  private openPrivacyPolicy(): void {
    this.openUrl(this.privacyPolicyUrl());
  }

  private async loadPrivacyConsent(): Promise<void> {
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const store = await preferences.getPreferences(context, PRIVACY_STORE);
      this.privacyAccepted = (await store.get(PRIVACY_KEY, false)) as boolean;
      this.notificationAsked = (await store.get(NOTIFICATION_ASKED_KEY, false)) as boolean;
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'privacy preferences unavailable: %{public}s',
        (error as BusinessError).message);
      this.privacyAccepted = false;
    } finally {
      this.privacyChecked = true;
    }
  }

  private async acceptPrivacy(): Promise<void> {
    let stored = false;
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const store = await preferences.getPreferences(context, PRIVACY_STORE);
      await store.put(PRIVACY_KEY, true);
      await store.flush();
      stored = true;
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'privacy consent could not be stored: %{public}s',
        (error as BusinessError).message);
    }
    if (stored) {
      this.privacyAccepted = true;
    }
  }

  private async withdrawPrivacy(): Promise<void> {
    let cleared = false;
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const store = await preferences.getPreferences(context, PRIVACY_STORE);
      await store.put(PRIVACY_KEY, false);
      await store.flush();
      cleared = true;
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'privacy consent could not be withdrawn: %{public}s',
        (error as BusinessError).message);
    }
    if (!cleared) {
      return;
    }
    this.privacyAccepted = false;
    this.declinePrivacy();
  }

  private declinePrivacy(): void {
    const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
    context.terminateSelf().catch((error: BusinessError): void => {
      hilog.error(0x0000, LOG_TAG, 'could not stop after declining privacy: %{public}s',
        error.message);
    });
  }

  // Diagnostic: resolves the app's own resources once per launch, so a hap
  // whose resource table failed to load is visible in hilog instead of only
  // showing up as a blank launcher icon. `app_name` lives in AppScope (outside
  // the module) and the ArkTS checker only sees module resources, so the probe
  // reads the module-scoped label instead — same table, same failure mode.
  private probeResources(): void {
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const manager = context.resourceManager;
      const label = manager.getStringSync($r('app.string.EntryAbility_label').id);
      const icon = manager.getMediaContentSync($r('app.media.app_icon').id);
      hilog.info(0x0000, LOG_TAG, 'resource probe: app_name=%{public}s icon=%{public}d bytes',
        label, icon.byteLength);
    } catch (error) {
      const failure = error as BusinessError;
      hilog.error(0x0000, LOG_TAG, 'resource probe failed: %{public}d %{public}s',
        failure.code, failure.message);
    }
  }

  // Hand the ArkTS-side services to the native module: sign-in opens the
  // system browser with an implicit Want, and import/export opens the system
  // document picker — neither is reachable from native code directly.
  private installBridge(context: object): void {
    const nativeModule = context as FlectarNativeModule;
    if (nativeModule.setBridge === undefined) {
      hilog.error(0x0000, LOG_TAG, 'native module has no setBridge');
      return;
    }
    this.nativeModule = nativeModule;
    nativeModule.setBridge({
      openUrl: (url: string): void => {
        this.openUrl(url);
      },
      pickOpen: (requestId: number): void => {
        this.pickOpen(requestId);
      },
      pickOpenMany: (requestId: number, limit: number): void => {
        this.pickOpenMany(requestId, limit);
      },
      pickSave: (requestId: number, sourcePath: string, suggestedName: string): void => {
        this.pickSave(requestId, sourcePath, suggestedName);
      },
      showIme: (password: boolean): void => {
        this.showIme(password);
      },
      imeUpdateCursor: (left: number, top: number, width: number, height: number): void => {
        this.imeUpdateCursor(left, top, width, height);
      },
      hideIme: (): void => {
        this.hideIme();
      },
      requestPasteboard: (): void => {
        this.requestPasteboardPermission();
      },
      notify: (sender: string, subject: string, threadId: number, messageId: number): void => {
        this.notify(sender, subject, threadId, messageId);
      },
      withdrawPrivacyConsent: (): void => {
        this.withdrawPrivacy();
      },
      requestNotificationPermission: (): void => {
        this.requestNotificationPermission();
      },
      setAppPreferredLanguage: (language: string): void => {
        this.updateAppPreferredLanguage(language);
      },
    });
    this.applySystemColorScheme(this.colorMode);
    this.applyReduceMotion();
    this.observeWindow();
    this.installClipboardListener();
    this.refreshNotificationAvailability();
  }

  private reportBridgeResult(
    requestId: number, path: string, name: string, error: string = ''): boolean {
    return this.nativeModule?.submitBridgeResult?.(requestId, path, name, error) ?? false;
  }

  // The Slint thread must never read the pasteboard: a cross-device clipboard
  // can stall it. Keep a UI-thread cache fresh and push it to native code.
  private installClipboardListener(): void {
    try {
      this.systemPasteboard = pasteboard.getSystemPasteboard();
      this.systemPasteboard.on('update', this.clipboardUpdateHandler);
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'clipboard listener failed: %{public}s',
        (error as BusinessError).message);
    }
    this.refreshClipboard();
  }

  private refreshClipboard(): void {
    const board = this.systemPasteboard ?? pasteboard.getSystemPasteboard();
    board.getData().then((data: pasteboard.PasteData): void => {
      let text = '';
      try {
        text = data.getPrimaryText() ?? '';
      } catch (error) {
        hilog.error(0x0000, LOG_TAG, 'clipboard text extraction failed: %{public}s',
          (error as BusinessError).message);
      }
      this.nativeModule?.setClipboardText?.(text);
    }).catch((error: BusinessError): void => {
      if (error.code === 201) {
        this.requestPasteboardPermission();
      } else {
        hilog.error(0x0000, LOG_TAG, 'clipboard read failed: %{public}d %{public}s',
          error.code, error.message);
      }
    });
  }

  private openUrl(url: string): void {
    // Links come from mail content; only hand web and dialer schemes to the
    // system. Everything else (file:, app:, custom schemes) stays in-app.
    const separator = url.indexOf(':');
    const scheme = separator > 0 ? url.substring(0, separator).toLowerCase() : '';
    if (scheme !== 'http' && scheme !== 'https' && scheme !== 'mailto' && scheme !== 'tel') {
      hilog.error(0x0000, LOG_TAG, 'refusing to open a non-web address (scheme=%{public}s)', scheme);
      return;
    }
    const abilityContext = this.getUIContext().getHostContext() as common.UIAbilityContext;
    const want: Want = {
      action: 'ohos.want.action.viewData',
      entities: ['entity.system.browsable'],
      uri: url,
    };
    abilityContext.startAbility(want)
      .then((): void => {
        hilog.info(0x0000, LOG_TAG, 'opened the sign-in page in the system browser');
      })
      .catch((error: BusinessError): void => {
        hilog.error(0x0000, LOG_TAG, 'could not open the system browser: %{public}s', error.message);
      });
  }

  // Select one document and stage a copy in the app cache; the native side
  // only ever sees the sandbox path. An empty path means "cancelled".
  private async pickOpen(requestId: number): Promise<void> {
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const viewPicker = new picker.DocumentViewPicker(context);
      const options = new picker.DocumentSelectOptions();
      options.maxSelectNumber = 1;
      const uris = await viewPicker.select(options);
      this.nativeModule?.pickerClosed?.(requestId);
      if (uris.length === 0) {
        this.reportBridgeResult(requestId, '', '', '');
        return;
      }
      await this.checkSpace(context, uris, SINGLE_DOCUMENT_LIMIT);
      const staged = await this.stageCopy(context, uris[0]);
      hilog.info(0x0000, LOG_TAG, 'picker imported a document (%{public}d picked)', uris.length);
      if (!this.reportBridgeResult(requestId, staged.path, staged.name, '')) {
        // The native request timed out or was cancelled while the picker was
        // open; do not leave the staged copy behind.
        fileIo.unlink(staged.path).catch((error: BusinessError): void => {
          hilog.error(0x0000, LOG_TAG, 'could not remove the orphaned import: %{public}s',
            error.message);
        });
      }
    } catch (error) {
      const failure = error as BusinessError;
      hilog.error(0x0000, LOG_TAG, 'document picker failed: %{public}d %{public}s',
        failure.code, failure.message);
      this.reportBridgeResult(requestId, '', '', failure.message ?? 'document picker failed');
    }
  }

  // Select several documents and stage each copy. The native side receives a
  // JSON array only after every copy succeeds; the paths never leave the app.
  private async pickOpenMany(requestId: number, limit: number): Promise<void> {
    const staged: StagedFile[] = [];
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const viewPicker = new picker.DocumentViewPicker(context);
      const options = new picker.DocumentSelectOptions();
      options.maxSelectNumber = Math.max(1, limit);
      const uris = await viewPicker.select(options);
      this.nativeModule?.pickerClosed?.(requestId);
      if (uris.length === 0) {
        this.reportBridgeResult(requestId, '', '', '');
        return;
      }
      await this.checkSpace(context, uris, Number.MAX_SAFE_INTEGER);
      for (const uri of uris) {
        staged.push(await this.stageCopy(context, uri));
      }
      const result = this.reportBridgeResult(requestId, JSON.stringify(staged), '', '');
      if (!result) {
        this.discardStaged(staged);
      }
    } catch (error) {
      this.discardStaged(staged);
      const failure = error as BusinessError;
      hilog.error(0x0000, LOG_TAG, 'multi-document picker failed: %{public}d %{public}s',
        failure.code, failure.message);
      this.reportBridgeResult(requestId, '', '', failure.message ?? 'document picker failed');
    }
  }

  private discardStaged(files: StagedFile[]): void {
    for (const file of files) {
      fileIo.unlink(file.path).catch((error: BusinessError): void => {
        hilog.error(0x0000, LOG_TAG, 'could not remove the orphaned import: %{public}s',
          error.message);
      });
    }
  }

  // Ask for a destination and copy the staged file there.
  private async pickSave(requestId: number, sourcePath: string, suggestedName: string): Promise<void> {
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const viewPicker = new picker.DocumentViewPicker(context);
      const options = new picker.DocumentSaveOptions();
      options.newFileNames = [suggestedName];
      const uris = await viewPicker.save(options);
      this.nativeModule?.pickerClosed?.(requestId);
      if (uris.length === 0) {
        this.reportBridgeResult(requestId, '', '', '');
        return;
      }
      const destination = await fileIo.open(
        uris[0], fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.TRUNC);
      try {
        const source = await fileIo.open(sourcePath, fileIo.OpenMode.READ_ONLY);
        try {
          await fileIo.copyFile(source.fd, destination.fd);
        } finally {
          await fileIo.close(source);
        }
      } finally {
        await fileIo.close(destination);
      }
      hilog.info(0x0000, LOG_TAG, 'picker exported a document');
      this.reportBridgeResult(requestId, uris[0], '', '');
    } catch (error) {
      const failure = error as BusinessError;
      hilog.error(0x0000, LOG_TAG, 'document save failed: %{public}d %{public}s',
        failure.code, failure.message);
      this.reportBridgeResult(requestId, '', '', failure.message ?? 'document save failed');
    }
  }

  // Slint enables the input method when an editable field gains focus. The
  // controller refuses event subscription until it is attached, so attach
  // first and register the text callbacks once it succeeds. A password field
  // changes the keyboard type, which the controller only reads at attach
  // time, so a type change detaches and attaches again.
  private showIme(password: boolean): void {
    try {
      if (this.imeAttachPending) {
        this.pendingImePassword = password;
        return;
      }
      const controller = inputMethod.getController();
      if (this.imeAttached && this.imePassword === password) {
        controller.showTextInput(inputMethod.RequestKeyboardReason.TOUCH)
          .then((): void => {
            // updateCursor can be rejected with 12800016 until the client is
            // editable; replay the latest caret once the keyboard is shown.
            this.applyImeCursor();
          })
          .catch((error: BusinessError): void => {
            hilog.error(0x0000, LOG_TAG, 'IME show failed: %{public}d %{public}s',
              error.code, error.message);
            if (error.code === 12800009 || error.code === 12800003) {
              // The service dropped the client (detached), e.g. after the app
              // left the foreground. Clear the flag and attach again so the
              // keyboard can come up on the next focus.
              this.imeAttached = false;
              this.showIme(password);
            }
          });
        return;
      }
      if (this.imeAttached) {
        this.imeAttachPending = true;
        controller.detach().then((): void => {
          this.imeAttached = false;
          this.imeAttachPending = false;
          this.imePassword = undefined;
          this.lastImeCursor = '';
          const pending = this.pendingImePassword;
          this.pendingImePassword = undefined;
          this.showIme(pending ?? password);
        }).catch((error: BusinessError): void => {
          this.imeAttachPending = false;
          const pending = this.pendingImePassword;
          this.pendingImePassword = undefined;
          if (pending !== undefined) {
            this.showIme(pending);
          }
          hilog.error(0x0000, LOG_TAG, 'IME detach failed: %{public}d %{public}s',
            error.code, error.message);
        });
        return;
      }
      const config: inputMethod.TextConfig = {
        inputAttribute: {
          textInputType: password
            ? inputMethod.TextInputType.VISIBLE_PASSWORD
            : inputMethod.TextInputType.TEXT,
          enterKeyType: password
            ? inputMethod.EnterKeyType.DONE
            : inputMethod.EnterKeyType.NEWLINE,
        },
      };
      // The touch reason asks the IME service to raise the soft keyboard even
      // when a physical keyboard is attached (a 2in1 policy would otherwise
      // suppress it).
      this.imeAttachPending = true;
      controller.attach(true, config, inputMethod.RequestKeyboardReason.TOUCH).then((): void => {
        this.imeAttached = true;
        this.imeAttachPending = false;
        this.imePassword = password;
        this.lastImeCursor = '';
        this.imeCursorRetries = 0;
        // The first cursor update usually arrives while attach() is pending;
        // replay it now that the controller accepts updateCursor().
        this.applyImeCursor();
        // (Re-)register exactly one set of handlers: a stale registration from
        // an earlier attach (or a duplicate one) would double-insert text.
        controller.off('insertText');
        controller.off('deleteLeft');
        controller.off('sendFunctionKey');
        controller.on('insertText', (text: string): void => {
          this.nativeModule?.imeCommit?.(text);
        });
        controller.on('deleteLeft', (length: number): void => {
          this.nativeModule?.imeDelete?.(length);
        });
        controller.on('sendFunctionKey', (functionKey: inputMethod.FunctionKey): void => {
          this.nativeModule?.imeFunction?.(functionKey.enterKeyType);
        });
        hilog.info(0x0000, LOG_TAG, 'IME attached');
        const pending = this.pendingImePassword;
        this.pendingImePassword = undefined;
        if (pending !== undefined && pending !== password) {
          this.showIme(pending);
        }
      }).catch((error: BusinessError): void => {
        this.imeAttachPending = false;
        const pending = this.pendingImePassword;
        this.pendingImePassword = undefined;
        if (pending !== undefined && pending !== password) {
          this.showIme(pending);
        }
        hilog.error(0x0000, LOG_TAG, 'IME attach failed: %{public}d %{public}s',
          error.code, error.message);
      });
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'IME setup failed: %{public}s',
        (error as BusinessError).message);
    }
  }

  // Slint reports the caret rectangle while the field is focused. The system
  // IME uses it to place the candidate window. Remember the latest rectangle
  // even while attach() is pending; applyImeCursor() replays it afterwards.
  private imeUpdateCursor(left: number, top: number, width: number, height: number): void {
    const old = this.pendingImeCursor;
    this.pendingImeCursor = { left, top, width, height };
    if (old === undefined || old.left !== left || old.top !== top ||
      old.width !== width || old.height !== height) {
      this.imeCursorRetries = 0;
    }
    this.applyImeCursor();
  }

  private imeCursorKey(cursor: ImeCursor): string {
    return `${cursor.left},${cursor.top},${cursor.width},${cursor.height}`;
  }

  private applyImeCursor(): void {
    if (!this.imeAttached || this.pendingImeCursor === undefined) {
      return;
    }
    const cursor = this.pendingImeCursor;
    const rect = this.imeCursorKey(cursor);
    try {
      // CursorInfo is in px on the screen, while Slint reports the caret in vp
      // relative to the surface. Offset it by where the surface is on the
      // screen now, which follows window moves, maximizing and split screen.
      const ui = this.getUIContext();
      const origin = ui.getComponentUtils().getRectangleById(SURFACE_ID).screenOffset;
      const key = `${origin.x},${origin.y},${rect}`;
      if (key === this.lastImeCursor) {
        return;
      }
      const px: inputMethod.CursorInfo = {
        left: origin.x + ui.vp2px(cursor.left),
        top: origin.y + ui.vp2px(cursor.top),
        width: ui.vp2px(cursor.width),
        height: ui.vp2px(cursor.height),
      };
      inputMethod.getController().updateCursor(px).then((): void => {
        // Only remember a rectangle that is still the latest one. An older
        // request completing late must not overwrite a newer caret.
        const current = this.pendingImeCursor;
        if (current !== undefined && this.imeCursorKey(current) === rect) {
          this.lastImeCursor = key;
          this.imeCursorRetries = 0;
        } else {
          this.applyImeCursor();
        }
      }).catch((error: BusinessError): void => {
        hilog.error(0x0000, LOG_TAG, 'IME cursor update failed: %{public}d', error.code);
        // 12800016 means the client is not editable yet (often while
        // showTextInput/attach is still settling). Do not mark this rectangle
        // as applied; retry the current one shortly, otherwise identical
        // updates would be deduplicated forever.
        const current = this.pendingImeCursor;
        if (current === undefined || this.imeCursorKey(current) !== rect) {
          this.applyImeCursor();
          return;
        }
        this.lastImeCursor = '';
        if (this.imeCursorRetries < 4) {
          this.imeCursorRetries += 1;
          setTimeout((): void => {
            this.applyImeCursor();
          }, 120);
        }
      });
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'IME cursor update failed: %{public}s',
        (error as BusinessError).message);
    }
  }

  private hideIme(): void {
    if (!this.imeAttached) {
      return;
    }
    try {
      inputMethod.getController().hideTextInput().catch((error: BusinessError): void => {
        hilog.error(0x0000, LOG_TAG, 'IME hide failed: %{public}d', error.code);
        if (error.code === 12800009 || error.code === 12800003) {
          // Already detached on the service side; forget the local state.
          this.imeAttached = false;
        }
      });
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'IME hide failed: %{public}s',
        (error as BusinessError).message);
    }
  }

  // Follow the app window once, so moving it re-places the IME candidate.
  private async observeWindow(): Promise<void> {
    if (this.observedWindow !== undefined) {
      return;
    }
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const appWindow = await window.getLastWindow(context);
      if (this.observedWindow !== undefined) {
        return;
      }
      this.observedWindow = appWindow;
      appWindow.on('windowRectChange', this.windowRectHandler);
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'could not observe the window: %{public}s',
        (error as BusinessError).message);
    }
  }

  private releaseWindowObserver(): void {
    if (this.observedWindow === undefined) {
      return;
    }
    this.observedWindow.off('windowRectChange', this.windowRectHandler);
    this.observedWindow = undefined;
  }

  private applyReduceMotion(): void {
    if (this.nativeModule === undefined) {
      return;
    }
    if (deviceInfo.sdkApiVersion >= 23) {
      try {
        this.nativeModule.reduceMotion?.(accessibility.isAnimationReduceEnabledSync());
      } catch (error) {
        hilog.error(0x0000, LOG_TAG, 'could not read animation reduction: %{public}s',
          (error as BusinessError).message);
      }
    }
  }

  private applySystemColorScheme(mode: number): void {
    try {
      let colorMode = mode;
      if (colorMode < 0) {
        const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
        colorMode = context.config.colorMode ?? ConfigurationConstant.ColorMode.COLOR_MODE_NOT_SET;
      }
      if (this.nativeModule === undefined || colorMode === this.appliedColorMode) {
        return;
      }
      this.appliedColorMode = colorMode;
      this.nativeModule.setColorScheme?.(
        colorMode === ConfigurationConstant.ColorMode.COLOR_MODE_DARK);
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'could not apply the system color mode: %{public}s',
        (error as BusinessError).message);
    }
  }

  private setNotificationAvailability(state: number): void {
    this.nativeModule?.setNotificationAvailability?.(state);
  }

  // The page owns the system notification state. Rust asks through the host
  // hook once an account exists; this keeps the cache current so the request
  // is not repeated after a refusal.
  private refreshNotificationAvailability(): void {
    notificationManager.isNotificationEnabled()
      .then((enabled: boolean): void => {
        this.setNotificationAvailability(enabled ? 1 : (this.notificationAsked ? 2 : 0));
      })
      .catch((error: BusinessError): void => {
        hilog.error(0x0000, LOG_TAG,
          'isNotificationEnabled failed: %{public}d %{public}s', error.code, error.message);
        this.setNotificationAvailability(0);
      });
  }

  private async persistNotificationAsked(asked: boolean): Promise<void> {
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const store = await preferences.getPreferences(context, PRIVACY_STORE);
      await store.put(NOTIFICATION_ASKED_KEY, asked);
      await store.flush();
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'notification state could not be stored: %{public}s',
        (error as BusinessError).message);
    }
  }

  // Rust calls this when the accounts change, starting with the first one. A
  // refusal must not bring the prompt back, so the page records that it asked;
  // only a failure to show the dialog leaves a retry open.
  private requestNotificationPermission(): void {
    if (this.notificationAsked) {
      this.refreshNotificationAvailability();
      return;
    }
    this.notificationAsked = true;
    this.persistNotificationAsked(true);
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      notificationManager.isNotificationEnabled()
        .then((enabled: boolean): void => {
          if (enabled) {
            this.setNotificationAvailability(1);
            return;
          }
          notificationManager.requestEnableNotification(context)
            .then((): void => {
              this.setNotificationAvailability(1);
              hilog.info(0x0000, LOG_TAG, 'notification permission granted');
            })
            .catch((error: BusinessError): void => {
              if (error.code === 1600004) {
                this.setNotificationAvailability(2);
              } else {
                this.notificationAsked = false;
                this.persistNotificationAsked(false);
                this.setNotificationAvailability(0);
              }
              hilog.error(0x0000, LOG_TAG,
                'notification permission not granted: %{public}d %{public}s',
                error.code, error.message);
            });
        })
        .catch((error: BusinessError): void => {
          this.notificationAsked = false;
          this.persistNotificationAsked(false);
          this.setNotificationAvailability(0);
          hilog.error(0x0000, LOG_TAG,
            'isNotificationEnabled failed: %{public}d %{public}s', error.code, error.message);
        });
    } catch (error) {
      this.notificationAsked = false;
      this.persistNotificationAsked(false);
      this.setNotificationAvailability(0);
      hilog.error(0x0000, LOG_TAG, 'notification permission request failed: %{public}s',
        (error as BusinessError).message);
    }
  }

  // Keep ArkTS resource loading aligned with the Slint language setting. The
  // system applies a chosen language at once, and 'default', which follows
  // the system language, from the next cold start.
  private updateAppPreferredLanguage(language: string): void {
    try {
      i18n.System.setAppPreferredLanguage(language);
      hilog.info(0x0000, LOG_TAG, 'app preferred language set: %{public}s', language);
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'could not set app preferred language: %{public}s',
        (error as BusinessError).message);
    }
  }

  // Publish one new-mail notification. Rust sends raw sender/subject fields;
  // fixed fallback text comes from resources so the notification follows the
  // application language. Tapping it opens the app; the want carries the
  // thread id for opening the thread once the app handles it.
  private async notify(
    sender: string, subject: string, threadId: number, messageId: number): Promise<void> {
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const manager = context.resourceManager;
      const title = sender.trim().length > 0
        ? sender
        : manager.getStringSync($r('app.string.notification_new_mail').id);
      const text = subject.trim().length > 0
        ? subject
        : manager.getStringSync($r('app.string.notification_no_subject').id);
      const id = Math.abs(threadId > 0 ? threadId : messageId) % 2147483647;
      const agent = await wantAgent.getWantAgent({
        wants: [{
          bundleName: context.abilityInfo.bundleName,
          abilityName: context.abilityInfo.name,
          parameters: { threadId: threadId },
        }],
        actionType: wantAgent.OperationType.START_ABILITY,
        requestCode: id,
      });
      const request: notificationManager.NotificationRequest = {
        id,
        notificationSlotType: notificationManager.SlotType.SOCIAL_COMMUNICATION,
        content: {
          notificationContentType:
            notificationManager.ContentType.NOTIFICATION_CONTENT_BASIC_TEXT,
          normal: { title, text },
        },
        wantAgent: agent,
      };
      await notificationManager.publish(request);
      hilog.info(0x0000, LOG_TAG, 'notification published');
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'notification failed: %{public}s',
        (error as BusinessError).message);
    }
  }

  private requestPasteboardPermission(): void {
    if (this.pasteboardPermissionRequested) {
      return;
    }
    this.pasteboardPermissionRequested = true;
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const manager = abilityAccessCtrl.createAtManager();
      const permissions: Permissions[] = ['ohos.permission.READ_PASTEBOARD'];
      manager.requestPermissionsFromUser(context, permissions)
        .then((result: PermissionRequestResult): void => {
          const granted = result.authResults.length > 0 && result.authResults[0] === 0;
          hilog.info(0x0000, LOG_TAG, `pasteboard permission granted: ${granted}`);
          if (granted) {
            this.refreshClipboard();
          } else {
            this.showPasteboardNotice();
          }
        })
        .catch((error: BusinessError): void => {
          hilog.error(0x0000, LOG_TAG,
            'pasteboard permission request failed: %{public}d', error.code);
        });
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'pasteboard permission request failed: %{public}s',
        (error as BusinessError).message);
    }
  }

  private showPasteboardNotice(): void {
    try {
      this.getUIContext().getPromptAction().showToast({
        message: 'Clipboard access is off; enable it in Settings to paste.',
        duration: 3000,
      });
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'pasteboard notice failed: %{public}s',
        (error as BusinessError).message);
    }
  }

  // Copy a picked (URI-granted) file into the app cache so native code never
  // touches provider URIs.
  /**
   * @throws { BusinessError } when the picked file cannot be opened or copied;
   * the caller reports it through the bridge result.
   */
  private pickerDisplayName(uri: string): string {
    try {
      const name = new fileUri.FileUri(uri).name;
      if (name.length > 0) {
        return name;
      }
    } catch (error) {
      // Fall back to the raw URI basename below on older providers.
    }
    let raw = uri.split('/').pop() ?? '';
    const query = raw.search(/[?#]/);
    if (query >= 0) {
      raw = raw.substring(0, query);
    }
    try {
      raw = decodeURIComponent(raw);
    } catch (error) {
      // Keep the encoded name rather than failing the attachment import.
    }
    return raw;
  }

  // Check the picked documents' sizes before anything is copied: their total
  // against `maxBytes`, and against the free space, which a copy never takes
  // below the reserve.
  private async checkSpace(
    context: common.UIAbilityContext, uris: string[], maxBytes: number): Promise<void> {
    let total = 0;
    for (const uri of uris) {
      const file = await fileIo.open(uri, fileIo.OpenMode.READ_ONLY);
      try {
        total += (await fileIo.stat(file.fd)).size;
      } finally {
        await fileIo.close(file);
      }
    }
    if (total > maxBytes) {
      throw new Error('File exceeds the transfer limit.');
    }
    if (total + FREE_SPACE_RESERVE > await statfs.getFreeSize(context.cacheDir)) {
      throw new Error('There is not enough free space on this device.');
    }
  }

  private async stageCopy(context: common.UIAbilityContext, uri: string): Promise<StagedFile> {
    const directory = `${context.cacheDir}/imports`;
    try {
      await fileIo.mkdir(directory, true);
    } catch (error) {
      // OHOS's mkdir reports 13900015 when the directory already exists (the
      // previous import leaves it behind), unlike Node's recursive mkdir. That
      // is harmless; a genuinely unusable directory surfaces in the open below.
      const failure = error as BusinessError;
      hilog.info(0x0000, LOG_TAG, 'stage: mkdir reported %{public}d; continuing', failure.code);
    }
    const source = await fileIo.open(uri, fileIo.OpenMode.READ_ONLY);
    try {
      // Never reuse the picked file name (or a predictable timestamp) in the
      // cache: it can leak the user's document name and collide with another
      // staged import. Keep only a short extension for content detection.
      // The display name is a separate field; FileUri.name returns the
      // decoded name, with a basename fallback for older providers.
      const raw = this.pickerDisplayName(uri);
      const dot = raw.lastIndexOf('.');
      const extension = dot > 0
        ? raw.substring(dot).replace(/[^A-Za-z0-9.]/g, '').substring(0, 12)
        : '';
      const destination = `${directory}/import-${util.generateRandomUUID(true)}${extension}`;
      const target = await fileIo.open(
        destination,
        fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.CREATE | fileIo.OpenMode.TRUNC,
      );
      try {
        await fileIo.copyFile(source.fd, target.fd);
      } finally {
        await fileIo.close(target);
      }
      return { path: destination, name: raw };
    } finally {
      await fileIo.close(source);
    }
  }
}
