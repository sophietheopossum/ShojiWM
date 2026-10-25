#!/usr/bin/env bash
# Install ShojiWM from the current source tree.
#
# This is intentionally a plain source install script, not a distro package.
# It installs the compositor, the TypeScript runtime files, a default user
# config if one does not already exist, a Wayland session entry, and the
# ShojiWM xdg-desktop-portal backend unless --no-portal is passed.
#
# With --rust-config the compositor is the Rust config example
# (src/shojiwm_rs/examples/default_config) instead of the TypeScript build. It
# reads its assets from this checkout, so keep the checkout where it is, and it
# cannot reload in place: a config change needs this script again and takes
# effect in the next session. The TypeScript runtime files (when missing) and
# user config are still installed, for switching back; the Rust compositor
# reads neither. A fallback is kept as /usr/lib/shojiwm/shoji_wm.previous: the
# /usr/bin/shoji_wm serving the session you install from, else the fallback
# already there, else the compositor being replaced.
# If the new one will not start, from a tty:
#   sudo install -m755 /usr/lib/shojiwm/shoji_wm.previous /usr/bin/shoji_wm
# An install without --rust-config removes the fallback.
#
# Usage:
#   dist/install.sh
#   dist/install.sh --dev        build quickly to allow faster testing for features (release-fast: no LTO, incremental)
#   dist/install.sh --debug      hardened allocator (heap-debug feature) and full debuginfo, for chasing heap corruption; combines with --dev
#   dist/install.sh --rust-config  install the Rust config example as the compositor; combines with the others
#   dist/install.sh --rust-config --expect-runtime=NAME  refuse a build whose --version names another runtime
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
EXPECT_RUNTIME=""

for arg in "$@"; do
    case "$arg" in
        --debug) DEBUG=1 ;;
        --dev) DEV=1 ;;
        --rust-config) RUST_CONFIG=1 ;;
        --expect-runtime=?*) EXPECT_RUNTIME="${arg#*=}" ;;
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

if [[ -n "$EXPECT_RUNTIME" && $RUST_CONFIG -eq 0 ]]; then
    echo "--expect-runtime needs --rust-config" >&2
    exit 2
fi

if [[ $RUST_CONFIG -eq 1 ]]; then
    # One --rust-config install per user at a time, whichever checkout it runs
    # from: the staging names below are fixed.
    LOCK_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
    if [[ ! -d "$LOCK_DIR" ]]; then
        LOCK_DIR="${XDG_CACHE_HOME:-$HOME/.cache}"
        mkdir -p "$LOCK_DIR"
    fi
    LOCK="${LOCK_DIR%/}/shojiwm-install-$(id -u).lock"
    if ! exec 9>>"$LOCK"; then
        echo "could not open the install lock $LOCK" >&2
        exit 1
    fi
    flock -n 9 || {
        RC=$?
        if [[ $RC -eq 1 ]]; then
            echo "another dist/install.sh --rust-config is running" >&2
        else
            echo "could not lock $LOCK (flock exited $RC)" >&2
        fi
        exit 1
    }
    # cargo can leave long-lived children (a build server, say): they must
    # not inherit the lock, so that it ends with this script.
    function cargo { command cargo "$@" 9<&-; }
fi

# --dev selects the release-fast profile; binaries land in a different
# target subdirectory, so resolve it here for the build and install steps.
PROFILE_DIR=release
PROFILE_FLAG=--release
if [[ $DEV -eq 1 ]]; then
    PROFILE_DIR=release-fast
    PROFILE_FLAG="--profile release-fast"
fi

if [[ $RUST_CONFIG -eq 1 ]]; then
    # Install from cargo's target directory: CARGO_TARGET_DIR or a
    # build.target-dir setting moves it, and a stale build left in target/
    # must not be installed instead. (The <triple>/ level that a build.target
    # setting adds is not followed.) Asked before the build, so that a missing
    # tool fails before a long compile rather than after it.
    TARGET_DIR="$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
        | python3 -I -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])' \
            2>/dev/null || true)"
    if [[ "$TARGET_DIR" != /* ]]; then
        echo "could not ask cargo for its target directory" >&2
        echo "this needs python3, and cargo metadata --format-version 1 --no-deps has to succeed" >&2
        exit 1
    fi
fi

if [[ $BUILD -eq 1 ]]; then
    CARGO_ARGS=($PROFILE_FLAG -p shoji_wm)
    if [[ $INSTALL_PORTAL -eq 1 ]]; then
        CARGO_ARGS+=(-p xdg-desktop-portal-shojiwm)
    fi
    HEAP_DEBUG_FEATURE=shoji_wm/heap-debug
    if [[ $RUST_CONFIG -eq 1 ]]; then
        # The example is the compositor with the Rust config compiled in.
        CARGO_ARGS=($PROFILE_FLAG -p shojiwm_rs --example default_config)
        HEAP_DEBUG_FEATURE=shojiwm_lib/heap-debug
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
    SHOJI_BIN="$TARGET_DIR/$PROFILE_DIR/examples/default_config"
    SHOJI_BUILD="cargo build $PROFILE_FLAG -p shojiwm_rs --example default_config"
    PORTAL_BIN="$TARGET_DIR/$PROFILE_DIR/xdg-desktop-portal-shojiwm"
fi

if [[ ! -x "$SHOJI_BIN" ]]; then
    echo "binary not found: $SHOJI_BIN" >&2
    echo "run without --no-build, or run $SHOJI_BUILD first" >&2
    exit 1
fi

if [[ $INSTALL_PORTAL -eq 1 && ! -x "$PORTAL_BIN" ]]; then
    echo "binary not found: $PORTAL_BIN" >&2
    echo "run without --no-build, or run cargo build $PROFILE_FLAG -p xdg-desktop-portal-shojiwm first" >&2
    exit 1
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT

if [[ $RUST_CONFIG -eq 1 ]]; then
    # --version prints "shoji_wm <version> (<runtime>)"; --expect-runtime
    # refuses a config of another name, such as another branch's build of the
    # example. The probe's environment is PATH and a HOME and XDG_RUNTIME_DIR
    # of its own, so a binary that ignores --version and starts a compositor
    # cannot join this session, its D-Bus activation environment or its logs.
    VERSION="$(env -i PATH="$PATH" HOME="$STAGE" XDG_RUNTIME_DIR="$STAGE" \
        timeout -k 1 5 "$SHOJI_BIN" --version 2>"$STAGE/version.err" 9<&- || true)"
    VERSION_ERR="$(head -c 2000 "$STAGE/version.err")"
    RUNTIME=""
    if [[ "$VERSION" =~ ^shoji_wm\ [^\ ]+\ \(([^\)]+)\)$ ]]; then
        RUNTIME="${BASH_REMATCH[1]}"
    fi
    if [[ -z "$RUNTIME" ]]; then
        echo "$SHOJI_BIN did not answer --version with \"shoji_wm <version> (<runtime>)\"" >&2
        if [[ -n "$VERSION" ]]; then
            echo "it printed: $VERSION" >&2
        fi
        if [[ -n "$VERSION_ERR" ]]; then
            echo "its stderr: $VERSION_ERR" >&2
        fi
        exit 1
    fi
    if [[ -n "$EXPECT_RUNTIME" && "$RUNTIME" != "$EXPECT_RUNTIME" ]]; then
        echo "$SHOJI_BIN is the Rust config \"$RUNTIME\", not \"$EXPECT_RUNTIME\"" >&2
        if [[ $BUILD -eq 0 ]]; then
            echo "run without --no-build" >&2
        else
            echo "this checkout builds \"$RUNTIME\"; is the right branch checked out?" >&2
        fi
        exit 1
    fi
fi

RUNTIME_STAGE="$STAGE/shojiwm-runtime"
mkdir -p "$RUNTIME_STAGE/packages" "$RUNTIME_STAGE/tools"

cp -a "$REPO_ROOT/packages/shoji_wm" "$RUNTIME_STAGE/packages/"
cp "$REPO_ROOT/tools/decoration-runtime.ts" "$RUNTIME_STAGE/tools/"

echo ">> installing compositor files (sudo)"
if [[ $RUST_CONFIG -eq 1 ]]; then
    # The fallback is the compositor serving the session this install runs in:
    # the process listening on $WAYLAND_DISPLAY. It is known to start, and its
    # /proc exe stays readable after a reinstall replaced /usr/bin/shoji_wm
    # ("(deleted)"). Any other shoji_wm (a nested test run, a hung session on
    # another VT) proves nothing. Every process holding the socket is checked,
    # since a child can share it. From a tty, with no WAYLAND_DISPLAY, the
    # fallback already in place is kept.
    PREVIOUS_SRC=""
    if [[ -n "${WAYLAND_DISPLAY:-}" ]]; then
        SOCKET="$WAYLAND_DISPLAY"
        if [[ "$SOCKET" != /* ]]; then
            SOCKET_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
            SOCKET="${SOCKET_DIR%/}/$SOCKET"
        fi
        for pid in $(ss -xlpn | awk -v s="$SOCKET" '$5 == s { print; exit }' \
                | grep -o 'pid=[0-9]*' | cut -d= -f2 || true); do
            EXE="$(readlink "/proc/$pid/exe" 2>/dev/null || true)"
            if [[ "${EXE%" (deleted)"}" == /usr/bin/shoji_wm ]]; then
                PREVIOUS_SRC="/proc/$pid/exe"
                break
            fi
        done
    fi
    if [[ -n "$PREVIOUS_SRC" ]]; then
        echo ">> fallback: the compositor serving this session ($PREVIOUS_SRC)"
    elif [[ -e /usr/lib/shojiwm/shoji_wm.previous ]]; then
        echo ">> fallback: keeping /usr/lib/shojiwm/shoji_wm.previous"
    elif [[ -x /usr/bin/shoji_wm ]]; then
        PREVIOUS_SRC=/usr/bin/shoji_wm
        echo ">> fallback: a copy of the installed /usr/bin/shoji_wm"
    else
        echo ">> fallback: none (no compositor is installed yet)"
    fi
    sudo mkdir -p /usr/lib/shojiwm
    # The staging names are fixed: clear whatever a killed run left.
    sudo rm -f /usr/bin/.shoji_wm.new /usr/lib/shojiwm/.shoji_wm.previous.new
    if [[ -n "$PREVIOUS_SRC" ]] \
        && ! sudo install -m755 "$PREVIOUS_SRC" /usr/lib/shojiwm/.shoji_wm.previous.new; then
        sudo rm -f /usr/lib/shojiwm/.shoji_wm.previous.new || true
        echo "could not copy the fallback from $PREVIOUS_SRC; nothing was changed" >&2
        exit 1
    fi
    # Staged beside the target and renamed over it, so an interrupted copy (a
    # full disk, a closed terminal) leaves the old binary in place. The
    # running session keeps its own copy either way.
    if ! sudo install -m755 "$SHOJI_BIN" /usr/bin/.shoji_wm.new; then
        sudo rm -f /usr/bin/.shoji_wm.new /usr/lib/shojiwm/.shoji_wm.previous.new || true
        echo "install failed; /usr/bin/shoji_wm left as it was" >&2
        exit 1
    fi
    # The fallback goes first: a run that stops between the two renames has
    # lost at most the old fallback, which a finished run replaces as well.
    if [[ -n "$PREVIOUS_SRC" ]] \
        && ! sudo mv -f /usr/lib/shojiwm/.shoji_wm.previous.new /usr/lib/shojiwm/shoji_wm.previous; then
        sudo rm -f /usr/bin/.shoji_wm.new /usr/lib/shojiwm/.shoji_wm.previous.new || true
        echo "could not replace /usr/lib/shojiwm/shoji_wm.previous; nothing was changed" >&2
        exit 1
    fi
    if ! sudo mv -f /usr/bin/.shoji_wm.new /usr/bin/shoji_wm; then
        sudo rm -f /usr/bin/.shoji_wm.new || true
        echo "could not replace /usr/bin/shoji_wm; it was left as it was" >&2
        exit 1
    fi
    # The TypeScript runtime files are installed only when missing: a Rust
    # compositor never reads them, and existing ones may belong to a
    # TypeScript shoji_wm.previous. A user config created below links to them.
    if [[ ! -e /usr/lib/shojiwm/packages/shoji_wm ]]; then
        sudo cp -a "$RUNTIME_STAGE/." /usr/lib/shojiwm/
    fi
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
    echo "Installed the Rust config ($RUNTIME)."
    echo "Development run: cargo run -p shojiwm_rs --example default_config --profile release-fast"
    echo "Installed run: select ShojiWM in your display manager (log out first if it is running), or run: shoji_wm --tty"
    if [[ -x /usr/lib/shojiwm/shoji_wm.previous ]]; then
        echo "If it will not start, from a tty:"
        echo "  sudo install -m755 /usr/lib/shojiwm/shoji_wm.previous /usr/bin/shoji_wm"
    fi
else
    echo "Development run: cargo run --profile release-fast -p shoji_wm -- --dev"
    echo "Installed run: select ShojiWM in your display manager, or run: shoji_wm --tty"
fi
