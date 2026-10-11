/** JSON is used at the ABI boundary so Rust retains ownership of application models. */
export type ServiceHandler = (id: number, kind: string, payload: string) => void;
export const start: (optionsJson: string) => void;
export const setViewport: (widthPx: number, heightPx: number, density: number, insetsJson: string) => void;
export const setInputGeometry: (windowId: number, surfaceScreenX: number, surfaceScreenY: number) => void;
export const handleShortcut: (keyCode: number, pressed: boolean, ctrl: boolean, shift: boolean, alt: boolean, meta: boolean) => boolean;
export const setForeground: (foreground: boolean) => void;
export const stop: () => void;
export const setServiceHandler: (handler: ServiceHandler) => void;
export const completeServiceRequest: (id: number, ok: boolean, payload: string) => void;
export const setClipboardText: (text: string) => void;
export const getClipboardText: () => string;
export const handleBack: () => boolean;
export const runDeferredSync: (optionsJson: string) => Promise<void>;
export const cancelDeferredSync: () => void;
/** Fixed in-memory fixture; native release builds return disabled without running PDFium. */
export const pdfiumSelfTest: () => Promise<string>;
