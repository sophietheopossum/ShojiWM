import { COMPOSITOR } from "shoji_wm";
import type { KeyBindingController } from "shoji_wm/types";
import type { HybridWindowManager } from "../window-manager";
import { workspacesEnabled } from "./settings";
import type { WorkspaceIpc } from "./workspace-ipc";

// Backlight (Fn+F4 / Fn+F5 emit the XF86MonBrightness* keysyms).
//
// The Duo has two panels with independent backlights, so the keys act on
// whichever one currently holds focus: eDP-1 is the 1920x1080 main panel,
// while the ScreenPad Plus enumerates as an internal DisplayPort output
// (DP-1, 1920x515) rather than a second eDP. Anything else — an external
// monitor, which has no sysfs backlight anyway — falls back to the main
// panel so the keys always do something predictable.
//
// brightnessctl writes through logind's D-Bus session interface, so no setuid
// binary or udev rule is needed. `-n` keeps a floor of one step: with no OSD
// yet, a fully black panel is indistinguishable from a crash.
// Its value must stay unwritten — the flag takes an *optional* argument, so a spaced `-n 1`
// leaves the 1 as a positional, which brightnessctl reads as an unknown
// operation and quietly downgrades the whole run to `info` (exit 0, no write).
const BACKLIGHT_BY_CONNECTOR: Record<string, string> = {
  "eDP-1": "intel_backlight",
  "DP-1": "asus_screenpad",
};

// Feature-detected: tsc checks against the repo SDK, but the session runs the
// installed one, which has no unbind until it is reinstalled.
const keyUnbind = (COMPOSITOR.key as Partial<KeyBindingController>).unbind;

export function bindKeys(
  windowManager: HybridWindowManager,
  ipc: WorkspaceIpc,
): { syncDesktopKeyBindings(enabled: boolean): void } {
  const { scheduleWorkspaceBroadcast } = ipc;

  COMPOSITOR.key.bind("terminal", "Super+T", () => {
    COMPOSITOR.process.spawn({ command: ["kitty"] });
  });

  // if kwallet6 is used as the password store, be sure to add the --password-store=kwallet6 flag
  COMPOSITOR.key.bind("chrome", "Super+B", () => {
    COMPOSITOR.process.spawn({
      command:
        "google-chrome-stable --enable-features=OzonePlatform --ozone-platform=wayland",
    });
  });

  COMPOSITOR.key.bind("discord", "Super+D", () => {
    COMPOSITOR.process.spawn({
      command:
        "discord --enable-features=UseOzonePlatform --ozone-platform=wayland --enable-wayland-ime --disable-gpu",
    });
  });

  COMPOSITOR.key.bind("dolphin", "Super+E", () => {
    COMPOSITOR.process.spawn({ command: "dolphin" });
  });

  COMPOSITOR.key.bind("play", "XF86AudioPlay", () => {
    COMPOSITOR.process.spawn({ command: "playerctl play-pause" });
  });
  COMPOSITOR.key.bind("pause", "XF86AudioPause", () => {
    COMPOSITOR.process.spawn({ command: "playerctl play-pause" });
  });
  COMPOSITOR.key.bind("next", "XF86AudioNext", () => {
    COMPOSITOR.process.spawn({ command: "playerctl next" });
  });
  COMPOSITOR.key.bind("prev", "XF86AudioPrev", () => {
    COMPOSITOR.process.spawn({ command: "playerctl previous" });
  });

  function adjustBrightness(delta: string) {
    const monitor = windowManager.getCurrentMonitorName();
    const device = BACKLIGHT_BY_CONNECTOR[monitor] ?? "intel_backlight";
    COMPOSITOR.process.spawn({
      command: `brightnessctl -d ${device} -n set ${delta}`,
    });
  }

  COMPOSITOR.key.bind("brightness-up", "XF86MonBrightnessUp", () => {
    adjustBrightness("5%+");
  });
  COMPOSITOR.key.bind("brightness-down", "XF86MonBrightnessDown", () => {
    adjustBrightness("5%-");
  });

  // Resolve the monitor under the cursor and toggle the start menu.
  // MinkaShell listens for the ui.startMenu broadcast on the IPC socket.
  function toggleStartMenu() {
    const monitor = windowManager.getCurrentMonitorName();
    ipc.server.broadcast("ui.startMenu", {
      connector: monitor,
      action: "toggle",
    });
  }
  COMPOSITOR.key.bind("start-menu", "Super+A", toggleStartMenu);
  // Super tap (fires on release only, when no other key/button was pressed in between).
  COMPOSITOR.key.bind("start-menu-tap", "Super", toggleStartMenu, {
    on: "release",
  });
  // Clipboard UI was dropped with shoji-bar-2 (Sophie's call); the cliphist
  // watchers in session.ts keep collecting history for a future picker, so
  // Super+V is intentionally unbound for now.
  COMPOSITOR.key.bind("screenshot", "Super+P", () => {
    COMPOSITOR.process.spawn({
      command: "hyprshot -m region --raw | swappy -f -",
    });
  });
  COMPOSITOR.key.bind("screenshot-freeze", "Super+Ctrl+P", () => {
    COMPOSITOR.process.spawn({
      command: "hyprshot -m region --freeze --raw | swappy -f -",
    });
  });
  // MinkaShot: freeze-frame capture UI (MinkaDE/MinkaShot). The running app
  // listens for this broadcast on the IPC socket, same pattern as the start
  // menu.
  COMPOSITOR.key.bind("minkashot", "Print", () => {
    ipc.server.broadcast("ui.minkashot", { action: "interactive" });
  });
  COMPOSITOR.key.bind("cycle-windows", "Alt+Tab", () => {
    windowManager.cycleWorkspaceFocus(1);
    scheduleWorkspaceBroadcast();
  });
  COMPOSITOR.key.bind("cycle-windows-back", "Alt+Shift+Tab", () => {
    windowManager.cycleWorkspaceFocus(-1);
    scheduleWorkspaceBroadcast();
  });
  COMPOSITOR.key.bind("toggle-tiling-mode", "Super+S", () => {
    windowManager.toggleCurrentWorkspaceTiling();
    scheduleWorkspaceBroadcast();
  });
  COMPOSITOR.key.bind("close-focused-window", "Super+Q", () => {
    windowManager.closeFocusedWindow();
  });
  COMPOSITOR.key.bind("close-focused-window-alt-f4", "Alt+F4", () => {
    windowManager.closeFocusedWindow();
  });
  COMPOSITOR.key.bind("toggle-focused-window-maximize", "Super+M", () => {
    windowManager.toggleFocusedWindowMaximize();
  });
  COMPOSITOR.key.bind("toggle-focused-window-fullscreen", "Super+F", () => {
    windowManager.toggleFocusedWindowFullscreen();
  });
  COMPOSITOR.key.bind("tile-focus-left-quick", "Super+Left", () => {
    windowManager.focusTile(-1);
  });
  COMPOSITOR.key.bind("tile-focus-right-quick", "Super+Right", () => {
    windowManager.focusTile(1);
  });
  COMPOSITOR.key.bind("tile-focus-left", "Super+Ctrl+Left", () => {
    windowManager.focusTile(-1);
  });
  COMPOSITOR.key.bind("tile-focus-right", "Super+Ctrl+Right", () => {
    windowManager.focusTile(1);
  });
  COMPOSITOR.key.bind("tile-move-left", "Super+Shift+Left", () => {
    windowManager.moveFocusedTile(-1);
    scheduleWorkspaceBroadcast();
  });
  COMPOSITOR.key.bind("tile-move-right", "Super+Shift+Right", () => {
    windowManager.moveFocusedTile(1);
    scheduleWorkspaceBroadcast();
  });
  // Virtual-desktop keys, bound only while desktops are on: an unbound shortcut
  // is not intercepted, so with desktops off these reach the focused app.
  const DESKTOP_KEY_BINDINGS: ReadonlyArray<readonly [string, string, () => void]> = [
    ["window-move-workspace-prev", "Super+Shift+Up", () =>
      windowManager.moveFocusedWindowToWorkspace(-1)],
    ["window-move-workspace-next", "Super+Shift+Down", () =>
      windowManager.moveFocusedWindowToWorkspace(1)],
    ["workspace-prev", "Super+Ctrl+Up", () => windowManager.switchWorkspace(-1)],
    ["workspace-next", "Super+Ctrl+Down", () => windowManager.switchWorkspace(1)],
  ];

  function syncDesktopKeyBindings(enabled: boolean): void {
    for (const [id, shortcut, run] of DESKTOP_KEY_BINDINGS) {
      if (enabled) {
        COMPOSITOR.key.bind(id, shortcut, () => {
          // Without unbind a live switch-off leaves the keys bound until the
          // next reload; they do nothing meanwhile.
          if (!workspacesEnabled()) {
            return;
          }
          run();
          scheduleWorkspaceBroadcast();
        });
      } else if (typeof keyUnbind === "function") {
        keyUnbind.call(COMPOSITOR.key, id);
      }
    }
  }
  syncDesktopKeyBindings(workspacesEnabled());

  let fpsCounter = false;
  COMPOSITOR.key.bind("fps", "Super+Shift+F", () => {
    fpsCounter = !fpsCounter;
    COMPOSITOR.debug.fpsCounter = fpsCounter;
  });

  let profileEnabled = false;
  COMPOSITOR.key.bind("profile", "Super+Shift+T", () => {
    profileEnabled = !profileEnabled;
    COMPOSITOR.debug.enableProfile(profileEnabled);
  });

  return { syncDesktopKeyBindings };
}
