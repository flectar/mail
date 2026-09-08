#!/usr/bin/env bash
# Render the actual onboarding UI with dummy configuration states; no OAuth requests.
set -euo pipefail
repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output_dir="${1:-$repo_dir/tmp/oauth-onboarding}"
mkdir -p "$output_dir"
temporary_dir="$(mktemp -d)"
temporary_ui="$(mktemp "$repo_dir/ui/.onboarding-render.XXXXXX.slint")"
trap 'rm -rf "$temporary_dir"; rm -f "$temporary_ui"' EXIT
slint-viewer --check "$repo_dir/ui/app.slint"
python3 - "$temporary_dir" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
base = {'startup_ready': True, 'connected_accounts': [], 'theme_mode': 'light'}
states = {
    'unconfigured': {},
    'dialog': {'oauth_setup_open': True},
    'saving': {'oauth_setup_open': True, 'oauth_settings_saving': True},
    'save-error': {'oauth_setup_open': True, 'oauth_settings_error': 'OAuth settings failed: database is read-only'},
    'google-configured': {'google_oauth_available': True},
    'configured': {'google_oauth_available': True, 'microsoft_oauth_available': True},
    'bundled': {'google_oauth_available': True, 'microsoft_oauth_available': True,
                'google_oauth_bundled': True, 'microsoft_oauth_bundled': True},
    'dialog-dark': {'oauth_setup_open': True, 'theme_mode': 'dark'},
    'unconfigured-dark': {'theme_mode': 'dark'},
}
for name, state in states.items():
    (root / (name + '.json')).write_text(json.dumps(base | state))
PY
for state in unconfigured dialog saving save-error google-configured configured bundled dialog-dark unconfigured-dark; do
    style=fluent
    if [[ "$state" == *-dark ]]; then style=fluent-dark; fi
    slint-viewer --style "$style" --load-data "$temporary_dir/$state.json" \
        --screenshot "$output_dir/$state.png" "$repo_dir/ui/app.slint"
done
sed -e 's/preferred-width: 1320px/preferred-width: 390px/' \
    -e 's/preferred-height: 800px/preferred-height: 844px/' \
    "$repo_dir/ui/app.slint" > "$temporary_ui"
for state in unconfigured dialog; do
    slint-viewer --load-data "$temporary_dir/$state.json" \
        --screenshot "$output_dir/mobile-$state.png" "$temporary_ui"
done
