# RPM support

The release RPM supports Fedora 43 and 44, openSUSE Tumbleweed and Leap 16.0,
and Enterprise Linux 10, on x86_64 and aarch64. Build on the oldest supported
Fedora (43) and validate the exact resulting artifact on every supported
distribution. Building on a newer Fedora can introduce unavailable ELF
symbol requirements even when the source and explicit `Requires` do not
change; alpha.7's Fedora 44 build required `GLIBC_2.43` from libm and failed
to install on Fedora 43.

`.github/workflows/native-platforms.yml` separates the build from the runtime
matrix. The latter starts clean containers without build dependencies,
installs the RPM with weak dependencies disabled, checks dynamically loaded
display libraries before installing desktop test tools, renders PDF fixtures as an
unprivileged user, checks desktop metadata, renders an application frame on
X11 and Wayland (Wayland only on Enterprise Linux 10, which has no X server),
and verifies removal. Publication requires the whole matrix
to pass. Transaction logs, requirements, the installed package list, and
startup results are retained as CI artifacts. Headless tests cover the default
CPU renderer; they do not replace manual KDE Discover or hardware GPU checks.

To reproduce the runtime check locally with Docker, from the repository root:

```bash
mkdir -p target/rpm-test
docker run --rm \
  --mount "type=bind,source=$PWD,target=/work,readonly" \
  --mount "type=bind,source=$PWD/target/rpm-test,target=/results" \
  fedora:43 \
  bash /work/scripts/test-rpm.sh /work/target/rpm/flectar-mail.rpm /results
```

Repeat with `fedora:44`, `opensuse/tumbleweed`, `opensuse/leap:16.0` and
`almalinux:10`. The test script changes system packages and creates a test
user, so run it only in a disposable container.

The display and font libraries that Slint and winit load at runtime are
required by soname, such as `libX11.so.6()(64bit)`, rather than by package
name. Fedora calls the package `libX11` and openSUSE `libX11-6`, but both
provide the soname, so one RPM resolves on either. Packages without a library
to name, such as GnuPG (`gnupg2` on Fedora, `gpg2` on openSUSE), use a rich
dependency that accepts either name.

The newest glibc symbol version the executable needs decides the oldest
distribution it can install on. The Fedora 43 build currently needs glibc
2.39, which Enterprise Linux 10 and Leap 16.0 have; Enterprise Linux 9 ships
2.34 and cannot install it. `requires.txt` in each runtime artifact lists the
versions in use.

When changing supported versions, update the runtime matrix, build container,
Fedora-specific Cargo cache key, README translations, `docs/install.md`, and
generated release notes together. Add a new distribution to the matrix before
advertising support. Keep the oldest supported version as the build baseline until its
support is deliberately retired. If native dependencies diverge so that one
RPM cannot pass the matrix, publish separately labelled per-release builds.

Keep RPM's automatic ELF requirements enabled. Removing a requirement or
using `--nodeps` hides an incompatible binary instead of fixing it. The public
RPM filename remains stable, and checksums/attestations cover the tested bytes.

## Signed repository

`scripts/build-rpm-repo.sh` signs both architectures' RPMs with one key,
builds the dnf and zypper repository with signed metadata, and writes
`flectar-mail.repo`. In `.github/workflows/native-platforms.yml` the
`rpm-repo` job runs it after the builds and replaces the RPM artifacts with
the signed files, so the release attaches exactly what the repository serves.
The runtime matrix installs from that repository through its `.repo` file,
with package and metadata signature checks on. Release tags publish it to
GitHub Pages at https://flectar.github.io/mail/rpm/, holding the latest
release only.

The key is the `RPM_SIGNING_KEY` repository secret, an ASCII-armored private
key, with its passphrase in `RPM_SIGNING_PASSPHRASE`. Pull requests and other
runs without it sign with a throwaway key, so every run exercises signing; a
release tag fails without the real key. The packages and metadata are held in
one place because zypper downloads packages relative to the repository and
ignores `xml:base`, so the RPMs can't stay on the GitHub release alone.

