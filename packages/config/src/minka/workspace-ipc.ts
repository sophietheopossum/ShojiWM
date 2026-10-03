import { COMPOSITOR, read } from "shoji_wm";
import {
  createIpcServer,
  wakeRust,
  type IpcClient,
  type IpcServer,
} from "shoji_wm/ipc";
import {
  WINDOW_STATE_RECT,
  type HybridWindowManager,
  type WorkspacesView,
} from "../window-manager";

// ---------------------------------------------------------------------------
// External IPC: expose the workspace layout to clients such as the bar.
//   workspaces.get           { rectsLease?: string } -> WorkspacesView (request/response;
//                            the token renews a 2 s windows.rects lease for this client)
//   workspaces.switch        { direction: -1 | 1 }                 (command)
//   workspaces.activate      { monitor: string, index: number }    (command)
//   workspaces.toggleTiling  { monitor?: string }                  (command)
//   workspaces.changed       -> WorkspacesView                     (broadcast)
//   windows.activate         { windowId: string }                  (command)
//   windows.close            { windowId: string }                  (command)
//   windows.reorder          { windowId, beforeId: string|null }   -> {ok, changed} (request/response)
//   windows.identify         { windowId, role: string|null }       (command)
//   windows.maximize         { windowId, maximized?: bool }        (command)
//   windows.minimize         { windowId: string }                  (command)
//   windows.setRect          { windowId, x, y, width, height }     (request/response)
//   windows.rects            -> { windows: [...] }                 (event, lease holders only)
//   snap.preview             -> { monitor, rect|null, kind }       (broadcast)
//   debug.geometry           -> { outputs, usable, insets, layers } (request/response)
// Served elsewhere on the same socket: dock.proximity (dock.ts),
// ui.startMenu + ui.minkashot (keybinds.ts), minka.revision + settings.*
// (settings.ts).
// ---------------------------------------------------------------------------

export interface DragTabRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface WorkspaceIpc {
  readonly server: IpcServer;
  // The workspaces view as clients see it: the window manager's, plus the
  // live drag tabs.
  view(): WorkspacesView;
  // Coalesce many state mutations within one tick into a single diffed broadcast.
  scheduleWorkspaceBroadcast(): void;
  scheduleRectsBroadcast(): void;
  // Registered by the decoration composition, one entry per window.
  publishDragTab(windowId: string, rect: () => DragTabRect | null): void;
}

// Live rect stream for MinkaMon's leader lines: pushed on every window
// move/resize event batch so the lines track drags at event rate instead
// of the client's fallback poll. Minimal payload (id + rect + drag tab),
// coalesced per tick.
//
// Sent only to clients holding a lease, not broadcast: MinkaShell, MinkaShot
// and MinkaFX stay connected all session and have no use for it. The SDK
// hands handlers a fresh IpcClient per request and has no disconnect hook,
// so a client leases the stream by passing `rectsLease: <token>` on its
// workspaces.get poll, and the lease lapses RECTS_LEASE_MS after the last one.
const RECTS_LEASE_MS = 2000;
const RECTS_MAX_SUBSCRIBERS = 8;

function rectsLeaseLive(renewedAt: number, now: number): boolean {
  const age = now - renewedAt;
  // A backwards clock step expires a lease rather than extending it.
  return age >= 0 && age <= RECTS_LEASE_MS;
}

// What the method handlers need beyond the public surface.
interface WorkspaceIpcInternals extends WorkspaceIpc {
  renewRectsLease(params: unknown, client: IpcClient): void;
}

// Opens the socket, then registers every workspace/window method on it.
export function createWorkspaceIpc(
  windowManager: HybridWindowManager,
): WorkspaceIpc {
  const ipc = createBroadcaster(windowManager);
  serveWorkspaces(ipc, windowManager);
  return ipc;
}

function createBroadcaster(
  windowManager: HybridWindowManager,
): WorkspaceIpcInternals {
  const server = createIpcServer();
  let lastWorkspacesJson = "";
  let workspaceBroadcastQueued = false;

  // Live drag-tab geometry per window id, registered by the decoration
  // composition. Entries are lazy computeds, so hover/pointer state is
  // only sampled when a view is actually built (MinkaMon's poll) — idle
  // windows still never re-evaluate on mouse motion.
  const dragTabRects = new Map<string, () => DragTabRect | null>();

  function attachDragTabs(view: WorkspacesView): WorkspacesView {
    const live = new Set<string>();
    for (const monitor of view.monitors) {
      for (const workspace of monitor.workspaces) {
        for (const win of workspace.windows) {
          live.add(win.id);
          win.dragTab = dragTabRects.get(win.id)?.() ?? null;
        }
      }
    }
    for (const id of dragTabRects.keys()) {
      if (!live.has(id)) {
        dragTabRects.delete(id);
      }
    }
    return view;
  }

  function view(): WorkspacesView {
    return attachDragTabs(
        windowManager.viewForIpc(),
    );
  }

  const rectsSubscribers = new Map<
    string,
    { client: IpcClient; renewedAt: number }
  >();

  function pruneRectsSubscribers(now: number) {
    for (const [token, subscriber] of rectsSubscribers) {
      if (!rectsLeaseLive(subscriber.renewedAt, now)) {
        rectsSubscribers.delete(token);
      }
    }
  }

  function renewRectsLease(params: unknown, client: IpcClient) {
    const token = (params as { rectsLease?: unknown } | null | undefined)
      ?.rectsLease;
    if (typeof token !== "string" || token.length === 0 || token.length > 64) {
      return;
    }
    const now = Date.now();
    // Re-insert so Map order is recency order for the eviction below.
    rectsSubscribers.delete(token);
    rectsSubscribers.set(token, { client, renewedAt: now });
    pruneRectsSubscribers(now);
    while (rectsSubscribers.size > RECTS_MAX_SUBSCRIBERS) {
      const oldest = rectsSubscribers.keys().next().value;
      if (oldest === undefined) {
        break;
      }
      rectsSubscribers.delete(oldest);
    }
  }

  let rectsBroadcastQueued = false;
  function scheduleRectsBroadcast() {
    // Nobody holds a lease: no microtask, no payload, no wake.
    if (rectsBroadcastQueued || rectsSubscribers.size === 0) {
      return;
    }
    rectsBroadcastQueued = true;
    void Promise.resolve().then(() => {
      rectsBroadcastQueued = false;
      pruneRectsSubscribers(Date.now());
      if (rectsSubscribers.size === 0) {
        return;
      }
      const windows = [];
      for (const window of windowManager.listWindows()) {
        const rect = window.state[WINDOW_STATE_RECT]();
        windows.push({
          id: window.id,
          x: read(rect.x),
          y: read(rect.y),
          width: read(rect.width),
          height: read(rect.height),
          dragTab: dragTabRects.get(window.id)?.() ?? null,
        });
      }
      const payload = { windows };
      for (const subscriber of rectsSubscribers.values()) {
        subscriber.client.send("windows.rects", payload);
      }
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
    const current = view();
    const json = JSON.stringify(current);
    if (json === lastWorkspacesJson) {
      return;
    }
    lastWorkspacesJson = json;
    server.broadcast("workspaces.changed", current);
    // Same contract as the windows.rects tap above, and for the same reason:
    // this runs in a microtask after the triggering event's response has been
    // drained, so without an explicit wake the broadcast sits unflushed until
    // some unrelated input wakes the compositor. That is why dock titles only
    // caught up when the mouse moved, and why a title that animates on its own
    // (a terminal spinner) looked frozen. Guarded by the JSON diff above, so a
    // title that has not actually changed still costs nothing.
    wakeRust();
  }

  function reconfigureProtocolWorkspaces() {
    COMPOSITOR.workspace.reconfigure();
  }

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

  return {
    server,
    view,
    scheduleWorkspaceBroadcast,
    scheduleRectsBroadcast,
    publishDragTab(windowId, rect) {
      dragTabRects.set(windowId, rect);
    },
    renewRectsLease,
  };
}

function serveWorkspaces(
  ipc: WorkspaceIpcInternals,
  windowManager: HybridWindowManager,
): void {
  const { server, scheduleWorkspaceBroadcast } = ipc;

  COMPOSITOR.workspace.configure(() => {
    const view = windowManager.viewForIpc();
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
    windowManager.activate(monitor, index);
    scheduleWorkspaceBroadcast();
  });

  server.handle("workspaces.get", (params, client) => {
    ipc.renewRectsLease(params, client);
    return ipc.view();
  });
  server.handle("workspaces.switch", (params) => {
    const direction = (params as { direction?: number } | undefined)?.direction;
    windowManager.switchWorkspace(direction === -1 ? -1 : 1);
    scheduleWorkspaceBroadcast();
  });
  server.handle("workspaces.activate", (params) => {
    const request = params as { monitor?: string; index?: number } | undefined;
    if (request?.monitor && typeof request.index === "number") {
      windowManager.activate(request.monitor, request.index);
      scheduleWorkspaceBroadcast();
    }
  });
  server.handle("workspaces.toggleTiling", (params) => {
    const monitor = (params as { monitor?: string } | undefined)?.monitor;
    if (monitor) {
      windowManager.toggleWorkspaceTilingForMonitor(monitor);
    } else {
      windowManager.toggleCurrentWorkspaceTiling();
    }
    scheduleWorkspaceBroadcast();
  });
  server.handle("windows.activate", (params) => {
    const windowId = (params as { windowId?: string } | undefined)?.windowId;
    if (typeof windowId === "string") {
      windowManager.activateWindowById(windowId);
      scheduleWorkspaceBroadcast();
    }
  });
  server.handle("windows.close", (params) => {
    const windowId = (params as { windowId?: string } | undefined)?.windowId;
    if (typeof windowId === "string") {
      windowManager.closeWindowById(windowId);
      scheduleWorkspaceBroadcast();
    }
  });
  // Dock drag-to-reorder (MinkaShell, 15/9/2026): put `windowId` directly
  // before `beforeId` in its own workspace's window order, or last when
  // `beforeId` is null. That order is the tile sequence on a tiled workspace
  // and the Alt+Tab ring everywhere. Never crosses workspaces, never focuses;
  // refused during a pointer tile drag. ok with changed:false is a no-op.
  server.handle("windows.reorder", (params) => {
    const request = params as
      | { windowId?: string; beforeId?: string | null }
      | undefined;
    if (
      typeof request?.windowId !== "string" ||
      (request.beforeId !== null && typeof request.beforeId !== "string")
    ) {
      return { ok: false, changed: false };
    }
    const outcome = windowManager.reorderWindowById(
      request.windowId,
      request.beforeId,
    );
    if (outcome === "moved") {
      scheduleWorkspaceBroadcast();
    }
    return { ok: outcome !== "refused", changed: outcome === "moved" };
  });
  // Client-declared semantic window roles ("typed segments", ported from
  // Arcan's SHMIF idea), for example: Minka apps claim what a window *is* — e.g.
  // "minkamon.disk" — so consumers (leader lines, overview arrangement,
  // MinkaShot's window capture) stop matching on mutable title strings.
  server.handle("windows.identify", (params) => {
    const request = params as
      | { windowId?: string; role?: string | null }
      | undefined;
    if (typeof request?.windowId === "string") {
      windowManager.setWindowRole(
        request.windowId,
        typeof request.role === "string" ? request.role : null,
      );
      scheduleWorkspaceBroadcast();
    }
  });
  // Debug helper (Rio maximize investigation): drive the same maximize path a
  // client CSD button takes, addressable by window id from outside the session.
  server.handle("windows.maximize", (params) => {
    const request = params as
      | { windowId?: string; maximized?: boolean }
      | undefined;
    if (!request?.windowId) {
      return;
    }
    const window = windowManager.findWindowById(request.windowId);
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
  server.handle("windows.setRect", (params) => {
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
    const ok = windowManager.setWindowRectById(request.windowId, {
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
  server.handle("windows.minimize", (params) => {
    const request = params as { windowId?: string } | undefined;
    if (!request?.windowId) {
      return;
    }
    const window = windowManager.findWindowById(request.windowId);
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
  server.handle("debug.geometry", () => {
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

  // Snap-zone preview: broadcast the active snap rect (floating edge zones, or the
  // opened tiling slot) to the bar, which renders the rounded preview overlay.
  //   snap.preview  { monitor, rect: {x,y,width,height} | null, kind: "floating"|"tiling" }
  let lastSnapJson = "";
  windowManager.setSnapPreviewBroadcaster((preview) => {
    const json = JSON.stringify(preview);
    if (json === lastSnapJson) {
      return;
    }
    lastSnapJson = json;
    server.broadcast("snap.preview", preview);
  });

  windowManager.setWorkspaceChangeBroadcaster(() => {
    scheduleWorkspaceBroadcast();
  });

  COMPOSITOR.onDisable(() => {
    server.close();
  });
}
