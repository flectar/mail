//! How the mobile hosts' memory signals map to [`MemoryPressure`].

use crate::memory::MemoryPressure;

/// Android's `ComponentCallbacks2.onTrimMemory` levels. Since Android 14 the
/// system only sends UI_HIDDEN (20) and BACKGROUND (40); BACKGROUND then comes
/// shortly before the cached process is frozen, which Chromium treats as
/// critical. Earlier releases also send the RUNNING_* levels and COMPLETE.
#[cfg(any(test, target_os = "android"))]
pub(crate) fn android_trim_level(level: i32, sdk: i32) -> Option<MemoryPressure> {
    const RUNNING_LOW: i32 = 10;
    const RUNNING_CRITICAL: i32 = 15;
    const BACKGROUND: i32 = 40;
    const COMPLETE: i32 = 80;
    if level >= COMPLETE || level == RUNNING_CRITICAL || (level >= BACKGROUND && sdk >= 34) {
        Some(MemoryPressure::Critical)
    } else if level >= RUNNING_LOW {
        Some(MemoryPressure::Moderate)
    } else {
        None
    }
}

/// OpenHarmony's `onMemoryLevel` levels. 0–2 describe free memory on the
/// whole device and 3–6 (API 24) the app's place in the background list, so
/// they don't compare as numbers.
#[cfg(any(test, target_env = "ohos"))]
pub(crate) fn ohos_memory_level(level: i32) -> Option<MemoryPressure> {
    match level {
        // LOW, UI_HIDDEN, BACKGROUND_MODERATE
        1 | 3 | 4 => Some(MemoryPressure::Moderate),
        // CRITICAL, BACKGROUND_LOW, BACKGROUND_CRITICAL
        2 | 5 | 6 => Some(MemoryPressure::Critical),
        // MODERATE leaves the device plenty, like Android's RUNNING_MODERATE.
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{MemoryPressure, android_trim_level, ohos_memory_level};

    #[test]
    fn android_levels_follow_the_running_release() {
        // Android 14 and later: only UI_HIDDEN and BACKGROUND arrive.
        assert_eq!(android_trim_level(20, 35), Some(MemoryPressure::Moderate));
        assert_eq!(android_trim_level(40, 34), Some(MemoryPressure::Critical));
        // Earlier releases.
        assert_eq!(android_trim_level(5, 33), None);
        assert_eq!(android_trim_level(10, 33), Some(MemoryPressure::Moderate));
        assert_eq!(android_trim_level(15, 33), Some(MemoryPressure::Critical));
        assert_eq!(android_trim_level(40, 33), Some(MemoryPressure::Moderate));
        assert_eq!(android_trim_level(60, 33), Some(MemoryPressure::Moderate));
        assert_eq!(android_trim_level(80, 26), Some(MemoryPressure::Critical));
    }

    #[test]
    fn openharmony_levels_map_one_by_one() {
        assert_eq!(ohos_memory_level(0), None);
        assert_eq!(ohos_memory_level(1), Some(MemoryPressure::Moderate));
        assert_eq!(ohos_memory_level(2), Some(MemoryPressure::Critical));
        assert_eq!(ohos_memory_level(3), Some(MemoryPressure::Moderate));
        assert_eq!(ohos_memory_level(4), Some(MemoryPressure::Moderate));
        assert_eq!(ohos_memory_level(5), Some(MemoryPressure::Critical));
        assert_eq!(ohos_memory_level(6), Some(MemoryPressure::Critical));
        assert_eq!(ohos_memory_level(7), None);
    }
}
