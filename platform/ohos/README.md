# OpenHarmony platform

`platform/ohos` hosts the OpenHarmony port: a small Slint platform backend
that renders into an ArkTS `XComponent` surface, the NAPI cdylibs that host
it, and the ArkTS hap shell used to build and install both the backend demo
and the mail application. It mirrors `platform/android`; the shared UI and
application code stay at the repository root.

## Layout

| Path | What it is |
|------|------------|
| `backend/` | `slint-ohos-backend`: Slint `Platform`/`WindowAdapter` for a single XComponent window. Software renderer by default, experimental femtovg/wgpu GPU path, input translation, IME hooks, clipboard, file dialogs, system fonts. |
| `mail/` | `flectar-mail-ohos` cdylib: NAPI module that starts the application and exposes the ArkTS bridge (file pickers, clipboard, IME text, notifications, dynamic exports). |
| `demo/` | `slint-ohos-demo`: minimal cdylib used as a smoke target for backend changes. |
| `shell/` | DevEco shell project (ArkTS). `page-templates/Index.mail.ets` is the real app page and `page-templates/Index.demo.ets` the demo page; `build-hap.sh` copies the selected template to `shell/entry/src/main/ets/pages/Index.ets`. |
| `scripts/env.sh` | Environment: SDK paths, `hdc`, `hap-sign-tool`, `LD_LIBRARY_PATH` for lld. Source it before any `cargo` command in this workspace. |
| `scripts/build-hap.sh` | End-to-end build: Rust cdylib → ArkTS `modules.abc` → hap staging → re-packed resources → signing. `APP=mail` or `APP=demo`. |
| `scripts/run-device.sh` | Installs the built hap, starts it and streams filtered logs (demo by default). |
| `scripts/cfg-ohos-align.py` | Mechanical helper that adds `target_env = "ohos"` to the platform cfg gates (M3 convergence). |
| `scripts/fetch-pdfium.sh` | Fetches the prebuilt `libpdfium.so` used by the PDF preview. |
| `prebuilt/arm64/` | Prebuilt native libraries that ride inside the hap (signature covers `libs/**`). |

## Prerequisites

All paths are the ones this workspace uses by default; override through the
environment variables understood by `scripts/env.sh`.

- HarmonyOS SDK root (`OHOS_SDK_ROOT`, default `~/.harmonybrew/opt/ohos-sdk`)
  plus its native subdirectory (`OHOS_SDK_NATIVE`).
- `hdc` and a DevEco-created signing material directory
  (`$HOME/Documents/ohos/config`, bound to the device UDID). The store
  passphrase is fetched at build time by the local `sign-huawei.sh` helper and
  is never stored in this repository.
- `node` with hvigor installed (`$HOME/arkts-build/node_modules/@ohos/hvigor`),
  and OpenJDK 17 (`$HOME/.harmonybrew/opt/openjdk@17`).
- The Rust `aarch64-unknown-linux-ohos` target (built natively on the device).

## Build and install

```sh
. platform/ohos/scripts/env.sh
APP=mail PROFILE=release CARGO_BUILD_JOBS=4 sh platform/ohos/scripts/build-hap.sh

hdc file send platform/ohos/build/flectar_mail_ohos-signed.hap /data/local/tmp/flectar.hap
hdc shell "bm install -r -d -p /data/local/tmp/flectar.hap"
hdc shell "aa start -a EntryAbility -b com.example.myapplication"
```

`bm install -r -d` is deliberate: `hdc install` rejects the hap ("install
version downgrade") because the shared bundle name carries the DevEco
project's version code.

## Why the hap is re-packed by hand

DevEco's `PackageHap` step needs the packing-tool jar, so `build-hap.sh`
builds the payload with hvigor only up to `modules.abc` and assembles the hap
itself. Three device-side quirks make the hand packing necessary — they are
handled (and asserted) in the script, do not "simplify" them away:

1. **Resources index.** hvigor's resource compile passes a `compression`
   option and emits a legacy `resources.index` this device cannot load; every
   `$string`/`$media` reference silently resolves to nothing (blank icon, app
   name replaced by the bundle name). The script re-runs restool without
   `compression` to obtain the `RestoolV2` index DevEco-built haps carry.
2. **Zip layout.** `resources.index` and `resources/**` must be STORED, and
   the archive must not contain directory entries (the installer rejects
   them).
3. **Resolved ids and module fields.** `labelId`/`iconId`/... are normally
   resolved from `$string:`/`$media:` references by `PackageHap`; the script
   injects them from the compiler's `id_defined.json`, together with the
   device types (2in1/tablet/phone), `ohos.permission.INTERNET` (without it
   every socket fails with EPERM) and the HarmonyOS 6.1 SDK coordinates.
   Notifications need no manifest permission: the app asks the user through
   `notificationManager.requestEnableNotification` and publishes through
   `@kit.NotificationKit`.

## Runtime notes

- **Logs**: `hdc shell "hilog -T slint-ohos,flectar-mail"` for the backend and
  application, `-T JSAPP` for the ArkTS console.
- **GPU rendering**: the femtovg/wgpu path (EGL/GLES) is attempted by default
  and falls back to the software renderer automatically — before the surface
  exists, after a failed attach, and permanently once an attach failed. Local
  runs can opt out with `FLECTAR_OHOS_SOFTWARE=1`; the device build has no
  environment, so the fallback is the safety net. The wgpu GL-backend
  subtleties (why `Backends::GL` must be requested explicitly) are documented
  in the porting notes.
- **Soft keyboard**: the page attaches the OHOS input-method controller when
  Slint requests the input method. On 2in1 devices with a physical keyboard
  the system suppresses the soft keyboard (system applications included)
  unless it is explicitly summoned; the built-in IME uses `Alt+Shift`.
- **Input**: XComponent touch events drive Slint's pointer handling; physical
  keyboard events are translated (key codes to text, modifiers, auto-repeat)
  in `backend/src/xcomponent.rs`.
- **Data**: the application keeps its data under the standard bundle data
  path; database snapshots export through the system save dialog.

## Tests

The backend has no automated device-side harness: `cargo test` covers the
shared crates at the repository root, and `APP=demo` (installed with
`scripts/run-device.sh`) is the manual smoke target for backend changes.
