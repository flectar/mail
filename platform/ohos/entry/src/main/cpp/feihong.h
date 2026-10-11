#pragma once
#include <cstddef>
#include <cstdint>
extern "C" {
int32_t feihong_start(const char* options);
void feihong_set_viewport(uint32_t width, uint32_t height, float density, const char* insets);
void feihong_surface(uint64_t generation, bool available);
void feihong_vsync();
void feihong_set_foreground(bool foreground);
void feihong_focus(bool focused);
void feihong_stop();
bool feihong_handle_back();
void feihong_pointer(int32_t kind, float x, float y, int32_t button);
void feihong_touch(int32_t kind, int32_t id, float x, float y);
void feihong_scroll(float x, float y, float dx, float dy);
void feihong_key(bool pressed, const char* text);
void feihong_ime_insert(const uint16_t* text, size_t length);
void feihong_ime_preview(const uint16_t* text, size_t length, int32_t start, int32_t end);
void feihong_ime_finish_preview();
void feihong_ime_selection(int32_t start, int32_t end);
void feihong_ime_delete(bool forward, int32_t units);
void feihong_complete_service_request(uint64_t id, bool ok, const char* payload);
int32_t feihong_run_deferred_sync(const char* options);
void feihong_cancel_deferred_sync();
void feihong_host_request(uint64_t id, const char* kind, const char* payload);
void feihong_native_log(const char* message);
}
