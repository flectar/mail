import { common } from '@kit.AbilityKit';
import { KeyboardAvoidMode } from '@kit.ArkUI';
import native from 'libfeihong.so';
import { NativeSession } from '../services/NativeSession';

interface SurfaceInsets { top: number; right: number; bottom: number; left: number }

@Entry
@Component
struct Index {
  aboutToAppear(): void {
    const uiContext = this.getUIContext();
    // The UIContext is ready in the page lifecycle, after Stage has loaded UIContent.
    uiContext.setKeyboardAvoidMode(KeyboardAvoidMode.RESIZE);
    const context = uiContext.getHostContext() as common.UIAbilityContext;
    NativeSession.start(context, uiContext);
  }

  onBackPress(): boolean { return native.handleBack(); }

  private shortcut(event: KeyEvent): boolean {
    try {
      if (event.type !== KeyType.Down && event.type !== KeyType.Up) {
        return native.handleShortcut(event.keyCode, false, false, false, false, false);
      }
      const ctrl = event.getModifierKeyState?.(['Ctrl']) ?? false;
      const shift = event.getModifierKeyState?.(['Shift']) ?? false;
      const alt = event.getModifierKeyState?.(['Alt']) ?? false;
      // Meta is exposed separately; getModifierKeyState accepts only Ctrl, Shift and Alt.
      return native.handleShortcut(event.keyCode, event.type === KeyType.Down, ctrl, shift, alt, event.metaKey !== 0);
    } catch (_) {
      return false;
    }
  }

  private resize(area: Area): void {
    const uiContext = this.getUIContext();
    const density = uiContext.vp2px(1);
    // The Stage page already avoids system bars; these are surface-relative insets.
    const insets: SurfaceInsets = { top: 0, right: 0, bottom: 0, left: 0 };
    NativeSession.updateSurfaceArea(area, density);
    native.setViewport(Math.round(Number(area.width) * density), Math.round(Number(area.height) * density),
      density, JSON.stringify(insets));
  }

  build() {
    Column() {
      XComponent({ id: 'feihong-surface', type: XComponentType.SURFACE, libraryname: 'feihong' })
        .width('100%')
        .height('100%')
        .focusable(true)
        .defaultFocus(true)
        .onKeyPreIme((event: KeyEvent): boolean => { return this.shortcut(event); })
        .onAreaChange((_oldArea: Area, newArea: Area): void => { this.resize(newArea); })
    }.width('100%').height('100%')
  }
}
