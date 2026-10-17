import { common, Want } from '@kit.AbilityKit';
import { BusinessError } from '@kit.BasicServicesKit';
import { fileIo, picker } from '@kit.CoreFileKit';
import { inputMethod } from '@kit.IMEKit';

// Services the native module invokes from Rust; the closures run on the ArkTS
// UI thread through a thread-safe function. `pickOpen`/`pickSave` are
// request/response: the page reports the outcome through `submitBridgeResult`.
interface FlectarBridge {
  openUrl: (url: string) => void;
  pickOpen: (requestId: number) => void;
  pickSave: (requestId: number, sourcePath: string, suggestedName: string) => void;
  showIme: () => void;
  hideIme: () => void;
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

  aboutToAppear(): void {
    this.probeResources();
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
      console.error('[flectar] IME listener cleanup failed');
    }
    this.imeAttached = false;
  }

  build() {
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
      console.info(`[flectar] resource probe: app_name='${label}' icon=${icon.byteLength} bytes`);
    } catch (error) {
      const failure = error as BusinessError;
      console.error(`[flectar] resource probe failed: code ${failure.code} ${failure.message}`);
    }
  }

  // Hand the ArkTS-side services to the native module: sign-in opens the
  // system browser with an implicit Want, and import/export opens the system
  // document picker — neither is reachable from native code directly.
  private installBridge(context: object): void {
    const nativeModule = context as FlectarNativeModule;
    if (nativeModule.setBridge === undefined) {
      console.error('[flectar] native module has no setBridge');
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
      console.error(`[flectar] refusing to open a non-web address (scheme '${scheme}')`);
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
        console.info('[flectar] opened the sign-in page in the system browser');
      })
      .catch((error: BusinessError): void => {
        console.error(`[flectar] could not open the system browser: ${error.message}`);
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
      console.info(`[flectar] picker imported a document (${uris.length} picked)`);
      this.reportBridgeResult(requestId, staged, '');
    } catch (error) {
      const failure = error as BusinessError;
      console.error(`[flectar] document picker failed: ${failure.code} ${failure.message}`);
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
      console.info('[flectar] picker exported a document');
      this.reportBridgeResult(requestId, uris[0], '');
    } catch (error) {
      const failure = error as BusinessError;
      console.error(`[flectar] document save failed: ${failure.code} ${failure.message}`);
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
            console.error(`[flectar] IME show failed: ${error.code} ${error.message}`);
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
        console.info('[flectar] IME attached');
      }).catch((error: BusinessError): void => {
        console.error(`[flectar] IME attach failed: ${error.code} ${error.message}`);
      });
    } catch (error) {
      console.error(`[flectar] IME setup failed: ${(error as BusinessError).message}`);
    }
  }

  private hideIme(): void {
    if (!this.imeAttached) {
      return;
    }
    try {
      inputMethod.getController().hideTextInput().catch((error: BusinessError): void => {
        console.error(`[flectar] IME hide failed: ${error.code}`);
        if (error.code === 12800009 || error.code === 12800003) {
          // Already detached on the service side; forget the local state.
          this.imeAttached = false;
        }
      });
    } catch (error) {
      console.error('[flectar] IME hide failed');
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
      console.info(`[flectar] stage: mkdir reported ${failure.code}; continuing`);
    }
    const segments = uri.split('/');
    const raw = segments.length > 0 ? segments[segments.length - 1] : 'document';
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
