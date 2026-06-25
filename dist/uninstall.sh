#!/usr/bin/env bash
# Uninstall files installed by dist/install.sh.
#
# By default this removes both ShojiWM and xdg-desktop-portal-shojiwm. Use
# --keep-portal if you only want to remove the compositor install.

: "${XDG_CONFIG_HOME:=$HOME/.config}"

set -euo pipefail

REMOVE_PORTAL=1
REMOVE_USER_CONFIG=0

for arg in "$@"; do
    case "$arg" in
        --keep-portal) REMOVE_PORTAL=0 ;;
        --user-config) REMOVE_USER_CONFIG=1 ;;
        -h|--help)
            awk 'NR == 1{next} /^#/{sub(/^# ?/, ""); print; next} {exit}' "$0"
            exit 0
            ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

if [[ $REMOVE_PORTAL -eq 1 ]]; then
    "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/uninstall-portal.sh"
fi

echo ">> removing ShojiWM files (sudo)"
sudo rm -rf \
    /usr/bin/shoji_wm \
    /usr/lib/shojiwm \
    /usr/share/shojiwm \
    /usr/share/wayland-sessions/shojiwm.desktop

if [[ $REMOVE_USER_CONFIG -eq 1 ]]; then
    echo ">> removing user config at $XDG_CONFIG_HOME/shojiwm"
    rm -rf "$CONFIG_HOME/shojiwm"
fi

echo "done."
