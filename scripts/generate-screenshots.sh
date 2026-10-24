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
  echo "slint-viewer 1.18+ is required" >&2
  exit 1
}
command -v cargo >/dev/null 2>&1 || {
  echo "Cargo is required to generate sender favicons and window controls" >&2
  exit 1
}
command -v jq >/dev/null 2>&1 || {
  echo "jq is required to prepare screenshot fixtures" >&2
  exit 1
}

slint-viewer --check "$ui"

# The PDF preview is an SVG fixture, not a rendered PDF. Derive its dark
# paper, ink, rules, and accent colors. Real PDF previews retain the
# document's original colors.
pdf_preview_light="$repo_dir/resources/screenshots/demo-pdf-preview.svg"
pdf_preview_dark="$temporary_dir/demo-pdf-preview-dark.svg"
sed \
  -e 's/#FFFFFF/#202328/g' \
  -e 's/#20262D/#ECEFF3/g' \
  -e 's/#475361/#C0C7D0/g' \
  -e 's/#626E7B/#A5AFBB/g' \
  -e 's/#C8D0D9/#46515E/g' \
  -e 's/#17365D/#A7C7F0/g' \
  -e 's/#EEF2F6/#2C343E/g' \
  "$pdf_preview_light" > "$pdf_preview_dark"

favicon_dir="$temporary_dir/favicons"
mapfile -t sender_addresses < <(
  jq -r '[.selected_address, (.emails[].address), (.thread_messages[]? | select(.outgoing | not) | .address)] | unique[]' "$fixture"
)
cargo run --quiet --example generate-screenshot-favicons -- \
  "$favicon_dir" "${sender_addresses[@]}"

prepare_data() {
  local output_name="$1"
  local width="$2"
  local height="$3"
  local theme="$4"
  local workspace_layout="${5:-default}"
  local theme_preset="${6:-default}"
  local active_view="${7:-mail}"
  local show_threads="${8:-false}"
  local show_account_markers="${9:-false}"
  local settings_tab="${10:-}"
  local calendar_mode="${11:-week}"
  local tab_strip="${12:-default}"
  local window_frame=""
  local data_file="$temporary_dir/$output_name.json"
  local email_file="$email_dir/demo-email-$theme_preset-$theme.json"
  local email="null"
  local pdf_preview="$pdf_preview_light"
  [[ "$theme" != "dark" ]] || pdf_preview="$pdf_preview_dark"
  # Desktop screenshots show the integrated macOS frame; the traffic lights
  # are painted after rendering because AppKit draws them natively.
  [[ "$output_name" != desktop-* ]] || window_frame="macos"
  [[ ! -f "$email_file" ]] || email="$(<"$email_file")"

  jq \
    --arg theme "$theme" \
    --arg workspace_layout "$workspace_layout" \
    --arg theme_preset "$theme_preset" \
    --arg active_view "$active_view" \
    --arg calendar_mode "$calendar_mode" \
    --arg settings_tab "$settings_tab" \
    --arg tab_strip "$tab_strip" \
    --arg window_frame "$window_frame" \
    --arg favicon_dir "$favicon_dir" \
    --arg pdf_preview "$pdf_preview" \
    --argjson show_threads "$show_threads" \
    --argjson show_account_markers "$show_account_markers" \
    --argjson email "$email" \
    -f "$repo_dir/scripts/prepare-screenshot-data.jq" "$fixture" > "$data_file"

}

# The reader renders the demo message once per theme preset and scheme, at
# the reading pane's width, so dark screenshots show the adapted body.
email_dir="$temporary_dir/email"
echo "Rendering demo-email bodies"
prepare_data email 1320 800 light
FLECTAR_SCREENSHOT_DATA="$temporary_dir/email.json" \
FLECTAR_EMAIL_SCREENSHOTS="$email_dir" cargo test --quiet --locked \
  -p flectar-mail --lib screenshot_email::email_screenshots -- --ignored --exact

# Desktop screenshots that still need their macOS traffic lights.
macos_screenshots=()

render() {
  local output_name="$1"
  local width="$2"
  local height="$3"
  local theme="$4"
  local theme_preset="${6:-default}"
  local style="fluent"
  local data_file="$temporary_dir/$output_name.json"
  local email_tiles
  [[ "$theme" != "dark" ]] || style="fluent-dark"
  temporary_ui="$(mktemp "$repo_dir/ui/.screenshot-app.XXXXXX.slint")"
  prepare_data "$@"

  # Compile the body images into the preview. Runtime image properties can
  # still be empty when the headless viewer takes its first snapshot.
  email_tiles="$(jq -r '
    [.email_tiles[] | "{image: @image-url(" + (.image | tojson)
      + "), y: " + (.y | tostring) + ", height: " + (.height | tostring) + "}"]
    | "[" + join(", ") + "]"
  ' "$data_file")"
  jq 'del(.email_tiles)' "$data_file" > "$data_file.tmp"
  mv "$data_file.tmp" "$data_file"

  sed \
    -e "s|in property <\[EmailTile\]> email_tiles: \[\];|in property <[EmailTile]> email_tiles: ${email_tiles};|" \
    -e "s/preferred-width: 1320px/preferred-width: ${width}px/" \
    -e "s/preferred-height: 800px/preferred-height: ${height}px/" \
    -e "s/in-out property <string> theme_mode: \"system\";/in-out property <string> theme_mode: \"${theme}\";\n    in-out property <string> screenshot_theme_preset: \"${theme_preset}\";/" \
    -e 's/    changed theme-mode => {/    init => {\n        AppTheme.preset = root.screenshot-theme-preset;\n        Palette.color-scheme = root.theme-mode == "dark" ? ColorScheme.dark\n            : root.theme-mode == "light" ? ColorScheme.light\n            : ColorScheme.unknown;\n    }\n\n    changed screenshot-theme-preset => { AppTheme.preset = root.screenshot-theme-preset; }\n\n    changed theme-mode => {/' \
    "$ui" > "$temporary_ui"

  echo "Rendering $output_name.png"
  SLINT_SCALE_FACTOR=2 slint-viewer \
    --style "$style" \
    --size "${width}x${height}" \
    --load-data "$data_file" \
    --screenshot "$output_dir/$output_name.png" \
    "$temporary_ui"
  [[ "$output_name" != desktop-* ]] || macos_screenshots+=("$output_dir/$output_name.png")
  rm -f "$temporary_ui"
  temporary_ui=""
}

# Set SCREENSHOT_ONLY to one or more comma-separated output names when
# refreshing selected assets.
render_selected() {
  local output_name="$1"
  local screenshot_only="${SCREENSHOT_ONLY:-}"
  if [[ -z "$screenshot_only" || ",${screenshot_only}," == *",${output_name},"* ]]; then
    render "$@"
  fi
}

render_selected desktop-light 1320 800 light
render_selected desktop-dark 1320 800 dark
render_selected desktop-profiles-light 1320 800 light default default mail false true
render_selected desktop-profiles-dark 1320 800 dark default default mail false true
render_selected desktop-thread-light 1320 800 light default default mail true
render_selected desktop-thread-dark 1320 800 dark default default mail true
render_selected desktop-tabs-light 1320 800 light default default mail false false "" week showcase
render_selected desktop-tabs-dark 1320 800 dark default default mail false false "" week showcase
render_selected desktop-minimal-light 1320 800 light minimal
render_selected desktop-minimal-dark 1320 800 dark minimal
render_selected desktop-teal-light 1320 800 light default teal
render_selected desktop-green-light 1320 800 light default green
render_selected desktop-purple-light 1320 800 light default purple
render_selected desktop-teal-dark 1320 800 dark default teal
render_selected desktop-green-dark 1320 800 dark default green
render_selected desktop-purple-dark 1320 800 dark default purple
render_selected desktop-calendar-light 1320 800 light default default calendar
render_selected desktop-calendar-dark 1320 800 dark default default calendar
render_selected desktop-contacts-light 1320 800 light default default contacts
render_selected desktop-contacts-dark 1320 800 dark default default contacts
render_selected desktop-files-light 1320 800 light default default files
render_selected desktop-files-dark 1320 800 dark default default files
render_selected desktop-storage-light 1320 800 light default default mail false false Data
render_selected desktop-storage-dark 1320 800 dark default default mail false false Data
render_selected mobile-light 390 844 light
render_selected mobile-dark 390 844 dark

render_selected desktop-agenda-light 1320 800 light default default calendar false false "" agenda
render_selected desktop-agenda-dark 1320 800 dark default default calendar false false "" agenda
render_selected tablet-agenda-light 768 1024 light default default calendar false false "" agenda
render_selected tablet-agenda-dark 768 1024 dark default default calendar false false "" agenda
render_selected mobile-agenda-light 390 844 light default default calendar false false "" agenda
render_selected mobile-agenda-dark 390 844 dark default default calendar false false "" agenda

if (( ${#macos_screenshots[@]} > 0 )); then
  cargo run --quiet --example paint-screenshot-traffic-lights -- "${macos_screenshots[@]}"
fi

# The integrated window frame is rendered from the real application window
# by an ignored test, because each platform's window controls are chosen at
# runtime rather than in the static Slint preview.
if [[ -z "${SCREENSHOT_ONLY:-}" || ",${SCREENSHOT_ONLY}," == *",frameless,"* ]]; then
  echo "Rendering frameless/*.png"
  prepare_data frameless 1320 800 light
  FLECTAR_SCREENSHOT_DATA="$temporary_dir/frameless.json" \
  FLECTAR_FRAMELESS_SCREENSHOTS="$output_dir/frameless" cargo test --quiet --locked \
    -p flectar-mail --lib screenshot_frameless::frameless_screenshots -- --ignored --exact
fi
