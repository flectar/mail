# OHOS owner-loop host contracts

These tests compile the **current production** `backend.rs`, `native.rs`,
`framebuffer.rs`, and `performance.rs` with Slint/i-slint-core **1.18.1**. They do
not start the mail application, use accounts, or connect to a device.

The runner executes the production module tests plus two FIFO/timer tests and
seven pacing/framebuffer tests and two input-clock tests (currently 25 passes). Pacing uses deterministic
host hooks to simulate VSync during present, fast-frame gating, request failure, and Surface
replacement. The framebuffer tests warm the actual retained Slint source until
an unchanged frame has no damage, then verify complete rendering after Surface
generation, equal-area dimensions, or scale changes. A temporary copy restores
the old post-present request ordering;
its expected failure proves the pacing contract detects that regression.
Another temporary copy invalidates only the initial framebuffer; its expected
failure proves the generation test detects missing later invalidation.
The input-clock tests use a deterministic monotonic platform clock and actual
Slint TouchArea callbacks. A due timer runs before touch callbacks, including
after preceding tasks advance time within the same batch, and each callback
observes the current clock. Slint 1.18.1 already refreshes its animation clock
inside pointer dispatch; that alone does not activate pending timers. A
temporary copy restores the old update-after-input order; its expected failure
detects the overdue timer running after touch rather than checking source text
alone. Another contract verifies a queued task observes the current clock.

## Run using existing host dependencies

Use a host `debug/deps` directory built by the same Rust toolchain as `rustc`:

```sh
python3 platform/ohos/tests/run_host_contracts.py \
  --deps-dir /path/to/host-target/debug/deps
```

If that directory contains several builds, specify the two rlibs explicitly:

```sh
python3 platform/ohos/tests/run_host_contracts.py \
  --deps-dir /path/to/host-target/debug/deps \
  --slint-rlib /path/to/host-target/debug/deps/libslint-HASH.rlib \
  --core-rlib /path/to/host-target/debug/deps/libi_slint_core-HASH.rlib
```

The matching `.d` files must identify version 1.18.1 for both packages. Other
transitive host dependencies must remain in the same directory. Rust 1.97 is
the validated toolchain. `--rustc` selects a compatible compiler; `--output-dir`
selects where to retain logs and the JSON validation report. The default is
`target/ohos-host-contracts/evidence`.

## Prepare dependencies once, if needed

The runner never invokes Cargo. To prepare an isolated host dependency build,
run this separately using a normal host Rust environment:

```sh
cargo test --manifest-path platform/ohos/rust/Cargo.toml \
  --no-default-features --no-run --target-dir target/ohos-host-contracts
```

The `application` feature stays disabled, so this command does not build the
mail GUI or protocol application. Use the resulting `debug/deps` directory with
the runner. If your environment sets `CARGO_BUILD_TARGET`, unset it or pass your
host triple explicitly with Cargo's `--target`; then use its
`<host-triple>/debug/deps` directory. OHOS cross-compiled rlibs cannot be used.

## Native accumulated-copy contracts

The standalone C++ tests include the production `native_damage.h` directly and
require no SDK, Rust dependencies, Cargo or device. Run from the repository root:

```sh
clang++ -std=c++17 -O2 -Wall -Wextra -Werror \
  platform/ohos/tests/native_damage_test.cpp \
  -o /private/tmp/feihong-native-damage-test
/private/tmp/feihong-native-damage-test
```

For AddressSanitizer and UndefinedBehaviorSanitizer, when supported by the host:

```sh
clang++ -std=c++17 -O1 -g -Wall -Wextra -Werror \
  -fsanitize=address,undefined -fno-omit-frame-pointer \
  platform/ohos/tests/native_damage_test.cpp \
  -o /private/tmp/feihong-native-damage-test-sanitized
/private/tmp/feihong-native-damage-test-sanitized
```

Both builds passed 13 contracts with Apple Clang 21.0.0. The tests exercise the
same production tracker and copy helper: cumulative damage across rotating
buffers, empty retries after failures, generation and dimensions, padding and
bounds, bounded ownership/LRU, reference failures, and background reset/resume.
They include 1,200 arbitrary rotations, failures and resizes. Native API results
and reference counts are simulated; these tests do not run OHOS buffer APIs or
establish device presentation, frame rate or power acceptance.

## Boundaries

The Rust owner loop and software renderer run unchanged. Only the temporary
native copy's **host branches** delegate to optional test hooks. Production
source changes make exact patch checks fail loudly instead of silently testing
an obsolete copy. All generated sources and test binaries live in a temporary
directory and are removed afterward. Tests run with `--test-threads=1` because
Slint platform installation belongs to each test thread.

These contracts verify FIFO, timer turns, scheduling order, generation recovery,
IME text semantics, current input clocks, retained RGBA colors, framebuffer invalidation and stride
behavior. The native hooks
simulate API results and callbacks; they do not verify OHOS VSync delivery,
NativeWindow, real-device FPS, power use, or system IME preview delivery. Those
remain device acceptance tests.

## Email font startup contracts

[FontContext prewarm contracts](FONT-PREWARM-CONTRACTS.md) compile functions
extracted from the current production renderer and its public diagnostic
forwarder. They exercise ready/pending publication, background failure, real
CJK/emoji font selection, collection clone isolation and explicitly enabled
debug diagnostics. The runner uses existing host dependencies without Cargo;
optional OHOS metadata checks are compilation only. Readiness misses retain
the original synchronous constructor cost.
