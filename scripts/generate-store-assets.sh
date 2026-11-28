#!/usr/bin/env bash
# Render the Google Play listing's images from the app's current interface
# into fastlane/metadata/android/en-US/images: the phone screenshots and the
# feature graphic. The app shows the invented inbox and contacts from
# resources/store, because Play's listing rules don't allow other brands'
# names and logos without permission. Run it after interface changes, as
# scripts/generate-screenshots.sh, and commit the images it writes.
#
# The feature graphic is set in Flectar's brand fonts, which stay in the
# private flectar.com repository and never enter this one. The render reads
# them from a checkout of it, FLECTAR_SITE or else flectar.com next to the
# main checkout of this repository. Without one, the script renders the
# screenshots and leaves the feature graphic as it is.
#
# Needs Cargo, which builds the app to draw the open message the way its
# reader does, slint-viewer 1.19 or newer, jq, resvg (cargo install resvg)
# and Chromium. Set CHROME when Chromium isn't google-chrome, chromium or
# chromium-browser on PATH.
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
store_dir="$repo_dir/resources/store"
images_dir="$repo_dir/fastlane/metadata/android/en-US/images"
ui="$repo_dir/ui/app.slint"
google_sans="$repo_dir/resources/fonts/google-sans-flex/GoogleSansFlex-VariableFont_GRAD,ROND,opsz,slnt,wdth,wght.ttf"
temporary_dir="$(mktemp -d)"
temporary_ui=""
trap 'rm -rf "$temporary_dir"; test -z "$temporary_ui" || rm -f "$temporary_ui"' EXIT

for tool in cargo slint-viewer jq resvg; do
  command -v "$tool" >/dev/null 2>&1 || {
    echo "$tool is required" >&2
    exit 1
  }
done
chrome="${CHROME:-}"
for candidate in google-chrome chromium chromium-browser; do
  [[ -n "$chrome" ]] || chrome="$(command -v "$candidate" || true)"
done
[[ -n "$chrome" ]] || {
  echo "Chromium is required; set CHROME to its executable" >&2
  exit 1
}

# Worktrees share the main checkout's git directory, so flectar.com is found
# next to the main checkout from any of them.
site="${FLECTAR_SITE:-}"
if [[ -z "$site" ]]; then
  git_dir="$(git -C "$repo_dir" rev-parse --path-format=absolute --git-common-dir 2>/dev/null ||
    echo "$repo_dir/.git")"
  site="$(dirname "$(dirname "$git_dir")")/flectar.com"
fi
brand_fonts="$site/src/assets/fonts"
if [[ -f "$brand_fonts/FlectarSerif-Regular.woff2" && -f "$brand_fonts/FlectarGrotesk-VF.woff2" ]]; then
  feature_graphic=true
elif [[ -n "${FLECTAR_SITE:-}" ]]; then
  echo "FLECTAR_SITE has no Flectar Serif and Flectar Grotesk in src/assets/fonts" >&2
  exit 1
else
  feature_graphic=false
fi

slint-viewer --edition 1.19 --check "$ui"

# The sender pictures, named after the domains the screenshot data looks up.
senders="$temporary_dir/senders"
mkdir -p "$senders"
for svg in "$store_dir"/senders/*.svg; do
  resvg --width 128 --height 128 "$svg" "$senders/$(basename "$svg" .svg).png"
done

# The list screenshots open the first message; the reader's opens the
# airline's check-in email, resources/store/store-email.html.
data="$temporary_dir/store-data.json"
message_data="$temporary_dir/store-message-data.json"
jq --arg open "" -f "$store_dir/store-data.jq" "$repo_dir/resources/screenshots/demo-data.json" > "$data"
jq --arg open hello@kite.example -f "$store_dir/store-data.jq" \
  "$repo_dir/resources/screenshots/demo-data.json" > "$message_data"
# slint-viewer drops a whole list when one of its pictures is missing.
missing="$(jq -r '[.selected_address, .emails[].address, .contacts[].email]
  | map(split("@") | last) | unique[]' "$data" "$message_data" | sort -u | while read -r domain; do
  [[ -f "$senders/$domain.png" ]] || printf '%s ' "$domain"
done)"
if [[ -n "$missing" ]]; then
  echo "resources/store/senders has no picture for: $missing" >&2
  exit 1
fi

# capture PAGE WIDTH HEIGHT SCALE OUTPUT
# WIDTH and HEIGHT are in CSS pixels; the image is SCALE times larger. Where
# the page draws nothing, the image is transparent.
capture() {
  local page="$1" width="$2" height="$3" scale="$4" output="$5"
  "$chrome" --headless=new --no-sandbox --disable-gpu --hide-scrollbars \
    --force-device-scale-factor="$scale" --window-size="$width,$height" \
    --default-background-color=00000000 --virtual-time-budget=5000 \
    --screenshot="$output" "file://$page" >/dev/null 2>&1
}

# prepare NAME THEME VIEW CALENDAR_MODE DATA [EMAIL]
# Writes what slint-viewer loads for one screenshot to $temporary_dir/NAME.
# EMAIL is the reader's projection of the open message's body.
prepare() {
  local name="$1" theme="$2" view="$3" calendar_mode="$4" source="$5" email="${6:-null}"
  mkdir -p "$temporary_dir/$name"
  jq \
    --arg theme "$theme" \
    --arg workspace_layout default \
    --arg theme_preset default \
    --arg active_view "$view" \
    --arg calendar_mode "$calendar_mode" \
    --arg settings_tab "" \
    --arg tab_strip default \
    --arg window_frame "" \
    --arg favicon_dir "$senders" \
    --arg pdf_preview "$repo_dir/resources/screenshots/demo-pdf-preview.svg" \
    --argjson show_threads false \
    --argjson show_account_markers false \
    --argjson email "$email" \
    -f "$repo_dir/scripts/prepare-screenshot-data.jq" "$source" |
    jq --arg senders "$senders" -f "$store_dir/store-view.jq" > "$temporary_dir/$name/data.json"
}

# shot NAME THEME [PAGE]
# Renders the app from NAME's data at 400 x 820 dp, two pixels per dp, puts it
# on a phone's screen and that on the 1080 x 1920 canvas, the 9:16 that Play
# asks for. With PAGE "message", the phone shows the reader, not the list.
shot() {
  local name="$1" theme="$2" page="${3:-list}"
  local style="fluent" directory="$temporary_dir/$name"
  [[ "$theme" != dark ]] || style="fluent-dark"

  temporary_ui="$(mktemp "$repo_dir/ui/.store-app.XXXXXX.slint")"
  sed \
    -e "s/preferred-width: 1320px/preferred-width: 400px/" \
    -e "s/preferred-height: 800px/preferred-height: 820px/" \
    -e "s/private property <string> mail-compact-page: \"list\";/private property <string> mail-compact-page: \"${page}\";/" \
    -e "s/in-out property <string> theme_mode: \"system\";/in-out property <string> theme_mode: \"${theme}\";/" \
    -e 's/    changed theme-mode => {/    init => {\n        Palette.color-scheme = root.theme-mode == "dark" ? ColorScheme.dark\n            : root.theme-mode == "light" ? ColorScheme.light\n            : ColorScheme.unknown;\n    }\n\n    changed theme-mode => {/' \
    "$ui" > "$temporary_ui"

  echo "Rendering $name"
  SLINT_SCALE_FACTOR=2 slint-viewer \
    --edition 1.19 \
    --style "$style" \
    --size 400x820 \
    --load-data "$directory/data.json" \
    --screenshot "$directory/app.png" \
    "$temporary_ui"
  rm -f "$temporary_ui"
  temporary_ui=""

  cp "$store_dir/screen.html" "$store_dir/screenshot.html" "$directory/"
  cp "$google_sans" "$directory/GoogleSansFlex.ttf"
  capture "$directory/screen.html#$theme" 400 860 2 "$directory/screen.png"
  capture "$directory/screenshot.html" 1080 1920 1 "$directory/screenshot.png"
  python3 "$repo_dir/scripts/png-opaque.py" "$directory/screenshot.png" "$images_dir/phoneScreenshots/$name.png"
}

prepare 1-inbox light mail week "$data"
prepare 3-calendar light calendar agenda "$data"
prepare 4-contacts light contacts week "$data"
prepare 5-inbox-dark dark mail week "$data"

# The open message's body, drawn by the app's own reader at the width a
# phone lays it out at, as scripts/generate-screenshots.sh does for the
# README's screenshots.
prepare 2-message light mail week "$message_data"
echo "Rendering the open message's body"
FLECTAR_SCREENSHOT_DATA="$temporary_dir/2-message/data.json" \
FLECTAR_EMAIL_SCREENSHOTS="$temporary_dir/email" cargo test --quiet --locked \
  --manifest-path "$repo_dir/Cargo.toml" -p flectar-mail --lib \
  screenshot_email::store_email_screenshot -- --ignored --exact
prepare 2-message light mail week "$message_data" "$(<"$temporary_dir/email/store-email.json")"

# Play shows the screenshots in file name order.
mkdir -p "$images_dir/phoneScreenshots"
rm -f "$images_dir"/phoneScreenshots/*.png
shot 1-inbox light
shot 2-message light message
shot 3-calendar light
shot 4-contacts light
shot 5-inbox-dark dark

# The brand fonts are copied for the render only and leave with the
# temporary directory.
if [[ "$feature_graphic" == true ]]; then
  graphic="$temporary_dir/graphic"
  mkdir -p "$graphic/fonts"
  cp "$store_dir/feature-graphic.html" "$temporary_dir/1-inbox/screen.png" "$graphic/"
  cp "$repo_dir/resources/app-icon/flectar-mail-masked.svg" "$graphic/icon.svg"
  cp "$brand_fonts/FlectarSerif-Regular.woff2" "$brand_fonts/FlectarGrotesk-VF.woff2" "$graphic/fonts/"
  echo "Rendering the feature graphic"
  capture "$graphic/feature-graphic.html" 1024 500 1 "$temporary_dir/feature-graphic.png"
  python3 "$repo_dir/scripts/png-opaque.py" "$temporary_dir/feature-graphic.png" "$images_dir/featureGraphic.png"
else
  echo "Left the feature graphic as it is, since its brand fonts need a flectar.com checkout (FLECTAR_SITE)" >&2
fi

python3 "$repo_dir/scripts/check-store-listing.py"
