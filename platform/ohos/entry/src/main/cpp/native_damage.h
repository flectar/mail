#pragma once

#include <algorithm>
#include <array>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <limits>
#include <utility>

namespace feihong {

struct DamageRect {
    uint32_t x = 0, y = 0, width = 0, height = 0;

    bool Empty() const { return width == 0 || height == 0; }
    static DamageRect Full(uint32_t width, uint32_t height) { return {0, 0, width, height}; }
    bool Within(uint32_t canvasWidth, uint32_t canvasHeight) const {
        return static_cast<uint64_t>(x) + width <= canvasWidth &&
            static_cast<uint64_t>(y) + height <= canvasHeight;
    }
    static DamageRect FromPhysicalBounds(int32_t x, int32_t y, uint32_t width, uint32_t height,
        uint32_t canvasWidth, uint32_t canvasHeight) {
        if (width == 0 || height == 0) return {};
        // Slint returns clipped physical coordinates. An unexpected rectangle
        // must cause a conservative full copy, never wrap or silently lose damage.
        if (x < 0 || y < 0) return Full(canvasWidth, canvasHeight);
        DamageRect result{static_cast<uint32_t>(x), static_cast<uint32_t>(y), width, height};
        return result.Within(canvasWidth, canvasHeight) ? result : Full(canvasWidth, canvasHeight);
    }
    void Union(const DamageRect& other) {
        if (other.Empty()) return;
        if (Empty()) { *this = other; return; }
        const uint32_t right = std::max(x + width, other.x + other.width);
        const uint32_t bottom = std::max(y + height, other.y + other.height);
        x = std::min(x, other.x);
        y = std::min(y, other.y);
        width = right - x;
        height = bottom - y;
    }
};

struct NativeBufferIdentity {
    uint32_t sequence, width, height, stride;
    int32_t format;
    uint64_t usage;
    bool operator==(const NativeBufferIdentity& other) const {
        return sequence == other.sequence && width == other.width && height == other.height &&
            stride == other.stride && format == other.format && usage == other.usage;
    }
};

// Outside an entry's pending rectangle its buffer equals the latest retained
// Rust source. Observe every valid source update BEFORE any native operation or
// foreground rejection. Clear only after a successful Unmap and FlushBuffer.
// A live native lease prevents sequence reuse after a buffer leaves its queue.
// Eviction/reset releases the lease; the next acquisition then starts full.
template<std::size_t Capacity = 8>
class NativeDamageTracker {
    static_assert(Capacity > 0);
    struct Lease {
        void* handle = nullptr;
        void (*release)(void*) = nullptr;
        Lease() = default;
        Lease(void* handle, void (*release)(void*)) : handle(handle), release(release) {}
        Lease(const Lease&) = delete;
        Lease& operator=(const Lease&) = delete;
        Lease(Lease&& other) noexcept : handle(other.handle), release(other.release) { other.handle = nullptr; }
        Lease& operator=(Lease&& other) noexcept {
            if (this != &other) {
                if (handle && release) release(handle);
                handle = other.handle;
                release = other.release;
                other.handle = nullptr;
            }
            return *this;
        }
        ~Lease() { if (handle && release) release(handle); }
    };
    struct Entry { NativeBufferIdentity identity{}; DamageRect pending{}; Lease lease{}; };
    std::array<Entry, Capacity> entries{};
    std::size_t count = 0;
    uint64_t sourceGeneration = 0;
    uint32_t sourceWidth = 0, sourceHeight = 0;
    int (*reference)(void*) = nullptr;
    void (*unreference)(void*) = nullptr;

public:
    NativeDamageTracker() = default;
    NativeDamageTracker(int (*reference)(void*), void (*unreference)(void*)) :
        reference(reference), unreference(unreference) {}
    void Reset() {
        for (auto& entry : entries) entry = Entry{};
        count = 0;
        sourceGeneration = 0;
        sourceWidth = sourceHeight = 0;
    }
    std::size_t Size() const { return count; }

    bool ObserveSource(uint64_t expectedGeneration, uint64_t currentGeneration,
        uint32_t width, uint32_t height, DamageRect damage) {
        if (expectedGeneration != currentGeneration || width == 0 || height == 0) return false;
        if (sourceGeneration != currentGeneration || sourceWidth != width || sourceHeight != height) {
            Reset();
            sourceGeneration = currentGeneration;
            sourceWidth = width;
            sourceHeight = height;
        }
        if (!damage.Within(width, height)) damage = DamageRect::Full(width, height);
        for (std::size_t i = 0; i < count; ++i) entries[i].pending.Union(damage);
        return true;
    }

    DamageRect PendingCopy(const NativeBufferIdentity& identity, void* buffer = nullptr) {
        std::size_t position = 0;
        while (position < count && entries[position].identity.sequence != identity.sequence) ++position;
        Entry entry{};
        if (position < count && entries[position].lease.handle == buffer) {
            entry = std::move(entries[position]);
            if (!(entry.identity == identity)) {
                entry.identity = identity;
                entry.pending = DamageRect::Full(sourceWidth, sourceHeight);
            }
        } else {
            // Failure to acquire ownership permits a full copy but no reuse or
            // acknowledgment. Existing entries keep their accumulated damage.
            if (reference && (!unreference || !buffer || reference(buffer) != 0))
                return DamageRect::Full(sourceWidth, sourceHeight);
            entry.identity = identity;
            entry.pending = DamageRect::Full(sourceWidth, sourceHeight);
            if (reference) entry.lease = Lease(buffer, unreference);
        }
        if (position == count) {
            if (count < Capacity) ++count;
            position = count - 1;
        }
        // Most recently acquired first; bounded eviction can only lose a
        // reuse opportunity. The evicted buffer is full on its next acquisition.
        for (std::size_t i = position; i > 0; --i) entries[i] = std::move(entries[i - 1]);
        entries[0] = std::move(entry);
        return entries[0].pending;
    }

    void Presented(const NativeBufferIdentity& identity, void* buffer = nullptr) {
        for (std::size_t i = 0; i < count; ++i) {
            if (entries[i].identity == identity && entries[i].lease.handle == buffer) {
                entries[i].pending = {};
                return;
            }
        }
    }
};

// The caller owns mapped memory of at least height rows. Preserve row padding
// and every pixel outside the accumulated rectangle. All validation precedes writes.
inline bool CopyRgbaDamage(uint8_t* destination, const uint8_t* source,
    uint32_t width, uint32_t height, uint32_t destinationStride, const DamageRect& damage) {
    if (!destination || !source || width == 0 || height == 0 || !damage.Within(width, height) ||
        static_cast<uint64_t>(width) * 4 > destinationStride) return false;
    const uint64_t sourceBytes = static_cast<uint64_t>(width) * height * 4;
    const uint64_t destinationBytes = static_cast<uint64_t>(height - 1) * destinationStride +
        static_cast<uint64_t>(width) * 4;
    if (sourceBytes > std::numeric_limits<std::size_t>::max() ||
        destinationBytes > std::numeric_limits<std::size_t>::max()) return false;
    const std::size_t bytes = static_cast<std::size_t>(damage.width) * 4;
    if (damage.Empty()) return true;
    for (uint32_t y = damage.y; y < damage.y + damage.height; ++y) {
        auto* row = destination + static_cast<std::size_t>(y) * destinationStride +
            static_cast<std::size_t>(damage.x) * 4;
        const auto* pixels = source + (static_cast<std::size_t>(y) * width + damage.x) * 4;
        std::memcpy(row, pixels, bytes);
    }
    return true;
}

} // namespace feihong
