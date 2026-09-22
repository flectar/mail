#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
command -v slint-tr-extractor >/dev/null || {
  echo "slint-tr-extractor 1.18.0 is required" >&2
  exit 1
}
command -v msgmerge >/dev/null || {
  echo "GNU gettext is required" >&2
  exit 1
}

work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT
cd "$repo_root"

find ui -type f -name '*.slint' -print0 \
  | sort -z \
  | xargs -0 slint-tr-extractor --no-default-translation-context \
      --package-name flectar-mail --package-version 0.1.0 \
      -o "$work_dir/flectar-mail.pot"

# Extraction timestamps are intentionally volatile; compare every semantic
# catalog line while ignoring only that generated header field.
diff -u \
  <(sed '/^"POT-Creation-Date:/d' lang/flectar-mail.pot) \
  <(sed '/^"POT-Creation-Date:/d' "$work_dir/flectar-mail.pot")

for language in de es tr zh_Hans; do
  msgmerge --quiet --no-fuzzy-matching \
    --output-file "$work_dir/flectar-mail-$language.po" \
    "lang/$language/LC_MESSAGES/flectar-mail.po" "$work_dir/flectar-mail.pot"
  diff -u \
    <(sed '/^"POT-Creation-Date:/d' "lang/$language/LC_MESSAGES/flectar-mail.po") \
    <(sed '/^"POT-Creation-Date:/d' "$work_dir/flectar-mail-$language.po")
done

bash scripts/check-ui-message-catalog.sh
