#!/usr/bin/env bash
# Regenerates the translation template from the .slint sources. Only the
# template lives in this repository's pull requests; Weblate merges it into
# lang/*/LC_MESSAGES/flectar-mail.po with its msgmerge add-on.
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
template="$repo_root/lang/flectar-mail.pot"
output=${1:-$template}
command -v slint-tr-extractor >/dev/null || {
  echo "slint-tr-extractor 1.18.1 is required" >&2
  exit 1
}

work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT
cd "$repo_root"

# Preview and screenshot scripts use temporary hidden .slint files.
find ui -type f -name '*.slint' ! -name '.*' -print0 \
  | sort -z \
  | xargs -0 slint-tr-extractor --no-default-translation-context \
      --package-name flectar-mail --package-version 0.1.0 \
      -o "$work_dir/flectar-mail.pot"

# Keep the committed creation date so that unchanged strings give an
# unchanged template; Weblate copies this header into every catalog.
creation_date=$(grep -m1 '^"POT-Creation-Date:' "$template" || true)
if [[ -n $creation_date ]]; then
  CREATION_DATE=$creation_date awk '
    !done && /^"POT-Creation-Date:/ { print ENVIRON["CREATION_DATE"]; done = 1; next }
    { print }
  ' "$work_dir/flectar-mail.pot" >"$work_dir/dated.pot"
  mv "$work_dir/dated.pot" "$work_dir/flectar-mail.pot"
fi
mv "$work_dir/flectar-mail.pot" "$output"
