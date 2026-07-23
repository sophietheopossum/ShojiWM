import {
  Box,
  ClientWindow,
  Image,
  ShaderEffect,
  COMPOSITOR,
  WindowBorder,
  backdropSource,
  compileEffect,
  compileLayerEffect,
  dualKawaseBlur,
  type SSDStyle,
  type WaylandWindow,
  computed,
  signal,
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
  InputAccelProfile,
  InputScrollMethod,
  ManagedWindowRect,
} from "shoji_wm/types";
import { 
    createIpcServer, 
    wakeRust
} from "shoji_wm/ipc";
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
  // XCursor theme + size, owned by MinkaConf's cursor page. Optional so
  // settings files from before revision 3 still parse.
  cursor?: { theme?: string; size?: number };
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
const MINKA_SETTINGS_PATH = `${process.env.HOME}/.config/minka-settings.json`;

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
  EDGE_DRAG_HALO_PX,
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
import type { 
    WorkspacesView,
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
//   windows.setRect          { windowId, x, y, width, height }     (request/response)
//   dock.proximity           { monitor: string, inside: bool }    (broadcast)
// ---------------------------------------------------------------------------
const WORKSPACE_IPC = createIpcServer();
let lastWorkspacesJson = "";
let workspaceBroadcastQueued = false;

// Live drag-tab geometry per window id, registered by the decoration
// composition below. Entries are lazy computeds, so hover/pointer state is
// only sampled when a view is actually built (MinkaMon's poll) — idle
// windows still never re-evaluate on mouse motion.
const DRAG_TAB_RECTS = new Map<
  string,
  () => { x: number; y: number; width: number; height: number } | null
>();

function attachDragTabs(view: WorkspacesView): WorkspacesView {
  const live = new Set<string>();
  for (const monitor of view.monitors) {
    for (const workspace of monitor.workspaces) {
      for (const win of workspace.windows) {
        live.add(win.id);
        win.dragTab = DRAG_TAB_RECTS.get(win.id)?.() ?? null;
      }
    }
  }
  for (const id of DRAG_TAB_RECTS.keys()) {
    if (!live.has(id)) {
      DRAG_TAB_RECTS.delete(id);
    }
  }
  return view;
}

// Live rect stream for MinkaMon's leader lines: pushed on every window
// move/resize event batch so the lines track drags at event rate instead
// of the client's fallback poll. Minimal payload (id + rect + drag tab),
// coalesced per tick; clients that don't know the event ignore it.
let rectsBroadcastQueued = false;
function scheduleRectsBroadcast() {
  if (rectsBroadcastQueued) {
    return;
  }
  rectsBroadcastQueued = true;
  void Promise.resolve().then(() => {
    rectsBroadcastQueued = false;
    // No listeners, no work: with MinkaMon closed this path costs nothing
    // and the runtime behaves exactly as if the tap didn't exist.
    if (WORKSPACE_IPC.clientCount() === 0) {
      return;
    }
    const windows = [];
    for (const window of HYBRID_WINDOW_MANAGER.listWindows()) {
      const rect = window.state[WINDOW_STATE_RECT]();
      windows.push({
        id: window.id,
        x: read(rect.x),
        y: read(rect.y),
        width: read(rect.width),
        height: read(rect.height),
        dragTab: DRAG_TAB_RECTS.get(window.id)?.() ?? null,
      });
    }
    WORKSPACE_IPC.broadcast("windows.rects", { windows });
    // This microtask runs AFTER the triggering event's request/response
    // cycle has been drained, so anything it touched in runtime state is
    // invisible to the compositor's scheduler until the next poll — which
    // otherwise only comes with further input ("updates only when the
    // mouse moves", regressed in 0.16.10). Same contract as IPC handlers:
    // wake the compositor explicitly.
    wakeRust();
  });
}

function broadcastWorkspaces() {
  const view = attachDragTabs(
      HYBRID_WINDOW_MANAGER.viewForIpc(),
  );
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
  attachDragTabs(
      HYBRID_WINDOW_MANAGER.viewForIpc(),
  ),
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
// Client-declared semantic window roles ("typed segments", ported from
// Arcan's SHMIF idea), for example: Minka apps claim what a window *is* — e.g.
// "minkamon.disk" — so consumers (leader lines, overview arrangement,
// MinkaShot's window capture) stop matching on mutable title strings.
WORKSPACE_IPC.handle("windows.identify", (params) => {
  const request = params as
    | { windowId?: string; role?: string | null }
    | undefined;
  if (typeof request?.windowId === "string") {
    HYBRID_WINDOW_MANAGER.setWindowRole(
      request.windowId,
      typeof request.role === "string" ? request.role : null,
    );
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

// Externally-driven move/resize (MinkaMon's full-overview arrangement).
WORKSPACE_IPC.handle("windows.setRect", (params) => {
  const request = params as
    | {
        windowId?: string;
        x?: number;
        y?: number;
        width?: number;
        height?: number;
      }
    | undefined;
  if (
    !request ||
    typeof request.windowId !== "string" ||
    typeof request.x !== "number" ||
    typeof request.y !== "number" ||
    typeof request.width !== "number" ||
    typeof request.height !== "number"
  ) {
    return { ok: false };
  }
  const ok = HYBRID_WINDOW_MANAGER.setWindowRectById(request.windowId, {
    x: request.x,
    y: request.y,
    width: request.width,
    height: request.height,
  });
  scheduleWorkspaceBroadcast();
  return { ok };
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
// 2 = full display schema (position/mode/enabled/mirror/hdr);
// 3 = cursor theme + size.
const MINKA_CONFIG_REVISION = 3;
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
  applyCursorSettings();
  return { ok: true };
});

// Cursor theme + size come from minka-settings.json (MinkaConf's cursor
// page). configure() applies live and also exports XCURSOR_THEME /
// XCURSOR_SIZE to the systemd and D-Bus activation environments, so apps
// launched afterwards agree with the compositor. Feature-detected so this
// config still evaluates on a ShojiWM build without the cursor API.
function applyCursorSettings() {
  const cursorApi = (
    COMPOSITOR as {
      cursor?: { configure(config: { theme: string; size: number }): void };
    }
  ).cursor;
  const cursor = activeSettings.cursor;
  if (!cursorApi || !cursor?.theme) {
    return;
  }
  cursorApi.configure({
    theme: cursor.theme,
    size: cursor.size ?? 24,
  });
}
applyCursorSettings();

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
// for later inspection.
// MINKA_SHELL_DIR overrides the installed location for repo-checkout sessions
// (set in shojiwm-env.fish); tarball installs land in /usr/share/minka.
COMPOSITOR.process.once("shell", {
  command: "qs -p \"${MINKA_SHELL_DIR:-/usr/share/minka/MinkaShell}\" > /tmp/minkashell.log 2>&1",
  runPolicy: "once-per-session",
});
// MinkaShot: freeze-frame screenshot tool. Runs as a daemon so the Print
// keybind's ui.minkashot broadcast always has a listener; overlays are
// pre-declared and hidden until armed, same philosophy as the shell.
COMPOSITOR.process.once("minkashot", {
  command: "qs -p \"${MINKA_SHOT_DIR:-/usr/share/minka/MinkaShot}\" > /tmp/minkashot.log 2>&1",
  runPolicy: "once-per-session",
});
// MinkaFX: the Guido-style wgpu overlay process (snap preview, future OSDs).
// Guarded so a missing/not-yet-built binary is a silent no-op instead of a
// failure. MINKA_FX_BIN overrides the installed path for repo-checkout runs.
COMPOSITOR.process.once("MinkaFX", {
  command: "MINKA_FX=\"${MINKA_FX_BIN:-/usr/bin/MinkaFX}\"; [ -x \"$MINKA_FX\" ] && exec \"$MINKA_FX\" > /tmp/minkafx.log 2>&1",
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
// MinkaShot: freeze-frame capture UI (MinkaDE/MinkaShot). The running app
// listens for this broadcast on the IPC socket, same pattern as the start
// menu.
COMPOSITOR.key.bind("minkashot", "Print", () => {
  WORKSPACE_IPC.broadcast("ui.minkashot", { action: "interactive" });
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
COMPOSITOR.key.bind("close-focused-window-alt-f4", "Alt+F4", () => {
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

// Global pointer position for the drag tabs: each tab centres on the mouse
// along its edge. Only compositions with a hovered edge depend on this
// signal, so idle windows do no work per pointer motion.
const [pointerPosition, setPointerPosition] = signal({ x: 0, y: 0 });

COMPOSITOR.event.onPointerMoveAsync((event) => {
  setPointerPosition({ x: event.position.x, y: event.position.y });
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
  scheduleRectsBroadcast();
});

COMPOSITOR.pointer.bindWindowMoveModifier("Super");
COMPOSITOR.pointer.bindWindowResizeModifier("Super");

COMPOSITOR.event.onWindowMove((event) => {
  HYBRID_WINDOW_MANAGER.onWindowMove(event);
  scheduleRectsBroadcast();
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

// Window corner rounding; the drag tabs clamp their travel to the flat part
// of each edge (between the corner arcs).
const WINDOW_CORNER_RADIUS = 10;

function naturalRootRect(window: WaylandWindow): ManagedWindowRect {
  const client = window.position;
  const chrome = EDGE_DRAG_HALO_PX + WINDOW_BORDER_PX;
  return {
    x: client.x - 
        chrome,
    y: client.y - 
        chrome,
    width: client.width + 
        chrome * 2,
    height: client.height + 
        chrome * 2,
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


  let innerComponents = <ClientWindow />;

  const TERMINALS = ["kitty", "ghostty"];

  if (TERMINALS.includes(window.appId() ?? "")) {
    innerComponents = (
      <ShaderEffect shader={backgroundShader} direction="column">
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

  // Maximized: no chrome at all
    // — edge to edge in the usable area
  // (maximizedRectForWindow applies no inset to match).
    // Without the halo a maximized window is not pointer-draggable; unmaximize re-centres it, so
  // it can never get stuck. Floating windows below keep the full chrome.
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

  // The transparent halo ring around the window is decoration chrome: the SSD
  // hit-test resolves clicks there to Move (outer resizeHitArea band wins for
  // resize), so the whole ring drags the window. Hovering it reveals a tab at
  // that edge as the visible affordance; the tab itself is plain chrome, so
  // grabbing it drags too. Chrome can't render above the client surface,
  // which is why the tab lives outside the window instead of overlapping it.
  const [hoveredEdge, setHoveredEdge] = useState<
    "top" | "bottom" | "left" | "right" | null
  >(null);
  const dragEdgeHover =
    (edge: "top" | "bottom" | "left" | "right") => (inside: boolean) => {
      if (inside) {
        setHoveredEdge(edge);
      } else if (read(hoveredEdge) === edge) {
        setHoveredEdge(null);
      }
    };
  // Trapezium drag tabs (SVG assets, red stipple + border) attached to the
  // window border, centred on the pointer along the hovered edge. The
  // position computeds read pointerPosition only while their edge is
  // hovered, so idle windows never re-evaluate on mouse motion.
  const DRAG_TAB_LENGTH = 72;
  const DRAG_TAB_THICKNESS = 12;
  // Travel limit: the tab slides along the flat part of the edge and pins at
  // the corner arcs. While pinned it stays visible (visibility follows the
  // hover strip, not the pointer-tab overlap) and stays draggable (the whole
  // halo is move chrome).
  const dragTabMin = EDGE_DRAG_HALO_PX + WINDOW_CORNER_RADIUS;
  const dragTabX = computed(() => {
    const edge = hoveredEdge();
    if (edge !== "top" && edge !== "bottom") {
      return 0;
    }
    const rect = managedRect();
    const max = Math.max(
      dragTabMin,
      read(rect.width) - 
        dragTabMin - 
        DRAG_TAB_LENGTH,
    );
    const centred = Math.round(
      pointerPosition.value.x - read(rect.x) - 
        DRAG_TAB_LENGTH 
        / 2,
    );
    return Math.min(max, Math.max(
        dragTabMin, 
        centred,
        ));
  });
  const dragTabY = computed(() => {
    const edge = hoveredEdge();
    if (edge !== "left" && edge !== "right") {
      return 0;
    }
    const rect = managedRect();
    const max = Math.max(
      dragTabMin,
      read(rect.height) - 
        dragTabMin -
        DRAG_TAB_LENGTH,
    );
    const centred = Math.round(
      pointerPosition.value.y - read(rect.y) - DRAG_TAB_LENGTH / 2,
    );
    return Math.min(max, Math.max(
        dragTabMin,
        centred,
        ));
  });

  // Published to the workspace IPC view (see attachDragTabs): the tab's
  // layout-space rect while an edge is hovered, null otherwise. Lazy — only
  // evaluated when a view is built.
  DRAG_TAB_RECTS.set(window.id, () => {
    const edge = read(hoveredEdge);
    if (!edge) {
      return null;
    }
    const rect = managedRect();
    switch (edge) {
      case "top":
        return {
          x: rect.x + dragTabX(),
          y: rect.y + EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS,
          width: DRAG_TAB_LENGTH,
          height: DRAG_TAB_THICKNESS,
        };
      case "bottom":
        return {
          x: rect.x + dragTabX(),
          y: rect.y + rect.height - EDGE_DRAG_HALO_PX,
          width: DRAG_TAB_LENGTH,
          height: DRAG_TAB_THICKNESS,
        };
      case "left":
        return {
          x: rect.x + EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS,
          y: rect.y + dragTabY(),
          width: DRAG_TAB_THICKNESS,
          height: DRAG_TAB_LENGTH,
        };
      case "right":
        return {
          x: rect.x + rect.width - EDGE_DRAG_HALO_PX,
          y: rect.y + dragTabY(),
          width: DRAG_TAB_THICKNESS,
          height: DRAG_TAB_LENGTH,
        };
    }
  });

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
      {/* No `position` here: the halo box must NOT establish a containing
          block, so its absolute children (strips + tabs) anchor to the
          decoration root's full rect — the halo's outer edge — instead of
          the padding-inset content box at the window border. */}
      <Box style={{ padding: EDGE_DRAG_HALO_PX }}>
        <WindowBorder
          style={{
            border: { px: WINDOW_BORDER_PX, color: borderColor },
            borderRadius: WINDOW_CORNER_RADIUS,
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
        <Box
          onHoverChange={dragEdgeHover("top")}
          style={{
            position: "absolute",
            top: 0,
            left: 0,
            right: 0,
            height: EDGE_DRAG_HALO_PX,
          }}
        />
        <Box
          onHoverChange={dragEdgeHover("bottom")}
          style={{
            position: "absolute",
            bottom: 0,
            left: 0,
            right: 0,
            height: EDGE_DRAG_HALO_PX,
          }}
        />
        <Box
          onHoverChange={dragEdgeHover("left")}
          style={{
            position: "absolute",
            left: 0,
            top: EDGE_DRAG_HALO_PX,
            bottom: EDGE_DRAG_HALO_PX,
            width: EDGE_DRAG_HALO_PX,
          }}
        />
        <Box
          onHoverChange={dragEdgeHover("right")}
          style={{
            position: "absolute",
            right: 0,
            top: EDGE_DRAG_HALO_PX,
            bottom: EDGE_DRAG_HALO_PX,
            width: EDGE_DRAG_HALO_PX,
          }}
        />
        <Image
          src="./assets/drag-tab-top.svg"
          style={{
            position: "absolute",
            top: EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS,
            left: dragTabX,
            width: DRAG_TAB_LENGTH,
            height: DRAG_TAB_THICKNESS,
            visible: hoveredEdge((edge) => edge === "top"),
          }}
        />
        <Image
          src="./assets/drag-tab-bottom.svg"
          style={{
            position: "absolute",
            bottom: EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS,
            left: dragTabX,
            width: DRAG_TAB_LENGTH,
            height: DRAG_TAB_THICKNESS,
            visible: hoveredEdge((edge) => edge === "bottom"),
          }}
        />
        <Image
          src="./assets/drag-tab-left.svg"
          style={{
            position: "absolute",
            left: EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS,
            top: dragTabY,
            width: DRAG_TAB_THICKNESS,
            height: DRAG_TAB_LENGTH,
            visible: hoveredEdge((edge) => edge === "left"),
          }}
        />
        <Image
          src="./assets/drag-tab-right.svg"
          style={{
            position: "absolute",
            right: EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS,
            top: dragTabY,
            width: DRAG_TAB_THICKNESS,
            height: DRAG_TAB_LENGTH,
            visible: hoveredEdge((edge) => edge === "right"),
          }}
        />
      </Box>
    </ManagedWindow>
  );
};

export default COMPOSITOR;
