#include "../entry/src/main/cpp/input_geometry.h"
#include <cassert>

int main() {
    const std::array<float, 4> caret{7.5f, 18.f, 1.5f, 24.f};
    feihong::InputGeometry windowed{42, 320., 96.};
    assert((windowed.CursorRect(caret) == std::array<double, 4>{327.5, 114., 1.5, 24.}));
    feihong::InputGeometry moved{42, -30.5, 200.};
    assert((moved.CursorRect(caret) == std::array<double, 4>{-23., 218., 1.5, 24.}));
    feihong::InputGeometry fullScreen{43, 0, 0};
    assert((fullScreen.CursorRect(caret) == std::array<double, 4>{7.5, 18., 1.5, 24.}));
}
