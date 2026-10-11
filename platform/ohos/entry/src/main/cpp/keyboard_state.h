#pragma once
#include <map>
#include <string>

namespace feihong {
struct KeyModifiers {
    bool ctrl = false, shift = false, alt = false, meta = false;
    bool HasShortcut() const { return ctrl || alt || meta; }
};

inline bool IsModifierKey(int code) {
    return code == 2045 || code == 2046 || code == 2047 || code == 2048 ||
        code == 2072 || code == 2073 || code == 2076 || code == 2077;
}

// Called under the native key mutex. preIME owns modifier snapshots once the
// Stage callback is available; postIME must not deliver those edges again.
class KeyboardState {
public:
    const KeyModifiers& Modifiers() const { return modifiers_; }
    bool HasPreIme() const { return preIme_; }

    template<class Emit> void Sync(KeyModifiers next, Emit emit) {
        Edge(modifiers_.shift, next.shift, "\x10", emit);
        Edge(modifiers_.ctrl, next.ctrl, "\x11", emit);
        Edge(modifiers_.alt, next.alt, "\x12", emit);
        Edge(modifiers_.meta, next.meta, "\x17", emit);
        modifiers_ = next;
    }

    template<class Emit> bool Shortcut(int code, bool pressed, KeyModifiers next,
                                      const std::string& text, Emit emit) {
        preIme_ = true;
        Sync(next, emit);
        if (IsModifierKey(code)) return false;
        auto captured = captured_.find(code);
        if (!pressed && captured != captured_.end()) {
            // The modifier can be released before this key. Pair with the
            // original press text rather than the current keyboard state.
            emit(false, captured->second.c_str());
            captured_.erase(captured);
            return true;
        }
        if (!pressed || !next.HasShortcut() || text.empty()) return false;
        if (captured == captured_.end()) captured = captured_.emplace(code, text).first;
        emit(true, captured->second.c_str());
        return true;
    }

    template<class Emit> void ControlAction(const char* text, Emit emit) {
        const auto physical = modifiers_;
        Sync({true, false, false, false}, emit);
        emit(true, text); emit(false, text);
        Sync(physical, emit);
    }

    template<class Emit> void Reset(Emit emit) {
        for (const auto& entry : captured_) emit(false, entry.second.c_str());
        captured_.clear();
        Sync({}, emit);
    }

private:
    template<class Emit> static void Edge(bool previous, bool next, const char* key, Emit emit) {
        if (previous != next) emit(next, key);
    }
    KeyModifiers modifiers_;
    bool preIme_ = false;
    std::map<int, std::string> captured_;
};
}
