import { COMPOSITOR, signal } from "shoji_wm";
import type { HybridWindowManager } from "../window-manager";
import { createDockProximity } from "./dock";
import type { WorkspaceIpc } from "./workspace-ipc";

export interface PointerPosition {
  x: number;
  y: number;
}

/**
 * The global pointer position for the decoration's drag tabs. Writing a signal
 * that no composition reads makes the runtime re-evaluate every window, so
 * pointer motion is only signalled while some window has a hovered edge.
 */
export interface PointerTracking {
  /** Where the pointer is now. Inside a computed, re-runs it on pointer motion. */
  current(): PointerPosition;
  /** A window's drag edge hover started or ended. */
  setEdgeHovered(windowId: string, hovered: boolean): void;
}

// Feeds compositor events to the window manager and keeps IPC clients in
// step. Returns the pointer tracking for the decoration's drag tabs.
export function wireWindowEvents(
  windowManager: HybridWindowManager,
  ipc: WorkspaceIpc,
): PointerTracking {
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

  // Windows with a hovered drag edge: only their compositions read the pointer.
  const edgeHoveredWindows = new Set<string>();

  COMPOSITOR.event.onClose((window) => {
    windowManager.onClose(window);
    edgeHoveredWindows.delete(window.id);
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
  // along its edge. The position itself is a plain value, always current, so a
  // tab whose hover just started reads where the pointer is now; the signal
  // only tells hovered compositions that it moved. Writing it while nothing is
  // hovered would re-evaluate every window on every pointer motion.
  let latestPointer: PointerPosition = { x: 0, y: 0 };
  const [pointerMoves, setPointerMoves] = signal(0);
  const trackDockProximity = createDockProximity(ipc.server);

  COMPOSITOR.event.onPointerMoveAsync((event) => {
    latestPointer = { x: event.position.x, y: event.position.y };
    if (edgeHoveredWindows.size > 0) {
      setPointerMoves(pointerMoves.peek() + 1);
    }
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

  return {
    current() {
      // Read for the dependency alone: a computed calling this re-runs on motion.
      pointerMoves.value;
      return latestPointer;
    },
    setEdgeHovered(windowId, hovered) {
      if (hovered) {
        edgeHoveredWindows.add(windowId);
      } else {
        edgeHoveredWindows.delete(windowId);
      }
    },
  };
}
