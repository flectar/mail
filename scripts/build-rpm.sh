#!/usr/bin/env bash
set -euo pipefail

project_dir="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
build_dir="$project_dir/target/rpm"
top_dir="$build_dir/rpmbuild"
source_dir="$top_dir/SOURCES"
package_root="$build_dir/package"
rpm_libdir="$(rpm --eval '%{_libdir}')"
version="$(sed -n '/^\[package\]$/,/^\[/s/^version = "\([^"]*\)"/\1/p' "$project_dir/Cargo.toml" | head -n 1)"

if [[ -z "$version" ]]; then
  printf 'Could not read the package version from Cargo.toml.\n' >&2
  exit 1
fi
if [[ "$version" == *-* ]]; then
  rpm_version="${version%%-*}"
  rpm_release="0.${version#*-}"
else
  rpm_version="$version"
  rpm_release="1"
fi

build_args=(--locked --bin flectar-mail --manifest-path "$project_dir/Cargo.toml" --release --no-default-features --features remote-content)
if [[ "${FLECTAR_SKIP_BUILD:-0}" == "1" ]]; then
  if [[ ! -x "$project_dir/target/release/flectar-mail" ]]; then
    printf 'FLECTAR_SKIP_BUILD=1 requires an existing release executable.\n' >&2
    exit 1
  fi
else
  cargo build "${build_args[@]}"
fi

rm -rf "$build_dir"
mkdir -p "$source_dir" "$top_dir/BUILD" "$top_dir/BUILDROOT" "$top_dir/RPMS" "$top_dir/SPECS" "$top_dir/SRPMS"
install -Dm0755 "$project_dir/target/release/flectar-mail" "$package_root/usr/bin/flectar-mail"
python3 "$project_dir/scripts/stage-pdfium.py" linux-x64 "$package_root$rpm_libdir/flectar-mail"
python3 "$project_dir/scripts/test-pdf-preview.py" "$package_root/usr/bin/flectar-mail"
install -m0755 "$package_root/usr/bin/flectar-mail" "$source_dir/flectar-mail"
install -m0755 "$package_root$rpm_libdir/flectar-mail/libpdfium.so" "$source_dir/libpdfium.so"
tar -C "$package_root$rpm_libdir/flectar-mail/pdfium-licenses" -czf "$source_dir/pdfium-licenses.tar.gz" .

python3 "$project_dir/scripts/stage-linux-metadata.py" "$build_dir/metadata" >/dev/null
install -m0644 "$build_dir/metadata/usr/share/applications/com.flectar.mail.desktop" "$source_dir/com.flectar.mail.desktop"
install -m0644 "$build_dir/metadata/usr/share/metainfo/com.flectar.mail.metainfo.xml" "$source_dir/com.flectar.mail.metainfo.xml"
install -m0644 "$project_dir/resources/app-icon/flectar-mail-masked-512.png" "$source_dir/com.flectar.mail.png"
install -m0644 "$project_dir/resources/app-icon/flectar-mail-masked.svg" "$source_dir/com.flectar.mail.svg"
install -m0644 "$project_dir/LICENSE" "$source_dir/LICENSE"
tar -C "$project_dir/LICENSES" -czf "$source_dir/LICENSES.tar.gz" .
install -m0644 "$project_dir/THIRD_PARTY_NOTICES.md" "$source_dir/THIRD_PARTY_NOTICES.md"
install -m0644 "$project_dir/resources/fonts/google-sans-flex/OFL.txt" "$source_dir/google-sans-flex-OFL.txt"
install -m0644 "$project_dir/resources/fonts/google-sans-flex/README.md" "$source_dir/google-sans-flex-README.md"

changelog_date="$(LC_ALL=C date --utc --date="@${SOURCE_DATE_EPOCH:-$(date +%s)}" '+%a %b %d %Y')"
sed \
  -e "s/@RPM_VERSION@/$rpm_version/g" \
  -e "s/@RPM_RELEASE@/$rpm_release/g" \
  -e "s/@RPM_CHANGELOG_DATE@/$changelog_date/g" \
  "$project_dir/resources/rpm/flectar-mail.spec.in" > "$top_dir/SPECS/flectar-mail.spec"

rpmbuild -bb \
  --define "_topdir $top_dir" \
  "$top_dir/SPECS/flectar-mail.spec"

mapfile -t packages < <(find "$top_dir/RPMS" -type f -name '*.rpm' -print)
if [[ "${#packages[@]}" != 1 ]]; then
  printf 'Expected one RPM, found %s.\n' "${#packages[@]}" >&2
  exit 1
fi
output="$build_dir/flectar-mail.rpm"
cp "${packages[0]}" "$output"
rpm -K "$output"
rpm -qpl "$output" >/dev/null
printf 'Built %s\n' "$output"
