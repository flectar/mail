import { common } from '@kit.AbilityKit';

export interface ScanOutcome {
  code: string;
  error: string;
}

// Scan Kit is a HarmonyOS kit. OpenHarmony builds pair devices through codes
// and displayed QR codes instead of scanning.
export async function scanQr(_context: common.UIAbilityContext): Promise<ScanOutcome> {
  return { code: '', error: 'Scan Kit is not available in this build.' };
}
