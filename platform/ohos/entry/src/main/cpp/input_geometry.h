#pragma once
#include <array>
#include <cstdint>

namespace feihong {
struct InputGeometry {
    int32_t windowId = 0;
    double screenX = 0, screenY = 0;

    std::array<double, 4> CursorRect(const std::array<float, 4>& local) const {
        return {screenX + local[0], screenY + local[1], local[2], local[3]};
    }
};
}
