#!/usr/bin/env bash
# Checks that this machine can build RetroWork (`build`) or run its UI tests
# (`ui`). Prints exactly what is missing and exits non-zero; installs nothing.
set -euo pipefail

what="${1:-build}"
missing=0

need() {
    # need <description> <how to get it> <command...>
    local description="$1" remedy="$2"
    shift 2
    if ! "$@" >/dev/null 2>&1; then
        printf 'MISSING: %s\n         %s\n' "$description" "$remedy" >&2
        missing=1
    fi
}

case "$what" in
build)
    need "ACL test tool (setfacl)" "Fedora/Debian: install acl" command -v setfacl
    need "Rust toolchain (cargo)" "https://rustup.rs" command -v cargo
    need "rustfmt" "rustup component add rustfmt" cargo fmt --version
    need "clippy" "rustup component add clippy" cargo clippy --version
    need "pkg-config" "Fedora: dnf install pkgconf-pkg-config | Debian: apt install pkg-config" command -v pkg-config
    need "Ninja for project integration tests" "Fedora: dnf install ninja-build | Debian: apt install ninja-build" command -v ninja
    need "C compiler and linker (cc)" "Fedora: dnf install gcc | Debian: apt install build-essential" command -v cc
    # Libraries GPUI links against on Linux (X11 and Wayland back ends, text).
    for entry in \
        "xcb|libxcb-devel|libxcb1-dev" \
        "xkbcommon|libxkbcommon-devel|libxkbcommon-dev" \
        "xkbcommon-x11|libxkbcommon-x11-devel|libxkbcommon-x11-dev" \
        "wayland-client|wayland-devel|libwayland-dev" \
        "freetype2|freetype-devel|libfreetype-dev" \
        "fontconfig|fontconfig-devel|libfontconfig1-dev"; do
        IFS='|' read -r module fedora debian <<<"$entry"
        need "development files for $module" \
            "Fedora: dnf install $fedora | Debian: apt install $debian" \
            pkg-config --exists "$module"
    done
    ;;
ui)
    need "gdbus for desktop portal readiness" "Install glib2" command -v gdbus
    need "D-Bus for isolated system-picker tests" "Install dbus-daemon" command -v dbus-daemon
    need "desktop file chooser portal" "Install xdg-desktop-portal" test -x /usr/libexec/xdg-desktop-portal
    need "GTK system-picker backend for native tests" "Install xdg-desktop-portal-gtk" test -x /usr/libexec/xdg-desktop-portal-gtk
    if [ -n "${RETROWORK_E2E_DISPLAY:-}" ]; then
        echo "UI tests will use the X display in RETROWORK_E2E_DISPLAY ($RETROWORK_E2E_DISPLAY)."
    else
        need "a Wayland session to nest the test X server in (WAYLAND_DISPLAY is not set)" \
            "run inside a Wayland session, or set RETROWORK_E2E_DISPLAY to an X display, e.g. after: Xvfb :99 -screen 0 1280x860x24 &" \
            test -n "${WAYLAND_DISPLAY:-}"
        need "Xwayland (private X server for UI tests)" \
            "Fedora: dnf install xorg-x11-server-Xwayland | Debian: apt install xwayland" \
            command -v Xwayland
    fi
    # GPUI renders with Vulkan; a software implementation (Mesa lavapipe) is enough.
    need "a Vulkan driver (hardware, or Mesa lavapipe for software rendering)" \
        "Fedora: dnf install mesa-vulkan-drivers | Debian: apt install mesa-vulkan-drivers" \
        sh -c 'ls /usr/share/vulkan/icd.d/*.json /etc/vulkan/icd.d/*.json 2>/dev/null | grep -q .'
    ;;
*)
    echo "usage: scripts/prereqs.sh [build|ui]" >&2
    exit 2
    ;;
esac

if [ "$missing" -ne 0 ]; then
    echo "prereqs ($what): something is missing, see above. Nothing was installed." >&2
    exit 1
fi
echo "prereqs ($what): ok"
