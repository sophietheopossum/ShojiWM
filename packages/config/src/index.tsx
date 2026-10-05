import { COMPOSITOR } from "shoji_wm";
import { HybridWindowManager } from "./window-manager";
import { createWindowComposition, naturalRootRect } from "./minka/decoration";
import { configureDisplays } from "./minka/displays";
import { wireWindowEvents } from "./minka/events";
import { configureInput } from "./minka/input";
import { bindKeys } from "./minka/keybinds";
import { configureRendering } from "./minka/rendering";
import {
  applyCursorSettings,
  configureSession,
  startSessionApps,
} from "./minka/session";
import { serveSettings, workspacesEnabled } from "./minka/settings";
import { createWorkspaceIpc } from "./minka/workspace-ipc";

// Minka's ShojiWM config. The pieces live in ./minka/, and importing them
// does nothing: this file decides the order they run in. Keep it that way,
// because the order matters. Two examples: the settings cursor must replace
// the default one, and the window manager must exist before anything that
// drives it.
//   settings.ts      minka-settings.json (MinkaConf), settings.* IPC
//   session.ts       environment, cursor, decoration mode, autostarted apps
//   workspace-ipc.ts the IPC socket: workspaces.*, windows.*, broadcasts
//   keybinds.ts      key bindings, including the virtual-desktop keys
//   displays.ts      output layout, input.ts pointer/keyboard/gestures
//   rendering.ts     blur effects and surface policy
//   events.ts        compositor events -> window manager, dock.ts proximity
//   decoration.tsx   window chrome and drag tabs

configureSession();

const HYBRID_WINDOW_MANAGER = new HybridWindowManager(naturalRootRect);
// Before onEnable registers the restore below, so a reload with desktops off
// folds the saved desktops instead of recreating them.
HYBRID_WINDOW_MANAGER.setWorkspacesEnabled(workspacesEnabled());
const HOT_RELOAD_WINDOW_MANAGER_STATE = "config.hybrid-window-manager";

COMPOSITOR.onDisable((event) => {
  if (event.isReloading) {
    const snapshot = HYBRID_WINDOW_MANAGER.snapshot();
    event.persist(HOT_RELOAD_WINDOW_MANAGER_STATE, snapshot);
  }
  HYBRID_WINDOW_MANAGER.dispose();
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

const WORKSPACE_IPC = createWorkspaceIpc(HYBRID_WINDOW_MANAGER);

startSessionApps();

const { syncDesktopKeyBindings } = bindKeys(HYBRID_WINDOW_MANAGER, WORKSPACE_IPC);

// settings.apply from MinkaConf: re-run everything the settings feed. No
// config reload; takes effect immediately.
serveSettings(WORKSPACE_IPC.server, (workspacesToggled) => {
  if (workspacesToggled) {
    HYBRID_WINDOW_MANAGER.setWorkspacesEnabled(workspacesEnabled());
    syncDesktopKeyBindings(workspacesEnabled());
    WORKSPACE_IPC.scheduleWorkspaceBroadcast();
  }
  COMPOSITOR.input.reconfigure();
  COMPOSITOR.output.reconfigure();
  applyCursorSettings();
});

configureDisplays();
configureInput(HYBRID_WINDOW_MANAGER);
configureRendering();

const pointer = wireWindowEvents(HYBRID_WINDOW_MANAGER, WORKSPACE_IPC);

COMPOSITOR.window.composition = createWindowComposition(
  HYBRID_WINDOW_MANAGER,
  WORKSPACE_IPC,
  pointer,
);

export default COMPOSITOR;
