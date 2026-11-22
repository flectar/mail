#!/usr/bin/env bash
# Run only in a disposable container of a supported RPM distribution (Fedora,
# openSUSE or an Enterprise Linux rebuild): installs packages and a test user.
# Installs Flectar Mail from the signed repository scripts/build-rpm-repo.sh
# builds, through its published .repo file pointed at the local copy, with
# package and metadata signature checks on.
# Usage: bash scripts/test-rpm.sh /absolute/path/rpm-repo /results
set -euo pipefail

# shellcheck disable=SC1091
. /etc/os-release
case " $ID ${ID_LIKE:-} " in
  *" suse "* | *" opensuse "*) family=suse ;;
  *" fedora "* | *" rhel "*) family=fedora ;;
  *)
    printf 'Unsupported distribution: %s\n' "$ID" >&2
    exit 1
    ;;
esac
# Weak dependencies stay off so they cannot hide missing requirements.
install_packages() {
  if [[ "$family" == suse ]]; then
    zypper --non-interactive install --no-recommends "$@"
  else
    dnf install -y --setopt=install_weak_deps=False "$@"
  fi
}
remove_packages() {
  if [[ "$family" == suse ]]; then
    zypper --non-interactive remove "$@"
  else
    dnf remove -y "$@"
  fi
}
printf 'Testing on %s\n' "${PRETTY_NAME:-$ID}"

project_dir="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
repo="$(realpath "${1:?Repository directory is required}")"
results="$(realpath "${2:?Results directory is required}")"
mkdir -p "$results"
if rpm -q flectar-mail; then
  printf 'Expected a clean container without Flectar Mail installed.\n' >&2
  exit 1
fi
# openSUSE's base image has no find, so pick the package with a glob.
packages=("$repo"/flectar-mail-*."$(uname -m)".rpm)
package=${packages[0]}
test -f "$package"

rpm -qip "$package"
rpm -qp --requires "$package" | tee "$results/requires.txt"
# The published .repo file, served from the local copy instead.
sed -e "s|^baseurl=.*|baseurl=file://$repo|" \
  -e "s|^gpgkey=.*|gpgkey=file://$repo/RPM-GPG-KEY-flectar-mail|" \
  "$repo/flectar-mail.repo" >"$results/flectar-mail.repo"
grep -qx 'gpgcheck=1' "$results/flectar-mail.repo"
grep -qx 'repo_gpgcheck=1' "$results/flectar-mail.repo"
if [[ "$family" == suse ]]; then
  cp "$results/flectar-mail.repo" /etc/zypp/repos.d/flectar-mail.repo
  zypper --non-interactive --gpg-auto-import-keys refresh flectar-mail
else
  cp "$results/flectar-mail.repo" /etc/yum.repos.d/flectar-mail.repo
fi
# Resolve dependencies before adding any test tools. Build packages and weak
# dependencies must not hide missing runtime requirements.
install_packages flectar-mail
rpm -V flectar-mail
# Installed through signature checks, by the repository's key.
# rpmsign signs the header alone on current rpm, the whole package on older.
rpm -q --queryformat '%|RSAHEADER?{%{RSAHEADER:pgpsig}}:{%|SIGPGP?{%{SIGPGP:pgpsig}}:{(none)}|}|\n' \
  flectar-mail | tee "$results/signature.txt"
if grep -q '(none)' "$results/signature.txt"; then
  printf 'Installed an unsigned package.\n' >&2
  exit 1
fi
test -x /usr/bin/flectar-mail
test -f /usr/lib64/flectar-mail/libpdfium.so
rpm -qa | sort > "$results/installed-packages.txt"

# Exercise dynamically loaded libraries and the installed PDF worker before
# desktop test dependencies can supply libraries missing from our Requires.
if [[ "$family" == suse ]]; then
  install_packages python3 shadow util-linux
else
  install_packages python3 shadow-utils util-linux
fi
python3 - <<'PY'
import ctypes

for library in (
    "libfontconfig.so.1", "libX11.so.6", "libX11-xcb.so.1",
    "libXcursor.so.1", "libXi.so.6", "libwayland-client.so.0",
    "libxkbcommon.so.0", "libxkbcommon-x11.so.0",
):
    ctypes.CDLL(library)
    print(f"Loaded runtime library: {library}")
PY
useradd --create-home flectar-rpm-test
chown flectar-rpm-test:flectar-rpm-test "$results"
runuser -u flectar-rpm-test -- python3 "$project_dir/scripts/test-pdf-preview.py" \
  /usr/bin/flectar-mail "$results/pdf-preview.png"

# Enterprise Linux 10 dropped the X server, Xvfb included, and runs X11
# programs through Xwayland, so it checks Wayland startup only.
x11=1
if [[ "$family" == suse ]]; then
  install_packages AppStream desktop-file-utils dbus-1-daemon \
    xorg-x11-server-Xvfb xvfb-run xauth weston
elif [[ "$ID" == fedora ]]; then
  install_packages appstream desktop-file-utils dbus-daemon \
    xorg-x11-server-Xvfb xorg-x11-xauth weston
else
  # Enterprise Linux ships weston in EPEL, which needs CodeReady Builder.
  dnf install -y dnf-plugins-core epel-release
  dnf config-manager --set-enabled crb
  install_packages appstream desktop-file-utils dbus-daemon weston
  x11=0
fi
desktop-file-validate /usr/share/applications/com.flectar.mail.desktop
appstreamcli validate --no-net /usr/share/metainfo/com.flectar.mail.metainfo.xml

# Require an actual rendered frame and a successful exit, rather than treating
# an application that hangs for 15 seconds as a successful launch.
startup=(python3 "$project_dir/scripts/benchmark-startup.py"
  --binary /usr/bin/flectar-mail --rounds 1 --settle-seconds 0 --idle-seconds 1)
if ((x11)); then
  # Ask for the 24-bit screen desktops use. openSUSE's xvfb-run defaults to an
  # 8-bit one, which softbuffer cannot draw to.
  runuser -u flectar-rpm-test -- dbus-run-session -- xvfb-run -a -s '-screen 0 1280x1024x24' \
    "${startup[@]}" --output "$results/x11.json"
fi

runtime_dir="$(mktemp -d)"
chown flectar-rpm-test:flectar-rpm-test "$runtime_dir"
runuser -u flectar-rpm-test -- env XDG_RUNTIME_DIR="$runtime_dir" \
  weston --backend=headless --renderer=pixman --socket=wayland-rpm --idle-time=0 \
  --no-config > "$results/wayland-compositor.log" 2>&1 &
weston_pid=$!
trap 'kill "$weston_pid" 2>/dev/null || true; wait "$weston_pid" 2>/dev/null || true; rm -rf "$runtime_dir"' EXIT
for ((attempt = 0; attempt < 100; attempt++)); do
  if [[ -S "$runtime_dir/wayland-rpm" ]]; then break; fi
  if ! kill -0 "$weston_pid" 2>/dev/null; then
    cat "$results/wayland-compositor.log" >&2
    exit 1
  fi
  sleep 0.1
done
test -S "$runtime_dir/wayland-rpm"
runuser -u flectar-rpm-test -- env -u DISPLAY \
  XDG_RUNTIME_DIR="$runtime_dir" WAYLAND_DISPLAY=wayland-rpm \
  dbus-run-session -- "${startup[@]}" --output "$results/wayland.json"

remove_packages flectar-mail
if [[ "$family" == suse ]]; then
  rm /etc/zypp/repos.d/flectar-mail.repo
else
  rm /etc/yum.repos.d/flectar-mail.repo
fi
if rpm -q flectar-mail; then exit 1; fi
test ! -e /usr/bin/flectar-mail
test ! -e /usr/lib64/flectar-mail
test ! -e /usr/share/applications/com.flectar.mail.desktop
test ! -e /usr/share/metainfo/com.flectar.mail.metainfo.xml
if ((x11)); then
  printf 'RPM installation, PDF rendering, X11/Wayland startup and removal passed.\n'
else
  printf 'RPM installation, PDF rendering, Wayland startup and removal passed.\n'
fi
