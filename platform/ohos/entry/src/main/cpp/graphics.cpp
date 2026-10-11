#include "graphics.h"
#include "input.h"
#include "feihong.h"
#include "native_damage.h"
#include <native_window/external_window.h>
#include <native_buffer/native_buffer.h>
#include <native_buffer/buffer_common.h>
#include <native_vsync/native_vsync.h>
#include <hilog/log.h>
#include <atomic>
#include <mutex>
#include <poll.h>
#include <qos/qos.h>
#include <unistd.h>
#include <algorithm>
#include <cmath>
#include <cerrno>
#include <chrono>
#include <cstdio>
#include <cstring>
#include <time.h>

namespace {
std::mutex surfaceMutex;
OHNativeWindow* nativeWindow = nullptr;
OH_NativeVSync* vsync = nullptr;
uint64_t generation = 0;
uint64_t presentedGeneration = 0;
uint64_t failureReportedGeneration = 0;
std::atomic<float> scale{1.0f};
std::atomic<bool> foreground{true};
std::atomic<bool> framePending{false};
uint32_t surfaceWidth = 1, surfaceHeight = 1;
std::string surfaceInsets = "{}";
// Bound ownership to eight buffers. Holding an object reference also prevents
// its sequence number from being recycled while a damage-history entry exists.
feihong::NativeDamageTracker<> nativeDamage{
    [](void* buffer) { return OH_NativeBuffer_Reference(static_cast<OH_NativeBuffer*>(buffer)); },
    [](void* buffer) { OH_NativeBuffer_Unreference(static_cast<OH_NativeBuffer*>(buffer)); }};

#ifdef FEIHONG_DEBUG_TESTS
// Numeric, aggregate diagnostics for this app's presentation pipeline. The
// UI may be idle between frames; window FPS is meaningful during the recorded
// continuous gesture, rather than an idle-screen performance alarm.
using PerfClock = std::chrono::steady_clock;
std::atomic<bool> nativePerformanceEnabled{false};
struct NativePerfWindow {
    PerfClock::time_point started = PerfClock::now();
    uint64_t attempts = 0, presented = 0;
    double totalUs = 0, maximumUs = 0;
    double requestUs = 0, fenceUs = 0, copyUs = 0, flushUs = 0;
    double configUs = 0, mapUs = 0, pixelCopyUs = 0, unmapUs = 0;
    double pixelCopyCpuUs = 0;
    uint64_t pixelCopyCpuSamples = 0;
} nativePerf;
struct NativePerfSample {
    bool enabled = nativePerformanceEnabled.load(std::memory_order_relaxed);
    PerfClock::time_point started = enabled ? PerfClock::now() : PerfClock::time_point{}, phaseStarted = started;
    uint32_t width, height;
    double requestUs = 0, fenceUs = 0, copyUs = 0, flushUs = 0;
    double configUs = 0, mapUs = 0, pixelCopyUs = 0, unmapUs = 0;
    double pixelCopyCpuUs = 0;
    timespec pixelCopyCpuStarted{};
    bool pixelCopyCpuStartedValid = false, pixelCopyCpuValid = false;
    uint32_t strideBytes = 0, bufferUsage = 0;
    bool presented = false;
    NativePerfSample(uint32_t width, uint32_t height) : width(width), height(height) {}
    void FinishPhase(double& target) {
        if (!enabled) return;
        const auto now = PerfClock::now();
        target = std::chrono::duration<double, std::micro>(now - phaseStarted).count();
        phaseStarted = now;
    }
    void BeginPixelCopyCpu() {
        if (enabled) pixelCopyCpuStartedValid = clock_gettime(CLOCK_THREAD_CPUTIME_ID, &pixelCopyCpuStarted) == 0;
    }
    void FinishPixelCopyCpu() {
        if (!pixelCopyCpuStartedValid) return;
        timespec ended{};
        if (clock_gettime(CLOCK_THREAD_CPUTIME_ID, &ended) != 0) return;
        pixelCopyCpuUs = static_cast<double>(ended.tv_sec - pixelCopyCpuStarted.tv_sec) * 1000000.0 +
            static_cast<double>(ended.tv_nsec - pixelCopyCpuStarted.tv_nsec) / 1000.0;
        pixelCopyCpuValid = pixelCopyCpuUs >= 0;
    }
    ~NativePerfSample() {
        if (!enabled) return;
        const auto now = PerfClock::now();
        const double total = std::chrono::duration<double, std::micro>(now - started).count();
        ++nativePerf.attempts;
        nativePerf.presented += presented;
        nativePerf.totalUs += total;
        nativePerf.maximumUs = std::max(nativePerf.maximumUs, total);
        nativePerf.requestUs += requestUs;
        nativePerf.fenceUs += fenceUs;
        nativePerf.copyUs += copyUs;
        nativePerf.flushUs += flushUs;
        nativePerf.configUs += configUs;
        nativePerf.mapUs += mapUs;
        nativePerf.pixelCopyUs += pixelCopyUs;
        nativePerf.unmapUs += unmapUs;
        if (pixelCopyCpuValid) {
            nativePerf.pixelCopyCpuUs += pixelCopyCpuUs;
            ++nativePerf.pixelCopyCpuSamples;
        }
        const double wallMs = std::chrono::duration<double, std::milli>(now - nativePerf.started).count();
        if (wallMs < 2000) return;
        const double divisor = static_cast<double>(nativePerf.attempts) * 1000.0;
        char copyCpuMs[32] = "none";
        if (nativePerf.pixelCopyCpuSamples) std::snprintf(copyCpuMs, sizeof(copyCpuMs), "%.2f",
            nativePerf.pixelCopyCpuUs / (static_cast<double>(nativePerf.pixelCopyCpuSamples) * 1000.0));
        OH_LOG_Print(LOG_APP, LOG_INFO, 0xff01, "FeihongPerf",
            "Native present size=%{public}ux%{public}u attempts=%{public}llu presented=%{public}llu "
            "wallMs=%{public}.1f windowFps=%{public}.2f avgMs=%{public}.2f maxMs=%{public}.2f "
            "requestMs=%{public}.2f fenceMs=%{public}.2f mapCopyMs=%{public}.2f flushMs=%{public}.2f "
            "configMs=%{public}.2f mapMs=%{public}.2f pixelCopyMs=%{public}.2f unmapMs=%{public}.2f "
            "pixelCopyCpuSamples=%{public}llu pixelCopyCpuMs=%{public}s strideBytes=%{public}u bufferUsage=%{public}u",
            width, height, static_cast<unsigned long long>(nativePerf.attempts),
            static_cast<unsigned long long>(nativePerf.presented), wallMs,
            static_cast<double>(nativePerf.presented) * 1000.0 / wallMs,
            nativePerf.totalUs / divisor, nativePerf.maximumUs / 1000.0,
            nativePerf.requestUs / divisor, nativePerf.fenceUs / divisor,
            nativePerf.copyUs / divisor, nativePerf.flushUs / divisor,
            nativePerf.configUs / divisor, nativePerf.mapUs / divisor,
            nativePerf.pixelCopyUs / divisor, nativePerf.unmapUs / divisor,
            static_cast<unsigned long long>(nativePerf.pixelCopyCpuSamples), copyCpuMs, strideBytes, bufferUsage);
        nativePerf = NativePerfWindow{};
    }
};
#endif

void Frame(long long, void*) { framePending.store(false); feihong_vsync(); }
void UpdateSize(OH_NativeXComponent* component, void* window) {
    uint64_t width = 0, height = 0;
    if (OH_NativeXComponent_GetXComponentSize(component, window, &width, &height) != 0) return;
    if (surfaceWidth != width || surfaceHeight != height) nativeDamage.Reset();
    surfaceWidth = static_cast<uint32_t>(width); surfaceHeight = static_cast<uint32_t>(height);
    feihong_set_viewport(surfaceWidth, surfaceHeight, scale.load(), surfaceInsets.c_str());
}
void Created(OH_NativeXComponent* component, void* window) {
    std::lock_guard<std::mutex> lock(surfaceMutex);
    nativeDamage.Reset();
    if (nativeWindow) OH_NativeWindow_NativeObjectUnreference(nativeWindow);
    nativeWindow = static_cast<OHNativeWindow*>(window);
    if (!nativeWindow || OH_NativeWindow_NativeObjectReference(nativeWindow) != 0) { nativeWindow = nullptr; return; }
    ++generation;
    OH_NativeWindow_NativeWindowHandleOpt(nativeWindow, SET_FORMAT, NATIVEBUFFER_PIXEL_FMT_RGBA_8888);
    OH_NativeWindow_NativeWindowHandleOpt(nativeWindow, SET_USAGE,
        static_cast<uint64_t>(NATIVEBUFFER_USAGE_CPU_READ | NATIVEBUFFER_USAGE_CPU_WRITE | NATIVEBUFFER_USAGE_HW_TEXTURE));
    OH_NativeWindow_NativeWindowHandleOpt(nativeWindow, SET_TIMEOUT, static_cast<int32_t>(1000));
    if (!vsync) vsync = OH_NativeVSync_Create("feihong", 7);
    framePending.store(false);
    UpdateSize(component, window); feihong_surface(generation, true); feihong_vsync();
    OH_LOG_Print(LOG_APP, LOG_INFO, 0xff01, "Feihong", "Surface created: %{public}ux%{public}u generation=%{public}llu",
        surfaceWidth, surfaceHeight, static_cast<unsigned long long>(generation));
}
void Changed(OH_NativeXComponent* component, void* window) {
    std::lock_guard<std::mutex> lock(surfaceMutex);
    if (nativeWindow == window) { nativeDamage.Reset(); UpdateSize(component, window); feihong_surface(++generation, true); feihong_vsync(); }
}
void Destroyed(OH_NativeXComponent*, void* window) {
    std::lock_guard<std::mutex> lock(surfaceMutex);
    if (nativeWindow != window) return;
    nativeDamage.Reset();
    feihong_surface(++generation, false);
    OH_NativeWindow_NativeObjectUnreference(nativeWindow); nativeWindow = nullptr;
    // Vsync user data is process lifetime. An already queued callback cannot touch the window.
    framePending.store(false);
}
}

namespace feihong {
void RecordInputBoundary(int action) {
#ifdef FEIHONG_DEBUG_TESTS
    if (!nativePerformanceEnabled.load(std::memory_order_relaxed)) return;
    timespec stamp{};
    if (clock_gettime(CLOCK_MONOTONIC, &stamp) != 0 || stamp.tv_sec < 0) return;
    const auto ns = static_cast<uint64_t>(stamp.tv_sec) * 1000000000ULL +
        static_cast<uint64_t>(stamp.tv_nsec);
    OH_LOG_Print(LOG_APP, LOG_INFO, 0xff01, "FeihongPerf",
        "Native input boundary action=%{public}d monotonicNs=%{public}llu",
        action, static_cast<unsigned long long>(ns));
#else
    (void)action;
#endif
}
void RegisterGraphics(OH_NativeXComponent* component) {
    static OH_NativeXComponent_Callback callbacks{Created, Changed, Destroyed, DispatchTouch};
    OH_NativeXComponent_RegisterCallback(component, &callbacks);
    RegisterInput(component);
}
void ResumeSurface() {
    std::lock_guard<std::mutex> lock(surfaceMutex);
    feihong_set_viewport(surfaceWidth, surfaceHeight, scale.load(), surfaceInsets.c_str());
    feihong_surface(generation, nativeWindow != nullptr); feihong_vsync();
}
void SetDensity(float density) { scale.store(std::isfinite(density) && density > 0 ? density : 1.0f); }
float Density() { return scale.load(); }
void SetInsets(const std::string& json) { std::lock_guard<std::mutex> lock(surfaceMutex); surfaceInsets = json; }
void SetForeground(bool value) {
    foreground.store(value);
    if (value) ResumeSurface();
    else {
        // Gate new submissions first, then wait for an in-flight presentation.
        // Releasing cached leases reduces background retention. On resume every
        // native buffer starts full, even when Slint's retained source has no damage.
        std::lock_guard<std::mutex> lock(surfaceMutex);
        nativeDamage.Reset();
    }
}
}

extern "C" int32_t feihong_request_frame() {
    std::lock_guard<std::mutex> lock(surfaceMutex);
    if (!nativeWindow || !vsync || !foreground.load()) return -1;
    if (framePending.exchange(true)) return 0;
    int result = OH_NativeVSync_RequestFrame(vsync, Frame, nullptr);
    if (result != 0) framePending.store(false);
    return result;
}

// A Vulkan surface retains its NativeWindow until it has been destroyed. The
// generation check prevents the GPU path from acquiring a replaced Surface.
extern "C" void* feihong_gpu_acquire_window(uint64_t expectedGeneration) {
    std::lock_guard<std::mutex> lock(surfaceMutex);
    if (!nativeWindow || !foreground.load() || generation != expectedGeneration) return nullptr;
    if (OH_NativeWindow_NativeObjectReference(nativeWindow) != 0) return nullptr;
    return nativeWindow;
}
extern "C" void feihong_gpu_release_window(void* window) {
    if (window) OH_NativeWindow_NativeObjectUnreference(window);
}
extern "C" bool feihong_gpu_surface_current(uint64_t expectedGeneration) {
    std::lock_guard<std::mutex> lock(surfaceMutex);
    return nativeWindow && foreground.load() && generation == expectedGeneration;
}

extern "C" void feihong_native_set_performance_diagnostics(bool enabled) {
#ifdef FEIHONG_DEBUG_TESTS
    std::lock_guard<std::mutex> lock(surfaceMutex);
    nativePerformanceEnabled.store(enabled, std::memory_order_relaxed);
    nativePerf = NativePerfWindow{};
#else
    (void)enabled;
#endif
}

// Called on the Rust UI owner thread, never on the ArkTS bridge thread.
// Background mail workers keep their own default scheduling policy.
extern "C" int32_t feihong_native_ui_qos(bool interactive) {
    QoS_Level before = QOS_DEFAULT;
    const int beforeStatus = OH_QoS_GetThreadQoS(&before);
    const int result = interactive ? OH_QoS_SetThreadQoS(QOS_USER_INTERACTIVE)
                                   : OH_QoS_ResetThreadQoS();
#ifdef FEIHONG_DEBUG_TESTS
    if (nativePerformanceEnabled.load(std::memory_order_relaxed) || result != 0) {
        QoS_Level after = QOS_DEFAULT;
        const int afterStatus = OH_QoS_GetThreadQoS(&after);
        OH_LOG_Print(LOG_APP, result == 0 ? LOG_INFO : LOG_WARN, 0xff01, "FeihongPerf",
            "UI QoS: interactive=%{public}d setStatus=%{public}d beforeStatus=%{public}d beforeLevel=%{public}d "
            "afterStatus=%{public}d afterLevel=%{public}d",
            interactive, result, beforeStatus, beforeStatus == 0 ? static_cast<int>(before) : -1,
            afterStatus, afterStatus == 0 ? static_cast<int>(after) : -1);
    }
#else
    (void)beforeStatus;
#endif
    return result;
}

static int32_t PresentPixels(uint64_t expectedGeneration, const uint8_t* pixels, uint32_t width, uint32_t height,
    bool rgba, feihong::DamageRect sourceDamage, bool damageAware = false) {
    // Holding the lock across request/map/flush makes SurfaceDestroyed wait until the
    // retained native object is no longer used. Generation rejects queued stale frames.
    std::lock_guard<std::mutex> lock(surfaceMutex);
    if (!nativeWindow || generation != expectedGeneration || !pixels) return -1;
    OH_NativeBuffer_Config config{};
    auto fail = [&](int32_t code) {
        if (failureReportedGeneration != generation) {
            failureReportedGeneration = generation;
            OH_LOG_Print(LOG_APP, LOG_ERROR, 0xff01, "Feihong",
                "Present failed: status=%{public}d requested=%{public}ux%{public}u surface=%{public}ux%{public}u "
                "buffer=%{public}dx%{public}d strideBytes=%{public}d format=%{public}d generation=%{public}llu",
                code, width, height, surfaceWidth, surfaceHeight, config.width, config.height,
                config.stride, config.format, static_cast<unsigned long long>(generation));
        }
        return code;
    };
    if (width != surfaceWidth || height != surfaceHeight || width == 0 || height == 0 ||
        static_cast<uint64_t>(width) * height > 32ULL * 1024 * 1024 ||
        static_cast<uint64_t>(width) * 4 > INT32_MAX || height > INT32_MAX) return fail(-1);
    // The historical RGB/RGBA entry points have no source identity or damage
    // contract. Keep them full-copy and prevent cross-ABI retained-source reuse.
    if (!damageAware) nativeDamage.Reset();
    // Rust's retained source already changed, even if the native request or a
    // foreground race rejects this submission. Every rotating buffer must retain
    // those changes; an empty next Slint region must still recover a failed frame.
    nativeDamage.ObserveSource(expectedGeneration, generation, width, height, sourceDamage);
    if (!foreground.load()) return -1;
#ifdef FEIHONG_DEBUG_TESTS
    NativePerfSample performance(width, height);
#endif
    OHNativeWindowBuffer* windowBuffer = nullptr; int fence = -1;
    int result = OH_NativeWindow_NativeWindowRequestBuffer(nativeWindow, &windowBuffer, &fence);
#ifdef FEIHONG_DEBUG_TESTS
    performance.FinishPhase(performance.requestUs);
#endif
    if (result != 0 || !windowBuffer) return fail(result == 0 ? -1 : result);
    auto abort = [&] { OH_NativeWindow_NativeWindowAbortBuffer(nativeWindow, windowBuffer); };
    if (fence >= 0) {
        pollfd descriptor{fence, POLLIN, 0};
        int ready; do { ready = poll(&descriptor, 1, 1000); } while (ready < 0 && errno == EINTR);
        close(fence);
        if (ready <= 0 || (descriptor.revents & (POLLERR | POLLNVAL))) { abort(); return fail(-2); }
    }
#ifdef FEIHONG_DEBUG_TESTS
    performance.FinishPhase(performance.fenceUs);
#endif
    OH_NativeBuffer* buffer = nullptr;
    result = OH_NativeBuffer_FromNativeWindowBuffer(windowBuffer, &buffer);
    if (result != 0 || !buffer) { abort(); return fail(result == 0 ? -3 : result); }
    OH_NativeBuffer_GetConfig(buffer, &config);
    if (config.width < static_cast<int32_t>(width) || config.height < static_cast<int32_t>(height) ||
        config.stride < static_cast<int32_t>(width * 4) || config.format != NATIVEBUFFER_PIXEL_FMT_RGBA_8888) {
        abort(); return fail(-4);
    }
    const feihong::NativeBufferIdentity identity{
        OH_NativeBuffer_GetSeqNum(buffer), static_cast<uint32_t>(config.width),
        static_cast<uint32_t>(config.height), static_cast<uint32_t>(config.stride),
        config.format, static_cast<uint64_t>(config.usage)};
    const auto copyDamage = damageAware ? nativeDamage.PendingCopy(identity, buffer) :
        feihong::DamageRect::Full(width, height);
    void* address = nullptr;
#ifdef FEIHONG_DEBUG_TESTS
    performance.strideBytes = static_cast<uint32_t>(config.stride);
    performance.bufferUsage = static_cast<uint32_t>(config.usage);
    performance.FinishPhase(performance.configUs);
#endif
    result = OH_NativeBuffer_Map(buffer, &address);
    if (result != 0 || !address) { abort(); return fail(result == 0 ? -5 : result); }
#ifdef FEIHONG_DEBUG_TESTS
    performance.FinishPhase(performance.mapUs);
    performance.BeginPixelCopyCpu();
#endif
    if (rgba) {
        if (!feihong::CopyRgbaDamage(static_cast<uint8_t*>(address), pixels, width, height,
            static_cast<uint32_t>(config.stride), copyDamage)) {
            OH_NativeBuffer_Unmap(buffer); abort(); return fail(-6);
        }
    } else {
        for (uint32_t y = 0; y < height; ++y) {
            auto* row = static_cast<uint8_t*>(address) + static_cast<size_t>(y) * config.stride;
            const auto* source = pixels + static_cast<size_t>(y) * width * 3;
            for (uint32_t x = 0; x < width; ++x) {
                row[x * 4] = source[x * 3]; row[x * 4 + 1] = source[x * 3 + 1];
                row[x * 4 + 2] = source[x * 3 + 2]; row[x * 4 + 3] = 255;
            }
        }
    }
#ifdef FEIHONG_DEBUG_TESTS
    performance.FinishPhase(performance.pixelCopyUs);
    performance.FinishPixelCopyCpu();
    performance.copyUs = performance.configUs + performance.mapUs + performance.pixelCopyUs;
#endif
    result = OH_NativeBuffer_Unmap(buffer);
    if (result != 0) { abort(); return fail(result); }
#ifdef FEIHONG_DEBUG_TESTS
    performance.FinishPhase(performance.unmapUs);
#endif
    Region region{nullptr, 0};
    result = OH_NativeWindow_NativeWindowFlushBuffer(nativeWindow, windowBuffer, -1, region);
    if (result != 0) { abort(); return fail(result); }
    if (damageAware) nativeDamage.Presented(identity, buffer);
    else nativeDamage.Reset();
    if (presentedGeneration != generation) {
        presentedGeneration = generation;
        OH_LOG_Print(LOG_APP, LOG_INFO, 0xff01, "Feihong", "First Slint frame: %{public}ux%{public}u generation=%{public}llu",
            width, height, static_cast<unsigned long long>(generation));
    }
#ifdef FEIHONG_DEBUG_TESTS
    performance.FinishPhase(performance.flushUs);
    performance.flushUs += performance.unmapUs;
    performance.presented = result == 0;
#endif
    return result;
}

extern "C" int32_t feihong_present_rgba(uint64_t generation, const uint8_t* pixels, uint32_t width, uint32_t height) {
    return PresentPixels(generation, pixels, width, height, true, feihong::DamageRect::Full(width, height));
}
extern "C" int32_t feihong_native_present_rgba_damage(const uint8_t* pixels, uint32_t width, uint32_t height,
    uint64_t generation, int32_t dirtyX, int32_t dirtyY, uint32_t dirtyWidth, uint32_t dirtyHeight) {
    return PresentPixels(generation, pixels, width, height, true,
        feihong::DamageRect::FromPhysicalBounds(dirtyX, dirtyY, dirtyWidth, dirtyHeight, width, height), true);
}
// Retain the original ABI for the saved diagnostic baseline archive.
extern "C" int32_t feihong_present_rgb(uint64_t generation, const uint8_t* pixels, uint32_t width, uint32_t height) {
    return PresentPixels(generation, pixels, width, height, false, feihong::DamageRect::Full(width, height));
}
