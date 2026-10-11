#pragma once
#include <ace/xcomponent/native_interface_xcomponent.h>
#include <string>
namespace feihong {
void RegisterGraphics(OH_NativeXComponent* component);
void ResumeSurface();
void SetDensity(float density);
void SetInsets(const std::string& json);
float Density();
void SetForeground(bool foreground);
// Numeric action boundary only while explicitly enabled debug diagnostics run.
void RecordInputBoundary(int action);
}
