import {
  AppIcon,
  Box,
  Button,
  ClientWindow,
  Image,
  ShaderEffect,
  Label,
  COMPOSITOR,
  WindowBorder,
  backdropSource,
  compileEffect,
  compileLayerEffect,
  dualKawaseBlur,
  type SSDStyle,
  type WaylandWindow,
  computed,
  useState,
  shaderStage,
  loadShader,
  layerSource,
  ManagedWindow,
  read,
  type DisplayConfigDraft,
  compilePopupEffect,
  popupSource,
} from "shoji_wm";
import type {
  CompositionRenderable,
  InputAccelProfile,
  InputScrollMethod,
  ManagedWindowRect,
} from "shoji_wm/types";
import { createIpcServer } from "shoji_wm/ipc";
import { readFileSync } from "node:fs";

// Full per-display schema MinkaConf's visual page writes.
interface MinkaDisplaySettings {
  scale?: number;
  position?: { x: number; y: number };
  resolution?: { width: number; height: number; refreshRate?: number } | "best";
  enabled?: boolean;
  mirror?: string | null;
  hdr?: boolean;
}

interface MinkaInputSettings {
  pointerAccel: number;
  accelProfile: string;
  naturalScroll: boolean;
  touchpad: {
    naturalScroll: boolean;
    tapToClick: boolean;
    scrollMethod: string;
    scrollFactor: number;
    disableWhileTyping: boolean;
  };
  // Optional: settings files written before the keyboard section lack it.
  keyboard?: {
    layout?: string;
    variant?: string;
  };
}

interface MinkaSettings {
  input: MinkaInputSettings;
  displays: Record<string, MinkaDisplaySettings | undefined>;
  // Shell-side keys (e.g. shell.layout) ride along untouched: MinkaConf owns
  // the whole file and MinkaShell reads it directly.
  shell?: { layout?: string };
}

// User-facing settings owned by MinkaConf. Read at runtime, NOT imported as
// a module: this config can live anywhere (repo checkout via SHOJI_CONFIG,
// symlink, installed copy) and ESM resolves relative imports against the
// module's realpath, while MinkaConf and MinkaShell always use the path
// below. Boot reads the file once; `settings.apply` over IPC swaps values at
// runtime (input/output factories re-run, no config reload). A missing or
// unparseable file falls back to defaults so a fresh machine boots before
// MinkaConf has ever written anything.
const MINKA_SETTINGS_PATH = `${process.env.HOME}/.config/shojiwm/src/minka-settings.json`;

// 8/7/2026 defaults per Sophie: adaptive accel, natural scroll off.
const MINKA_SETTINGS_DEFAULTS: MinkaSettings = {
  input: {
    pointerAccel: 0.4,
    accelProfile: "adaptive",
    naturalScroll: false,
    touchpad: {
      naturalScroll: false,
      tapToClick: true,
      scrollMethod: "twoFinger",
      scrollFactor: 1,
      disableWhileTyping: false,
    },
    keyboard: {
      layout: "us",
      variant: "",
    },
  },
  displays: {},
};

function loadMinkaSettings(): MinkaSettings {
  try {
    return JSON.parse(readFileSync(MINKA_SETTINGS_PATH, "utf8")) as MinkaSettings;
  } catch (error) {
    console.warn(
      `minka-settings: using defaults (cannot read ${MINKA_SETTINGS_PATH}: ${error})`,
    );
    return MINKA_SETTINGS_DEFAULTS;
  }
}

let activeSettings: MinkaSettings = loadMinkaSettings();
import {
  HybridWindowManager,
  TITLEBAR_HEIGHT,
  WINDOW_BORDER_PX,
  WINDOW_STATE_FULLSCREEN,
  WINDOW_STATE_MAXIMIZED,
  WINDOW_STATE_MINIMIZED,
  WINDOW_STATE_MINIMIZE_VISUAL_IDLE,
  WINDOW_STATE_TILE_DRAGGING,
  WINDOW_STATE_TILED,
  WINDOW_STATE_VISIBLE_OUTPUTS,
  WINDOW_STATE_RECT,
  WINDOW_STATE_WORKSPACE_VISIBLE,
  WINDOW_STATE_WORKSPACE_OFFSET_Y,
  WINDOW_STATE_WORKSPACE_OPACITY,
} from "./window-manager";

COMPOSITOR.env.apply({
  QT_QPA_PLATFORM: "wayland;xcb",
  QT_QPA_PLATFORMTHEME: "qt6ct",
  GLFW_IM_MODULE: "ibus",
  ELECTRON_OZONE_PLATFORM_HINT: "wayland",
  // Firefox/hellfire was running through XWayland here (8/7/2026), where
  // fractional scaling makes click hit-testing unreliable — "some clicks
  // don't work". Force native Wayland for this session only; KDE keeps
  // whatever it was doing.
  MOZ_ENABLE_WAYLAND: "1",
  MOZ_DBUS_REMOTE: "1",
});
COMPOSITOR.env.publish();

COMPOSITOR.cursor.configure({
  theme: "Bibata-Modern-Ice",
  size: 24,
});

COMPOSITOR.window.decoration.configure((window, context) => {
  const appId = (window.appId() ?? "").toLowerCase();
  const isFirefox =
    appId === "firefox" ||
    appId.endsWith(".firefox") ||
    appId.includes("firefoxdeveloperedition");

  // The KDE manager advertises CSD before per-window metadata is available.
  // Keep that baseline while appId is unknown: sending an early SSD response
  // makes some Firefox/Chromium versions permanently build reduced chrome.
  if (appId.length === 0) {
    return { mode: context.clientPreference ?? "client" };
  }

  // Firefox can repeatedly renegotiate when CSD is rejected. Keep CSD even
  // when it relies on the manager default and sends no explicit preference.
  if (isFirefox) {
    return { mode: "client" };
  }

  return { mode: "server" };
});

const HYBRID_WINDOW_MANAGER = new HybridWindowManager(naturalRootRect);
const HOT_RELOAD_WINDOW_MANAGER_STATE = "config.hybrid-window-manager";
const FULLSCREEN_Z_INDEX = 2_000_000_000;

COMPOSITOR.onDisable((event) => {
  if (event.isReloading) {
    const snapshot = HYBRID_WINDOW_MANAGER.snapshot();
    event.persist(HOT_RELOAD_WINDOW_MANAGER_STATE, snapshot);
  }
});

COMPOSITOR.onEnable((event) => {
  if (event.isReloading) {
    const snapshot = event.restore<
      ReturnType<typeof HYBRID_WINDOW_MANAGER.snapshot>
    >(HOT_RELOAD_WINDOW_MANAGER_STATE);
    if (snapshot) {
      HYBRID_WINDOW_MANAGER.restore(snapshot);
    }
  }
});

// ---------------------------------------------------------------------------
// External IPC: expose the workspace layout to clients such as the bar.
//   workspaces.get           -> WorkspacesView                     (request/response)
//   workspaces.switch        { direction: -1 | 1 }                 (command)
//   workspaces.activate      { monitor: string, index: number }    (command)
//   workspaces.toggleTiling  { monitor?: string }                  (command)
//   workspaces.changed       -> WorkspacesView                     (broadcast)
//   windows.activate         { windowId: string }                  (command)
//   dock.proximity           { monitor: string, inside: bool }    (broadcast)
// ---------------------------------------------------------------------------
const WORKSPACE_IPC = createIpcServer();
let lastWorkspacesJson = "";
let workspaceBroadcastQueued = false;

function broadcastWorkspaces() {
  const view = HYBRID_WINDOW_MANAGER.viewForIpc();
  const json = JSON.stringify(view);
  if (json === lastWorkspacesJson) {
    return;
  }
  lastWorkspacesJson = json;
  WORKSPACE_IPC.broadcast("workspaces.changed", view);
}

function reconfigureProtocolWorkspaces() {
  COMPOSITOR.workspace.reconfigure();
}

// Coalesce many state mutations within one tick into a single diffed broadcast.
function scheduleWorkspaceBroadcast() {
  // Protocol state must be staged before the current runtime response is
  // written; otherwise key bindings/Waybar activations only reach external
  // bars on a later, unrelated runtime request.
  reconfigureProtocolWorkspaces();
  if (workspaceBroadcastQueued) {
    return;
  }
  workspaceBroadcastQueued = true;
  void Promise.resolve().then(() => {
    workspaceBroadcastQueued = false;
    broadcastWorkspaces();
  });
}

COMPOSITOR.workspace.configure(() => {
  const view = HYBRID_WINDOW_MANAGER.viewForIpc();
  return {
    groups: view.monitors.map((monitor) => ({
      id: monitor.name,
      outputs: [monitor.name],
      workspaces: monitor.workspaces.map((workspace) => ({
        id: `${monitor.name}:${workspace.index}`,
        name: String(workspace.index),
        coordinates: [Math.max(0, workspace.index - 1)],
        active: workspace.active,
        hidden: !workspace.active && workspace.windowCount === 0,
      })),
    })),
  };
});

COMPOSITOR.workspace.event.onActivate((event) => {
  const [monitor, rawIndex] = event.workspaceId.split(":");
  const index = Number(rawIndex);
  if (!monitor || !Number.isInteger(index) || index < 1) {
    return;
  }
  HYBRID_WINDOW_MANAGER.activate(monitor, index);
  scheduleWorkspaceBroadcast();
});

WORKSPACE_IPC.handle("workspaces.get", () =>
  HYBRID_WINDOW_MANAGER.viewForIpc(),
);
WORKSPACE_IPC.handle("workspaces.switch", (params) => {
  const direction = (params as { direction?: number } | undefined)?.direction;
  HYBRID_WINDOW_MANAGER.switchWorkspace(direction === -1 ? -1 : 1);
  scheduleWorkspaceBroadcast();
});
WORKSPACE_IPC.handle("workspaces.activate", (params) => {
  const request = params as { monitor?: string; index?: number } | undefined;
  if (request?.monitor && typeof request.index === "number") {
    HYBRID_WINDOW_MANAGER.activate(request.monitor, request.index);
    scheduleWorkspaceBroadcast();
  }
});
WORKSPACE_IPC.handle("workspaces.toggleTiling", (params) => {
  const monitor = (params as { monitor?: string } | undefined)?.monitor;
  if (monitor) {
    HYBRID_WINDOW_MANAGER.toggleWorkspaceTilingForMonitor(monitor);
  } else {
    HYBRID_WINDOW_MANAGER.toggleCurrentWorkspaceTiling();
  }
  scheduleWorkspaceBroadcast();
});
WORKSPACE_IPC.handle("windows.activate", (params) => {
  const windowId = (params as { windowId?: string } | undefined)?.windowId;
  if (typeof windowId === "string") {
    HYBRID_WINDOW_MANAGER.activateWindowById(windowId);
    scheduleWorkspaceBroadcast();
  }
});
WORKSPACE_IPC.handle("windows.close", (params) => {
  const windowId = (params as { windowId?: string } | undefined)?.windowId;
  if (typeof windowId === "string") {
    HYBRID_WINDOW_MANAGER.closeWindowById(windowId);
    scheduleWorkspaceBroadcast();
  }
});
// Debug helper (Rio maximize investigation): drive the same maximize path a
// client CSD button takes, addressable by window id from outside the session.
WORKSPACE_IPC.handle("windows.maximize", (params) => {
  const request = params as
    | { windowId?: string; maximized?: boolean }
    | undefined;
  if (!request?.windowId) {
    return;
  }
  const window = HYBRID_WINDOW_MANAGER.findWindowById(request.windowId);
  if (!window) {
    return;
  }
  if (request.maximized === false) {
    window.unmaximize();
  } else {
    window.maximize();
  }
  scheduleWorkspaceBroadcast();
});

// Bar window-controls: minimize the window (restore goes through
// windows.activate, which unminimizes and focuses).
WORKSPACE_IPC.handle("windows.minimize", (params) => {
  const request = params as { windowId?: string } | undefined;
  if (!request?.windowId) {
    return;
  }
  const window = HYBRID_WINDOW_MANAGER.findWindowById(request.windowId);
  if (!window) {
    return;
  }
  window
      .minimize();
  scheduleWorkspaceBroadcast();
});

// Diagnostic dump for the window-sizing investigation (7/2026): everything
// the runtime believes about outputs, layer exclusive zones, and the usable
// areas derived from them. Queryable from another session while the
// compositor is still running (VT switch, not logout):
//   printf '{"id":1,"method":"debug.geometry"}\n' \
//     | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/shojiwm-<display>.sock
// Config-schema revision handshake. Bump whenever the settings schema or
// the factories consuming it change, so MinkaConf can tell the user the
// running session predates the edit ("reload with Super+Shift+R") instead
// of silently half-applying. History: 1 = input + display scale;
// 2 = full display schema (position/mode/enabled/mirror/hdr).
const MINKA_CONFIG_REVISION = 2;
WORKSPACE_IPC.handle("minka.revision", () => ({
  revision: MINKA_CONFIG_REVISION,
}));

// Effective user settings, for MinkaConf to display.
WORKSPACE_IPC.handle("settings.get", () => activeSettings);
// Live-apply from MinkaConf: swap the active settings and re-run the input
// and output factories. No config reload; takes effect immediately.
WORKSPACE_IPC.handle("settings.apply", (params) => {
  if (!params || typeof params !== "object") {
    return { ok: false, error: "expected a settings object" };
  }
  activeSettings = params as MinkaSettings;
  COMPOSITOR.input.reconfigure();
  COMPOSITOR.output.reconfigure();
  return { ok: true };
});

WORKSPACE_IPC.handle("debug.geometry", () => {
  const usable: Record<string, unknown> = {};
  const insets: Record<string, unknown> = {};
  for (const name of COMPOSITOR.output.list) {
    usable[name] = COMPOSITOR.layer.usableArea(name);
    insets[name] = COMPOSITOR.layer.reservedInsets(name);
  }
  return {
    outputs: COMPOSITOR.output.current,
    usable,
    insets,
    layers: COMPOSITOR.layer.current,
  };
});

// ---------------------------------------------------------------------------
// Dock proximity: watch the pointer and broadcast enter/leave for the bottom
// strip of each monitor. The bar uses this in place of a layer-shell trigger
// surface (which would otherwise capture clicks meant for the windows below).
// ---------------------------------------------------------------------------
// Two thresholds with hysteresis:
//   - SHOW: pointer must be in the bottom 10px to trigger reveal
//   - HIDE: once visible, pointer must leave the bottom 120px to dismiss
// This gives a precise "reach for the dock" trigger while keeping the dock
// stable once the user is interacting with it (so brushing the cursor a few
// dozen pixels above the dock body does not flicker it away).
const DOCK_SHOW_ZONE_PX = 10;
const DOCK_HIDE_ZONE_PX = 120;
const dockProximityByMonitor = new Map<string, boolean>();

function pointerInBottomStrip(
  monitor: string,
  pointerX: number,
  pointerY: number,
  stripPx: number,
): boolean {
  const output = COMPOSITOR.output.get(monitor);
  if (!output || !output.resolution) {
    return false;
  }
  const width = output.resolution.width / output.scale;
  const height = output.resolution.height / output.scale;
  const left = output.position.x;
  const top = output.position.y;
  const right = left + width;
  const bottom = top + height;
  return (
    pointerX >= left &&
    pointerX < right &&
    pointerY >= bottom - stripPx &&
    pointerY < bottom
  );
}

function nextDockProximity(
  monitor: string,
  pointerX: number,
  pointerY: number,
  onTrackedMonitor: boolean,
): boolean {
  if (!onTrackedMonitor) return false;
  const wasInside = dockProximityByMonitor.get(monitor) === true;
  // While outside, only the narrow show-zone counts (10px).
  // While inside, the wide hide-zone keeps it open (120px).
  return pointerInBottomStrip(
    monitor,
    pointerX,
    pointerY,
    wasInside ? DOCK_HIDE_ZONE_PX : DOCK_SHOW_ZONE_PX,
  );
}

function updateDockProximity(monitor: string, inside: boolean) {
  if (dockProximityByMonitor.get(monitor) === inside) {
    return;
  }
  dockProximityByMonitor.set(monitor, inside);
  WORKSPACE_IPC.broadcast("dock.proximity", { monitor, inside });
}

// Snap-zone preview: broadcast the active snap rect (floating edge zones, or the
// opened tiling slot) to the bar, which renders the rounded preview overlay.
//   snap.preview  { monitor, rect: {x,y,w,h} | null, kind: "floating"|"tiling" }
let lastSnapJson = "";
HYBRID_WINDOW_MANAGER.setSnapPreviewBroadcaster((preview) => {
  const json = JSON.stringify(preview);
  if (json === lastSnapJson) {
    return;
  }
  lastSnapJson = json;
  WORKSPACE_IPC.broadcast("snap.preview", preview);
});

HYBRID_WINDOW_MANAGER.setWorkspaceChangeBroadcaster(() => {
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.onDisable(() => {
  WORKSPACE_IPC.close();
});


COMPOSITOR.process.once("fcitx5", {
  command: "fcitx5 -d",
  runPolicy: "once-per-session",
});


// MinkaShell (Quickshell-based) is the session shell now; shoji-bar-2 is retired.
// Logs go to /tmp/minkashell.log so warnings and crashes survive the session
// for later inspection (Sophie has to switch to KDE to debug with Claude).
COMPOSITOR.process.once("shell", {
  command: "qs -p \"$HOME/Documents/src/MinkaDE/MinkaShell\" > /tmp/minkashell.log 2>&1",
  runPolicy: "once-per-session",
});
// MinkaFX: the Guido-style wgpu overlay process (snap preview, future OSDs).
// Guarded so a not-yet-built binary is a silent no-op instead of a failure.
COMPOSITOR.process.once("MinkaFX", {
  command: "[ -x \"$HOME/Documents/src/MinkaDE/MinkaFX/target/release/MinkaFX\" ] && exec \"$HOME/Documents/src/MinkaDE/MinkaFX/target/release/MinkaFX\" > /tmp/minkafx.log 2>&1",
  runPolicy: "once-per-session",
});
// Polkit authentication agent: without one, anything that needs privilege
// escalation (pamac, GParted, systemd prompts…) fails silently because
// polkit has nowhere to send the password dialog. KDE sessions start their
// own; ours must too. `once` rather than a restarting service: polkit
// permits one agent per session, so a duplicate spawn exits immediately
// and a restart-on-exit policy would loop on that.
// TODO(Minka): replace with a themed MinkaConf-family agent eventually.
COMPOSITOR.process.once("polkit-agent", {
  command: "/usr/lib/polkit-kde-authentication-agent-1",
  runPolicy: "once-per-session",
});
// cliphist clipboard history watchers. Text and image need separate watchers;
// run as services so they are restarted if they ever exit.
COMPOSITOR.process.service("cliphist-text", {
  command: ["wl-paste", "--type", "text", "--watch", "cliphist", "store"],
  restart: "on-exit",
});
COMPOSITOR.process.service("cliphist-image", {
  command: ["wl-paste", "--type", "image", "--watch", "cliphist", "store"],
  restart: "on-exit",
});

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

// Resolve the monitor under the cursor and toggle the start menu.
// MinkaShell listens for the ui.startMenu broadcast on the IPC socket.
function toggleStartMenu() {
  const monitor = HYBRID_WINDOW_MANAGER.getCurrentMonitorName();
  WORKSPACE_IPC.broadcast("ui.startMenu", {
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
// watchers above keep collecting history for a future picker, so Super+V is
// intentionally unbound for now.
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
COMPOSITOR.key.bind("cycle-windows", "Alt+Tab", () => {
  HYBRID_WINDOW_MANAGER.cycleWorkspaceFocus(1);
  scheduleWorkspaceBroadcast();
});
COMPOSITOR.key.bind("cycle-windows-back", "Alt+Shift+Tab", () => {
  HYBRID_WINDOW_MANAGER.cycleWorkspaceFocus(-1);
  scheduleWorkspaceBroadcast();
});
COMPOSITOR.key.bind("toggle-tiling-mode", "Super+S", () => {
  HYBRID_WINDOW_MANAGER.toggleCurrentWorkspaceTiling();
  scheduleWorkspaceBroadcast();
});
COMPOSITOR.key.bind("close-focused-window", "Super+Q", () => {
  HYBRID_WINDOW_MANAGER.closeFocusedWindow();
});
COMPOSITOR.key.bind("toggle-focused-window-maximize", "Super+M", () => {
  HYBRID_WINDOW_MANAGER.toggleFocusedWindowMaximize();
});
COMPOSITOR.key.bind("tile-focus-left-quick", "Super+Left", () => {
  HYBRID_WINDOW_MANAGER.focusTile(-1);
});
COMPOSITOR.key.bind("tile-focus-right-quick", "Super+Right", () => {
  HYBRID_WINDOW_MANAGER.focusTile(1);
});
COMPOSITOR.key.bind("tile-focus-left", "Super+Ctrl+Left", () => {
  HYBRID_WINDOW_MANAGER.focusTile(-1);
});
COMPOSITOR.key.bind("tile-focus-right", "Super+Ctrl+Right", () => {
  HYBRID_WINDOW_MANAGER.focusTile(1);
});
COMPOSITOR.key.bind("tile-move-left", "Super+Shift+Left", () => {
  HYBRID_WINDOW_MANAGER.moveFocusedTile(-1);
  scheduleWorkspaceBroadcast();
});
COMPOSITOR.key.bind("tile-move-right", "Super+Shift+Right", () => {
  HYBRID_WINDOW_MANAGER.moveFocusedTile(1);
  scheduleWorkspaceBroadcast();
});
COMPOSITOR.key.bind("window-move-workspace-prev", "Super+Shift+Up", () => {
  HYBRID_WINDOW_MANAGER.moveFocusedWindowToWorkspace(-1);
  scheduleWorkspaceBroadcast();
});
COMPOSITOR.key.bind("window-move-workspace-next", "Super+Shift+Down", () => {
  HYBRID_WINDOW_MANAGER.moveFocusedWindowToWorkspace(1);
  scheduleWorkspaceBroadcast();
});
COMPOSITOR.key.bind("workspace-prev", "Super+Ctrl+Up", () => {
  HYBRID_WINDOW_MANAGER.switchWorkspace(-1);
  scheduleWorkspaceBroadcast();
});
COMPOSITOR.key.bind("workspace-next", "Super+Ctrl+Down", () => {
  HYBRID_WINDOW_MANAGER.switchWorkspace(1);
  scheduleWorkspaceBroadcast();
});

let fpsCounter = false;
COMPOSITOR.key.bind("fps", "Super+Shift+F", () => {
  fpsCounter = !fpsCounter;
  COMPOSITOR.debug.fpsCounter = fpsCounter;
});

// Displays are fully user-managed through MinkaConf (minka-settings.json):
// scale, explicit position, mode, enable/disable, mirroring, and the HDR
// opt-in (HDR only engages when the sink's EDID advertises PQ support).
// Connectors with no entry get KDE-parity defaults: best mode, auto
// position, scale 1.0.
let profileEnabled = false;
COMPOSITOR.key.bind("profile", "Super+Shift+T", () => {
  profileEnabled = !profileEnabled;
  COMPOSITOR.debug.enableProfile(profileEnabled);
});

COMPOSITOR.output.configure((context) => {
  const display: DisplayConfigDraft = {};

  const names = new Set<string>(Object.keys(activeSettings.displays));
  for (const output of context.connected) {
    names.add(output.name);
  }

  for (const name of names) {
    const entry = activeSettings.displays[name];
    if (entry?.enabled === false) {
      display[name] = { mode: "disabled" };
      continue;
    }
    if (entry?.mirror) {
      display[name] = { mode: "mirror", source: entry.mirror };
      continue;
    }
    display[name] = {
      mode: "extend",
      resolution: entry?.resolution ?? "best",
      position: entry?.position ?? "auto",
      scale: entry?.scale ?? 1.0,
      hdr: entry?.hdr === true,
    };
  }

  // Lid-closed docked mode: the external monitor replaces the built-ins.
  const isDocked = context.connected.some(
    (output) => output.name === "HDMI-A-1",
  );
  if (isDocked) {
    display["eDP-1"] = { mode: "disabled" };
    display["eDP-2"] = { mode: "disabled" };
  }

  // Keep the live layout's bounding box anchored at (0,0), like xrandr does.
  // X11 toolkits reading RandR through the Xwayland bridge assume the screen
  // starts at the top-left monitor corner (GTK clips menu workareas against
  // it). MinkaConf normalizes the arrangement it saves, but only across the
  // displays connected at the time — so a disconnect (e.g. the TV owning the
  // top-left corner) can leave the remaining subset with a floating origin.
  // Translating every output by the same delta preserves the arrangement and
  // is invisible to the user; "auto" positions are left to the compositor.
  const connectedNames = new Set(
      context.connected
          .map(
              (output) => output.name
          )
  );
  const positioned: { x: number; y: number }[] = [];
  for (const [name, entry] of Object.entries(display)) {
    if (
      entry.mode === "extend" &&
      typeof entry.position === "object" &&
      connectedNames
          .has(
              name
          )
    ) {
      positioned
          .push(
              entry.position
          );
    }
  }
  if (positioned.length > 0) {
    const minX = Math
        .min(...positioned.map((position) => position.x));
    const minY = Math
        .min(...positioned.map((position) => position.y));
    if (minX !== 0 || minY !== 0) {
      for (const position of positioned) {
        position.x -= minX;
        position.y -= minY;
      }
    }
  }

  return display;
});

// Pointer/touchpad behavior comes from minka-settings.json (MinkaConf).
// 8/7/2026 defaults per Sophie: adaptive accel at +0.4 (the old flat/0.0
// was "acceleration too low") and natural scroll off everywhere.
COMPOSITOR.input.configure((input, _context) => {
  const inputSettings = activeSettings.input;
  input.global = {
    touchpad: {
      tapToClick: inputSettings.touchpad.tapToClick,
      naturalScroll: inputSettings.touchpad.naturalScroll,
      scrollMethod: inputSettings.touchpad.scrollMethod as InputScrollMethod,
      disableWhileTyping: inputSettings.touchpad.disableWhileTyping,
      scrollFactor: inputSettings.touchpad.scrollFactor,
      pointerAccel: inputSettings.pointerAccel,
      accelProfile: inputSettings.accelProfile as InputAccelProfile,
    },
    pointer: {
      pointerAccel: inputSettings.pointerAccel,
      accelProfile: inputSettings.accelProfile as InputAccelProfile,
      naturalScroll: inputSettings.naturalScroll,
    },
    keyboard: {
      layout: inputSettings.keyboard?.layout || "us",
      ...(inputSettings.keyboard?.variant
        ? { variant: inputSettings.keyboard.variant }
        : {}),
      options: "caps:ctrl_modifier",
      repeatRate: 60,
      repeatDelay: 250,
    },
  };

  input.device["Razer Razer Blade Keyboard"] = {
    keyboard: {
      layout: "us",
    },
  };
});

HYBRID_WINDOW_MANAGER.configureWorkspaceGestureSpeed({
  workspaceScrollFactor: 1.5,
  workspaceScrollKineticFactor: 1,
  workspaceSwitchFactor: 1,
  workspaceSwitchVelocityFactor: 1,
});

COMPOSITOR.effect.background_effect = compileEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
  pipeline: [dualKawaseBlur({ radius: 4, passes: 2 })],
});

const LAYER_BLUR_MASK = compileLayerEffect({
  input: backdropSource(),
  capturePadding: 24,
  invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
  // The mask stage intentionally outputs transparency (the blur is clipped
  // to the layer's own alpha), so the pipeline's alpha must survive the
  // finish/display passes instead of being forced opaque.
  alpha: "preserve",
  pipeline: [
    dualKawaseBlur({ radius: 4, passes: 2 }),
    shaderStage(loadShader("./src/layer-blur-mask.frag"), {
      textures: {
        layer_mask: layerSource(),
      },
      uniforms: {
        opacity_threshold: 0.25,
        mask_feather: 0.04,
      },
    }),
  ],
});

COMPOSITOR.effect.layer = (layer) => {
  if (layer.namespace() === "no_blur") {
    return {};
  }

  return {
    behind: LAYER_BLUR_MASK,
  };
};

const POPUP_BLUR = compilePopupEffect({
  input: backdropSource(),
  capturePadding: 4 * 2 * 2 + 24 + 32,
  invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
  // The mask stage intentionally outputs transparency (the blur is clipped
  // to the layer's own alpha), so the pipeline's alpha must survive the
  // finish/display passes instead of being forced opaque.
  alpha: "preserve",
  pipeline: [
    dualKawaseBlur({ radius: 4, passes: 2 }),
    shaderStage(loadShader("./src/layer-blur-mask.frag"), {
      textures: {
        layer_mask: popupSource(),
      },
      uniforms: {
        opacity_threshold: 0.25,
        mask_feather: 0.04,
      },
    }),
  ],
});

COMPOSITOR.effect.popup = (popup) => {
  if (popup.parentKind === "window") {
    return {};
  }

  return {
    behind: POPUP_BLUR,
  };
};

// GTK3 tooltips (waybar) declare their whole rect opaque despite transparent
// rounded corners, which paints the corners as a solid fill and culls the
// behind-blur. Ignore the declaration for layer-shell popups.
COMPOSITOR.rendering.surfacePolicy = (surface) => {
  if (surface.kind === "popup" && surface.parentKind === "layer") {
    return { opaqueRegion: "ignore" };
  }
  return null;
};

// The dock displays live window titles, so a title change must refresh the
// IPC view. The broadcast is JSON-diffed and coalesced per tick, so noisy
// title churn (terminals) only goes out when the string actually changed.
const titleSubscriptions = new Map<string, () => void>();

COMPOSITOR.event.onOpen((window) => {
  HYBRID_WINDOW_MANAGER.onOpen(window);
  titleSubscriptions.set(
    window.id,
    window.title.subscribe(() => scheduleWorkspaceBroadcast()),
  );
});

COMPOSITOR.event.onFirstCommit((window) => {
  HYBRID_WINDOW_MANAGER.onFirstCommit(window);
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.event.onStartClose((window) => {
  HYBRID_WINDOW_MANAGER.onStartClose(window);
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.event.onClose((window) => {
  HYBRID_WINDOW_MANAGER.onClose(window);
  titleSubscriptions.get(window.id)?.();
  titleSubscriptions.delete(window.id);
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.event.onFocus((window, focused) => {
  HYBRID_WINDOW_MANAGER.onFocus(window, focused);
  if (focused) {
    HYBRID_WINDOW_MANAGER.recordFocus(window.id);
  }
  // Broadcast on loss of focus too: when the unfocus event lands in a later
  // tick than the gain, a gain-only broadcast snapshots BOTH windows as
  // focused and nothing ever corrects it — the dock/bar keep highlighting
  // the previously focused window (Sophie's "selection border persists").
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.event.onPointerMoveAsync((event) => {
  HYBRID_WINDOW_MANAGER.onPointerMove(event);

  // Dock proximity: update only the monitor the pointer is currently on,
  // and emit "leave" for other monitors that were previously inside. The
  // narrow/wide threshold is hysteretic per current state.
  const pointerX = event.position.x;
  const pointerY = event.position.y;
  for (const monitor of COMPOSITOR.output.list) {
    const inside = nextDockProximity(
      monitor,
      pointerX,
      pointerY,
      monitor === event.outputName,
    );
    updateDockProximity(monitor, inside);
  }
});

COMPOSITOR.event.onGestureSwipeAsync((event) => {
  HYBRID_WINDOW_MANAGER.onGestureSwipe(event);
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.event.onOutputChange((event) => {
  HYBRID_WINDOW_MANAGER.onOutputChange(event);
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.event.onCreateLayer(() => {
  HYBRID_WINDOW_MANAGER.refreshUsableAreaLayouts();
});

COMPOSITOR.event.onUpdateLayer(() => {
  HYBRID_WINDOW_MANAGER.refreshUsableAreaLayouts();
});

COMPOSITOR.event.onDestroyLayer(() => {
  HYBRID_WINDOW_MANAGER.refreshUsableAreaLayouts();
});

COMPOSITOR.event.onWindowResize((event) => {
  HYBRID_WINDOW_MANAGER.onWindowResize(event);
});

COMPOSITOR.pointer.bindWindowMoveModifier("Super");
COMPOSITOR.pointer.bindWindowResizeModifier("Super");

COMPOSITOR.event.onWindowMove((event) => {
  HYBRID_WINDOW_MANAGER.onWindowMove(event);
  // A drag can hand the window to another monitor's workspace (adoption in
  // onWindowMove); without a broadcast the dock keeps listing it on the old
  // output until some unrelated event refreshes the view.
  if (event.phase === "end" || event.phase === "cancel") {
    scheduleWorkspaceBroadcast();
  }
});

COMPOSITOR.event.onWindowMaximizeRequest((event) => {
  HYBRID_WINDOW_MANAGER.onWindowMaximizeRequest(event);
  // The workspaces view carries maximized/minimized per window (the bar's
  // window controls render from it), so state changes must broadcast.
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.event.onWindowMinimizeRequest((event) => {
  HYBRID_WINDOW_MANAGER.onWindowMinimizeRequest(event);
  scheduleWorkspaceBroadcast();
});

COMPOSITOR.event.onWindowFullscreenRequest((event) => {
  HYBRID_WINDOW_MANAGER.onWindowFullscreenRequest(event);
});

COMPOSITOR.event.onWindowActivateRequest((event) => {
  HYBRID_WINDOW_MANAGER.onWindowActivateRequest(event);
  scheduleWorkspaceBroadcast();
});

function naturalRootRect(window: WaylandWindow): ManagedWindowRect {
  const client = window.position;
  return {
    x: client.x - WINDOW_BORDER_PX,
    y: client.y - TITLEBAR_HEIGHT - WINDOW_BORDER_PX,
    width: client.width + WINDOW_BORDER_PX * 2,
    height: client.height + TITLEBAR_HEIGHT + WINDOW_BORDER_PX * 2,
  };
}

COMPOSITOR.window.composition = (window: WaylandWindow) => {
  const workspaceVisible = window.state[WINDOW_STATE_WORKSPACE_VISIBLE];
  const workspaceOffsetY = window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y];
  const workspaceOpacity = window.state[WINDOW_STATE_WORKSPACE_OPACITY];
  const tileDragging = window.state[WINDOW_STATE_TILE_DRAGGING];
  const managedRect = computed(() => {
    const rect = window.state[WINDOW_STATE_RECT]();
    return {
      x: read(rect.x),
      y: read(rect.y) + workspaceOffsetY(),
      width: read(rect.width),
      height: read(rect.height),
    };
  });
  const forceRectSize = computed(
    () => window.isResizable() && !window.isTransient(),
  );
  const tiled = computed(
    () => window.appId() === "mpv" || window.state[WINDOW_STATE_TILED](),
  );
  const minimizeVisualIdle = window.state[WINDOW_STATE_MINIMIZE_VISUAL_IDLE];
  const inactive = computed(
    () => minimizeVisualIdle() || (!workspaceVisible() && !tileDragging()),
  );

  // Eternal Darkness red (Theme.red / Theme.redDim until shared theme.json).
  const borderColor = window.isFocused((focused) =>
    focused ? "#e0263c" : "#8f1e2d",
  );
  const titlebarBackground = window.isFocused((focused) =>
    focused ? "#1f243080" : "#2a2f3a80",
  );
  const titleColor = window.isFocused((focused) =>
    focused ? "#f5f7fa" : "#c9d1d9",
  );

  const titlebarStyle: SSDStyle = {
    height: TITLEBAR_HEIGHT,
    paddingX: 8,
    gap: 8,
    alignItems: "center",
    background: titlebarBackground,
  };

  const backgroundShader = compileEffect({
    input: backdropSource(),
    capturePadding: 24,
    invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
    pipeline: [
      dualKawaseBlur({ radius: 4, passes: 2 }),
      shaderStage(loadShader("./src/liquid-glass.frag"), {
        uniforms: {
          glass_radius_px: 10.0,
          distortion_depth: 0.2,
          distortion_strength: 0.15,
          chromatic_shift_px: 3.0,
          glass_tint: 0.9,
        },
      }),
    ],
  });

  const titleOnlyShader = compileEffect({
    input: backdropSource(),
    capturePadding: 24,
    invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
    pipeline: [dualKawaseBlur({ radius: 4, passes: 2 })],
  });

  const appIcon = (
    <AppIcon icon={window.icon} style={{ width: 16, height: 16 }} />
  );
  const label = (
    <Label
      text={window.title}
      style={{
        color: titleColor,
        fontFamily: ["Noto Sans CJK JP", "Noto Color Emoji"],
        fontSize: 13,
        fontWeight: 600,
        flexGrow: 1,
        flexShrink: 1,
        minWidth: 0,
      }}
    />
  );
  const minimizeButton = <MinimizeButton window={window} />;
  const maximizeButton = <MaximizeButton window={window} />;
  const closeButton = <CloseButton window={window} />;

  let innerComponents = (
    <Box direction="column">
      <ShaderEffect
        shader={titleOnlyShader}
        direction="row"
        style={titlebarStyle}
      >
        {appIcon}
        {label}
        {minimizeButton}
        {maximizeButton}
        {closeButton}
      </ShaderEffect>
      <ClientWindow />
    </Box>
  );

  const TERMINALS = ["kitty", "ghostty"];

  if (TERMINALS.includes(window.appId() ?? "")) {
    innerComponents = (
      <ShaderEffect shader={backgroundShader} direction="column">
        <Box direction="row" style={titlebarStyle}>
          {appIcon}
          {label}
          {minimizeButton}
          {maximizeButton}
          {closeButton}
        </Box>
        <ClientWindow />
      </ShaderEffect>
    );
  }

  // Fullscreen: drop all chrome (titlebar, border, rounded corners) and let
  // the client surface fill its managed rect edge to edge. The rect is set to
  // the whole output by onWindowFullscreenRequest. Rendering nothing but the
  // bare ClientWindow is also what lets the tty backend promote the client
  // buffer to the primary plane (direct scanout).
  if (window.state[WINDOW_STATE_FULLSCREEN]()) {
    return (
      <ManagedWindow
        rect={managedRect}
        zIndex={FULLSCREEN_Z_INDEX}
        visibleOutputs={window.state[WINDOW_STATE_VISIBLE_OUTPUTS]}
        opacity={workspaceOpacity}
        forceRectSize={forceRectSize}
        tiled={tiled}
        idle={inactive}
        interactive={inactive((value) => !value)}
        // Permit low-latency tearing for fullscreen windows. The compositor only actually tears
        // once the window is on the direct-scanout fast path and is committing faster than the
        // refresh rate (i.e. games), so this is a no-op for ordinary fullscreen apps. Narrow it
        // per app if desired, e.g. `allowTearing={isGame(window.appId())}`.
        allowTearing={true}
      >
        <ClientWindow />
      </ManagedWindow>
    );
  }

  if (window.decoration().mode === "client") {
    return (
      <ManagedWindow
        rect={managedRect}
        zIndex={HYBRID_WINDOW_MANAGER.getWindowZIndex(window)}
        visibleOutputs={window.state[WINDOW_STATE_VISIBLE_OUTPUTS]}
        opacity={workspaceOpacity}
        forceRectSize={forceRectSize}
        tiled={tiled}
        idle={inactive}
        interactive={inactive((value) => !value)}
      >
        <ClientWindow />
      </ManagedWindow>
    );
  }

  // Maximized: keep the titlebar but drop the border, padding and rounded
  // corners — square frame, edge to edge in the usable area
  // (maximizedRectForWindow applies no inset to match). Floating windows
  // below keep the full chrome.
  if (window.state[WINDOW_STATE_MAXIMIZED]()) {
    return (
      <ManagedWindow
        rect={managedRect}
        zIndex={HYBRID_WINDOW_MANAGER.getWindowZIndex(window)}
        visibleOutputs={window.state[WINDOW_STATE_VISIBLE_OUTPUTS]}
        opacity={workspaceOpacity}
        forceRectSize={forceRectSize}
        tiled={tiled}
        idle={inactive}
        interactive={inactive((value) => !value)}
      >
        <Box direction="row">{innerComponents}</Box>
      </ManagedWindow>
    );
  }

  return (
    <ManagedWindow
      rect={managedRect}
      zIndex={HYBRID_WINDOW_MANAGER.getWindowZIndex(window)}
      visibleOutputs={window.state[WINDOW_STATE_VISIBLE_OUTPUTS]}
      opacity={workspaceOpacity}
      forceRectSize={forceRectSize}
      tiled={tiled}
      idle={inactive}
      interactive={inactive((value) => !value)}
    >
      <WindowBorder
        style={{
          border: { px: WINDOW_BORDER_PX, color: borderColor },
          borderRadius: 10,
          background: "#10131900",
          padding: 0,
          paddingX: 0,
          paddingRight: 0,
        }}
        interaction={{
          resizeHitArea: {
            edgePx: 8,
            cornerPx: 14,
          },
        }}
      >
        <Box direction="row">{innerComponents}</Box>
      </WindowBorder>
    </ManagedWindow>
  );
};

const CloseButton = ({ window }: { window: WaylandWindow }) => {
  const [hover, setHover] = useState(false);

  const borderColor = hover((hover) => (hover ? "#00000000" : "#F0808030"));

  let icon: CompositionRenderable | null = null;
  if (hover()) {
    icon = (
      <Image
        src="./assets/x.svg"
        style={{
          width: 16,
          height: 16,
          position: "absolute",
          zIndex: 1,
          pointerEvents: "none",
        }}
      />
    );
  }

  return (
    <Box style={{ position: "relative", flexShrink: 0 }}>
      <Button
        onHoverChange={setHover}
        style={{
          width: 16,
          height: 16,
          borderRadius: 8,
          background: "#FFFFFF20",
          border: { px: 1, color: borderColor },
        }}
        onClick={window.close}
      />
      {icon}
    </Box>
  );
};

const MaximizeButton = ({ window }: { window: WaylandWindow }) => {
  const [hover, setHover] = useState(false);

  const borderColor = computed(() => {
    if (!window.isResizable()) {
      return "#00000000";
    }
    return hover() ? "#00000000" : "#00BFFF30";
  });
  const shouldHover = computed(() => hover() && window.isResizable());

  let icon: CompositionRenderable | null = null;
  if (shouldHover()) {
    const src = window.isMaximized((maximized) => {
      return maximized ? "./assets/minimize-2.svg" : "./assets/maximize-2.svg";
    });

    icon = (
      <Image
        src={src}
        style={{
          width: 16,
          height: 16,
          position: "absolute",
          zIndex: 1,
          pointerEvents: "none",
        }}
      />
    );
  }

  return (
    <Box style={{ position: "relative", flexShrink: 0 }}>
      <Button
        onHoverChange={setHover}
        style={{
          width: 16,
          height: 16,
          borderRadius: 8,
          background: "#FFFFFF20",
          border: { px: 1, color: borderColor },
        }}
        onClick={() => {
          if (!read(window.isResizable)) {
            return;
          }

          if (read(window.isMaximized)) {
            window.unmaximize();
          } else {
            window.maximize();
          }
        }}
      />
      {icon}
    </Box>
  );
};

const MinimizeButton = ({ window }: { window: WaylandWindow }) => {
  const [hover, setHover] = useState(false);

  const borderColor = hover((hover) => (hover ? "#00000000" : "#F8FF7530"));

  let icon: CompositionRenderable | null = null;
  if (hover()) {
    icon = (
      <Image
        src="./assets/minus.svg"
        style={{
          width: 16,
          height: 16,
          position: "absolute",
          zIndex: 1,
          pointerEvents: "none",
        }}
      />
    );
  }

  return (
    <Box style={{ position: "relative", flexShrink: 0 }}>
      <Button
        onHoverChange={setHover}
        style={{
          width: 16,
          height: 16,
          borderRadius: 8,
          background: "#FFFFFF20",
          border: { px: 1, color: borderColor },
        }}
        onClick={() => window.minimize()}
      />
      {icon}
    </Box>
  );
};

export default COMPOSITOR;
