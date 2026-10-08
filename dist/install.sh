#!/usr/bin/env bash
# Install ShojiWM from the current source tree.
#
# This is intentionally a plain source install script, not a distro package.
# It installs the compositor, the TypeScript runtime files, a default user
# config if one does not already exist, a Wayland session entry, and the
# ShojiWM xdg-desktop-portal backend unless --no-portal is passed.
#
# With --rust-config the compositor is Minka's Rust config
# (src/shojiwm_rs/examples/default_config) instead of the TypeScript build.
# Everything else is installed the same way, except the TypeScript runtime
# files under /usr/lib/shojiwm: the Rust config never reads them, so they stay
# paired with the TypeScript binaries kept there. The compositor serving the
# session the install runs in (the one listening on $WAYLAND_DISPLAY) is kept
# as /usr/lib/shojiwm/shoji_wm.previous; from a tty the one already there is
# kept. From a tty, when the new one will not start:
#   sudo install -m755 /usr/lib/shojiwm/shoji_wm.previous /usr/bin/shoji_wm
# The Rust config reads its shaders and icons from this checkout's
# packages/config, found through the path it was built at, so keep the
# checkout where it is. It cannot be reloaded in place: a change takes effect
# in the next session.
#
# Usage:
#   dist/install.sh
#   dist/install.sh --dev        build quickly to allow faster testing for features (release-fast: no LTO, incremental)
#   dist/install.sh --debug      hardened allocator (heap-debug feature) and full debuginfo, for chasing heap corruption; combines with --dev
#   dist/install.sh --rust-config  install Minka's Rust config as the compositor; combines with the others
#   dist/install.sh --no-build
#   dist/install.sh --no-portal
#   dist/install.sh --no-config

set -euo pipefail

: "${XDG_CONFIG_HOME:=$HOME/.config}"

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

BUILD=1
DEBUG=0
DEV=0
INSTALL_PORTAL=1
INSTALL_CONFIG=1
RUST_CONFIG=0

for arg in "$@"; do
    case "$arg" in
        --debug) DEBUG=1 ;;
        --dev) DEV=1 ;;
        --rust-config) RUST_CONFIG=1 ;;
        --no-build) BUILD=0 ;;
        --no-portal) INSTALL_PORTAL=0 ;;
        --no-config) INSTALL_CONFIG=0 ;;
        -h|--help)
            awk 'NR == 1{next} /^#/{sub(/^# ?/, ""); print; next} {exit}' "$0"
            exit 0
            ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

if [[ $RUST_CONFIG -eq 1 ]]; then
    # One --rust-config install at a time: its staging names are fixed.
    exec 9<"$REPO_ROOT/dist/install.sh"
    if ! flock -n 9; then
        echo "another dist/install.sh --rust-config is running" >&2
        exit 1
    fi
fi

# --dev selects the release-fast profile; binaries land in a different
# target subdirectory, so resolve it here for the build and install steps.
PROFILE_DIR=release
PROFILE_FLAG=--release
if [[ $DEV -eq 1 ]]; then
    PROFILE_DIR=release-fast
    PROFILE_FLAG="--profile release-fast"
fi

if [[ $BUILD -eq 1 ]]; then
    if [[ $RUST_CONFIG -eq 1 ]]; then
        # The example is the compositor with the Rust config compiled in.
        CARGO_ARGS=($PROFILE_FLAG -p shojiwm_rs --example default_config)
        HEAP_DEBUG_FEATURE=shojiwm_lib/heap-debug
    else
        CARGO_ARGS=($PROFILE_FLAG -p shoji_wm)
        HEAP_DEBUG_FEATURE=shoji_wm/heap-debug
        if [[ $INSTALL_PORTAL -eq 1 ]]; then
            CARGO_ARGS+=(-p xdg-desktop-portal-shojiwm)
        fi
    fi
    if [[ $DEBUG -eq 1 ]]; then
        # Hardened mimalloc (guard pages, encoded free lists): heap
        # corruption aborts at the faulting write instead of detonating
        # later. Expect higher memory use and a small slowdown.
        CARGO_ARGS+=(--features "$HEAP_DEBUG_FEATURE")
        # Full debuginfo so core dumps symbolize cleanly. Cargo maps
        # profile names to env keys with hyphens as underscores.
        export CARGO_PROFILE_RELEASE_DEBUG=true
        export CARGO_PROFILE_RELEASE_FAST_DEBUG=true
    fi
    echo ">> cargo build ${CARGO_ARGS[*]}"
    cargo build "${CARGO_ARGS[@]}"
    if [[ $RUST_CONFIG -eq 1 && $INSTALL_PORTAL -eq 1 ]]; then
        # --example limits a build to example targets, so the portal's
        # binary needs a build of its own.
        echo ">> cargo build $PROFILE_FLAG -p xdg-desktop-portal-shojiwm"
        cargo build $PROFILE_FLAG -p xdg-desktop-portal-shojiwm
    fi
fi

SHOJI_BIN="$REPO_ROOT/target/$PROFILE_DIR/shoji_wm"
SHOJI_BUILD="cargo build $PROFILE_FLAG -p shoji_wm"
PORTAL_BIN="$REPO_ROOT/target/$PROFILE_DIR/xdg-desktop-portal-shojiwm"
if [[ $RUST_CONFIG -eq 1 ]]; then
    # Install from wherever cargo builds: CARGO_TARGET_DIR or a
    # build.target-dir setting moves it, and a stale build left in target/
    # must not be installed instead.
    TARGET_DIR="$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
        | grep -o '"target_directory":"[^"]*"' | head -n1 | cut -d'"' -f4 || true)"
    : "${TARGET_DIR:=${CARGO_TARGET_DIR:-$REPO_ROOT/target}}"
    SHOJI_BIN="$TARGET_DIR/$PROFILE_DIR/examples/default_config"
    SHOJI_BUILD="cargo build $PROFILE_FLAG -p shojiwm_rs --example default_config"
    PORTAL_BIN="$TARGET_DIR/$PROFILE_DIR/xdg-desktop-portal-shojiwm"
fi

if [[ ! -x "$SHOJI_BIN" ]]; then
    echo "binary not found: $SHOJI_BIN" >&2
    echo "run without --no-build, or run $SHOJI_BUILD first" >&2
    exit 1
fi

if [[ $RUST_CONFIG -eq 1 ]]; then
    # Upstream's example has the same name, so a build of another branch
    # leaves its config here.
    version="$(timeout 5 "$SHOJI_BIN" --version 2>/dev/null || true)"
    if [[ "$version" != *"(minka)" ]]; then
        echo "$SHOJI_BIN is not the Minka config (--version: ${version:-nothing})" >&2
        if [[ $BUILD -eq 0 ]]; then
            echo "run without --no-build" >&2
        else
            echo "this checkout builds a different default_config; is the Minka branch checked out?" >&2
        fi
        exit 1
    fi
fi

if [[ $INSTALL_PORTAL -eq 1 && ! -x "$PORTAL_BIN" ]]; then
    echo "binary not found: $PORTAL_BIN" >&2
    echo "run without --no-build, or run cargo build $PROFILE_FLAG -p xdg-desktop-portal-shojiwm first" >&2
    exit 1
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

RUNTIME_STAGE="$STAGE/shojiwm-runtime"
mkdir -p "$RUNTIME_STAGE/packages" "$RUNTIME_STAGE/tools"

cp -a "$REPO_ROOT/packages/shoji_wm" "$RUNTIME_STAGE/packages/"
cp "$REPO_ROOT/tools/decoration-runtime.ts" "$RUNTIME_STAGE/tools/"

echo ">> installing compositor files (sudo)"
if [[ $RUST_CONFIG -eq 1 ]]; then
    # The fallback is the compositor serving the session this install runs in:
    # the process listening on $WAYLAND_DISPLAY. Serving it proves it starts,
    # even after a reinstall replaced /usr/bin/shoji_wm ("(deleted)"), and its
    # /proc exe stays readable. Any other shoji_wm (a nested test run, a hung
    # session on another VT) proves nothing. From a tty, with no
    # WAYLAND_DISPLAY, the fallback already in place is kept;
    # /usr/bin/shoji_wm is copied only when there is none.
    PREVIOUS_SRC=""
    if [[ -n "${WAYLAND_DISPLAY:-}" ]]; then
        socket="$WAYLAND_DISPLAY"
        [[ "$socket" == /* ]] || socket="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}/$socket"
        pid="$(ss -xlpn 2>/dev/null | awk -v s="$socket" '$5 == s { print; exit }' \
            | grep -o 'pid=[0-9]*' | head -n1 | cut -d= -f2 || true)"
        exe=""
        if [[ -n "$pid" ]]; then
            exe="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
        fi
        if [[ "${exe%" (deleted)"}" == /usr/bin/shoji_wm ]]; then
            PREVIOUS_SRC="/proc/$pid/exe"
        fi
    fi
    if [[ -z "$PREVIOUS_SRC" && ! -e /usr/lib/shojiwm/shoji_wm.previous && -x /usr/bin/shoji_wm ]]; then
        PREVIOUS_SRC=/usr/bin/shoji_wm
    fi
    sudo mkdir -p /usr/lib/shojiwm
    # The staging names are fixed: clear whatever a killed run left.
    sudo rm -f /usr/bin/.shoji_wm.new /usr/lib/shojiwm/.shoji_wm.previous.new
    if [[ -n "$PREVIOUS_SRC" ]] \
        && ! sudo install -m755 "$PREVIOUS_SRC" /usr/lib/shojiwm/.shoji_wm.previous.new; then
        sudo rm -f /usr/lib/shojiwm/.shoji_wm.previous.new
        echo "could not copy the fallback from $PREVIOUS_SRC; nothing was changed" >&2
        exit 1
    fi
    # Staged beside the target and renamed over it, so an interrupted copy (a
    # full disk, a closed terminal) leaves the old binary in place. The
    # running session keeps its own copy either way.
    if ! sudo install -m755 "$SHOJI_BIN" /usr/bin/.shoji_wm.new; then
        sudo rm -f /usr/bin/.shoji_wm.new /usr/lib/shojiwm/.shoji_wm.previous.new
        echo "install failed; /usr/bin/shoji_wm left as it was" >&2
        exit 1
    fi
    sudo mv -f /usr/bin/.shoji_wm.new /usr/bin/shoji_wm
    # The fallback changes only once the new binary is in place.
    if [[ -n "$PREVIOUS_SRC" ]] \
        && ! sudo mv -f /usr/lib/shojiwm/.shoji_wm.previous.new /usr/lib/shojiwm/shoji_wm.previous; then
        echo "the new compositor is installed, but its fallback is still" >&2
        echo "/usr/lib/shojiwm/.shoji_wm.previous.new" >&2
        exit 1
    fi
    # The TypeScript runtime files are left alone: the Rust config never reads
    # them, and they belong to the TypeScript binaries kept beside them
    # (shoji_wm.typescript, or a TypeScript shoji_wm.previous).
else
    sudo rm -rf /usr/lib/shojiwm
    sudo install -Dm755 "$SHOJI_BIN" /usr/bin/shoji_wm
    sudo mkdir -p /usr/lib/shojiwm
    sudo cp -a "$RUNTIME_STAGE/." /usr/lib/shojiwm/
fi
sudo install -Dm644 "$REPO_ROOT/dist/shojiwm.desktop" \
    /usr/share/wayland-sessions/shojiwm.desktop

echo ">> installing default config template (sudo)"
sudo rm -rf /usr/share/shojiwm/default-config
sudo mkdir -p /usr/share/shojiwm/default-config
sudo cp -a "$REPO_ROOT/packages/config/." /usr/share/shojiwm/default-config/

if [[ $INSTALL_CONFIG -eq 1 ]]; then
    USER_CONFIG_DIR="$XDG_CONFIG_HOME/shojiwm"
    CREATED_CONFIG=0
    if [[ ! -e "$USER_CONFIG_DIR/src/index.tsx" ]]; then
        echo ">> creating user config at $USER_CONFIG_DIR"
        mkdir -p "$USER_CONFIG_DIR"
        cp -a "$REPO_ROOT/packages/config/." "$USER_CONFIG_DIR/"
        CREATED_CONFIG=1
    else
        echo ">> keeping existing user config at $USER_CONFIG_DIR"
    fi

    mkdir -p "$USER_CONFIG_DIR/node_modules"
    ln -sfn /usr/lib/shojiwm/packages/shoji_wm "$USER_CONFIG_DIR/node_modules/shoji_wm"
    if [[ $CREATED_CONFIG -eq 1 || ! -e "$USER_CONFIG_DIR/package.json" ]]; then
        cat > "$USER_CONFIG_DIR/package.json" <<'EOF'
{
  "name": "shojiwm-user-config",
  "private": true,
  "type": "module",
  "dependencies": {
    "shoji_wm": "file:/usr/lib/shojiwm/packages/shoji_wm"
  }
}
EOF
    fi
    if [[ $CREATED_CONFIG -eq 1 || ! -e "$USER_CONFIG_DIR/tsconfig.json" ]]; then
        cat > "$USER_CONFIG_DIR/tsconfig.json" <<'EOF'
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "jsx": "react-jsx",
    "jsxImportSource": "shoji_wm",
    "strict": true,
    "verbatimModuleSyntax": true,
    "noEmit": true
  }
}
EOF
    fi
fi

if [[ $INSTALL_PORTAL -eq 1 ]]; then
    echo ">> installing xdg-desktop-portal-shojiwm files (sudo)"
    sudo install -Dm755 "$PORTAL_BIN" /usr/bin/xdg-desktop-portal-shojiwm
    sudo install -Dm644 "$REPO_ROOT/dist/shojiwm.portal" \
        /usr/share/xdg-desktop-portal/portals/shojiwm.portal
    sudo install -Dm644 "$REPO_ROOT/dist/org.freedesktop.impl.portal.desktop.shojiwm.service" \
        /usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.shojiwm.service
    sudo install -Dm644 "$REPO_ROOT/dist/xdg-desktop-portal-shojiwm.service" \
        /usr/lib/systemd/user/xdg-desktop-portal-shojiwm.service

    echo ">> writing user portals.conf"
    mkdir -p "$XDG_CONFIG_HOME/xdg-desktop-portal"
    cat > "$XDG_CONFIG_HOME/xdg-desktop-portal/shojiwm-portals.conf" <<'EOF'
[preferred]
default=gtk
org.freedesktop.impl.portal.ScreenCast=shojiwm
EOF

    echo ">> reloading systemd user services"
    #sleep is needed to prevent dbus errors
    sleep 1
    systemctl --user daemon-reload
    systemctl --user stop xdg-desktop-portal-shojiwm.service 2>/dev/null || true
    systemctl --user restart xdg-desktop-portal 2>/dev/null || true
fi

echo ""
echo "done."
if [[ $RUST_CONFIG -eq 1 ]]; then
    echo "Installed the Rust config: log out and back in to run it."
    if [[ -x /usr/lib/shojiwm/shoji_wm.previous ]]; then
        echo "If it will not start, from a tty:"
        echo "  sudo install -m755 /usr/lib/shojiwm/shoji_wm.previous /usr/bin/shoji_wm"
    fi
else
    echo "Development run: cargo run --profile release-fast -p shoji_wm -- --dev"
    echo "Installed run: select ShojiWM in your display manager, or run: shoji_wm --tty"
fi
