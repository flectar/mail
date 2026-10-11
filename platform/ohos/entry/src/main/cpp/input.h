#pragma once
#include "input_geometry.h"
#include <ace/xcomponent/native_interface_xcomponent.h>
namespace feihong {
void RegisterInput(OH_NativeXComponent* component);
void DispatchTouch(OH_NativeXComponent* component, void* window);
void SetInputGeometry(int32_t windowId, double screenX, double screenY);
bool HandleShortcut(int32_t keyCode, bool pressed, bool ctrl, bool shift, bool alt, bool meta);
void ResetInputKeys();
bool HideInputKeyboardForBack();
}
