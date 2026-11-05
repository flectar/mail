#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
command -v msgfmt >/dev/null || {
  echo "GNU gettext is required" >&2
  exit 1
}

work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT
cd "$repo_root"

bash scripts/update-translation-template.sh "$work_dir/flectar-mail.pot"
if ! diff -u lang/flectar-mail.pot "$work_dir/flectar-mail.pot"; then
  echo "lang/flectar-mail.pot is out of date; run scripts/update-translation-template.sh." >&2
  exit 1
fi

# Weblate writes the catalogs and merges new template strings into them, so
# they may lag the template and keep Weblate's line wrapping. Check only that
# each one compiles.
for catalog in lang/*/LC_MESSAGES/flectar-mail.po; do
  msgfmt --check --check-header --check-format -o /dev/null "$catalog"
done

bash scripts/check-ui-message-catalog.sh
