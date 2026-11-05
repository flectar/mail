#!/usr/bin/env bash
# Fails unless each given language translates every string in the current
# template, with no fuzzy entries. Releases require this for Spanish and
# Russian; finish missing strings on Weblate, then merge its pull request.
set -euo pipefail

repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
command -v msgmerge >/dev/null || {
  echo "GNU gettext is required" >&2
  exit 1
}
if [[ $# -eq 0 ]]; then
  echo "usage: $0 LANGUAGE..." >&2
  exit 2
fi

work_dir=$(mktemp -d)
trap 'rm -rf -- "$work_dir"' EXIT
cd "$repo_root"

status=0
for language in "$@"; do
  merged="$work_dir/$language.po"
  msgmerge --quiet --no-fuzzy-matching --output-file "$merged" \
    "lang/$language/LC_MESSAGES/flectar-mail.po" lang/flectar-mail.pot
  untranslated=$(msgattrib --untranslated --no-obsolete "$merged")
  fuzzy=$(msgattrib --only-fuzzy --no-obsolete "$merged")
  if [[ -n $untranslated || -n $fuzzy ]]; then
    printf '%s: ' "$language" >&2
    LC_ALL=C msgfmt --statistics -o /dev/null "$merged"
    status=1
  fi
done
exit "$status"
