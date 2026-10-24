# Contributing to Flectar Mail

Thank you for helping improve Flectar Mail. Keep changes focused, reviewable,
and safe for a mail client that handles private user data.

## Before starting

- Search existing issues and pull requests before opening a duplicate.
- Open an issue before substantial features, architecture changes, new
  dependencies, protocol changes, or user-visible licensing changes.
- Never submit real mailbox contents, credentials, OAuth tokens, signing
  material, or personal fixture data. Use reserved `.example` domains.

## Translations

Translate the application through [Weblate](https://translate.flectar.com/).
README translations are maintained directly in this repository:

- Use [README.md](README.md) as the source and add
  `docs/readme/README.<locale>.md`, following
  [Spanish](docs/readme/README.es.md) or
  [Simplified Chinese](docs/readme/README.zh_Hans.md).
- Translate the complete document, including image alternative text, while
  preserving link destinations, commands, filenames, and technical identifiers.
  Resolve relative links from `docs/readme/`: use `../../resources/` for images,
  `../../README.md` for English, and sibling filenames for other translations.
- Add the language's native name to the language selector in every README
  translation and in the English README. Highlight each file's own language.
- Keep the same sections and shared screenshots. When updating documentation,
  update translations where possible and note any remaining gaps in the pull
  request.

## Accessibility and security

Read [ACCESSIBILITY.md](ACCESSIBILITY.md) when changing the interface or
reporting an accessibility barrier. Report vulnerabilities privately using
[SECURITY.md](SECURITY.md), rather than opening a public issue.

## Platform code

Shared code selects platform behaviour with capability aliases such as
`mobile`, `desktop` and `linux_desktop`, not operating-system names. Only the
platform layer names a mobile operating system. Read
[Platform architecture](platform/README.md) before changing platform-specific
code or proposing a new platform; new platforms start as community-maintained
ports.

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
python3 scripts/check-platform-gates.py
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
