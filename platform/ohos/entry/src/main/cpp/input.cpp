#include "input.h"
#include "graphics.h"
#include "feihong.h"
#include "keyboard_state.h"
#include <arkui/ui_input_event.h>
#include <inputmethod/inputmethod_controller_capi.h>
#include <inputmethod/inputmethod_text_editor_proxy_capi.h>
#include <inputmethod/inputmethod_inputmethod_proxy_capi.h>
#include <inputmethod/inputmethod_attach_options_capi.h>
#include <inputmethod/inputmethod_cursor_info_capi.h>
#include <inputmethod/inputmethod_text_config_capi.h>
#include <atomic>
#include <mutex>
#include <string>
#include <algorithm>
#include <array>
#include <dlfcn.h>

namespace {
std::mutex textMutex;
// Stage geometry updates can notify the proxy while the Rust owner updates
// its editor. Keep the proxy alive across each native call; callbacks only
// acquire textMutex and enqueue copied events, never this lifecycle mutex.
std::mutex inputMethodMutex;
std::u16string surrounding;
int cursor = 0, anchor = 0, inputType = 0, textOffset = 0;
std::array<float, 4> caret{};
feihong::InputGeometry inputGeometry;
InputMethod_TextEditorProxy* editor = nullptr;
InputMethod_InputMethodProxy* inputMethod = nullptr;
bool editorEnabled = false; // Protected by inputMethodMutex.
// Attachment also stays active with a hardware keyboard or after hiding the
// software keyboard. Only the input method's callback knows what is visible.
std::atomic<bool> keyboardVisible{false};
std::mutex keyMutex;
feihong::KeyboardState keyboard;
void EmitKey(bool pressed, const char* text) { feihong_key(pressed, text); }

#ifdef FEIHONG_DEBUG_TESTS
// State-only traces distinguish an IME that retains composition in its own UI
// from preview callbacks reaching Slint. Never log editor contents or positions.
// The budget is protected by textMutex and renewed after a successful attach.
unsigned int imeTraceBudget = 64;
void TraceImeLocked(const std::string& message) {
    const bool secretInput = inputType == IME_TEXT_INPUT_TYPE_VISIBLE_PASSWORD ||
        inputType == IME_TEXT_INPUT_TYPE_NUMBER_PASSWORD ||
        inputType == IME_TEXT_INPUT_TYPE_SCREEN_LOCK_PASSWORD ||
        inputType == IME_TEXT_INPUT_TYPE_NEW_PASSWORD;
    if (secretInput || imeTraceBudget == 0) return;
    --imeTraceBudget;
    feihong_native_log(message.c_str());
}
void TraceIme(const char* message) {
    std::lock_guard<std::mutex> lock(textMutex);
    TraceImeLocked(message);
}
#endif

void GetConfig(InputMethod_TextEditorProxy*, InputMethod_TextConfig* config) {
    std::lock_guard<std::mutex> lock(textMutex);
    OH_TextConfig_SetInputType(config, static_cast<InputMethod_TextInputType>(inputType));
    OH_TextConfig_SetSelection(config, anchor, cursor);
    OH_TextConfig_SetWindowId(config, inputGeometry.windowId);
    [[maybe_unused]] const auto previewStatus = OH_TextConfig_SetPreviewTextSupport(config, true);
#ifdef FEIHONG_DEBUG_TESTS
    TraceImeLocked("InputMethod GetConfig previewSupport=true status=" + std::to_string(previewStatus) +
        " inputType=" + std::to_string(inputType));
#endif
    // The NDK text listener has no raw-key delivery callback. Shortcuts arrive
    // through Stage's preIME handler; ordinary text stays with the input method.
    // Resolve the API 26 setting dynamically to keep API 20 loadable.
    using ConsumeKeys = InputMethod_ErrorCode (*)(InputMethod_TextConfig*, bool);
    static auto consumeKeys = reinterpret_cast<ConsumeKeys>(dlsym(RTLD_DEFAULT, "OH_TextConfig_SetConsumeKeyEvents"));
    if (consumeKeys) consumeKeys(config, false);
    InputMethod_CursorInfo* info = nullptr;
    if (OH_TextConfig_GetCursorInfo(config, &info) == IME_ERR_OK && info) {
        auto rect = inputGeometry.CursorRect(caret);
        OH_CursorInfo_SetRect(info, rect[0], rect[1], rect[2], rect[3]);
    }
}
void Insert(InputMethod_TextEditorProxy*, const char16_t* text, size_t length) {
#ifdef FEIHONG_DEBUG_TESTS
    TraceIme("InputMethod Insert callback");
#endif
    feihong_ime_insert(reinterpret_cast<const uint16_t*>(text), length);
}
void DeleteForward(InputMethod_TextEditorProxy*, int32_t length) { feihong_ime_delete(true, length); }
void DeleteBackward(InputMethod_TextEditorProxy*, int32_t length) { feihong_ime_delete(false, length); }
void KeyboardStatus(InputMethod_TextEditorProxy*, InputMethod_KeyboardStatus status) {
    // Hide/Show/Detach can call this synchronously while holding the lifecycle
    // mutex, so callbacks must never acquire that mutex.
    if (status == IME_KEYBOARD_STATUS_SHOW) keyboardVisible.store(true);
    else if (status == IME_KEYBOARD_STATUS_HIDE) keyboardVisible.store(false);
}
void Enter(InputMethod_TextEditorProxy*, InputMethod_EnterKeyType) {
    std::lock_guard<std::mutex> lock(keyMutex);
    EmitKey(true, "\n"); EmitKey(false, "\n");
}
void Move(InputMethod_TextEditorProxy*, InputMethod_Direction direction) {
    std::lock_guard<std::mutex> lock(keyMutex);
    const char* key = direction == IME_DIRECTION_LEFT ? "\xef\x9c\x82" :
        direction == IME_DIRECTION_RIGHT ? "\xef\x9c\x83" :
        direction == IME_DIRECTION_UP ? "\xef\x9c\x80" : "\xef\x9c\x81";
    feihong_key(true, key); feihong_key(false, key);
}
void Selection(InputMethod_TextEditorProxy*, int32_t start, int32_t end) {
    std::lock_guard<std::mutex> lock(textMutex);
    feihong_ime_selection(start + textOffset, end + textOffset);
}
void Extend(InputMethod_TextEditorProxy*, InputMethod_ExtendAction action) {
    const char* key = action == IME_EXTEND_ACTION_SELECT_ALL ? "a" :
        action == IME_EXTEND_ACTION_CUT ? "x" : action == IME_EXTEND_ACTION_COPY ? "c" :
        action == IME_EXTEND_ACTION_PASTE ? "v" : nullptr;
    if (!key) return;
    std::lock_guard<std::mutex> lock(keyMutex);
    keyboard.ControlAction(key, EmitKey);
}
void LeftText(InputMethod_TextEditorProxy*, int32_t number, char16_t text[], size_t* length) {
    if (!length || !text) return;
    std::lock_guard<std::mutex> lock(textMutex);
    size_t end = std::min(static_cast<size_t>(std::max(cursor, 0)), surrounding.size());
    size_t start = end - std::min(end, static_cast<size_t>(std::max(number, 0)));
    if (start < end && surrounding[start] >= 0xdc00 && surrounding[start] <= 0xdfff) ++start;
    size_t count = std::min(*length, end - start);
    if (count && surrounding[start + count - 1] >= 0xd800 && surrounding[start + count - 1] <= 0xdbff) --count;
    std::copy_n(surrounding.data() + start, count, text); *length = count;
}
void RightText(InputMethod_TextEditorProxy*, int32_t number, char16_t text[], size_t* length) {
    if (!length || !text) return;
    std::lock_guard<std::mutex> lock(textMutex);
    size_t start = std::min(static_cast<size_t>(std::max(cursor, 0)), surrounding.size());
    size_t count = std::min({*length, surrounding.size() - start, static_cast<size_t>(std::max(number, 0))});
    if (count && surrounding[start + count - 1] >= 0xd800 && surrounding[start + count - 1] <= 0xdbff) --count;
    std::copy_n(surrounding.data() + start, count, text); *length = count;
}
int32_t CursorIndex(InputMethod_TextEditorProxy*) { std::lock_guard<std::mutex> lock(textMutex); return cursor; }
int32_t PrivateCommand(InputMethod_TextEditorProxy*, InputMethod_PrivateCommand*[], size_t) {
    // Attach validates every mandatory callback, including this one. Slint
    // has no input-method-specific command protocol or retained native data.
    return IME_ERR_OK;
}
int32_t Preview(InputMethod_TextEditorProxy*, const char16_t* text, size_t length, int32_t start, int32_t end) {
    std::lock_guard<std::mutex> lock(textMutex);
#ifdef FEIHONG_DEBUG_TESTS
    const char* rangeMode = start == -1 && end == -1 ? "default" :
        start >= 0 && end >= start ? "explicit" : "invalid";
    TraceImeLocked(std::string("InputMethod Preview callback range=") + rangeMode);
#endif
    if (start >= 0) start += textOffset;
    if (end >= 0) end += textOffset;
    feihong_ime_preview(reinterpret_cast<const uint16_t*>(text), length, start, end); return 0;
}
void FinishPreview(InputMethod_TextEditorProxy*) {
#ifdef FEIHONG_DEBUG_TESTS
    TraceIme("InputMethod FinishPreview callback");
#endif
    feihong_ime_finish_preview();
}

void CreateEditor() {
    if (editor) return;
    editor = OH_TextEditorProxy_Create();
    if (!editor) return;
    OH_TextEditorProxy_SetGetTextConfigFunc(editor, GetConfig);
    OH_TextEditorProxy_SetInsertTextFunc(editor, Insert);
    OH_TextEditorProxy_SetDeleteForwardFunc(editor, DeleteForward);
    OH_TextEditorProxy_SetDeleteBackwardFunc(editor, DeleteBackward);
    OH_TextEditorProxy_SetSendKeyboardStatusFunc(editor, KeyboardStatus);
    OH_TextEditorProxy_SetSendEnterKeyFunc(editor, Enter);
    OH_TextEditorProxy_SetMoveCursorFunc(editor, Move);
    OH_TextEditorProxy_SetHandleSetSelectionFunc(editor, Selection);
    OH_TextEditorProxy_SetHandleExtendActionFunc(editor, Extend);
    OH_TextEditorProxy_SetGetLeftTextOfCursorFunc(editor, LeftText);
    OH_TextEditorProxy_SetGetRightTextOfCursorFunc(editor, RightText);
    OH_TextEditorProxy_SetGetTextIndexAtCursorFunc(editor, CursorIndex);
    OH_TextEditorProxy_SetReceivePrivateCommandFunc(editor, PrivateCommand);
    OH_TextEditorProxy_SetSetPreviewTextFunc(editor, Preview);
    OH_TextEditorProxy_SetFinishTextPreviewFunc(editor, FinishPreview);
}

bool AttachEditor() {
    CreateEditor(); if (!editor) return false;
    auto* options = OH_AttachOptions_Create(true);
    if (!options) { feihong_native_log("InputMethod attach options unavailable"); return false; }
    keyboardVisible.store(false);
    auto status = OH_InputMethodController_Attach(editor, options, &inputMethod);
    OH_AttachOptions_Destroy(options);
#ifdef FEIHONG_DEBUG_TESTS
    {
        std::lock_guard<std::mutex> lock(textMutex);
        if (status == IME_ERR_OK) imeTraceBudget = 64;
        TraceImeLocked("InputMethod Attach status=" + std::to_string(status));
    }
#endif
    if (status != IME_ERR_OK) {
        keyboardVisible.store(false);
        inputMethod = nullptr;
        feihong_native_log(("InputMethod attach failed: status=" + std::to_string(status)).c_str());
        return false;
    }
    return true;
}
void NotifyCursor(const std::array<double, 4>& rect) {
    if (!inputMethod) return;
    auto* info = OH_CursorInfo_Create(rect[0], rect[1], rect[2], rect[3]);
    if (info) { OH_InputMethodProxy_NotifyCursorUpdate(inputMethod, info); OH_CursorInfo_Destroy(info); }
}

std::string Utf8(uint32_t cp) {
    if (cp < 128) return std::string(1, static_cast<char>(cp));
    return {static_cast<char>(0xe0 | (cp >> 12)), static_cast<char>(0x80 | ((cp >> 6) & 63)), static_cast<char>(0x80 | (cp & 63))};
}
std::string KeyText(int code, bool shifted, bool controlled) {
    if (code >= 2017 && code <= 2042) return std::string(1, static_cast<char>((shifted && !controlled ? 'A' : 'a') + code - 2017));
    if (code >= 2000 && code <= 2009) return std::string(1, shifted ? ")!@#$%^&*("[code - 2000] : '0' + code - 2000);
    switch (code) {
    case 2012: return Utf8(0xf700); case 2013: return Utf8(0xf701);
    case 2014: return Utf8(0xf702); case 2015: return Utf8(0xf703);
    case 2045: return "\x12"; case 2046: return "\x13";
    case 2047: return "\x10"; case 2048: return "\x15";
    case 2049: return "\t"; case 2050: return " "; case 2054: return "\n"; case 2055: return "\b";
    case 2056: return shifted ? "~" : "`"; case 2057: return shifted ? "_" : "-";
    case 2058: return shifted ? "+" : "="; case 2059: return shifted ? "{" : "[";
    case 2060: return shifted ? "}" : "]"; case 2061: return shifted ? "|" : "\\";
    case 2062: return shifted ? ":" : ";"; case 2063: return shifted ? "\"" : "'";
    case 2064: return shifted ? "?" : "/"; case 2043: return shifted ? "<" : ","; case 2044: return shifted ? ">" : ".";
    case 2068: return Utf8(0xf72c); case 2069: return Utf8(0xf72d); case 2070: return "\x1b";
    case 2071: return "\x7f"; case 2072: return "\x11"; case 2073: return "\x16";
    case 2076: return "\x17"; case 2077: return "\x18";
    case 2081: return Utf8(0xf729); case 2082: return Utf8(0xf72b); case 2083: return Utf8(0xf727);
    default: if (code >= 2090 && code <= 2101) return Utf8(0xf704 + code - 2090); return {};
    }
}
bool Key(OH_NativeXComponent* component, void*) {
    OH_NativeXComponent_KeyEvent* event = nullptr;
    OH_NativeXComponent_KeyAction action; OH_NativeXComponent_KeyCode code;
    if (OH_NativeXComponent_GetKeyEvent(component, &event) != 0 || !event ||
        OH_NativeXComponent_GetKeyEventAction(event, &action) != 0 || OH_NativeXComponent_GetKeyEventCode(event, &code) != 0) return false;
    bool pressed = static_cast<int>(action) == 0;
    int value = static_cast<int>(code);
    std::lock_guard<std::mutex> lock(keyMutex);
    if (feihong::IsModifierKey(value)) {
        if (keyboard.HasPreIme()) return false;
        auto next = keyboard.Modifiers();
        if (value == 2047 || value == 2048) next.shift = pressed;
        if (value == 2072 || value == 2073) next.ctrl = pressed;
        if (value == 2045 || value == 2046) next.alt = pressed;
        if (value == 2076 || value == 2077) next.meta = pressed;
        keyboard.Sync(next, EmitKey);
        return true;
    }
    const auto mods = keyboard.Modifiers();
    auto text = KeyText(value, mods.shift, mods.ctrl);
    if (text.empty()) return false;
    // Native XComponent key delivery follows the system input-method filter:
    // consumed composition keys never reach here. Forward its unconsumed
    // ASCII/edit/navigation fallback, including while an editor is attached.
    // Shortcuts are intercepted separately by Stage's preIME handler.
    feihong_key(pressed, text.c_str()); return true;
}
void Focus(OH_NativeXComponent*, void*) { feihong_focus(true); }
void Blur(OH_NativeXComponent*, void*) {
    feihong::ResetInputKeys();
    feihong_focus(false);
}
void Mouse(OH_NativeXComponent* component, void* window) {
    OH_NativeXComponent_MouseEvent event{};
    if (OH_NativeXComponent_GetMouseEvent(component, window, &event) != 0) return;
    int kind = event.action == OH_NATIVEXCOMPONENT_MOUSE_PRESS ? 0 : event.action == OH_NATIVEXCOMPONENT_MOUSE_RELEASE ? 1 : 2;
    int button = event.button == OH_NATIVEXCOMPONENT_RIGHT_BUTTON ? 1 : event.button == OH_NATIVEXCOMPONENT_MIDDLE_BUTTON ? 2 : 0;
    if (kind != 2) feihong::RecordInputBoundary(kind);
    // This API reports vp; the ABI uses physical pixels consistently.
    feihong_pointer(kind, event.x * feihong::Density(), event.y * feihong::Density(), button);
}
void Hover(OH_NativeXComponent*, bool hovered) { if (!hovered) feihong_pointer(3, 0, 0, 0); }
void Axis(OH_NativeXComponent*, ArkUI_UIInputEvent* event, ArkUI_UIInputEvent_Type) {
    if (!event) return;
    float dx = static_cast<float>(-OH_ArkUI_AxisEvent_GetHorizontalAxisValue(event));
    float dy = static_cast<float>(-OH_ArkUI_AxisEvent_GetVerticalAxisValue(event));
    if (OH_ArkUI_UIInputEvent_GetToolType(event) == UI_INPUT_EVENT_TOOL_TYPE_MOUSE) {
        // Match ArkUI's desktop convention: 15 wheel degrees = 21vp. The
        // reported angle already includes the user's scroll-step preference.
        float factor = 21.0f / 15.0f * feihong::Density();
        dx *= factor; dy *= factor;
        { std::lock_guard<std::mutex> lock(keyMutex);
          if (keyboard.Modifiers().shift) std::swap(dx, dy); }
    }
    feihong_scroll(OH_ArkUI_PointerEvent_GetX(event), OH_ArkUI_PointerEvent_GetY(event),
        dx, dy);
}
}

namespace feihong {
bool HideInputKeyboardForBack() {
    if (!keyboardVisible.load()) return false;
    std::lock_guard<std::mutex> lifecycle(inputMethodMutex);
    if (!inputMethod || !keyboardVisible.load()) return false;
    const auto status = OH_InputMethodProxy_HideKeyboard(inputMethod);
    if (status == IME_ERR_OK) keyboardVisible.store(false);
    else feihong_native_log(("InputMethod hide failed: status=" + std::to_string(status)).c_str());
    // Keep the editor attached and consume this Back even if the service fails
    // to hide it. A later Slint Enable (pointer click) can show it again.
    return true;
}
bool HandleShortcut(int32_t keyCode, bool pressed, bool ctrl, bool shift, bool alt, bool meta) {
    std::lock_guard<std::mutex> lock(keyMutex);
    return keyboard.Shortcut(keyCode, pressed, {ctrl, shift, alt, meta},
        KeyText(keyCode, shift, ctrl), EmitKey);
}
void ResetInputKeys() {
    std::lock_guard<std::mutex> lock(keyMutex);
    keyboard.Reset(EmitKey);
}
void SetInputGeometry(int32_t windowId, double screenX, double screenY) {
    std::lock_guard<std::mutex> lifecycle(inputMethodMutex);
    bool changedWindow;
    std::array<double, 4> rect;
    {
        std::lock_guard<std::mutex> snapshot(textMutex);
        changedWindow = inputGeometry.windowId != windowId;
        inputGeometry = {windowId, screenX, screenY};
        rect = inputGeometry.CursorRect(caret);
    }
    if (windowId == 0) {
        // Stage clears geometry while closing, before Rust's queued Suspend.
        // Never reattach the old editor to a default/unknown window.
        editorEnabled = false;
        ResetInputKeys();
        keyboardVisible.store(false);
        if (inputMethod) {
            OH_InputMethodProxy_HideKeyboard(inputMethod);
            OH_InputMethodController_Detach(inputMethod); inputMethod = nullptr;
        }
        return;
    }
    if (inputMethod && changedWindow) {
        keyboardVisible.store(false);
        OH_InputMethodController_Detach(inputMethod); inputMethod = nullptr;
    }
    if (editorEnabled && !inputMethod && !AttachEditor()) return;
    // Window moves need a fresh absolute caret even if no text/cursor changed.
    NotifyCursor(rect);
}
void DispatchTouch(OH_NativeXComponent* component, void* window) {
    OH_NativeXComponent_TouchEvent event{};
    if (OH_NativeXComponent_GetTouchEvent(component, window, &event) != 0) return;
    if (event.type == OH_NATIVEXCOMPONENT_DOWN || event.type == OH_NATIVEXCOMPONENT_UP)
        RecordInputBoundary(event.type == OH_NATIVEXCOMPONENT_DOWN ? 0 : 1);
    // XComponent touch coordinates are physical pixels. Preserve finger ids so
    // Slint can synthesize scrolling, pinch and cancellation correctly.
    feihong_touch(static_cast<int>(event.type), event.id, event.x, event.y);
}
void RegisterInput(OH_NativeXComponent* component) {
    static OH_NativeXComponent_MouseEvent_Callback mouse{Mouse, Hover};
    OH_NativeXComponent_RegisterMouseEventCallback(component, &mouse);
    OH_NativeXComponent_RegisterKeyEventCallbackWithResult(component, Key);
    OH_NativeXComponent_RegisterFocusEventCallback(component, Focus);
    OH_NativeXComponent_RegisterBlurEventCallback(component, Blur);
    OH_NativeXComponent_RegisterUIInputEventCallback(component, Axis, ARKUI_UIINPUTEVENT_TYPE_AXIS);
}
}

extern "C" void feihong_native_ime_update(bool enabled, const uint16_t* text, size_t length,
    int32_t newCursor, int32_t newAnchor, float x, float y, float width, float height, int32_t type) {
    // Editor state comes exclusively from the Rust UI thread. Stage may update
    // geometry; serialize proxy calls while callbacks use the snapshot lock.
    std::lock_guard<std::mutex> lifecycle(inputMethodMutex);
    std::u16string snapshot;
    std::array<double, 4> screenCaret;
    int snapshotCursor, snapshotAnchor;
    {
        std::lock_guard<std::mutex> lock(textMutex);
        // NotifySelectionChange accepts at most 8K UTF16 units. Keep a bounded
        // window around the caret and translate IME ranges back to full text.
        size_t position = std::min(length, static_cast<size_t>(std::max(newCursor, 0)));
        size_t start = position > 4096 ? position - 4096 : 0;
        size_t end = std::min(length, start + 8192);
        if (start < length && text[start] >= 0xdc00 && text[start] <= 0xdfff) ++start;
        if (end < length && end > start && text[end - 1] >= 0xd800 && text[end - 1] <= 0xdbff) --end;
        surrounding.assign(reinterpret_cast<const char16_t*>(text) + start, end - start);
        textOffset = static_cast<int>(start);
        cursor = std::clamp(newCursor - textOffset, 0, static_cast<int>(surrounding.size()));
        anchor = std::clamp(newAnchor - textOffset, 0, static_cast<int>(surrounding.size()));
        snapshot = surrounding; snapshotCursor = cursor; snapshotAnchor = anchor;
        caret = {x, y, width, height}; inputType = type;
        screenCaret = inputGeometry.CursorRect(caret);
    }
    editorEnabled = enabled;
    if (!enabled || inputGeometry.windowId == 0) {
        keyboardVisible.store(false);
        if (inputMethod) { OH_InputMethodProxy_HideKeyboard(inputMethod); OH_InputMethodController_Detach(inputMethod); inputMethod = nullptr; }
        return;
    }
    if (!inputMethod && !AttachEditor()) return;
    OH_InputMethodProxy_NotifySelectionChange(inputMethod, snapshot.data(), snapshot.size(), snapshotAnchor, snapshotCursor);
    OH_InputMethodProxy_NotifyConfigurationChange(inputMethod, IME_ENTER_KEY_UNSPECIFIED, static_cast<InputMethod_TextInputType>(type));
    NotifyCursor(screenCaret);
}

extern "C" void feihong_native_ime_show() {
    // Slint distinguishes an explicit Enable from text/caret Update. A user
    // may hide the keyboard without losing focus, then click the same editor.
    std::lock_guard<std::mutex> lifecycle(inputMethodMutex);
    if (!editorEnabled || !inputMethod || keyboardVisible.load()) return;
    const auto status = OH_InputMethodProxy_ShowKeyboard(inputMethod);
    if (status != IME_ERR_OK)
        feihong_native_log(("InputMethod show failed: status=" + std::to_string(status)).c_str());
}
