# HarmonyOS patch to jmap-client 0.4.2

Source: <https://crates.io/crates/jmap-client/0.4.2>, upstream
<https://github.com/stalwartlabs/jmap-client>. Original MIT / Apache-2.0
licenses and source are included.

Original crate archive SHA-256:
`4deab22e057d24e32122f0fc6e2d667a124fdd6a0d8ef3ed4f8a89923c11084f`.
Published source VCS commit: `aaed913153c4eb1b9a47a0a65311d4203b147109`.

Two small changes support the sandboxed native host:

1. Use reqwest's `rustls-no-provider` feature so the host-selected `ring`
   provider is respected, instead of unconditionally enabling AWS-LC as well.
2. On `target_env = "ohos"`, the three internal HTTP builders use explicit
   rustls/ring configuration with Mozilla `webpki-roots`. Desktop retains its
   existing platform verifier and public APIs remain unchanged.

This source copy must be refreshed together with its version/pinned source
and these changes when updating the upstream dependency.
