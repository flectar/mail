#include "feihong.h"
#include "graphics.h"
#include "input.h"
#include <napi/native_api.h>
#include <hilog/log.h>
#include <database/pasteboard/oh_pasteboard.h>
#include <database/udmf/udmf.h>
#include <database/udmf/uds.h>
#include <memory>
#include <mutex>
#include <string>
#include <vector>
#include <cstring>
#include <limits>
#include <algorithm>
#include <cmath>

#ifdef FEIHONG_DEBUG_TESTS
extern "C" int feihong_pdfium_probe(char* report, size_t capacity);
#endif

namespace {
constexpr size_t MAX_TEXT = 16 * 1024 * 1024;
std::mutex handlerMutex;
napi_threadsafe_function serviceHandler = nullptr;
struct Request { uint64_t id; std::string kind, payload; };
struct HandlerLifetime { napi_threadsafe_function handler; napi_env env; bool foreground; };
std::vector<HandlerLifetime*> handlers;
#ifdef FEIHONG_DEBUG_TESTS
std::mutex pdfiumLifecycleMutex;
bool uiHasStarted = false; // The retained Rust owner thread never resets this.
bool pdfiumProbeRunning = false;
#endif
void SelectHandler() {
    serviceHandler = handlers.empty() ? nullptr : handlers.back()->handler;
    for (auto it = handlers.rbegin(); it != handlers.rend(); ++it) {
        if ((*it)->foreground) { serviceHandler = (*it)->handler; break; }
    }
}
void SetHandlerForeground(napi_env env, bool value) {
    std::lock_guard<std::mutex> lock(handlerMutex);
    HandlerLifetime* latest = nullptr;
    for (auto* handler : handlers) {
        if (handler->env == env) { handler->foreground = false; latest = handler; }
    }
    if (latest) latest->foreground = value;
    SelectHandler();
}

napi_value Undefined(napi_env env) { napi_value value; napi_get_undefined(env, &value); return value; }
bool Text(napi_env env, napi_value value, std::string& output) {
    size_t size = 0;
    if (napi_get_value_string_utf8(env, value, nullptr, 0, &size) != napi_ok || size > MAX_TEXT) return false;
    std::vector<char> data(size + 1); size_t copied = 0;
    if (napi_get_value_string_utf8(env, value, data.data(), data.size(), &copied) != napi_ok) return false;
    output.assign(data.data(), copied); return true;
}
napi_value Error(napi_env env, const char* message) { napi_throw_error(env, nullptr, message); return nullptr; }
void CallService(napi_env env, napi_value callback, void*, void* data) {
    std::unique_ptr<Request> request(static_cast<Request*>(data));
    if (!env || !callback) {
        feihong_complete_service_request(request->id, false, "{\"error\":\"host stopped\"}"); return;
    }
    napi_value argv[3], result;
    napi_create_double(env, static_cast<double>(request->id), &argv[0]);
    napi_create_string_utf8(env, request->kind.c_str(), request->kind.size(), &argv[1]);
    napi_create_string_utf8(env, request->payload.c_str(), request->payload.size(), &argv[2]);
    napi_status status = napi_call_function(env, Undefined(env), callback, 3, argv, &result);
    if (status != napi_ok) {
        // A throwing handler must not strand Rust's pending service request.
        bool pending = false; napi_is_exception_pending(env, &pending);
        if (pending) { napi_value exception; napi_get_and_clear_last_exception(env, &exception); }
        feihong_complete_service_request(request->id, false, "{\"error\":\"host handler failed\"}");
    }
}
void CleanupHandler(void* data) {
    std::unique_ptr<HandlerLifetime> lifetime(static_cast<HandlerLifetime*>(data));
    std::lock_guard<std::mutex> lock(handlerMutex);
    handlers.erase(std::remove(handlers.begin(), handlers.end(), lifetime.get()), handlers.end());
    SelectHandler();
    napi_release_threadsafe_function(lifetime->handler, napi_tsfn_abort);
}
napi_value SetServiceHandler(napi_env env, napi_callback_info info) {
    size_t argc = 1; napi_value argv[1]; napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
    napi_valuetype type;
    if (argc != 1 || napi_typeof(env, argv[0], &type) != napi_ok || type != napi_function)
        return Error(env, "setServiceHandler expects a function");
    napi_value name; napi_create_string_utf8(env, "Feihong services", NAPI_AUTO_LENGTH, &name);
    napi_threadsafe_function handler = nullptr;
    if (napi_create_threadsafe_function(env, argv[0], nullptr, name, 256, 1, nullptr, nullptr,
        nullptr, CallService, &handler) != napi_ok) return Error(env, "cannot create service handler");
    napi_unref_threadsafe_function(env, handler);
    auto* lifetime = new HandlerLifetime{handler, env, false};
    if (napi_add_env_cleanup_hook(env, CleanupHandler, lifetime) != napi_ok) {
        napi_release_threadsafe_function(handler, napi_tsfn_abort); delete lifetime;
        return Error(env, "cannot retain service handler");
    }
    // A WorkScheduler environment must not redirect requests from an active
    // foreground window. Each environment owns its TSFN until its own cleanup.
    std::vector<HandlerLifetime*> retired;
    {
        std::lock_guard<std::mutex> lock(handlerMutex);
        for (auto it = handlers.begin(); it != handlers.end();) {
            auto* previous = *it;
            if (previous->env != env) { ++it; continue; }
            lifetime->foreground = lifetime->foreground || previous->foreground;
            previous->foreground = false;
            // Ability recreation can register another callback in the same
            // environment. Retire its old strong reference to the ArkTS host.
            if (napi_remove_env_cleanup_hook(env, CleanupHandler, previous) == napi_ok) {
                retired.push_back(previous); it = handlers.erase(it);
            } else ++it;
        }
        handlers.push_back(lifetime); SelectHandler();
    }
    for (auto* previous : retired) {
        napi_release_threadsafe_function(previous->handler, napi_tsfn_abort);
        delete previous;
    }
    return Undefined(env);
}
napi_value CompleteService(napi_env env, napi_callback_info info) {
    size_t argc = 3; napi_value argv[3]; napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
    double id = 0; bool ok = false; std::string payload;
    if (argc != 3 || napi_get_value_double(env, argv[0], &id) != napi_ok || !std::isfinite(id) ||
        id < 0 || id > 9007199254740991. || std::floor(id) != id ||
        napi_get_value_bool(env, argv[1], &ok) != napi_ok || !Text(env, argv[2], payload))
        return Error(env, "invalid service response");
    feihong_complete_service_request(static_cast<uint64_t>(id), ok, payload.c_str()); return Undefined(env);
}
napi_value Start(napi_env env, napi_callback_info info) {
#ifdef FEIHONG_DEBUG_TESTS
    {
        std::lock_guard<std::mutex> lock(pdfiumLifecycleMutex);
        if (pdfiumProbeRunning) return Error(env, "wait for PDFium cold-start self test before start");
        uiHasStarted = true;
    }
#endif
    size_t argc = 1; napi_value argv[1]; napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
    std::string options;
    if (argc != 1 || !Text(env, argv[0], options)) return Error(env, "start expects options JSON");
    if (feihong_start(options.c_str()) != 0) return Error(env, "native startup rejected options");
    SetHandlerForeground(env, true);
    feihong::ResumeSurface(); return Undefined(env);
}
napi_value Viewport(napi_env env, napi_callback_info info) {
    size_t argc = 4; napi_value argv[4]; napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
    uint32_t width, height; double scale; std::string insets;
    if (argc != 4 || napi_get_value_uint32(env, argv[0], &width) != napi_ok ||
        napi_get_value_uint32(env, argv[1], &height) != napi_ok || napi_get_value_double(env, argv[2], &scale) != napi_ok ||
        !Text(env, argv[3], insets)) return Error(env, "invalid viewport");
    feihong::SetDensity(static_cast<float>(scale));
    feihong::SetInsets(insets);
    feihong_set_viewport(width, height, feihong::Density(), insets.c_str()); return Undefined(env);
}
napi_value InputGeometry(napi_env env, napi_callback_info info) {
    size_t argc = 3; napi_value argv[3]; napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
    double windowId = 0, screenX = 0, screenY = 0;
    if (argc != 3 || napi_get_value_double(env, argv[0], &windowId) != napi_ok ||
        !std::isfinite(windowId) || windowId < 0 || windowId > std::numeric_limits<int32_t>::max() ||
        std::floor(windowId) != windowId || napi_get_value_double(env, argv[1], &screenX) != napi_ok ||
        napi_get_value_double(env, argv[2], &screenY) != napi_ok || !std::isfinite(screenX) || !std::isfinite(screenY))
        return Error(env, "setInputGeometry expects a window ID and physical screen coordinates");
    feihong::SetInputGeometry(static_cast<int32_t>(windowId), screenX, screenY); return Undefined(env);
}
napi_value Foreground(napi_env env, napi_callback_info info) {
    size_t argc = 1; napi_value argv[1]; napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr); bool value;
    if (argc != 1 || napi_get_value_bool(env, argv[0], &value) != napi_ok) return Error(env, "foreground expects boolean");
    SetHandlerForeground(env, value);
    if (!value) feihong::ResetInputKeys();
    feihong::SetForeground(value); feihong_set_foreground(value); return Undefined(env);
}
napi_value Stop(napi_env env, napi_callback_info) { SetHandlerForeground(env, false); feihong::ResetInputKeys(); feihong::SetForeground(false); feihong_stop(); return Undefined(env); }
napi_value Shortcut(napi_env env, napi_callback_info info) {
    size_t argc = 6; napi_value argv[6]; napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
    double code = 0; bool pressed = false, ctrl = false, shift = false, alt = false, meta = false;
    if (argc != 6 || napi_get_value_double(env, argv[0], &code) != napi_ok ||
        !std::isfinite(code) || code < 0 || code > std::numeric_limits<int32_t>::max() || std::floor(code) != code ||
        napi_get_value_bool(env, argv[1], &pressed) != napi_ok || napi_get_value_bool(env, argv[2], &ctrl) != napi_ok ||
        napi_get_value_bool(env, argv[3], &shift) != napi_ok || napi_get_value_bool(env, argv[4], &alt) != napi_ok ||
        napi_get_value_bool(env, argv[5], &meta) != napi_ok) return Error(env, "invalid shortcut event");
    napi_value result; napi_get_boolean(env,
        feihong::HandleShortcut(static_cast<int32_t>(code), pressed, ctrl, shift, alt, meta), &result); return result;
}
napi_value Back(napi_env env, napi_callback_info) {
    // Release the input lifecycle lock before waiting for the Rust owner.
    const bool handled = feihong::HideInputKeyboardForBack() || feihong_handle_back();
    napi_value result; napi_get_boolean(env, handled, &result); return result;
}

bool ReadClipboard(std::string& text) {
    OH_Pasteboard* clipboard = OH_Pasteboard_Create(); if (!clipboard) return false;
    int status = 0; OH_UdmfData* data = OH_Pasteboard_GetData(clipboard, &status);
    bool result = false;
    if (data && status == 0) {
        auto* plain = OH_UdsPlainText_Create();
        if (plain && OH_UdmfData_GetPrimaryPlainText(data, plain) == 0) {
            const char* content = OH_UdsPlainText_GetContent(plain);
            if (content && strnlen(content, MAX_TEXT + 1) <= MAX_TEXT) { text = content; result = true; }
        }
        if (plain) OH_UdsPlainText_Destroy(plain);
    }
    if (data) OH_UdmfData_Destroy(data);
    OH_Pasteboard_Destroy(clipboard); return result;
}
bool WriteClipboard(const char* text) {
    auto* clipboard = OH_Pasteboard_Create(); auto* data = OH_UdmfData_Create();
    auto* record = OH_UdmfRecord_Create(); auto* plain = OH_UdsPlainText_Create();
    bool ok = clipboard && data && record && plain &&
        OH_UdsPlainText_SetContent(plain, text) == 0 && OH_UdmfRecord_AddPlainText(record, plain) == 0 &&
        OH_UdmfData_AddRecord(data, record) == 0 && OH_Pasteboard_SetData(clipboard, data) == 0;
    if (plain) OH_UdsPlainText_Destroy(plain); if (record) OH_UdmfRecord_Destroy(record);
    if (data) OH_UdmfData_Destroy(data); if (clipboard) OH_Pasteboard_Destroy(clipboard); return ok;
}
napi_value SetClipboard(napi_env env, napi_callback_info info) {
    size_t argc = 1; napi_value argv[1]; napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr); std::string text;
    if (argc != 1 || !Text(env, argv[0], text)) return Error(env, "clipboard expects text");
    if (!WriteClipboard(text.c_str())) return Error(env, "clipboard write failed"); return Undefined(env);
}
napi_value GetClipboard(napi_env env, napi_callback_info) {
    std::string text; if (!ReadClipboard(text)) return Error(env, "clipboard read unavailable or permission denied");
    napi_value value; napi_create_string_utf8(env, text.c_str(), text.size(), &value); return value;
}

struct Deferred { napi_async_work work; napi_deferred promise; std::string options; int32_t status = -1; };
napi_value DeferredSync(napi_env env, napi_callback_info info) {
    size_t argc = 1; napi_value argv[1]; napi_get_cb_info(env, info, &argc, argv, nullptr, nullptr);
    auto task = std::make_unique<Deferred>();
    if (argc != 1 || !Text(env, argv[0], task->options)) return Error(env, "runDeferredSync expects JSON");
    napi_value promise, name; napi_create_promise(env, &task->promise, &promise);
    napi_create_string_utf8(env, "Feihong deferred sync", NAPI_AUTO_LENGTH, &name);
    napi_status result = napi_create_async_work(env, nullptr, name,
        [](napi_env, void* data) { auto* task = static_cast<Deferred*>(data); task->status = feihong_run_deferred_sync(task->options.c_str()); },
        [](napi_env env, napi_status status, void* data) {
            std::unique_ptr<Deferred> task(static_cast<Deferred*>(data));
            if (status == napi_ok && task->status == 0) napi_resolve_deferred(env, task->promise, Undefined(env));
            else { napi_value message, error; napi_create_string_utf8(env, "deferred sync failed or cancelled", NAPI_AUTO_LENGTH, &message);
                napi_create_error(env, nullptr, message, &error); napi_reject_deferred(env, task->promise, error); }
            napi_delete_async_work(env, task->work);
        }, task.get(), &task->work);
    if (result != napi_ok || napi_queue_async_work(env, task->work) != napi_ok) {
        if (result == napi_ok) napi_delete_async_work(env, task->work);
        return Error(env, "cannot schedule deferred sync");
    }
    task.release(); return promise;
}
napi_value CancelDeferred(napi_env env, napi_callback_info) { feihong_cancel_deferred_sync(); return Undefined(env); }

napi_value PdfiumSelfTest(napi_env env, napi_callback_info) {
    napi_deferred promise; napi_value result;
    if (napi_create_promise(env, &promise, &result) != napi_ok) return Error(env, "cannot create test promise");
#ifdef FEIHONG_DEBUG_TESTS
    {
        std::lock_guard<std::mutex> lock(pdfiumLifecycleMutex);
        if (uiHasStarted || pdfiumProbeRunning) {
            napi_value message, error;
            napi_create_string_utf8(env, "PDFium self test requires cold startup before the Rust UI", NAPI_AUTO_LENGTH, &message);
            napi_create_error(env, nullptr, message, &error);
            napi_reject_deferred(env, promise, error); return result;
        }
        pdfiumProbeRunning = true;
    }
    struct Probe { napi_async_work work = nullptr; napi_deferred promise; char report[512] = {0}; };
    auto task = std::make_unique<Probe>(); task->promise = promise;
    napi_value name; napi_create_string_utf8(env, "Feihong PDFium ABI probe", NAPI_AUTO_LENGTH, &name);
    napi_status status = napi_create_async_work(env, nullptr, name,
        [](napi_env, void* data) { auto* task = static_cast<Probe*>(data);
            feihong_pdfium_probe(task->report, sizeof(task->report)); },
        [](napi_env env, napi_status status, void* data) {
            std::unique_ptr<Probe> task(static_cast<Probe*>(data));
            { std::lock_guard<std::mutex> lock(pdfiumLifecycleMutex); pdfiumProbeRunning = false; }
            const char* report = status == napi_ok ? task->report : "{\"status\":\"cancelled\"}";
            OH_LOG_Print(LOG_APP, LOG_INFO, 0xff01, "Feihong", "PDFium self test: %{public}s", report);
            napi_value value; napi_create_string_utf8(env, report, NAPI_AUTO_LENGTH, &value);
            napi_resolve_deferred(env, task->promise, value);
            napi_delete_async_work(env, task->work);
        }, task.get(), &task->work);
    if (status != napi_ok || napi_queue_async_work(env, task->work) != napi_ok) {
        if (task->work) napi_delete_async_work(env, task->work);
        { std::lock_guard<std::mutex> lock(pdfiumLifecycleMutex); pdfiumProbeRunning = false; }
        return Error(env, "cannot schedule PDFium ABI probe");
    }
    task.release();
#else
    napi_value value; napi_create_string_utf8(env, "disabled", NAPI_AUTO_LENGTH, &value);
    napi_resolve_deferred(env, promise, value);
#endif
    return result;
}

napi_value Init(napi_env env, napi_value exports) {
    const napi_property_descriptor properties[] = {
        {"start", nullptr, Start, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"setViewport", nullptr, Viewport, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"setInputGeometry", nullptr, InputGeometry, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"handleShortcut", nullptr, Shortcut, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"setForeground", nullptr, Foreground, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"stop", nullptr, Stop, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"pdfiumSelfTest", nullptr, PdfiumSelfTest, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"setServiceHandler", nullptr, SetServiceHandler, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"completeServiceRequest", nullptr, CompleteService, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"setClipboardText", nullptr, SetClipboard, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"getClipboardText", nullptr, GetClipboard, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"handleBack", nullptr, Back, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"runDeferredSync", nullptr, DeferredSync, nullptr, nullptr, nullptr, napi_default, nullptr},
        {"cancelDeferredSync", nullptr, CancelDeferred, nullptr, nullptr, nullptr, napi_default, nullptr},
    };
    napi_define_properties(env, exports, sizeof(properties) / sizeof(properties[0]), properties);
    bool hasComponent = false; napi_has_named_property(env, exports, OH_NATIVE_XCOMPONENT_OBJ, &hasComponent);
    if (hasComponent) {
        napi_value value; OH_NativeXComponent* component = nullptr;
        if (napi_get_named_property(env, exports, OH_NATIVE_XCOMPONENT_OBJ, &value) == napi_ok &&
            napi_unwrap(env, value, reinterpret_cast<void**>(&component)) == napi_ok && component)
            feihong::RegisterGraphics(component);
    }
    return exports;
}
}

extern "C" void feihong_host_request(uint64_t id, const char* kind, const char* payload) {
    auto request = std::make_unique<Request>(Request{id, kind ? kind : "", payload ? payload : ""});
    napi_status result = napi_closing;
    { std::lock_guard<std::mutex> lock(handlerMutex);
        if (serviceHandler) result = napi_call_threadsafe_function(serviceHandler, request.get(), napi_tsfn_nonblocking); }
    if (result == napi_ok) request.release();
    else feihong_complete_service_request(id, false, "{\"error\":\"host unavailable\"}");
}
extern "C" void feihong_native_log(const char* message) {
    OH_LOG_Print(LOG_APP, LOG_ERROR, 0xff01, "Feihong", "%{public}s", message ? message : "");
}
extern "C" void feihong_native_clipboard_set(const char* text) {
    if (!text || !WriteClipboard(text)) feihong_native_log("Clipboard write failed");
}
extern "C" size_t feihong_native_clipboard_get(char* text, size_t capacity) {
    // A two-call Rust read uses a thread-local snapshot so changing the system
    // clipboard between length/copy cannot overflow or expose unrelated data.
    thread_local std::string snapshot;
    if (!text || capacity == 0) {
        snapshot.clear(); if (!ReadClipboard(snapshot)) return std::numeric_limits<size_t>::max(); return snapshot.size();
    }
    if (snapshot.size() + 1 > capacity) return std::numeric_limits<size_t>::max();
    memcpy(text, snapshot.c_str(), snapshot.size() + 1); size_t length = snapshot.size(); snapshot.clear(); return length;
}

static napi_module module{1, 0, nullptr, Init, "feihong", nullptr, {0}};
extern "C" __attribute__((constructor)) void RegisterFeihong() { napi_module_register(&module); }
