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
  echo "Cargo is required to generate sender favicons" >&2
  exit 1
}
command -v jq >/dev/null 2>&1 || {
  echo "jq is required to prepare screenshot fixtures" >&2
  exit 1
}

slint-viewer --check "$ui"

demo_email_svg="$output_dir/demo-email.svg"
demo_email_png="$temporary_dir/demo-email.png"
demo_email_ui="$temporary_dir/demo-email.slint"
ln -s "$demo_email_svg" "$temporary_dir/demo-email.svg"
cat > "$demo_email_ui" <<'EOF'
export component DemoEmail inherits Window {
    preferred-width: 720px;
    preferred-height: 760px;

    Image {
        width: 100%;
        height: 100%;
        source: @image-url("demo-email.svg");
        image-fit: fill;
    }
}
EOF
SLINT_SCALE_FACTOR=2 slint-viewer \
  --style fluent \
  --screenshot "$demo_email_png" \
  "$demo_email_ui"

favicon_dir="$temporary_dir/favicons"
mapfile -t sender_addresses < <(
  jq -r '[.selected_address, (.emails[].address), (.thread_messages[]? | select(.outgoing | not) | .address)] | unique[]' "$fixture"
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
  local show_threads="${8:-false}"
  local show_account_markers="${9:-false}"
  local settings_tab="${10:-}"
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
    --arg settings_tab "$settings_tab" \
    --arg favicon_dir "$favicon_dir" \
    --arg demo_email "$demo_email_png" \
    --argjson show_threads "$show_threads" \
    --argjson show_account_markers "$show_account_markers" \
    '
      def favicon_path(address):
        ($favicon_dir + "/" + (address | split("@") | last | ascii_downcase) + ".png");
      .theme_mode = $theme
      | .["MotionSettings.enabled"] = false
      | .workspace_layout = $workspace_layout
      | .screenshot_theme_preset = $theme_preset
      | .active_view = $active_view
      | .settings_open = ($settings_tab != "")
      | .settings_tab = (if $settings_tab == "" then (.settings_tab // "General") else $settings_tab end)
      | .text_mode = false
      | .show_account_markers = $show_account_markers
      | .storage_loading = false
      | .storage_loaded = true
      | .storage_total = "18.42 GB"
      | .storage_error = ""
      | .storage_usage = [
          {"kind": "mail", "size": "9.84 GB", "fraction": 0.5342, "offset": 0, "color": "#3b82f6"},
          {"kind": "attachments", "size": "5.21 GB", "fraction": 0.2828, "offset": 0.5342, "color": "#f59e0b"},
          {"kind": "files", "size": "1.76 GB", "fraction": 0.0955, "offset": 0.817, "color": "#22c55e"},
          {"kind": "databases", "size": "1.34 GB", "fraction": 0.0727, "offset": 0.9125, "color": "#a855f7"},
          {"kind": "other", "size": "279.6 MB", "fraction": 0.0148, "offset": 0.9852, "color": "#94a3b8"}
        ]
      | .email_tiles |= map(.image = $demo_email)
      | .selected_favicon = favicon_path(.selected_address)
      | .selected_has_favicon = true
      | .connected_accounts |= map(
          .mail_protocol = (.mail_protocol // "")
          | .profile_id = (if .id == 2 then "support" else "work" end)
          | .profile_name = (if .id == 2 then "Support" else "Work" end)
          | .profile_color = (if .id == 2 then "#ea580c" else "#8b5cf6" end)
          | .has_profile = true
          | .has_account_color = false
          | .jmap_url = (.jmap_url // "")
          | .imap_security = (.imap_security // "auto")
          | .smtp_security = (.smtp_security // "auto")
          | .trusted_certificate_pem = (.trusted_certificate_pem // "")
          | .mail_history = (.mail_history // "sixMonths")
          | .caldav_url = (.caldav_url // "")
          | .caldav_username = (.caldav_username // "")
          | .carddav_connected = (.carddav_connected // false)
          | .carddav_enabled = (.carddav_enabled // false)
          | .carddav_status = (.carddav_status // "")
          | .carddav_url = (.carddav_url // "")
          | .carddav_username = (.carddav_username // "")
        )
      | .mail_profiles = [
          {"id": "work", "name": "Work", "color": "#8b5cf6", "account_count": 1},
          {"id": "support", "name": "Support", "color": "#ea580c", "account_count": 1}
        ]
      | .sidebar_rows |= map(
          .mailbox.is_selectable = (if .mailbox.is_selectable == null then true else .mailbox.is_selectable end)
          | .mailbox.can_create_children = (.mailbox.can_create_children // false)
          | .mailbox.can_rename = (.mailbox.can_rename // false)
          | .mailbox.can_delete = (.mailbox.can_delete // false)
          | .label.account_id = (.label.account_id // -1)
          | .label.account_name = (.label.account_name // "")
          | .label.display_name = (.label.display_name // .label.name // "")
          | .label.depth = (.label.depth // 0)
          | .label.has_children = (.label.has_children // false)
          | .label.expanded = (if .label.expanded == null then true else .label.expanded end)
          | .label.can_create_children = (.label.can_create_children // false)
          | .label.is_global = (.label.is_global // false)
        )
      | .mail_labels |= map(
          .account_id = (.account_id // -1)
          | .account_name = (.account_name // "")
          | .display_name = (.display_name // .name // "")
          | .depth = (.depth // 0)
          | .has_children = (.has_children // false)
          | .expanded = (if .expanded == null then true else .expanded end)
          | .can_create_children = (.can_create_children // false)
          | .is_global = (.is_global // false)
        )
      | .mail_label_results |= map(
          .account_id = (.account_id // -1)
          | .account_name = (.account_name // "")
          | .display_name = (.display_name // .name // "")
          | .depth = (.depth // 0)
          | .has_children = (.has_children // false)
          | .expanded = (if .expanded == null then true else .expanded end)
          | .can_create_children = (.can_create_children // false)
          | .is_global = (.is_global // false)
        )
      | (.thread_messages // [] | length) as $thread_count
      | .emails |= map(
          .has_replied = (.has_replied // false)
          | .message_count = (if $show_threads and .selected then ([1, $thread_count] | max) else (.message_count // 1) end)
          | .checked = (.checked // false)
          | .account_id = (if .account == "Flectar Support" then 2 else 1 end)
          | .account_color = (if .account == "Flectar Support" then "#ea580c" else "#8b5cf6" end)
          | .account_profile = (if .account == "Flectar Support" then "Support" else "Work" end)
          | .show_account_marker = $show_account_markers
          | .labels |= map(
              .account_id = (.account_id // -1)
              | .account_name = (.account_name // "")
              | .display_name = (.display_name // .name // "")
              | .depth = (.depth // 0)
              | .has_children = (.has_children // false)
              | .expanded = (if .expanded == null then true else .expanded end)
              | .can_create_children = (.can_create_children // false)
              | .is_global = (.is_global // false)
            )
          | .favicon = favicon_path(.address)
          | .favicon_small = favicon_path(.address)
          | .has_favicon = true
        )
      | .selected_address as $selected_address
      | .thread_messages = (if $show_threads then (.thread_messages // []) else [] end)
      | .thread_messages |= map(
          .favicon = favicon_path(if .outgoing then $selected_address else .address end)
          | .favicon_small = favicon_path(if .outgoing then $selected_address else .address end)
          | .has_favicon = (.outgoing | not)
        )
      | .selected_thread_index = (if $show_threads then (.selected_thread_index // 0) else 0 end)
    ' "$fixture" > "$data_file"

  sed \
    -e "s/preferred-width: 1320px/preferred-width: ${width}px/" \
    -e "s/preferred-height: 800px/preferred-height: ${height}px/" \
    -e "s/in-out property <string> theme_mode: \"system\";/in-out property <string> theme_mode: \"${theme}\";\n    in-out property <string> screenshot_theme_preset: \"${theme_preset}\";/" \
    -e 's/    changed theme-mode => {/    init => {\n        AppTheme.preset = root.screenshot-theme-preset;\n        Palette.color-scheme = root.theme-mode == "dark" ? ColorScheme.dark\n            : root.theme-mode == "light" ? ColorScheme.light\n            : ColorScheme.unknown;\n    }\n\n    changed screenshot-theme-preset => { AppTheme.preset = root.screenshot-theme-preset; }\n\n    changed theme-mode => {/' \
    "$ui" > "$temporary_ui"

  echo "Rendering $output_name.png"
  # Slint can snapshot before runtime-loaded images have populated the
  # software renderer's cache. A discarded first render keeps the committed
  # screenshot deterministic without replacing editable SVG sources.
  SLINT_SCALE_FACTOR=2 slint-viewer \
    --style "$style" \
    --size "${width}x${height}" \
    --load-data "$data_file" \
    --screenshot "$temporary_dir/$output_name-warmup.png" \
    "$temporary_ui"
  SLINT_SCALE_FACTOR=2 slint-viewer \
    --style "$style" \
    --size "${width}x${height}" \
    --load-data "$data_file" \
    --screenshot "$output_dir/$output_name.png" \
    "$temporary_ui"
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
