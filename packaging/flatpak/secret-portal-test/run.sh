#!/usr/bin/env bash
# Tests the Flatpak's credentials against a real Secret portal, with GNOME
# Keyring as its backend. Run it inside dbus-run-session after installing
# com.flectar.mail.yml, as the flatpak-secrets job in quality.yml does.
set -euo pipefail
mkdir -p ~/.config/xdg-desktop-portal
printf '[preferred]\ndefault=gtk;\norg.freedesktop.impl.portal.Secret=gnome-keyring;\n' > ~/.config/xdg-desktop-portal/portals.conf
export XDG_CURRENT_DESKTOP=GNOME
printf 'e2e-keyring-password' | gnome-keyring-daemon --unlock --components=secrets --daemonize
test -f /usr/share/xdg-desktop-portal/portals/gnome-keyring.portal
app() { flatpak run "$@" 2>&1 | tail -1; }
expect() {
  local want=$1 got
  shift
  got=$(app "$@")
  echo "$* -> $got"
  if [[ "$got" != "$want" ]]; then
    echo "FAIL: expected $want" >&2
    exit 1
  fi
}

# An earlier bundle with keyring access stored a credential there.
expect "OK seeded" --talk-name=org.freedesktop.secrets com.flectar.mail seed-keyring
expect "OK old-keyring-secret" --talk-name=org.freedesktop.secrets com.flectar.mail keyring-load
# Flathub's build: no keyring access, so this works only through the Secret portal.
expect "OK stored" com.flectar.mail store
expect "OK e2e-portal-secret" com.flectar.mail load
sealed=$(find ~/.var/app/com.flectar.mail -name credentials.sealed)
echo "sealed file: $sealed"
test -s "$sealed"
if grep -a -q -e e2e-portal-secret -e 1:password "$sealed"; then echo "FAIL: plaintext in the sealed file" >&2; exit 1; fi
head -c 8 "$sealed"; echo
# Without keyring access the old credential is out of reach, and nothing breaks.
expect "ERR auth failed: no stored credential" com.flectar.mail load-moved
# The bundle with keyring access moves it into the sealed file and off the keyring.
expect "OK old-keyring-secret" --talk-name=org.freedesktop.secrets com.flectar.mail load-moved
expect "OK old-keyring-secret" com.flectar.mail load-moved
keyring_after=$(app --talk-name=org.freedesktop.secrets com.flectar.mail keyring-load)
echo "keyring after the move -> $keyring_after"
[[ "$keyring_after" == ERR* ]] || { echo "FAIL: the keyring still holds the moved credential" >&2; exit 1; }
echo "PASS secret portal end to end"
