import { COMPOSITOR, signal, type ReadonlySignal } from "shoji_wm";
import type { HybridWindowManager } from "../window-manager";
import { createDockProximity } from "./dock";
import type { WorkspaceIpc } from "./workspace-ipc";

export interface PointerPosition {
  x: number;
  y: number;
}

// Feeds compositor events to the window manager and keeps IPC clients in
// step. Returns the global pointer position for the decoration's drag tabs.
export function wireWindowEvents(
  windowManager: HybridWindowManager,
  ipc: WorkspaceIpc,
): ReadonlySignal<PointerPosition> {
  const { scheduleWorkspaceBroadcast, scheduleRectsBroadcast } = ipc;

  // The dock displays live window titles, so a title change must refresh the
  // IPC view. The broadcast is JSON-diffed and coalesced per tick, so noisy
  // title churn (terminals) only goes out when the string actually changed.
  const titleSubscriptions = new Map<string, () => void>();

  COMPOSITOR.event.onOpen((window) => {
    windowManager.onOpen(window);
    titleSubscriptions.set(
      window.id,
      window.title.subscribe(() => scheduleWorkspaceBroadcast()),
    );
  });

  COMPOSITOR.event.onInitialConfigure((window) => {
    windowManager.onInitialConfigure(window);
  });

  COMPOSITOR.event.onFirstCommit((window) => {
    windowManager.onFirstCommit(window);
    scheduleWorkspaceBroadcast();
  });

  COMPOSITOR.event.onStartClose((window) => {
    windowManager.onStartClose(window);
    scheduleWorkspaceBroadcast();
  });

  COMPOSITOR.event.onClose((window) => {
    windowManager.onClose(window);
    titleSubscriptions.get(window.id)?.();
    titleSubscriptions.delete(window.id);
    scheduleWorkspaceBroadcast();
  });

  COMPOSITOR.event.onFocus((window, focused) => {
    windowManager.onFocus(window, focused);
    if (focused) {
      windowManager.recordFocus(window.id);
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
  const [pointerPosition, setPointerPosition] = signal<PointerPosition>({ x: 0, y: 0 });
  const trackDockProximity = createDockProximity(ipc.server);

  COMPOSITOR.event.onPointerMoveAsync((event) => {
    setPointerPosition({ x: event.position.x, y: event.position.y });
    windowManager.onPointerMove(event);
    trackDockProximity(event);
  });

  COMPOSITOR.event.onGestureSwipe((event) => {
    windowManager.onGestureSwipe(event);
    scheduleWorkspaceBroadcast();
  });

  COMPOSITOR.event.onOutputChange((event) => {
    windowManager.onOutputChange(event);
    scheduleWorkspaceBroadcast();
  });

  COMPOSITOR.event.onCreateLayer(() => {
    windowManager.refreshUsableAreaLayouts();
  });

  COMPOSITOR.event.onUpdateLayer(() => {
    windowManager.refreshUsableAreaLayouts();
  });

  COMPOSITOR.event.onDestroyLayer(() => {
    windowManager.refreshUsableAreaLayouts();
  });

  COMPOSITOR.event.onWindowResize((event) => {
    windowManager.onWindowResize(event);
    scheduleRectsBroadcast();
  });

  COMPOSITOR.event.onWindowMove((event) => {
    windowManager.onWindowMove(event);
    scheduleRectsBroadcast();
    // A drag can hand the window to another monitor's workspace (adoption in
    // onWindowMove); without a broadcast the dock keeps listing it on the old
    // output until some unrelated event refreshes the view.
    if (event.phase === "end" || event.phase === "cancel") {
      scheduleWorkspaceBroadcast();
    }
  });

  COMPOSITOR.event.onWindowMaximizeRequest((event) => {
    windowManager.onWindowMaximizeRequest(event);
    // The workspaces view carries maximized/minimized per window (the bar's
    // window controls render from it), so state changes must broadcast.
    scheduleWorkspaceBroadcast();
  });

  COMPOSITOR.event.onWindowMinimizeRequest((event) => {
    windowManager.onWindowMinimizeRequest(event);
    scheduleWorkspaceBroadcast();
  });

  COMPOSITOR.event.onWindowFullscreenRequest((event) => {
    windowManager.onWindowFullscreenRequest(event);
  });

  COMPOSITOR.event.onWindowActivateRequest((event) => {
    windowManager.onWindowActivateRequest(event);
    scheduleWorkspaceBroadcast();
  });

  return pointerPosition;
}
