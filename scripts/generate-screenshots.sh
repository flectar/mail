#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture="$repo_dir/resources/screenshots/demo-data.json"
ui="$repo_dir/ui/app.slint"
output_dir="$repo_dir/resources/screenshots"
temporary_dir="$(mktemp -d)"
temporary_ui=""
trap 'rm -rf "$temporary_dir"; test -z "$temporary_ui" || rm -f "$temporary_ui"' EXIT

command -v slint-viewer >/dev/null 2>&1 || {
  echo "slint-viewer 1.17+ is required" >&2
  exit 1
}
command -v cargo >/dev/null 2>&1 || {
  echo "Cargo is required to generate sender favicons" >&2
  exit 1
}
command -v jq >/dev/null 2>&1 || {
  echo "jq is required to prepare screenshot fixtures" >&2
  exit 1
}

slint-viewer --check "$ui"

favicon_dir="$temporary_dir/favicons"
mapfile -t sender_addresses < <(
  jq -r '[.selected_address, (.emails[].address)] | unique[]' "$fixture"
)
cargo run --quiet --example generate-screenshot-favicons -- \
  "$favicon_dir" "${sender_addresses[@]}"

render() {
  local output_name="$1"
  local width="$2"
  local height="$3"
  local theme="$4"
  local workspace_layout="${5:-default}"
  local theme_preset="${6:-default}"
  local active_view="${7:-mail}"
  local style="fluent"
  local data_file="$temporary_dir/$output_name.json"

  if [[ "$theme" == "dark" ]]; then
    style="fluent-dark"
  fi

  temporary_ui="$(mktemp "$repo_dir/ui/.screenshot-app.XXXXXX.slint")"

  jq \
    --arg theme "$theme" \
    --arg workspace_layout "$workspace_layout" \
    --arg theme_preset "$theme_preset" \
    --arg active_view "$active_view" \
    --arg favicon_dir "$favicon_dir" \
    '
      def favicon_path(address):
        ($favicon_dir + "/" + (address | split("@") | last | ascii_downcase) + ".png");
      .theme_mode = $theme
      | .workspace_layout = $workspace_layout
      | .screenshot_theme_preset = $theme_preset
      | .active_view = $active_view
      | .text_mode = false
      | .selected_favicon = favicon_path(.selected_address)
      | .selected_has_favicon = true
      | .emails |= map(
          .has_replied = (.has_replied // false)
          | .checked = (.checked // false)
          | .account_id = (.account_id // 0)
          | .favicon = favicon_path(.address)
          | .favicon_small = favicon_path(.address)
          | .has_favicon = true
        )
    ' "$fixture" > "$data_file"

  sed \
    -e "s/preferred-width: 1320px/preferred-width: ${width}px/" \
    -e "s/preferred-height: 800px/preferred-height: ${height}px/" \
    -e 's/in-out property <string> theme_mode: "system";/in-out property <string> theme_mode: "system";\n    in-out property <string> screenshot_theme_preset: "default";/' \
    -e 's/    changed theme-mode => {/    changed screenshot-theme-preset => { AppTheme.preset = root.screenshot-theme-preset; }\n\n    changed theme-mode => {/' \
    "$ui" > "$temporary_ui"

  SLINT_SCALE_FACTOR=2 slint-viewer \
    --style "$style" \
    --load-data "$data_file" \
    --screenshot "$output_dir/$output_name.png" \
    "$temporary_ui"
  rm -f "$temporary_ui"
  temporary_ui=""
}

render desktop-light 1320 800 light
render desktop-dark 1320 800 dark
render desktop-minimal-light 1320 800 light minimal
render desktop-minimal-dark 1320 800 dark minimal
render desktop-teal-light 1320 800 light default teal
render desktop-green-light 1320 800 light default green
render desktop-purple-light 1320 800 light default purple
render desktop-teal-dark 1320 800 dark default teal
render desktop-green-dark 1320 800 dark default green
render desktop-purple-dark 1320 800 dark default purple
render desktop-calendar-light 1320 800 light default default calendar
render desktop-contacts-light 1320 800 light default default contacts
render mobile-light 390 844 light
render mobile-dark 390 844 dark
