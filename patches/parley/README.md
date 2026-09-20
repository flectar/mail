# Emoji presentation backport

The HTML renderer uses a patched Parley 0.11.1 until a compatible upstream
release includes [Parley #811](https://github.com/linebender/parley/pull/811),
which fixes [#744](https://github.com/linebender/parley/issues/744). Plain
digits, `#`, `*`, `©`, and `®` have the Unicode `Emoji` property, but default
to text presentation. Previously they entered emoji fallback before script
fallback when an email requested an unavailable font (Mail #16/#23).

## Sources and adaptation

- `crates/parley`: published 0.11.1, revision
  `eea3503dd6cf17130cbb07348e0ff2c918300e94`, plus
  `0001-emoji-presentation.patch`.
- `crates/parley_emoji`: the sequence detector, README, and MIT license copied
  unchanged from PR #811 at `479e1980aeca0db63999cedce1b6f31332fe0a7b`.
  Its standalone Cargo manifest is kept alongside this patch queue.
- The shaping integration follows that revision's `CharCluster::fill`.
  It reads the existing ICU property tables directly for emoji-capable
  clusters, avoiding a second fork of the generated `parley_data` tables and
  changes to 0.11's analysis metadata. The detector does not allocate.
- Font-selection presentation is separate from character/editor metadata.
  Sender-specified fonts retain their existing precedence. No generic font
  is inserted into CSS family stacks.
- The crate's published builder tests depend on omitted `parley_dev` and font
  fixtures. That module and unused `oxipng` test dependency are disabled;
  its self-contained analysis tests and our presentation tests run in CI.
  Application tests cover font selection
  and actual CPU/GPU output with a deterministic bitmap emoji font.

This retains the upstream detector's documented limitations for malformed
sequences, mixed presentation within a grapheme, and legacy modifier sequences
with intervening VS16. It does not add bitmap emoji support to the CPU painter.

## Verification and upgrades

```sh
python3 scripts/verify-parley.py
cargo test --manifest-path crates/parley/Cargo.toml --lib --locked --target-dir target/parley-tests
cargo test -p flectar-mail --lib renderer::regression --locked
# On a host with a WGPU adapter:
cargo test -p flectar-mail --lib issue_23_gpu --locked -- --ignored
```

The verifier downloads checksum-pinned sources into a temporary directory,
applies the patch, and compares every file to both checked-in crates. It also
checks desktop/Android patch wiring and the packaged MIT notice.

When a compatible upstream release fixes presentation, remove this backport,
both vendored crates, their manifest patches, and the verification step; update
both lockfiles and rerun the application regressions. Keep the regression
fixtures. Do not restore the removed Blitz CSS fallback workaround.
