import { abilityAccessCtrl, common } from '@kit.AbilityKit';
import { UIContext } from '@kit.ArkUI';
import { util } from '@kit.ArkTS';
import { BusinessError, pasteboard, print } from '@kit.BasicServicesKit';
import { fileIo, fileUri, picker } from '@kit.CoreFileKit';
import { notificationManager } from '@kit.NotificationKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import native from 'libfeihong.so';

interface UrlRequest { url: string }
interface ExportRequest { path: string; name: string }
interface ClipboardRequest { text: string; html?: string }
interface NotificationRequest { title: string; body: string; messageId?: string; sound?: boolean }
interface PrintRequest { path: string; name: string }
interface PassphraseRequest { fingerprint: string; title?: string }
interface ImportResult { path: string | null; name?: string }
interface ExportResult { saved: boolean }
interface ClipboardResult { text: string; html?: string }
interface PassphraseResult { passphrase: string | null }
interface ServiceError { error: string; code?: number }

class PasswordPrompt {
  title: string;
  fingerprint: string;
  value: string = '';
  dialogId: number = -1;
  settled: boolean = false;
  private uiContext: UIContext;
  private resolve: (value: string | null) => void;

  constructor(request: PassphraseRequest, uiContext: UIContext, resolve: (value: string | null) => void) {
    this.title = request.title ?? '解锁 OpenPGP 私钥';
    this.fingerprint = request.fingerprint;
    this.uiContext = uiContext;
    this.resolve = resolve;
  }

  finish(value: string | null): void {
    if (this.settled) { return; }
    this.settled = true;
    this.value = '';
    this.resolve(value);
    if (this.dialogId >= 0) {
      try { this.uiContext.getPromptAction().closeCustomDialog(this.dialogId); }
      catch (_) { /* The window may already have released its dialogs. */ }
      this.dialogId = -1;
    }
  }
}

@Builder
function passwordDialog(state: PasswordPrompt) {
  Column({ space: 16 }) {
    Text(state.title).fontSize(20).fontWeight(FontWeight.Medium)
    Text(state.fingerprint).fontSize(12).fontColor('#666666').maxLines(3)
    TextInput({ placeholder: '私钥口令' })
      .type(InputType.Password)
      .copyOption(CopyOptions.None)
      .onChange((value: string) => { state.value = value; })
      .onSubmit(() => { state.finish(state.value); })
      .defaultFocus(true)
    Row({ space: 16 }) {
      Button('取消').onClick(() => { state.finish(null); })
      Button('解锁').onClick(() => { state.finish(state.value); })
    }.justifyContent(FlexAlign.End).width('100%')
  }.padding(24).width('100%').constraintSize({ maxWidth: 480 })
}

/** Only system integration lives here; application UI and models remain in Rust/Slint. */
export class ServiceBridge {
  private context: common.Context;
  private uiContext?: UIContext;
  private disposed: boolean = false;
  private password?: PasswordPrompt;
  private pending: Set<number> = new Set<number>();
  private foreground: boolean;

  constructor(context: common.Context, uiContext?: UIContext) {
    this.context = context;
    this.uiContext = uiContext;
    this.foreground = uiContext !== undefined;
  }

  setForeground(value: boolean): void { this.foreground = value; }

  install(): void {
    native.setServiceHandler((id: number, kind: string, payload: string): void => {
      this.handle(id, kind, payload).catch(() => {
        // Never log the service payload: it can contain passwords or mail contents.
        hilog.error(0xF100, 'Feihong', 'Host service completion failed: %{public}s', kind);
      });
    });
  }

  dispose(): void {
    if (this.disposed) { return; }
    this.disposed = true;
    this.password?.finish(null);
    const closed: ServiceError = { error: 'Application window closed' };
    this.pending.forEach((id: number): void => {
      try { native.completeServiceRequest(id, false, JSON.stringify(closed)); }
      catch (_) { hilog.error(0xF100, 'Feihong', 'Unable to cancel host service'); }
    });
    this.pending.clear();
  }

  private requireUI(): common.UIAbilityContext {
    if (this.disposed || !this.foreground || this.uiContext === undefined) {
      throw new Error('This operation requires a foreground application window');
    }
    return this.context as common.UIAbilityContext;
  }

  private async handle(id: number, kind: string, payload: string): Promise<void> {
    this.pending.add(id);
    try {
      if (this.disposed) { throw new Error('Application window closed'); }
      const result: string = await this.dispatch(id, kind, payload);
      // Late picker results must still reach Rust, which owns cleanup for stale IDs.
      native.completeServiceRequest(id, true, result);
    } catch (error) {
      const businessError = error as BusinessError;
      const result: ServiceError = { error: businessError.message || 'System operation failed', code: businessError.code };
      native.completeServiceRequest(id, false, JSON.stringify(result));
    } finally {
      this.pending.delete(id);
    }
  }

  private async dispatch(id: number, kind: string, payload: string): Promise<string> {
    switch (kind) {
      case 'open_url': {
        const request = JSON.parse(payload) as UrlRequest;
        await this.requireUI().startAbility({ action: 'ohos.want.action.viewData', uri: request.url });
        return '{}';
      }
      case 'pick_import': return this.importFile(id);
      case 'pick_export': return this.exportFile(JSON.parse(payload) as ExportRequest);
      case 'clipboard_write': return this.writeClipboard(JSON.parse(payload) as ClipboardRequest);
      case 'clipboard_read': return this.readClipboard();
      case 'notify': return this.notify(JSON.parse(payload) as NotificationRequest);
      case 'print': {
        const request = JSON.parse(payload) as PrintRequest;
        this.checkSandboxPath(request.path);
        await print.print([fileUri.getUriFromPath(request.path)], this.requireUI());
        return '{}';
      }
      case 'openpgp_passphrase': return this.requestPassphrase(JSON.parse(payload) as PassphraseRequest);
      default: throw new Error('Unsupported system service: ' + kind);
    }
  }

  private checkSandboxPath(path: string): void {
    const allowed = [this.context.filesDir, this.context.cacheDir, this.context.tempDir, this.context.databaseDir];
    if (path.includes('/../') || !allowed.some((dir: string): boolean => path.startsWith(dir + '/'))) {
      throw new Error('File is outside the application sandbox');
    }
  }

  private async importFile(id: number): Promise<string> {
    const viewPicker = new picker.DocumentViewPicker(this.requireUI());
    const options: picker.DocumentSelectOptions = { maxSelectNumber: 1 };
    const uris = await viewPicker.select(options);
    if (uris.length === 0) {
      const cancelled: ImportResult = { path: null };
      return JSON.stringify(cancelled);
    }
    const directory = this.context.cacheDir + '/feihong-imports';
    if (!await fileIo.access(directory)) { await fileIo.mkdir(directory, true); }
    let name = 'import';
    let destination = '';
    try {
      const source = await fileIo.open(uris[0], fileIo.OpenMode.READ_ONLY);
      try {
        // File.name is provider metadata; staging names are never attachment names.
        name = source.name;
        if (name.length === 0 || name === '.' || name === '..' || /[\\/\u0000-\u001f\u007f-\u009f]/.test(name)) {
          throw new Error('The selected file has an invalid name');
        }
        // ASCII bounds the complete cache basename below the filesystem's byte limit.
        const cacheName = name.replace(/[^a-zA-Z0-9._-]/g, '_').slice(-128) || 'import';
        destination = directory + '/' + util.generateRandomUUID() + '-' + cacheName;
        await fileIo.copyFile(source.fd, destination);
      }
      finally { await fileIo.close(source.fd); }
    } catch (error) {
      // An unsuccessful copy has no ImportedDocument owner in Rust.
      try { await fileIo.unlink(destination); } catch (_) { }
      throw new Error((error as BusinessError).message || 'Document import failed');
    }
    if (this.disposed) {
      try {
        await fileIo.unlink(destination);
      } catch (_) {
        // Dispose already cancelled this ID; a late success lets Rust retry cleanup.
        const late: ImportResult = { path: destination, name: name };
        return JSON.stringify(late);
      }
      throw new Error('Import cancelled because the application window closed');
    }
    const result: ImportResult = { path: destination, name: name };
    return JSON.stringify(result);
  }

  private async exportFile(request: ExportRequest): Promise<string> {
    this.checkSandboxPath(request.path);
    const viewPicker = new picker.DocumentViewPicker(this.requireUI());
    const name = request.name.replace(/[\\/]/g, '_') || 'export';
    const options: picker.DocumentSaveOptions = { newFileNames: [name] };
    const uris = await viewPicker.save(options);
    if (uris.length === 0) {
      const cancelled: ExportResult = { saved: false };
      return JSON.stringify(cancelled);
    }
    const source = await fileIo.open(request.path, fileIo.OpenMode.READ_ONLY);
    try {
      const destination = await fileIo.open(uris[0], fileIo.OpenMode.WRITE_ONLY | fileIo.OpenMode.TRUNC);
      try { await fileIo.copyFile(source.fd, destination.fd); }
      finally { await fileIo.close(destination.fd); }
    } finally { await fileIo.close(source.fd); }
    const result: ExportResult = { saved: true };
    return JSON.stringify(result);
  }

  private async writeClipboard(request: ClipboardRequest): Promise<string> {
    const board = pasteboard.getSystemPasteboard();
    if (request.text.length === 0 && !request.html) {
      await board.clearData();
    } else {
      const data = pasteboard.createData(pasteboard.MIMETYPE_TEXT_PLAIN, request.text);
      if (request.html && request.html.length > 0) { data.addRecord(pasteboard.MIMETYPE_TEXT_HTML, request.html); }
      await board.setData(data);
    }
    return '{}';
  }

  private async readClipboard(): Promise<string> {
    const manager = abilityAccessCtrl.createAtManager();
    const permissions = await manager.requestPermissionsFromUser(this.requireUI(), ['ohos.permission.READ_PASTEBOARD']);
    if (permissions.authResults.some((result: number): boolean => result !== 0)) {
      throw new Error('Clipboard permission was denied');
    }
    const data = await pasteboard.getSystemPasteboard().getData();
    const result: ClipboardResult = { text: data.getPrimaryText() || '', html: data.getPrimaryHtml() || undefined };
    return JSON.stringify(result);
  }

  private async notify(request: NotificationRequest): Promise<string> {
    if (!await notificationManager.isNotificationEnabled()) {
      if (this.uiContext === undefined) { throw new Error('Notifications have not been enabled'); }
      await notificationManager.requestEnableNotification(this.requireUI());
    }
    // Content notifications default to no alert tone; channel changes remain under OS/user control.
    const slot = request.sound === false ? notificationManager.SlotType.CONTENT_INFORMATION :
      notificationManager.SlotType.SOCIAL_COMMUNICATION;
    await notificationManager.addSlot(slot);
    let notificationId = 0;
    const key = request.messageId ?? request.title + request.body;
    for (let index = 0; index < key.length; index++) {
      notificationId = ((notificationId * 31) + key.charCodeAt(index)) & 0x7fffffff;
    }
    const notification: notificationManager.NotificationRequest = {
      id: notificationId,
      notificationSlotType: slot,
      isAlertOnce: true,
      content: {
        notificationContentType: notificationManager.ContentType.NOTIFICATION_CONTENT_BASIC_TEXT,
        normal: { title: request.title, text: request.body }
      }
    };
    await notificationManager.publish(notification);
    return '{}';
  }

  private async requestPassphrase(request: PassphraseRequest): Promise<string> {
    this.requireUI();
    const uiContext = this.uiContext;
    if (uiContext === undefined) { throw new Error('A window is required to unlock a private key'); }
    if (this.password && !this.password.settled) { throw new Error('A private-key unlock dialog is already open'); }
    const passphrase = await new Promise<string | null>((resolve, reject) => {
      const state = new PasswordPrompt(request, uiContext, resolve);
      this.password = state;
      uiContext.getPromptAction().openCustomDialog({
        builder: () => { passwordDialog(state); },
        autoCancel: true,
        onWillDismiss: (action: DismissDialogAction): void => {
          state.dialogId = -1;
          state.finish(null);
          action.dismiss();
        }
      }).then((id: number) => {
        if (state.settled) { uiContext.getPromptAction().closeCustomDialog(id); }
        else { state.dialogId = id; }
      }).catch((error: BusinessError) => {
        state.settled = true;
        state.value = '';
        reject(error);
      });
    });
    const result: PassphraseResult = { passphrase: passphrase };
    return JSON.stringify(result);
  }
}
