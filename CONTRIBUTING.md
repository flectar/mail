# Contributing to Flectar Mail

Thank you for helping improve Flectar Mail. Keep changes focused, reviewable,
and safe for a mail client that handles private user data.

## Before starting

- Search existing issues and pull requests before opening a duplicate.
- Open an issue before substantial features, architecture changes, new
  dependencies, protocol changes, or user-visible licensing changes.
- Never submit real mailbox contents, credentials, OAuth tokens, signing
  material, or personal fixture data. Use reserved `.example` domains.

## Contribution licensing

Contributions are licensed under the same license that applies to the files
being changed, usually `AGPL-3.0-only`. Contributors retain copyright and must
have the right to submit their work under that license. If an employer or
another organization owns your work, obtain its authorization before
contributing.

## Pull requests

- Keep one logical change per pull request.
- Explain the problem, the chosen solution, tests, and user-visible effects.
- Add or update tests for behavior changes.
- Disclose material use of generative tools. You remain responsible for every
  submitted line and for confirming that generated material has valid
  provenance and compatible licensing.
- Preserve third-party copyright and license notices.
- Do not add dependencies or copied assets without documenting their source,
  version, license, and required notices.

## Local validation

Run the checks relevant to your change:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
slint-viewer --check ui/app.slint
```

UI changes must be rendered and inspected in light and dark themes. Responsive
changes must also be checked at the documented phone and tablet preview sizes.
Use fictional data in screenshots.

Icon sources, generation and sizing are documented in
[Lucide icons](ui/icons/lucide/README.md).

## Licensing of accepted contributions

Accepted contributions are published as part of Flectar Mail under
`AGPL-3.0-only`, unless a file is explicitly identified as third-party material
under another compatible license.
