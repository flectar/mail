#include "../entry/src/main/cpp/keyboard_state.h"
#include <cassert>
#include <string>
#include <utility>
#include <vector>

int main() {
    feihong::KeyboardState keyboard;
    std::vector<std::pair<bool, std::string>> events;
    auto emit = [&](bool pressed, const char* text) { events.emplace_back(pressed, text); };

    // Ctrl+Z is consumed before IME, including key-up after Ctrl is released.
    assert(!keyboard.Shortcut(2072, true, {true, false, false, false}, "\x11", emit));
    assert(keyboard.Shortcut(2042, true, {true, false, false, false}, "z", emit));
    assert(!keyboard.Shortcut(2072, false, {}, "\x11", emit));
    assert(keyboard.Shortcut(2042, false, {}, "Z", emit));
    assert((events == std::vector<std::pair<bool, std::string>>{
        {true, "\x11"}, {true, "z"}, {false, "\x11"}, {false, "z"}}));
    assert(keyboard.HasPreIme()); // postIME modifier edges must be suppressed.

    // Ctrl+Shift+Z preserves Shift until its actual release (redo/selection).
    events.clear();
    assert(keyboard.Shortcut(2042, true, {true, true, false, false}, "z", emit));
    assert(keyboard.Shortcut(2042, false, {false, true, false, false}, "Z", emit));
    assert(keyboard.Modifiers().shift && !keyboard.Modifiers().ctrl);
    assert(!keyboard.Shortcut(2017, true, {false, true, false, false}, "A", emit));
    assert(!keyboard.Shortcut(2047, false, {}, "\x10", emit));
    assert((events == std::vector<std::pair<bool, std::string>>{
        {true, "\x10"}, {true, "\x11"}, {true, "z"},
        {false, "\x11"}, {false, "z"}, {false, "\x10"}}));

    // Native IME copy synthesizes Ctrl without losing a physically held Shift.
    events.clear(); keyboard.Sync({false, true, false, false}, emit); events.clear();
    keyboard.ControlAction("c", emit);
    assert(keyboard.Modifiers().shift && !keyboard.Modifiers().ctrl);
    assert((events == std::vector<std::pair<bool, std::string>>{
        {false, "\x10"}, {true, "\x11"}, {true, "c"}, {false, "c"},
        {true, "\x10"}, {false, "\x11"}}));

    // Alt/Meta shortcuts remain paired; focus loss releases every held key.
    events.clear();
    assert(keyboard.Shortcut(2017, true, {false, false, true, true}, "a", emit));
    assert(!keyboard.Shortcut(99999, true, {false, false, true, true}, "", emit));
    keyboard.Reset(emit);
    assert(!keyboard.Modifiers().HasShortcut() && !keyboard.Modifiers().shift);
    assert(!keyboard.Shortcut(2017, false, {}, "a", emit));
    assert((events == std::vector<std::pair<bool, std::string>>{
        {false, "\x10"}, {true, "\x12"}, {true, "\x17"}, {true, "a"},
        {false, "a"}, {false, "\x12"}, {false, "\x17"}}));
}
