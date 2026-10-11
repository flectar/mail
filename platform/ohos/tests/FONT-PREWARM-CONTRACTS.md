# Email FontContext prewarm contracts

The OHOS owner thread clones an independent email FontContext. At startup,
`prewarm_email_font_ctx` launches the unchanged production font builder on
Tokio's blocking pool. It publishes only a completed successful context through
`OnceLock::set`. The owner uses `get().cloned()`; it does not join the worker or
use `get_or_init` for this context. If the result is not ready, the original
synchronous constructor remains available. This preserves behavior and avoids
waiting for the background job, but **a readiness miss can still incur the
original synchronous font initialization cost**.

The call occurs after native font paths are configured, after the shared runtime
and deterministic UI emoji font registration, and before `AppWindow::new`.
The existing complete builder, path order, script fallback loop and generic
family rules are unchanged. It does not clone or modify Slint's shared writable
collection. A completed template and each document clone retain their own
collection state; the font byte blobs remain shared.

Font initialization diagnostics use their own `OnceLock<fn(&str)>` callback.
The OHOS launcher installs `native::log` only for a debug application run with
its existing explicit `performance_diagnostics` option enabled. Neither the
core nor the launcher writes environment variables to enable them. When the
callback is absent, the code does not take initialization timestamps, format a
message or call a logger. Release builds ignore the diagnostic callback.
`FeihongFontInit` contains only the numeric `background`, `success`, `published`,
`ready` and `bootstrap_ms` fields. It contains no font path, account or message
content. The existing broad renderer timing environment does not enable this
separate callback.

## What is tested

`run_font_prewarm_contracts.py` extracts the current production prewarm,
diagnostic, initial/thread-local context functions, the public main diagnostic
forwarder and complete font builder. It compiles those bodies with existing
local Parley 0.11.1 and Tokio host dependencies using `rustc`; it does not invoke
Cargo, build a HAP, read accounts or connect to a device. Temporary copies remove
only OHOS target gating to execute the OHOS path on the host. Private hooks
observe constructor calls, hold the actual background constructor at a gate,
and inject a single background failure. The complete builder is invoked with
host system-font discovery disabled, one actual CJK font and the production
Noto Emoji font. Bundled sans and bullet fonts remain the production assets.

The six host runtime cases cover:

- Ready publication: the owner constructs no font collection, clones the
  completed template, and preserves five script fallback sequences, SansSerif,
  family/blob identities and 33 Chinese, Traditional Chinese, Latin, digit and
  emoji glyph selections. Changing one document clone's collection leaves the
  next clone and the published template unchanged.
- Pending publication: the owner finishes the original fallback constructor
  while the background gate remains closed. Its thread-local context stays
  unchanged after publication; a fresh owner thread uses the published template.
- Background panic: the worker returns without publishing a context; the
  original owner-thread fallback still has Chinese coverage.
- Explicit debug callback: background and ready-hit numeric spans are emitted.
- Default-off callback, including the broad renderer timing environment:
  no font initialization diagnostic is emitted.
- Release-style cfg with an explicit callback: diagnostics remain silent.

Three source mutation controls use the same harness. Sharing the owner font
collection must lose isolation and fail; publishing a default context after a
background panic must fail the publication contract; making startup wait for
its blocking job must time out while the private worker gate remains closed.
All mutations occur only in the evidence directory.

The optional two OHOS checks compile the actual extracted functions and public
forwarder against existing `aarch64-unknown-linux-ohos` library metadata, with
debug assertions both enabled and disabled. The static OnceLock and Tokio
closure establish FontContext/Collection/SourceCache Send, Sync and Clone
requirements at compile time. These are **metadata compilation checks, not
OHOS execution, installation, input, pixel or FPS tests**. Host proc-macro
artifacts must also be available through `--host-proc-macro-dir`.

## Run

Run from the repository root, using dependency libraries built by the same
Rust compiler. `--cjk-font` is required and must name an existing font covering
the supplied Chinese and Traditional Chinese characters; the fixture uses real
Fontique queries. No system font asset path is assumed. `--repository` defaults
to the checkout containing this runner and can be overridden explicitly. The
runner supplies resolved fixture paths through compile-time environment
variables rather than substituting them into Rust source text.

Set `HOST_DEPS`, `HOST_PARLEY` and `HOST_TOKIO` to the existing dependency
directory and matching `.rlib` files, and `CJK_FONT` to your CJK font. For the
optional OHOS metadata checks, also set `OHOS_DEPS`, `OHOS_PARLEY` and
`HOST_PROC_MACROS` to the corresponding existing artifacts.

```sh
python3 platform/ohos/tests/run_font_prewarm_contracts.py \
  --source src/renderer.rs \
  --main-source src/main.rs \
  --deps-dir "$HOST_DEPS" \
  --parley-rlib "$HOST_PARLEY" \
  --tokio-rlib "$HOST_TOKIO" \
  --cjk-font "$CJK_FONT" \
  --output-dir target/ohos-host-contracts/font-prewarm \
  --ohos-parley-rlib "$OHOS_PARLEY" \
  --ohos-deps-dir "$OHOS_DEPS" \
  --host-proc-macro-dir "$HOST_PROC_MACROS"
```

Omit all three OHOS arguments for a host-only run. The runner records compile
commands, runner/harness/source hashes, resolved fixture inputs, the CJK font
hash, logs and results in `validation.json`, with evidence files mode 0600.
Local paths stay in the ignored evidence directory. `source-preservation.json`
records the one-time comparison against the
pre-change builder; its function SHA256 is
`e9bb7fd11673eb0ef1ab04e3187dc30b17826558d152c501003b359e91e721f0`.

The host font universe and features differ from a MatePad. Reconstructing two
independent collections from a multi-family TTC can already return a different
family order in Fontique 0.11.1 because `register_fonts` returns a FamilyId-keyed
HashMap iteration. The contract therefore compares the single completed
production template with its clones. It does not claim fixed ordering across
independent old/new constructions or infer device latency from host durations.
