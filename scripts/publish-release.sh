#!/usr/bin/env bash
# Publish only after all packages and checksums have been staged.
set -euo pipefail
: "${GITHUB_REF_NAME:?}"

# A failed upload can be retried while the release is still a draft. Never
# replace already published downloads: publish a new version instead.
if existing_draft="$(gh release view "$GITHUB_REF_NAME" --json isDraft --jq .isDraft 2>/dev/null)"; then
  if [[ "$existing_draft" != true ]]; then
    printf 'Release %s is already published; refusing to replace its assets.\n' "$GITHUB_REF_NAME" >&2
    exit 1
  fi
else
  gh release create "$GITHUB_REF_NAME" \
    --verify-tag --draft \
    --title "Flectar Mail ${GITHUB_REF_NAME#v}" \
    --notes-file release-notes.md --generate-notes
fi

gh release upload "$GITHUB_REF_NAME" dist/* --clobber
# Previews are full releases too, and each one becomes the latest, which the
# latest-download links, such as the AltStore source's address, follow.
gh release edit "$GITHUB_REF_NAME" --draft=false --prerelease=false --latest=true
