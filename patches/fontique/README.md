# Fontique patch maintenance

The vendored crate is the published **0.11.1** archive plus the ordered
patches in [`series`](series). The archive SHA-256 checksum is in
[`upstream.toml`](upstream.toml). Do not edit the vendored code without
regenerating its patch.

## Why

OpenHarmony reports `target_os = "linux"` but ships no Fontconfig, and
`yeslogic-fontconfig-sys` is a link-time dependency of fontique's system font
backend. The patch:

- drops the Fontconfig dependency and backend for `target_env = "ohos"` and
  folds OHOS into fontique's dummy system backend;
- silences the deprecated `Script::from_str_unchecked` calls that upstream's
  script table uses (`#![allow(deprecated)]`), instead of patching hundreds of
  call sites.

Upstream tracking: <https://github.com/linebender/fontique>. If an OHOS font
backend or a Fontconfig opt-out lands there, drop this patch.

Verify with `scripts/fontique-patches.sh verify` (also run in `quality.yml`).
