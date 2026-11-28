import { common } from '@kit.AbilityKit';
import { BusinessError } from '@kit.BasicServicesKit';
import { scanBarcode, scanCore } from '@kit.ScanKit';
import { hilog } from '@kit.PerformanceAnalysisKit';

const LOG_TAG = 'FlectarOhos';
// Scan Kit rejects with this when the person closes the scanner.
const SCAN_CANCELED = 1000500002;

export interface ScanOutcome {
  code: string;
  error: string;
}

// Scan a QR code on Scan Kit's own screen, which needs no camera permission.
// An empty code and error means the person closed the scanner.
export async function scanQr(context: common.UIAbilityContext): Promise<ScanOutcome> {
  try {
    const result = await scanBarcode.startScanForResult(context, {
      scanTypes: [scanCore.ScanType.QR_CODE],
      enableMultiMode: false,
      enableAlbum: true,
    });
    return { code: result.originalValue, error: '' };
  } catch (error) {
    const failure = error as BusinessError;
    if (failure.code === SCAN_CANCELED) {
      return { code: '', error: '' };
    }
    hilog.error(0x0000, LOG_TAG, 'QR code scanner failed: %{public}d %{public}s',
      failure.code, failure.message);
    return { code: '', error: failure.message ?? 'QR code scanner failed' };
  }
}
