# SettingsDialog retained lazy construction

Run against existing host libraries; this does not compile the application, call
Cargo, use a device or read account configuration:

```sh
python3 platform/ohos/tests/run_settings_lazy_contracts.py \
  --deps-dir /path/to/host/debug/deps \
  --slint-rlib /path/to/host/debug/deps/libslint-HASH.rlib \
  --build-deps-dir /path/to/compiler/debug/deps \
  --slint-build-rlib /path/to/compiler/debug/deps/libslint_build-HASH.rlib
```

Both libraries require dependency `.d` files identifying exactly **1.18.1**.
The Slint build library must have its `renderer-software` feature enabled.
`--slint-rlib` can be omitted if there is one matching library. The renderer's
SVG and font resources are pre-embedded by the compiler for this isolated host
fixture, so the fixture does not require image decoder features in the runtime.
This differs from the device resource representation and is not a device
performance measurement or device appearance acceptance.

The runner copies the **actual production SettingsDialog and ThemeEditor** to a
temporary directory, rebases their file references, and adds private test
exports. All UI bodies and original functions remain present. Observers count
real child construction and read actual ScrollView `viewport-y`; no test binding
stores or restores that offset. Private command exports invoke the original
search/modal/theme functions. The outer conditional matches the app's creation
order: configure globals, tab and callbacks, then construct the dialog.

Six runtime contracts exercise:

- Initial CPU General constructs no unvisited page/modal; search selects Contacts
  and Storage; repeated visits keep each page instance.
- Opening Accounts directly and sending a real mouse-wheel event changes the
  actual ScrollView; switching away and back retains both instance and offset.
- Non-CPU rendering keeps all six pages and four modals eager.
- Original modal open/close/reopen functions retain instances; the unchanged
  Settings closing watcher clears CalDAV secret/id, profile picker and modal
  visibility. Synthetic form fields survive tab switches.
- Actual ThemeEditor snapshots and mutates AppTheme, retains the preview across
  tabs, and rolls it back through the original Settings closing watcher. A
  second preview/cancel checks a fresh snapshot.
- Destroying the entire dialog releases visited instances; reopening General
  leaves Accounts unconstructed and its next visit receives a fresh offset.

The negative control removes only the six visited latches from the construction
conditions, restoring destroy-on-tab behavior. The real wheel/scroll contract
must fail with a new Accounts instance and reset offset. `--skip-negative` is
provided for fixture development, not complete acceptance.

Outputs are retained with mode `0600` under
`target/ohos-host-contracts/settings-lazy`: compiler diagnostics, test logs,
source hashes, the negative-control result and synthetic General/Accounts
renders. The runner checks that production SettingsDialog did not change during
validation. Native focus, IME, device rendering and timing remain real-device
acceptance work.

Production scope deliberately keeps General eager for ThemeEditor's private
preview/snapshot lifetime, and retains all pages/modals after first construction
until the enclosing SettingsDialog is destroyed. The three small confirmation
dialogs and inline CardDAV section are not independently delayed.
