# Minimal OHOS Vulkan WSI patch

Source: `wgpu-hal` 29.0.3, downloaded from
https://crates.io/api/v1/crates/wgpu-hal/29.0.3/download.
Original archive SHA-256:
`31f8e1a9e7a8512f276f7c62e018c7fa8d60954303fed2e5750114332049193f`.
MIT/Apache-2.0 upstream licenses are retained.

The only behavioral changes are under `target_env = "ohos"`:

- Load the platform Vulkan loader `libvulkan.so`.
- Enable `VK_OHOS_surface` instead of Unix desktop window/DRM extensions.
- Resolve `vkCreateSurfaceOHOS` using the instance dispatch table and create a
  surface with the exact `VkSurfaceCreateInfoOHOS` layout from SDK26's
  `vulkan/vulkan_ohos.h` (extension available since API10).
- Use raw-window-handle 0.6.2's native `OhosNdkWindowHandle` and
  `OhosDisplayHandle`, carrying the real `OHNativeWindow*` from XComponent.
  Android handle behavior remains unchanged.

The native host must retain the window for the complete Vulkan surface lifetime
and reject a stale generation/background surface before presenting. The CPU
renderer stays the default. This source patch alone is not a GPU feature or
device validation claim; Cargo opt-in, a Skia29 renderer and device tests are
required before enabling it.

The standalone ARM64 Vulkan-only source check passed on 2026-10-10 using SDK26:

```sh
CARGO_TARGET_DIR=/private/tmp/feihong-wgpu29-check CARGO_INCREMENTAL=0 \
  cargo check --manifest-path crates/wgpu-hal-ohos29/Cargo.toml --lib \
  --target aarch64-unknown-linux-ohos --no-default-features \
  --features vulkan --locked --offline
```

Independent C assertions against the SDK header and Rust assertions using this
structure and actual ash types both verified size 32, alignment 8, field offsets
0/8/16/24, 32-bit tags/flags and structure type 1000685000. The SDK function
pointer signature also matched `vkCreateSurfaceOHOS`. This checks compilation
and ABI layout only; no Vulkan instance, device or frame was created.
