import { abilityAccessCtrl, common, PermissionRequestResult, Permissions, Want } from '@kit.AbilityKit';
import { BusinessError } from '@kit.BasicServicesKit';
import { fileIo, picker } from '@kit.CoreFileKit';
import { inputMethod } from '@kit.IMEKit';
import { notificationManager } from '@kit.NotificationKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { preferences } from '@kit.ArkData';
import { webview } from '@kit.ArkWeb';
import { i18n } from '@kit.LocalizationKit';

const LOG_TAG = 'FlectarOhos';
const PRIVACY_STORE = 'flectar_ohos';
const PRIVACY_KEY = 'privacy_accepted';

// Consent-screen copy. The page follows the same rules as the bundled policy
// (privacy.html): the application language when known, otherwise the system
// language, with English as the fallback. The selector offers the same five
// languages as the policy page.
interface ConsentCopy {
  title: string;
  welcome: string;
  summary: string;
  bulletLocal: string;
  bulletClipboard: string;
  bulletWithdraw: string;
  viewPolicy: string;
  agree: string;
  decline: string;
  language: string;
  systemLanguage: string;
}

const CONSENT_EN: ConsentCopy = {
    title: 'Privacy Policy',
    welcome: 'Welcome to Flectar Mail',
    summary: 'Flectar Mail processes your mail, attachments and account data locally on ' +
      'this device and does not upload them to Flectar servers. See the Privacy Policy ' +
      'for details.',
    bulletLocal: 'Mail and attachments are processed on this device only',
    bulletClipboard: 'The clipboard is read only when you paste',
    bulletWithdraw: 'Consent can be withdrawn in Settings → About',
    viewPolicy: 'View privacy policy',
    agree: 'Agree and continue',
    decline: 'Decline',
    language: 'Language',
    systemLanguage: 'System language',
};

const CONSENT_ZH_HANS: ConsentCopy = {
    title: '隐私政策',
    welcome: '欢迎使用 Flectar Mail',
    summary: '本应用在本机处理您的邮件、附件与账户数据，不会上传至 Flectar 服务器。' +
      '详情请阅读《隐私政策》。',
    bulletLocal: '邮件与附件只在本机处理，不上传 Flectar 服务器',
    bulletClipboard: '剪贴板仅在你主动粘贴时读取纯文本',
    bulletWithdraw: '同意可随时在「设置 → 关于」中撤回',
    viewPolicy: '查看隐私政策',
    agree: '同意并继续',
    decline: '不同意',
    language: '语言',
    systemLanguage: '跟随系统',
};

const CONSENT_ES: ConsentCopy = {
    title: 'Política de privacidad',
    welcome: 'Te damos la bienvenida a Flectar Mail',
    summary: 'Flectar Mail procesa tu correo, archivos adjuntos y datos de cuenta ' +
      'localmente en este dispositivo y no los sube a los servidores de Flectar. ' +
      'Consulta la Política de privacidad para obtener más detalles.',
    bulletLocal: 'El correo y los archivos adjuntos se procesan solo en este dispositivo',
    bulletClipboard: 'El portapapeles solo se lee cuando pegas',
    bulletWithdraw: 'Puedes retirar el consentimiento en Ajustes → Acerca de',
    viewPolicy: 'Ver la política de privacidad',
    agree: 'Aceptar y continuar',
    decline: 'Rechazar',
    language: 'Idioma',
    systemLanguage: 'Idioma del sistema',
};

const CONSENT_TR: ConsentCopy = {
    title: 'Gizlilik Politikası',
    welcome: 'Flectar Mail’e hoş geldiniz',
    summary: 'Flectar Mail postalarınızı, eklerinizi ve hesap verilerinizi bu cihazda ' +
      'yerel olarak işler ve Flectar sunucularına yüklemez. Ayrıntılar için Gizlilik ' +
      'Politikasına bakın.',
    bulletLocal: 'Posta ve ekler yalnızca bu cihazda işlenir',
    bulletClipboard: 'Pano yalnızca yapıştırdığınızda okunur',
    bulletWithdraw: 'Onayı Ayarlar → Hakkında bölümünden geri çekebilirsiniz',
    viewPolicy: 'Gizlilik politikasını görüntüle',
    agree: 'Kabul et ve devam et',
    decline: 'Reddet',
    language: 'Dil',
    systemLanguage: 'Sistem dili',
};

const CONSENT_RU: ConsentCopy = {
    title: 'Политика конфиденциальности',
    welcome: 'Добро пожаловать в Flectar Mail',
    summary: 'Flectar Mail обрабатывает вашу почту, вложения и данные учётной записи ' +
      'локально на этом устройстве и не загружает их на серверы Flectar. Подробности — ' +
      'в Политике конфиденциальности.',
    bulletLocal: 'Почта и вложения обрабатываются только на этом устройстве',
    bulletClipboard: 'Буфер обмена читается только при вставке',
    bulletWithdraw: 'Согласие можно отозвать в разделе «Настройки → О программе»',
    viewPolicy: 'Просмотреть политику конфиденциальности',
    agree: 'Принять и продолжить',
    decline: 'Отклонить',
    language: 'Язык',
    systemLanguage: 'Системный язык',
};

function consentCopyFor(language: string): ConsentCopy {
  if (language === 'zh_Hans') {
    return CONSENT_ZH_HANS;
  }
  if (language === 'es') {
    return CONSENT_ES;
  }
  if (language === 'tr') {
    return CONSENT_TR;
  }
  if (language === 'ru') {
    return CONSENT_RU;
  }
  return CONSENT_EN;
}

// Services the native module invokes from Rust; the closures run on the ArkTS
// UI thread through a thread-safe function. `pickOpen`/`pickSave` are
// request/response: the page reports the outcome through `submitBridgeResult`.
interface FlectarBridge {
  openUrl: (url: string) => void;
  pickOpen: (requestId: number) => void;
  pickSave: (requestId: number, sourcePath: string, suggestedName: string) => void;
  showIme: () => void;
  hideIme: () => void;
  notify: (title: string, body: string) => void;
  requestPasteboard: () => void;
  openPrivacyPolicy: (lang: string) => void;
  withdrawPrivacyConsent: () => void;
}

// The exports of the native XComponent module (`onLoad` hands them over).
interface FlectarNativeModule {
  setBridge?: (bridge: FlectarBridge) => void;
  submitBridgeResult?: (requestId: number, path: string, error: string) => void;
  imeCommit?: (text: string) => void;
  imeDelete?: (count: number) => void;
  imeFunction?: (key: number) => void;
}

@Entry
@Component
struct Index {
  private nativeModule: FlectarNativeModule | undefined = undefined;
  private imeAttached: boolean = false;
  private pasteboardPermissionRequested: boolean = false;
  @State private showPrivacyPolicy: boolean = false;
  @State private privacyChecked: boolean = false;
  @State private privacyAccepted: boolean = false;
  private privacyController: webview.WebviewController = new webview.WebviewController();
  private privacyLanguage: string = 'system';
  @State private consentMode: string = 'system';
  @State private consentSelected: number = 0;
  @State private consentOptions: SelectOption[] = [];
  @State private consentText: ConsentCopy = CONSENT_EN;

  aboutToAppear(): void {
    this.probeResources();
    this.initConsentLanguage();
    this.loadPrivacyConsent();
  }

  // The IME controller is process-scoped; its listeners capture this page, so
  // they are released when the page goes away (they are also re-registered
  // idempotently on every attach).
  aboutToDisappear(): void {
    try {
      const controller = inputMethod.getController();
      controller.off('insertText');
      controller.off('deleteLeft');
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'IME listener cleanup failed: %{public}s',
        (error as BusinessError).message);
    }
    this.imeAttached = false;
  }

  @Builder
  privacyConsentView(): void {
    Column() {
      Column() {
        Row({ space: 8 }) {
          Blank()
          Text(this.consentText.language)
            .fontSize(13)
            .fontColor('#8E8E93')
          Select(this.consentOptions)
            .selected(this.consentSelected)
            .value(this.consentOptions.length > this.consentSelected
              ? this.consentOptions[this.consentSelected].value : '')
            .font({ size: 13 })
            .fontColor('#1C1C1E')
            .backgroundColor('#F2F2F7')
            .borderRadius(6)
            .onSelect((index: number, value: string): void => {
              this.selectConsentLanguage(index);
            })
        }
        .width('100%')
        .padding({ top: 8 })

        Scroll() {
          Column({ space: 14 }) {
            Image($r('app.media.app_icon'))
              .width(64)
              .height(64)
              .borderRadius(14)
            Text(this.consentText.title)
              .fontSize(24)
              .fontWeight(700)
              .fontColor('#1C1C1E')
            Text(this.consentText.welcome)
              .fontSize(16)
              .fontWeight(600)
              .fontColor('#1C1C1E')
            Text(this.consentText.summary)
              .fontSize(15)
              .lineHeight(24)
              .fontColor('#3C3C43')

            Column({ space: 14 }) {
              Row({ space: 10 }) {
                Image($r('app.media.ic_check_blue'))
                  .width(16)
                  .height(16)
                  .objectFit(ImageFit.Contain)
                Text(this.consentText.bulletLocal)
                  .fontSize(14)
                  .lineHeight(20)
                  .fontColor('#1C1C1E')
                  .layoutWeight(1)
              }
              .width('100%')
              .alignItems(VerticalAlign.Top)

              Row({ space: 10 }) {
                Image($r('app.media.ic_check_blue'))
                  .width(16)
                  .height(16)
                  .objectFit(ImageFit.Contain)
                Text(this.consentText.bulletClipboard)
                  .fontSize(14)
                  .lineHeight(20)
                  .fontColor('#1C1C1E')
                  .layoutWeight(1)
              }
              .width('100%')
              .alignItems(VerticalAlign.Top)

              Row({ space: 10 }) {
                Image($r('app.media.ic_check_blue'))
                  .width(16)
                  .height(16)
                  .objectFit(ImageFit.Contain)
                Text(this.consentText.bulletWithdraw)
                  .fontSize(14)
                  .lineHeight(20)
                  .fontColor('#1C1C1E')
                  .layoutWeight(1)
              }
              .width('100%')
              .alignItems(VerticalAlign.Top)
            }
            .width('100%')
            .padding(16)
            .borderRadius(10)
            .backgroundColor('#F7F7F9')
          }
          .width('100%')
          .alignItems(HorizontalAlign.Start)
          .padding({ top: 24, bottom: 24 })
        }
        .width('100%')
        .layoutWeight(1)
        .scrollBar(BarState.Off)

        Column({ space: 10 }) {
          Row({ space: 12 }) {
            Image($r('app.media.ic_privacy'))
              .width(20)
              .height(20)
              .objectFit(ImageFit.Contain)
            Text(this.consentText.viewPolicy)
              .fontSize(16)
              .fontWeight(600)
              .fontColor('#1C1C1E')
              .layoutWeight(1)
            Text('›')
              .fontSize(22)
              .fontColor('#8E8E93')
          }
          .width('100%')
          .height(48)
          .padding({ left: 16, right: 16 })
          .borderRadius(8)
          .backgroundColor('#FFFFFF')
          .border({ width: 1, color: '#E2E2E6' })
          .onClick((): void => {
            this.openPrivacyPolicy(this.consentMode);
          })

          Row({ space: 12 }) {
            Image($r('app.media.ic_agree'))
              .width(20)
              .height(20)
              .objectFit(ImageFit.Contain)
            Text(this.consentText.agree)
              .fontSize(16)
              .fontWeight(600)
              .fontColor('#FFFFFF')
              .layoutWeight(1)
            Text('›')
              .fontSize(22)
              .fontColor('#FFFFFF')
          }
          .width('100%')
          .height(48)
          .padding({ left: 16, right: 16 })
          .borderRadius(8)
          .backgroundColor('#0A59F7')
          .onClick((): void => {
            this.acceptPrivacy();
          })

          Row({ space: 12 }) {
            Image($r('app.media.ic_decline'))
              .width(20)
              .height(20)
              .objectFit(ImageFit.Contain)
            Text(this.consentText.decline)
              .fontSize(16)
              .fontWeight(500)
              .fontColor('#8E8E93')
              .layoutWeight(1)
          }
          .width('100%')
          .height(48)
          .padding({ left: 16, right: 16 })
          .borderRadius(8)
          .backgroundColor('#F5F5F7')
          .onClick((): void => {
            this.declinePrivacy();
          })
        }
        .width('100%')
        .padding({ bottom: 16 })
      }
      .width('100%')
      .constraintSize({ maxWidth: 520 })
      .height('100%')
    }
    .width('100%')
    .height('100%')
    .alignItems(HorizontalAlign.Center)
    .padding({ left: 20, right: 20 })
    .backgroundColor('#FFFFFF')
    .bindContentCover(this.showPrivacyPolicy, this.privacyPolicyView(), {
      modalTransition: ModalTransition.DEFAULT,
    })
  }

  @Builder
  privacyPolicyView(): void {
    Column() {
      Row({ space: 4 }) {
        Row() {
          Image($r('app.media.ic_back'))
            .width(20)
            .height(20)
            .objectFit(ImageFit.Contain)
        }
        .width(36)
        .height(36)
        .borderRadius(18)
        .justifyContent(FlexAlign.Center)
        .accessibilityText('Back')
        .onClick((): void => {
          this.showPrivacyPolicy = false;
        })
        Text('Privacy policy')
          .fontSize(18)
          .fontWeight(600)
          .fontColor('#1C1C1E')
          .layoutWeight(1)
      }
      .width('100%')
      .backgroundColor('#FFFFFF')
      .padding({ left: 12, right: 12, top: 8, bottom: 8 })

      Web({ src: $rawfile('privacy.html'), controller: this.privacyController })
        .width('100%')
        .layoutWeight(1)
        .onPageEnd((): void => {
          this.applyPrivacyLanguage();
        })
    }
    .width('100%')
    .height('100%')
    .backgroundColor('#FFFFFF')
  }

  build(): void {
    if (!this.privacyChecked) {
      Column() {
        LoadingProgress()
          .width(48)
          .height(48)
      }
      .width('100%')
      .height('100%')
      .justifyContent(FlexAlign.Center)
      .backgroundColor('#FFFFFF')
    } else if (!this.privacyAccepted) {
      this.privacyConsentView()
    } else {
      Column() {
        XComponent({
        id: 'flectar_mail_surface',
        type: XComponentType.SURFACE,
        libraryname: 'flectar_mail_ohos',
      })
        .width('100%')
        .height('100%')
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
      .bindContentCover(this.showPrivacyPolicy, this.privacyPolicyView(), {
        modalTransition: ModalTransition.DEFAULT,
      })
    }
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
      pickSave: (requestId: number, sourcePath: string, suggestedName: string): void => {
        this.pickSave(requestId, sourcePath, suggestedName);
      },
      showIme: (): void => {
        this.showIme();
      },
      hideIme: (): void => {
        this.hideIme();
      },
      notify: (title: string, body: string): void => {
        this.notify(title, body);
      },
      requestPasteboard: (): void => {
        this.requestPasteboardPermission();
      },
      openPrivacyPolicy: (lang: string): void => {
        this.openPrivacyPolicy(lang);
      },
      withdrawPrivacyConsent: (): void => {
        this.withdrawPrivacyConsent();
      },
    });
  }

  private reportBridgeResult(requestId: number, path: string, error: string): void {
    this.nativeModule?.submitBridgeResult?.(requestId, path, error);
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
      if (uris.length === 0) {
        this.reportBridgeResult(requestId, '', '');
        return;
      }
      const staged = this.stageCopy(context, uris[0]);
      hilog.info(0x0000, LOG_TAG, 'picker imported a document (%{public}d picked)', uris.length);
      this.reportBridgeResult(requestId, staged, '');
    } catch (error) {
      const failure = error as BusinessError;
      hilog.error(0x0000, LOG_TAG, 'document picker failed: %{public}d %{public}s',
        failure.code, failure.message);
      this.reportBridgeResult(requestId, '', failure.message ?? 'document picker failed');
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
      if (uris.length === 0) {
        this.reportBridgeResult(requestId, '', '');
        return;
      }
      const destination = fileIo.openSync(uris[0], fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.TRUNC);
      try {
        const source = fileIo.openSync(sourcePath, fileIo.OpenMode.READ_ONLY);
        try {
          await fileIo.copyFile(source.fd, destination.fd);
        } finally {
          fileIo.closeSync(source);
        }
      } finally {
        fileIo.closeSync(destination);
      }
      hilog.info(0x0000, LOG_TAG, 'picker exported a document');
      this.reportBridgeResult(requestId, uris[0], '');
    } catch (error) {
      const failure = error as BusinessError;
      hilog.error(0x0000, LOG_TAG, 'document save failed: %{public}d %{public}s',
        failure.code, failure.message);
      this.reportBridgeResult(requestId, '', failure.message ?? 'document save failed');
    }
  }

  // Slint enables the input method when an editable field gains focus. The
  // controller refuses event subscription until it is attached, so attach
  // first and register the text callbacks once it succeeds.
  private showIme(): void {
    try {
      const controller = inputMethod.getController();
      if (this.imeAttached) {
        controller.showTextInput(inputMethod.RequestKeyboardReason.TOUCH)
          .catch((error: BusinessError): void => {
            hilog.error(0x0000, LOG_TAG, 'IME show failed: %{public}d %{public}s',
              error.code, error.message);
            if (error.code === 12800009 || error.code === 12800003) {
              // The service dropped the client (detached), e.g. after the app
              // left the foreground. Clear the flag and attach again so the
              // keyboard can come up on the next focus.
              this.imeAttached = false;
              this.showIme();
            }
          });
        return;
      }
      const config: inputMethod.TextConfig = {
        inputAttribute: {
          textInputType: inputMethod.TextInputType.TEXT,
          enterKeyType: inputMethod.EnterKeyType.NEWLINE,
        },
      };
      // The touch reason asks the IME service to raise the soft keyboard even
      // when a physical keyboard is attached (a 2in1 policy would otherwise
      // suppress it).
      controller.attach(true, config, inputMethod.RequestKeyboardReason.TOUCH).then((): void => {
        this.imeAttached = true;
        // (Re-)register exactly one set of handlers: a stale registration from
        // an earlier attach (or a duplicate one) would double-insert text.
        controller.off('insertText');
        controller.off('deleteLeft');
        controller.on('insertText', (text: string): void => {
          this.nativeModule?.imeCommit?.(text);
        });
        controller.on('deleteLeft', (length: number): void => {
          this.nativeModule?.imeDelete?.(length);
        });
        hilog.info(0x0000, LOG_TAG, 'IME attached');
      }).catch((error: BusinessError): void => {
        hilog.error(0x0000, LOG_TAG, 'IME attach failed: %{public}d %{public}s',
          error.code, error.message);
      });
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'IME setup failed: %{public}s',
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

  // New-mail notification, published by the native module through the
  // publisher installed on the platform context. The permission itself is
  // requested by EntryAbility once the page has loaded.
  private notify(title: string, body: string): void {
    try {
      const request: notificationManager.NotificationRequest = {
        id: Date.now() % 2147483647,
        content: {
          notificationContentType:
            notificationManager.ContentType.NOTIFICATION_CONTENT_BASIC_TEXT,
          normal: { title, text: body },
        },
      };
      notificationManager.publish(request)
        .then((): void => {
          hilog.info(0x0000, LOG_TAG, 'notification published');
        })
        .catch((error: BusinessError): void => {
          hilog.error(0x0000, LOG_TAG,
            'notification publish failed: %{public}d %{public}s', error.code, error.message);
        });
    } catch (error) {
      hilog.error(0x0000, LOG_TAG,
        'notification failed: %{public}s', (error as BusinessError).message);
    }
  }

  // The clipboard is readable only with the ACL permission
  // ohos.permission.READ_PASTEBOARD. Native code asks for it after the first
  // denied paste; granting takes effect for the next paste.
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
          if (!granted) {
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

  // The consent screen gates the XComponent: no native module (and therefore
  // no account or mail processing) starts before the user agrees.
  private async loadPrivacyConsent(): Promise<void> {
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const store = await preferences.getPreferences(context, PRIVACY_STORE);
      this.privacyAccepted = (await store.get(PRIVACY_KEY, false)) as boolean;
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'privacy preferences unavailable: %{public}s',
        (error as BusinessError).message);
      this.privacyAccepted = false;
    } finally {
      this.privacyChecked = true;
    }
  }

  private async acceptPrivacy(): Promise<void> {
    try {
      const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
      const store = await preferences.getPreferences(context, PRIVACY_STORE);
      await store.put(PRIVACY_KEY, true);
      await store.flush();
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'privacy consent could not be stored: %{public}s',
        (error as BusinessError).message);
    }
    this.privacyAccepted = true;
  }

  private declinePrivacy(): void {
    const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
    context.terminateSelf().catch((error: BusinessError): void => {
      hilog.error(0x0000, LOG_TAG, 'could not stop after declining privacy: %{public}s',
        error.message);
    });
  }

  // Mirrors privacy.html's language resolution: the configured mode when it
  // is one of the five bundled languages, otherwise the system language with
  // English as the fallback. The selector lists the same choices in the same
  // order, so choosing a language here also drives the policy the row opens.
  private resolveSystemLanguage(): string {
    let language = 'en';
    try {
      language = i18n.System.getSystemLanguage().toLowerCase();
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'system language unavailable: %{public}s',
        (error as BusinessError).message);
    }
    if (language.startsWith('zh')) {
      return 'zh_Hans';
    }
    if (language.startsWith('es')) {
      return 'es';
    }
    if (language.startsWith('tr')) {
      return 'tr';
    }
    if (language.startsWith('ru')) {
      return 'ru';
    }
    return 'en';
  }

  private initConsentLanguage(): void {
    const resolved = this.resolveSystemLanguage();
    this.consentMode = 'system';
    this.consentSelected = 0;
    this.consentText = consentCopyFor(resolved);
    this.consentOptions = this.buildLanguageOptions(resolved);
  }

  private selectConsentLanguage(index: number): void {
    const modes: string[] = ['system', 'en', 'es', 'tr', 'zh_Hans', 'ru'];
    if (index < 0 || index >= modes.length) {
      return;
    }
    const mode = modes[index];
    const resolved = mode === 'system' ? this.resolveSystemLanguage() : mode;
    this.consentMode = mode;
    this.consentSelected = index;
    this.consentText = consentCopyFor(resolved);
    this.consentOptions = this.buildLanguageOptions(resolved);
  }

  private buildLanguageOptions(resolved: string): SelectOption[] {
    return [
      { value: consentCopyFor(resolved).systemLanguage },
      { value: 'English' },
      { value: 'Español' },
      { value: 'Türkçe' },
      { value: '简体中文' },
      { value: 'Русский' },
    ];
  }

  // Shows the bundled privacy policy (About and onboarding rows). The page
  // starts from the configured language and falls back to the system one.
  private openPrivacyPolicy(language: string): void {
    this.privacyLanguage = language;
    this.showPrivacyPolicy = true;
    // The page applies the language once it is loaded (onPageEnd); calling
    // runJavaScript before the Web component is bound crashes ArkWeb.
  }

  // Withdrawing clears the stored consent and stops the application, so no
  // further account or mail processing happens; the next start asks again.
  private withdrawPrivacyConsent(): void {
    const context = this.getUIContext().getHostContext() as common.UIAbilityContext;
    this.clearPrivacyConsent(context);
  }

  private async clearPrivacyConsent(context: common.UIAbilityContext): Promise<void> {
    try {
      const store = await preferences.getPreferences(context, PRIVACY_STORE);
      await store.put(PRIVACY_KEY, false);
      await store.flush();
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'privacy consent could not be cleared: %{public}s',
        (error as BusinessError).message);
    }
    try {
      this.getUIContext().getPromptAction().showToast({
        message: 'Privacy consent withdrawn. The app will close.',
        duration: 2000,
      });
    } catch (error) {
      hilog.error(0x0000, LOG_TAG, 'withdraw toast failed');
    }
    setTimeout((): void => {
      context.terminateSelf().catch((error: BusinessError): void => {
        hilog.error(0x0000, LOG_TAG, 'could not stop after withdrawing consent: %{public}s',
          error.message);
      });
    }, 1500);
  }

  private applyPrivacyLanguage(): void {
    const language = JSON.stringify(this.privacyLanguage);
    this.privacyController.runJavaScript(`window.setPolicyLanguage(${language})`)
      .catch((error: BusinessError): void => {
        hilog.error(0x0000, LOG_TAG, 'privacy language failed: %{public}s', error.message);
      });
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
  private stageCopy(context: common.UIAbilityContext, uri: string): string {
    const directory = `${context.cacheDir}/imports`;
    try {
      fileIo.mkdirSync(directory, true);
    } catch (error) {
      // OHOS's mkdir reports 13900015 when the directory already exists (the
      // previous import leaves it behind), unlike Node's recursive mkdir. That
      // is harmless; a genuinely unusable directory surfaces in the open below.
      const failure = error as BusinessError;
      hilog.info(0x0000, LOG_TAG, 'stage: mkdir reported %{public}d; continuing', failure.code);
    }
    const segments = uri.split('/');
    const raw = segments.pop() ?? 'document';
    // The picker chooses the file name; keep it to a conservative character
    // set and length so it cannot escape the cache directory.
    const cleaned = raw.replace(/[^A-Za-z0-9._-]/g, '_').substring(0, 96);
    const name = cleaned.length > 0 ? cleaned : 'document';
    const destination = `${directory}/${Date.now()}-${name}`;
    const source = fileIo.openSync(uri, fileIo.OpenMode.READ_ONLY);
    try {
      const target = fileIo.openSync(
        destination,
        fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.CREATE | fileIo.OpenMode.TRUNC,
      );
      try {
        fileIo.copyFileSync(source.fd, target.fd);
      } finally {
        fileIo.closeSync(target);
      }
    } finally {
      fileIo.closeSync(source);
    }
    return destination;
  }
}
