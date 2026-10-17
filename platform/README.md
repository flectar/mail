# Platform architecture

Flectar Mail is one shared application with thin, per-platform hosts.

| Layer | Location | Contains |
| --- | --- | --- |
| Core | `crates/flectar-mail-core` | Protocols, sync, storage, search. No UI and no host APIs. |
| Application | `src/`, `ui/` | Slint UI, controllers, rendering. Shared by every platform. |
| Platform layer | `src/platform/` | The application's only code that names a mobile operating system. |
| Hosts | `platform/<name>/` | Native entry points, packaging and native services (JNI, Objective-C). |

Desktop builds use the `flectar-mail` binary (`src/desktop.rs`), and the
release workflows package it for Linux, Windows and macOS. Mobile hosts embed
the same library.

## Capability aliases

Shared code never asks which operating system it runs on, only what the
platform provides. `build.rs` and `crates/flectar-mail-core/build.rs` define
these aliases with [`cfg_aliases`](https://crates.io/crates/cfg_aliases):

| Alias | Meaning |
| --- | --- |
| `mobile` | A host-managed sandbox: no file dialogs, tray, GnuPG, desktop renderer choice or system keychain access from the core. |
| `desktop` | `not(mobile)` |
| `linux_desktop` | A Linux desktop session (portals, X11/Wayland, Secret Service). Excludes OpenHarmony, which also reports `target_os = "linux"`. |
| `glibc` | Linux with the GNU C library. |
| `rename_noreplace` | Core only: kernels with an atomic no-replace directory rename. |

Use them like any other `cfg`: `#[cfg(desktop)]`, `cfg!(mobile)`.

Cargo cannot evaluate build-script aliases in `[target.'cfg(...)']` tables, so
the manifests repeat the operating-system lists there with a comment naming the
alias. Keep the two in step.

`scripts/check-platform-gates.py` runs in CI and rejects `target_os =
"android"`, `"ios"` or `target_env = "ohos"` outside the build scripts and
`src/platform/`, and a bare `target_os = "linux"` anywhere in the application.
Windows and macOS conditions are allowed where code really is specific to them,
such as native window chrome or keyboard shortcut conventions.

## Host contract

A host runs the shared application by building a `PlatformContext` and calling
`flectar_mail::run` (or `run_desktop`). The context carries every
host-provided service:

| Field | Desktop | Mobile host |
| --- | --- | --- |
| `paths` | `Paths::default_dirs()` | App-private data and cache directories |
| `documents` | Native file dialogs | Implements `documents::DocumentProvider` with the system picker |
| `credentials` | System keychain (`SystemCredentialStore`) | Implements `CredentialStore` with the platform keystore |
| `oauth_redirects` | Loopback redirect broker | Implements `OAuthRedirectBroker` (app links, custom schemes) |

Mobile hosts use `PlatformContext::app_private(data_root, cache_dir, documents,
credentials, oauth_redirects)`, which keeps durable data below
`data_root/data`. A host without secure storage yet must pass
`UnavailableCredentialStore` so account setup reports the gap. Never fall back
to a plaintext store.

Each mobile operating system also has a module in `src/platform/` that
provides these crate-internal items, which shared code calls without a `cfg`:

| Item | Purpose |
| --- | --- |
| `select_renderer()` | Install the Slint backend if the host has not, and return the renderer name for diagnostics. |
| `pdfium_library()` | Path of the bundled PDFium library. |
| `lower_current_thread_priority()` | Run the PDF worker thread at background priority. |
| `install_preview_lifecycle()` | Register for background/memory-pressure notifications, which must call `flectar_mail::suspend_file_preview()`. |

Hosts that launch through `src/desktop.rs` (desktop and iOS) also provide
`platform::launch_context()`.

### Known gaps

- iOS has no Keychain-backed `CredentialStore` yet and uses
  `UnavailableCredentialStore`.

## Adding a platform

1. Add the target to the `mobile` alias in both build scripts, if it is a
   mobile host, and to the matching `[target.'cfg(...)']` lists in both
   `Cargo.toml` files.
2. Add `src/platform/<name>.rs` with the items above and re-export it from
   `src/platform/mod.rs`.
3. Add `platform/<name>/` with the native entry point, packaging, and the
   native implementations of the `PlatformContext` services. Hosts with their
   own Cargo workspace repeat the root `[patch.crates-io]` entries and use
   `default-features = false`.
4. Run `python3 scripts/check-platform-gates.py` and the lint for the new target.
   If any shared code still needs changes, the platform needs a new
   capability alias, not a new operating-system condition at the call site.
5. Document the build in `platform/<name>/README.md` and add a CI job for the
   platform's support tier.

## Support tiers

| Tier | Platforms | Expectations |
| --- | --- | --- |
| 1 | Linux, Windows, macOS | Release packages. Lint and tests pass on every pull request. |
| 2 | Android, iOS | Experimental builds. Lint passes on every pull request; a broken build blocks merging. |
| 3 | Community ports | In-tree with a named maintainer listed in the host README. Built by a manually dispatched workflow, not on pull requests. A port that stays broken while its maintainer can't be reached is marked unmaintained, and is removed only if nobody takes it over. |

New platforms start at tier 3. A maintainer may promote a platform once it has
been kept green for several releases and has an owner for its release builds.

## Signing and secrets

Release signing material and provider client secrets exist only as GitHub
secrets used by manually dispatched or release workflows, never in pull
request workflows or the repository. Pull request checks build unsigned.

## Vendored dependencies

Prefer upstream fixes. When a platform needs a patched dependency before
upstream releases it, follow the existing format in `patches/<crate>/`: a
README explaining the change and its upstream link, the pinned upstream
revision and checksums, and the patch series. Add a verification step to
`.github/workflows/quality.yml`, and remove the vendored copy once upstream
ships the fix.
