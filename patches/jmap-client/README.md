# A proxy and TLS settings for jmap-client

Flectar Mail sends its JMAP requests through its own HTTP client, which goes
through the proxy chosen in Settings (Mail #250). jmap-client 0.4.2 builds a
new HTTP client for every request it sends itself and has no way to give them
a proxy. Flectar Mail relies on it for two requests, the session request when
an account connects and the event source that pushes new mail, so both would
connect straight to the server, past the proxy.

Those HTTP clients also check certificates with reqwest's own verifier, while
every other connection of the app checks them as mail connections do
(`net::tls` in the core). On Android only the app's checks work, so JMAP
servers would be unreachable there.

## Sources and adaptation

`crates/jmap-client` is the published 0.4.2 archive, revision
`aaed913153c4eb1b9a47a0a65311d4203b147109`, plus two patches, which are ours.
`0001-proxy.patch` adds `ClientBuilder::proxy`, keeps the proxy in the
`Client`, and gives it to every HTTP client the crate builds: the session
request, `send`, `refresh_session`, blob upload and download, and the event
source. `0002-tls.patch` adds `ClientBuilder::tls_config` the same way, for a
rustls configuration reqwest then uses as it is, when the crate is built with
rustls. Without them, the requests are built as before. The latest release,
0.4.3, only replaces the crate's base64 dependency, so it would need the same
patches.

The standalone lockfile is omitted, and the application lockfiles resolve the
patched crate.

## Verification and removal

```sh
python3 scripts/verify-jmap-client.py
```

The verifier downloads the checksum-pinned archive, checks its revision,
applies the patches in order, and compares every file with the vendored
crate. It also checks that the root, Android and OpenHarmony workspaces patch
the crate and that their lockfiles resolve it locally.

When a jmap-client release lets callers choose the proxy and the TLS
configuration, remove the vendored crate, this directory, the three manifest
patches, the root workspace exclusion, the `.gitignore` and `.gitattributes`
entries, the verifier, its CI step, and its paragraph in
`THIRD_PARTY_NOTICES.md`. Then update the three lockfiles and
`packaging/flatpak/cargo-sources.json`.
