#include "../entry/src/main/cpp/native_damage.h"
#include <functional>
#include <iostream>
#include <map>
#include <stdexcept>
#include <string>
#include <vector>

using feihong::DamageRect;
using feihong::NativeBufferIdentity;

static void Check(bool condition, const char* message) {
    if (!condition) throw std::runtime_error(message);
}
static bool Same(DamageRect a, DamageRect b) {
    return a.x == b.x && a.y == b.y && a.width == b.width && a.height == b.height;
}

enum class Failure { None, Request, Fence, Map, Unmap, Flush, Foreground };

struct Fixture {
    uint32_t width = 6, height = 4, stride = 36;
    uint64_t generation = 1;
    feihong::NativeDamageTracker<> tracker;
    std::vector<uint8_t> source;
    struct Buffer { NativeBufferIdentity identity{}; std::vector<uint8_t> pixels; };
    std::map<uint32_t, Buffer> buffers;
    DamageRect lastCopy{};

    Fixture() { Resize(6, 4, false); }
    NativeBufferIdentity Identity(uint32_t sequence) const { return {sequence, width, height, stride, 12, 1547}; }
    void Resize(uint32_t w, uint32_t h, bool newGeneration = true) {
        if (newGeneration) ++generation;
        width = w; height = h; stride = width * 4 + 12;
        source.resize(static_cast<std::size_t>(width) * height * 4);
        for (std::size_t i = 0; i < source.size(); ++i) source[i] = static_cast<uint8_t>(i * 7 + generation);
        Check(tracker.ObserveSource(generation, generation, width, height, DamageRect::Full(width, height)), "valid resized source");
    }
    void Change(DamageRect damage, uint8_t value) {
        Check(damage.Within(width, height), "fixture damage bounds");
        for (uint32_t y = damage.y; y < damage.y + damage.height; ++y)
            for (uint32_t x = damage.x; x < damage.x + damage.width; ++x)
                for (uint32_t channel = 0; channel < 4; ++channel)
                    source[(static_cast<std::size_t>(y) * width + x) * 4 + channel] = value + channel;
        Check(tracker.ObserveSource(generation, generation, width, height, damage), "track valid source");
    }
    void NoChange() {
        Check(tracker.ObserveSource(generation, generation, width, height, {}), "empty valid source");
    }
    void Verify(const Buffer& buffer) const {
        for (uint32_t y = 0; y < height; ++y) {
            for (uint32_t byte = 0; byte < width * 4; ++byte)
                Check(buffer.pixels[static_cast<std::size_t>(y) * stride + byte] ==
                    source[static_cast<std::size_t>(y) * width * 4 + byte], "presented buffer must equal complete latest source");
            for (uint32_t byte = width * 4; byte < stride; ++byte)
                Check(buffer.pixels[static_cast<std::size_t>(y) * stride + byte] == 0xcd, "padding must remain untouched");
        }
    }
    bool Present(uint32_t sequence, Failure failure = Failure::None) {
        // The tracker/copy are the production implementations. The driver only
        // simulates OS ownership outcomes; no fake OS outcome is device evidence.
        if (failure == Failure::Foreground || failure == Failure::Request || failure == Failure::Fence) return false;
        auto& buffer = buffers[sequence];
        const auto identity = Identity(sequence);
        if (!(buffer.identity == identity)) {
            buffer.identity = identity;
            buffer.pixels.assign(static_cast<std::size_t>(stride) * height, 0xcd);
        }
        lastCopy = tracker.PendingCopy(identity);
        if (failure == Failure::Map) return false;
        Check(feihong::CopyRgbaDamage(buffer.pixels.data(), source.data(), width, height, stride, lastCopy), "production copy succeeds");
        if (failure == Failure::Unmap || failure == Failure::Flush) return false;
        tracker.Presented(identity);
        Verify(buffer);
        return true;
    }
    void WarmThree() { Check(Present(1) && Present(2) && Present(3), "warm all rotating buffers"); }
};

static void RotatingBuffersAccumulateAllSourceChanges() {
    Fixture fixture;
    fixture.WarmThree();
    fixture.Change({0, 0, 1, 1}, 80);
    Check(fixture.Present(1), "first buffer updates");
    fixture.Change({4, 2, 1, 1}, 90);
    Check(fixture.Present(2), "second catches both changes");
    Check(Same(fixture.lastCopy, {0, 0, 5, 3}), "accumulated bounding box, not current-only damage");
    Check(fixture.Present(1), "first catches later change");
    Check(Same(fixture.lastCopy, {4, 2, 1, 1}), "only successful acquired buffer was cleared");
    Check(fixture.Present(3), "third catches whole history");
    Check(Same(fixture.lastCopy, {0, 0, 5, 3}), "third did not lose prior damage");
}

static void FailuresAndForegroundRejectionRetainDamageForEmptyRetry() {
    for (const auto failure : {Failure::Request, Failure::Fence, Failure::Map, Failure::Unmap, Failure::Flush, Failure::Foreground}) {
        Fixture fixture;
        fixture.WarmThree();
        fixture.Change({2, 1, 1, 2}, 170);
        Check(!fixture.Present(1, failure), "OS failure expected");
        fixture.NoChange();
        Check(fixture.Present(1), "empty Slint retry recovers retained damage");
        Check(Same(fixture.lastCopy, {2, 1, 1, 2}), "failed submission never clears pending");
        Check(fixture.Present(2) && fixture.Present(3), "other buffers retain failed source update");
    }
}

static void UnknownFailedBufferRemainsFull() {
    for (const auto failure : {Failure::Map, Failure::Unmap, Failure::Flush}) {
        Fixture fixture;
        Check(!fixture.Present(10, failure), "unknown buffer failure expected");
        fixture.NoChange();
        Check(fixture.Present(10), "unknown buffer retry succeeds");
        Check(Same(fixture.lastCopy, DamageRect::Full(fixture.width, fixture.height)), "unknown failed buffer stays full");
    }
}

static void GenerationResizeAndStaleSourceAreIsolated() {
    Fixture fixture;
    fixture.WarmThree();
    Check(!fixture.tracker.ObserveSource(0, fixture.generation, fixture.width, fixture.height, {1, 1, 1, 1}), "stale source rejected");
    Check(fixture.Present(1) && fixture.lastCopy.Empty(), "stale source does not dirty current generation");
    fixture.Resize(4, 6); // Same retained byte count, different row geometry.
    Check(fixture.Present(1), "resized buffer presents full new source");
    Check(Same(fixture.lastCopy, DamageRect::Full(4, 6)), "same-count resize must invalidate history");
    fixture.tracker.Reset(); // Surface destroyed/recreated at the same dimensions.
    fixture.NoChange();
    Check(fixture.Present(2), "recreated surface accepts retained complete source");
    Check(Same(fixture.lastCopy, DamageRect::Full(4, 6)), "recreated buffers start full even with no new damage");
}

static void EvictedSequenceAndConfigChangeStartFull() {
    feihong::NativeDamageTracker<2> tracker;
    Check(tracker.ObserveSource(1, 1, 8, 4, {}), "initial source");
    const NativeBufferIdentity a{1, 8, 4, 40, 12, 1547}, b{2, 8, 4, 40, 12, 1547}, c{3, 8, 4, 40, 12, 1547};
    for (const auto identity : {a, b, c}) {
        Check(Same(tracker.PendingCopy(identity), DamageRect::Full(8, 4)), "unknown sequence full");
        tracker.Presented(identity);
    }
    Check(tracker.Size() == 2, "bounded cache");
    Check(Same(tracker.PendingCopy(a), DamageRect::Full(8, 4)), "evicted sequence full");
    tracker.Presented(a);
    auto changed = a;
    changed.stride = 44;
    Check(Same(tracker.PendingCopy(changed), DamageRect::Full(8, 4)), "same sequence new stride full");
    tracker.Presented(a); // Stale metadata cannot acknowledge the new allocation.
    Check(Same(tracker.PendingCopy(changed), DamageRect::Full(8, 4)), "only exact metadata clears");
}

static void CopyUsesActualStrideAndTouchesOnlyDamage() {
    const uint32_t width = 5, height = 3, stride = 28;
    std::vector<uint8_t> source(width * height * 4, 0x82), destination(stride * height, 0xcd);
    const DamageRect damage{2, 1, 2, 1};
    Check(feihong::CopyRgbaDamage(destination.data(), source.data(), width, height, stride, damage), "partial production copy");
    for (uint32_t y = 0; y < height; ++y) for (uint32_t byte = 0; byte < stride; ++byte) {
        const bool changed = y == 1 && byte >= 8 && byte < 16;
        Check(destination[y * stride + byte] == (changed ? 0x82 : 0xcd), "outside damage and padding untouched");
    }
    const auto snapshot = destination;
    Check(feihong::CopyRgbaDamage(destination.data(), source.data(), width, height, stride, {}), "empty damage accepted");
    Check(destination == snapshot, "empty region copies zero bytes");
    Check(!feihong::CopyRgbaDamage(destination.data(), source.data(), width, height, 19, damage), "insufficient stride rejected");
    Check(!feihong::CopyRgbaDamage(destination.data(), source.data(), width, height, stride, {4, 0, 2, 1}), "out-of-bounds rejected");
    Check(!feihong::CopyRgbaDamage(nullptr, source.data(), width, height, stride, damage), "null destination rejected");
    Check(!feihong::CopyRgbaDamage(destination.data(), nullptr, width, height, stride, damage), "null source rejected");
    Check(destination == snapshot, "validation failures write nothing");
}

static void InvalidBoundingBoxesConservativelyBecomeFull() {
    const auto full = DamageRect::Full(6, 4);
    Check(Same(DamageRect::FromPhysicalBounds(-1, 0, 1, 1, 6, 4), full), "negative origin full");
    Check(Same(DamageRect::FromPhysicalBounds(5, 0, 2, 1, 6, 4), full), "outside extent full");
    Check(Same(DamageRect::FromPhysicalBounds(INT32_MAX, 0, UINT32_MAX, 1, 6, 4), full), "overflowing origin+extent full");
    Check(DamageRect::FromPhysicalBounds(0, 0, 0, 0, 6, 4).Empty(), "empty physical region preserved");
    Check(Same(DamageRect::FromPhysicalBounds(2, 1, 1, 2, 6, 4), {2, 1, 1, 2}), "valid exact region");
    std::vector<uint8_t> pixels(16, 0x42);
    Check(!feihong::CopyRgbaDamage(pixels.data(), pixels.data(), UINT32_MAX, UINT32_MAX, UINT32_MAX, full), "arithmetic size guard precedes pointer access");
}

static void LegacyFullSourceInvalidatesOtherBuffers() {
    Fixture fixture;
    fixture.WarmThree();
    fixture.Change(DamageRect::Full(fixture.width, fixture.height), 201);
    Check(fixture.Present(1), "legacy full source acquired buffer");
    fixture.tracker.Reset(); // Production clears history across the legacy ABI boundary.
    fixture.NoChange();
    Check(fixture.Present(2), "next damage-aware source starts full");
    Check(Same(fixture.lastCopy, DamageRect::Full(fixture.width, fixture.height)), "legacy cross-ABI source cannot be reused partially");
}

static void RepeatedArbitraryRotationAndFailureAlwaysProducesLatestPixels() {
    Fixture fixture;
    uint32_t random = 0x6f686f73;
    for (unsigned i = 0; i < 1200; ++i) {
        random = random * 1664525u + 1013904223u;
        if (i % 101 == 0) fixture.Resize(i % 2 ? 4 : 6, i % 2 ? 6 : 4);
        const auto x = random % fixture.width;
        const auto y = (random >> 8) % fixture.height;
        fixture.Change({x, y, 1, 1}, static_cast<uint8_t>(random >> 16));
        const uint32_t sequence = (random >> 12) % 23; // More buffers than the bounded cache.
        const auto failure = static_cast<Failure>((random >> 24) % 7);
        fixture.Present(sequence, failure);
        fixture.NoChange();
        Check(fixture.Present(sequence), "retry after arbitrary OS outcome");
        Check(fixture.tracker.Size() <= 8, "cache stays bounded");
    }
    for (uint32_t sequence = 0; sequence < 23; ++sequence) Check(fixture.Present(sequence), "all buffers reach final source");
}

struct FakeLeaseBuffer {
    unsigned references = 0, referenceCalls = 0, releaseCalls = 0;
    bool referenceAllowed = true, retired = false, destroyed = false;
    static int Reference(void* pointer) {
        auto& buffer = *static_cast<FakeLeaseBuffer*>(pointer);
        ++buffer.referenceCalls;
        if (!buffer.referenceAllowed || buffer.destroyed) return -1;
        ++buffer.references;
        return 0;
    }
    static void Unreference(void* pointer) {
        auto& buffer = *static_cast<FakeLeaseBuffer*>(pointer);
        Check(buffer.references > 0, "lease reference balance");
        --buffer.references;
        ++buffer.releaseCalls;
        if (buffer.retired && buffer.references == 0) buffer.destroyed = true;
    }
    void Retire() { retired = true; if (references == 0) destroyed = true; }
};

static void OwnedLeasePreventsSequenceReuseAndHitDoubleReference() {
    FakeLeaseBuffer original, replacement;
    feihong::NativeDamageTracker<2> tracker(FakeLeaseBuffer::Reference, FakeLeaseBuffer::Unreference);
    const NativeBufferIdentity identity{9, 8, 4, 40, 12, 1547};
    Check(tracker.ObserveSource(1, 1, 8, 4, {}), "lease source");
    tracker.PendingCopy(identity, &original);
    tracker.Presented(identity, &original);
    Check(tracker.PendingCopy(identity, &original).Empty(), "owned hit reuses complete buffer");
    Check(original.references == 1 && original.referenceCalls == 1, "hit does not add reference");
    auto changed = identity;
    changed.stride = 44;
    Check(Same(tracker.PendingCopy(changed, &original), DamageRect::Full(8, 4)), "owned config change full");
    Check(original.referenceCalls == 1, "config change retains same lease without double reference");
    original.Retire();
    Check(!original.destroyed, "leased retired buffer stays live so allocator cannot reuse its sequence");
    tracker.Reset();
    Check(original.destroyed && original.references == 0 && original.releaseCalls == 1, "reset releases retired object");
    // The allocator may now legitimately reuse the old sequence and metadata.
    Check(tracker.ObserveSource(1, 1, 8, 4, {}), "new source after ownership reset");
    Check(Same(tracker.PendingCopy(identity, &replacement), DamageRect::Full(8, 4)), "replacement sequence starts full");
}

static void OwnedLeaseLruGenerationDimensionsAndLegacyRelease() {
    FakeLeaseBuffer a, b, c;
    const NativeBufferIdentity ia{1, 8, 4, 40, 12, 1547}, ib{2, 8, 4, 40, 12, 1547}, ic{3, 8, 4, 40, 12, 1547};
    {
        feihong::NativeDamageTracker<2> tracker(FakeLeaseBuffer::Reference, FakeLeaseBuffer::Unreference);
        tracker.ObserveSource(1, 1, 8, 4, {});
        tracker.PendingCopy(ia, &a); tracker.PendingCopy(ib, &b);
        tracker.PendingCopy(ia, &a); // Touch A; B is both tracker/lease LRU.
        tracker.PendingCopy(ic, &c);
        Check(tracker.Size() == 2 && a.references == 1 && b.references == 0 && c.references == 1, "coupled LRU evicts B lease and history together");
        tracker.ObserveSource(2, 2, 8, 4, {});
        Check(tracker.Size() == 0 && a.references == 0 && c.references == 0, "generation reset releases leases");
        tracker.PendingCopy(ia, &a);
        tracker.ObserveSource(2, 2, 4, 8, {});
        Check(a.references == 0 && tracker.Size() == 0, "equal-area dimension change releases leases");
        tracker.PendingCopy(ib, &b);
        tracker.Reset(); // Same operation used for Surface reset and legacy ABI.
        Check(b.references == 0 && tracker.Size() == 0, "Surface/legacy reset releases leases");
        tracker.PendingCopy(ic, &c);
    }
    Check(a.references == 0 && b.references == 0 && c.references == 0, "RAII exit releases remaining lease");
}

static void FailedReferenceIsUncachedFullAndPreservesOtherPending() {
    FakeLeaseBuffer known, denied;
    denied.referenceAllowed = false;
    feihong::NativeDamageTracker<2> tracker(FakeLeaseBuffer::Reference, FakeLeaseBuffer::Unreference);
    const NativeBufferIdentity good{1, 8, 4, 40, 12, 1547}, bad{2, 8, 4, 40, 12, 1547};
    tracker.ObserveSource(1, 1, 8, 4, {});
    tracker.PendingCopy(good, &known); tracker.Presented(good, &known);
    tracker.ObserveSource(1, 1, 8, 4, {1, 1, 2, 1});
    Check(Same(tracker.PendingCopy(bad, &denied), DamageRect::Full(8, 4)), "failed reference falls back to full");
    tracker.Presented(bad, &denied);
    Check(tracker.Size() == 1 && denied.references == 0 && denied.releaseCalls == 0, "failed reference never cached or acknowledged");
    Check(Same(tracker.PendingCopy(good, &known), {1, 1, 2, 1}), "other owned entry keeps pending damage");
    Check(Same(tracker.PendingCopy(bad, &denied), DamageRect::Full(8, 4)), "next denied acquisition still full");
}

static void BackgroundResetReleasesAndEmptyResumeCopiesFullSource() {
    FakeLeaseBuffer buffer;
    feihong::NativeDamageTracker<> tracker(FakeLeaseBuffer::Reference, FakeLeaseBuffer::Unreference);
    const NativeBufferIdentity identity{1, 5, 3, 28, 12, 1547};
    std::vector<uint8_t> source(5 * 3 * 4, 0x62), destination(28 * 3, 0xcd);
    tracker.ObserveSource(1, 1, 5, 3, DamageRect::Full(5, 3));
    Check(feihong::CopyRgbaDamage(destination.data(), source.data(), 5, 3, 28,
        tracker.PendingCopy(identity, &buffer)), "first complete source copied");
    tracker.Presented(identity, &buffer);
    source[(1 * 5 + 3) * 4] = 0x90;
    tracker.ObserveSource(1, 1, 5, 3, {3, 1, 1, 1});
    // Background gating abandons the presentation, then discards the cache.
    tracker.Reset();
    Check(buffer.references == 0 && buffer.releaseCalls == 1 && tracker.Size() == 0, "background reset releases owned lease");
    tracker.ObserveSource(1, 1, 5, 3, {}); // Resume need not repaint the retained Rust source.
    const auto damage = tracker.PendingCopy(identity, &buffer);
    Check(Same(damage, DamageRect::Full(5, 3)), "empty source resume still full native copy");
    Check(feihong::CopyRgbaDamage(destination.data(), source.data(), 5, 3, 28, damage), "resume copies valid complete source");
    tracker.Presented(identity, &buffer);
    for (uint32_t y = 0; y < 3; ++y) for (uint32_t byte = 0; byte < 28; ++byte)
        Check(destination[y * 28 + byte] == (byte < 20 ? source[y * 20 + byte] : 0xcd), "resume source exact and padding intact");
    Check(buffer.references == 1 && buffer.referenceCalls == 2, "resumed cache has exactly one new ownership lease");
}

int main() {
    const std::vector<std::pair<const char*, std::function<void()>>> tests{
        {"rotating_buffers_accumulate_all_source_changes", RotatingBuffersAccumulateAllSourceChanges},
        {"failed_and_foreground_rejected_frames_survive_empty_retry", FailuresAndForegroundRejectionRetainDamageForEmptyRetry},
        {"unknown_failed_buffer_stays_full", UnknownFailedBufferRemainsFull},
        {"generation_resize_stale_source_isolation", GenerationResizeAndStaleSourceAreIsolated},
        {"eviction_and_config_change_force_full", EvictedSequenceAndConfigChangeStartFull},
        {"production_copy_stride_padding_bounds_zero_write", CopyUsesActualStrideAndTouchesOnlyDamage},
        {"invalid_physical_bounds_and_arithmetic_guards", InvalidBoundingBoxesConservativelyBecomeFull},
        {"legacy_source_boundary_forces_full", LegacyFullSourceInvalidatesOtherBuffers},
        {"1200_arbitrary_rotations_failures_and_resizes", RepeatedArbitraryRotationAndFailureAlwaysProducesLatestPixels},
        {"owned_lease_sequence_lifetime_and_no_double_ref", OwnedLeasePreventsSequenceReuseAndHitDoubleReference},
        {"owned_lease_lru_generation_dimensions_legacy_raii_release", OwnedLeaseLruGenerationDimensionsAndLegacyRelease},
        {"failed_reference_uncached_full_preserves_other_pending", FailedReferenceIsUncachedFullAndPreservesOtherPending},
        {"background_reset_release_empty_source_resume_full", BackgroundResetReleasesAndEmptyResumeCopiesFullSource},
    };
    for (const auto& test : tests) {
        try { test.second(); std::cout << "PASS " << test.first << '\n'; }
        catch (const std::exception& error) { std::cerr << "FAIL " << test.first << ": " << error.what() << '\n'; return 1; }
    }
    std::cout << "PASS " << tests.size() << " tests; no native OS or device execution\n";
}
