#!/usr/bin/env bash
# Publish only after all desktop packages and checksums have been staged.
set -euo pipefail
: "${GITHUB_REF_NAME:?}"
: "${RELEASE_PRERELEASE:?}"
case "$RELEASE_PRERELEASE" in true|false) ;; *) exit 1 ;; esac

# A failed upload can be retried while the release is still a draft. Never
# replace already published downloads: publish a new version instead.
if existing_draft="$(gh release view "$GITHUB_REF_NAME" --json isDraft --jq .isDraft 2>/dev/null)"; then
  if [[ "$existing_draft" != true ]]; then
    printf 'Release %s is already published; refusing to replace its assets.\n' "$GITHUB_REF_NAME" >&2
    exit 1
  fi
else
  gh release create "$GITHUB_REF_NAME" \
    --verify-tag --draft --prerelease="$RELEASE_PRERELEASE" \
    --title "Flectar Mail ${GITHUB_REF_NAME#v}" \
    --notes-file release-notes.md --generate-notes
fi

gh release upload "$GITHUB_REF_NAME" dist/* --clobber
gh release edit "$GITHUB_REF_NAME" \
  --draft=false --prerelease="$RELEASE_PRERELEASE" \
  --latest="$([[ "$RELEASE_PRERELEASE" == false ]] && echo true || echo false)"
