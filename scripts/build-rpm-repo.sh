#!/usr/bin/env bash
# build-rpm-repo.sh OUT_DIR RPM...
#
# Signs the RPMs and builds the dnf and zypper repository of them in OUT_DIR,
# with signed metadata, the public key and a .repo file. GitHub Pages serves
# that directory, and the release attaches the same signed RPMs.
#
# The key is RPM_SIGNING_KEY, an ASCII-armored private key, with its
# passphrase in RPM_SIGNING_PASSPHRASE. Without one, as in pull requests, a
# throwaway key signs instead, so every run checks signing, the repository and
# signed installs the same way. FLECTAR_REQUIRE_SIGNING_KEY=1, set for release
# tags, refuses to fall back.
set -euo pipefail

if (($# < 2)); then
  echo "Usage: $0 OUT_DIR RPM..." >&2
  exit 2
fi
out=$1
shift
repo_url=${FLECTAR_RPM_REPO_URL:-https://flectar.github.io/mail/rpm}

GNUPGHOME=$(mktemp -d)
export GNUPGHOME
trap 'rm -rf "$GNUPGHOME"' EXIT
passphrase_file="$GNUPGHOME/passphrase"
if [[ -n "${RPM_SIGNING_KEY:-}" ]]; then
  gpg --batch --quiet --import <<<"$RPM_SIGNING_KEY"
  printf '%s' "${RPM_SIGNING_PASSPHRASE:-}" >"$passphrase_file"
elif [[ "${FLECTAR_REQUIRE_SIGNING_KEY:-0}" == 1 ]]; then
  echo "RPM_SIGNING_KEY is required to sign a release." >&2
  exit 1
else
  echo "::notice::No RPM signing key; signing with a throwaway key."
  head -c 32 /dev/urandom | base64 >"$passphrase_file"
  gpg --batch --quiet --pinentry-mode loopback --passphrase-file "$passphrase_file" \
    --quick-gen-key "Flectar Mail throwaway test key <noreply@flectar.com>" rsa4096 sign 1d
fi
fingerprint=$(gpg --batch --list-secret-keys --with-colons | awk -F: '/^fpr:/ {print $10; exit}')
sign_args="--batch --pinentry-mode loopback --passphrase-file $passphrase_file"

rm -rf "$out"
mkdir -p "$out"
for package in "$@"; do
  name=$(rpm -qp --queryformat '%{NAME}-%{VERSION}-%{RELEASE}.%{ARCH}.rpm' "$package")
  install -m0644 "$package" "$out/$name"
  rpmsign --addsign \
    --define "_gpg_name $fingerprint" \
    --define "_gpg_sign_cmd_extra_args $sign_args" \
    "$out/$name" >/dev/null
done

key="$out/RPM-GPG-KEY-flectar-mail"
gpg --batch --armor --export "$fingerprint" >"$key"
# Check every package against the exported key, in an empty RPM database.
rpmdb=$(mktemp -d)
rpmkeys --dbpath "$rpmdb" --import "$key"
for package in "$out"/*.rpm; do
  rpmkeys --dbpath "$rpmdb" --checksig "$package" | tee /dev/stderr | grep -q ': digests signatures OK$'
done
rm -rf "$rpmdb"

createrepo_c --quiet "$out"
# dnf checks repomd.xml.asc when repo_gpgcheck is on, and zypper does too; zypper
# also offers repomd.xml.key to import.
# shellcheck disable=SC2086 # sign_args is a list of options.
gpg $sign_args --local-user "$fingerprint" --armor --detach-sign "$out/repodata/repomd.xml"
cp "$key" "$out/repodata/repomd.xml.key"
gpg --batch --verify "$out/repodata/repomd.xml.asc" "$out/repodata/repomd.xml"

cat >"$out/flectar-mail.repo" <<EOF
[flectar-mail]
name=Flectar Mail
baseurl=$repo_url
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=$repo_url/RPM-GPG-KEY-flectar-mail
EOF
printf 'Built the repository in %s, signed by %s\n' "$out" "$fingerprint"
