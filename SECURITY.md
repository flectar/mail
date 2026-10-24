# Security policy

## Supported versions

Flectar Mail is in active development and is not yet stable. Please report
security issues affecting the latest published prerelease or the current
`main` branch. Fixes are developed on `main` and included in subsequent
releases; older prereleases do not have a separate security maintenance line.

## Reporting a vulnerability

Email [hello@flectar.com](mailto:hello@flectar.com) with the subject
“Flectar Mail security report”. Please do not disclose vulnerabilities in
public issues, discussions, or pull requests before a fix can be coordinated.

Include, where possible:

- The affected version or commit, operating system, and installation source.
- A description of the issue, its potential impact, and any prerequisites.
- Steps to reproduce it using test accounts and fictional data.
- A minimal proof of concept or sanitized logs that help explain the issue.

Reports can cover any part of Flectar Mail, including message rendering,
attachments, account authentication, credential storage, protocol handling,
and release artifacts. Do not send real mailbox contents, passwords, OAuth
tokens, or private keys. For a malicious message, start with a sanitized
description and coordinate how to share a reproducer privately.

Maintainers will review reports and coordinate fixes and public disclosure with
reporters. Response and resolution times depend on maintainer availability and
the issue's complexity; there is no guaranteed response deadline.

## Other reports

Use [GitHub issues](https://github.com/flectar/mail/issues) for ordinary bugs
and feature requests. For accessibility barriers, see
[ACCESSIBILITY.md](ACCESSIBILITY.md).
