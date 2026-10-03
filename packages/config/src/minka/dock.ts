import { COMPOSITOR, type PointerMoveEvent } from "shoji_wm";
import type { IpcServer } from "shoji_wm/ipc";

// ---------------------------------------------------------------------------
// Dock proximity: watch the pointer and broadcast enter/leave for the bottom
// strip of each monitor. The bar uses this in place of a layer-shell trigger
// surface (which would otherwise capture clicks meant for the windows below).
//   dock.proximity  { monitor: string, inside: bool }  (broadcast)
// ---------------------------------------------------------------------------
// Two thresholds with hysteresis:
//   - SHOW: pointer must be in the bottom 10px to trigger reveal
//   - HIDE: once visible, pointer must leave the bottom 120px to dismiss
// This gives a precise "reach for the dock" trigger while keeping the dock
// stable once the user is interacting with it (so brushing the cursor a few
// dozen pixels above the dock body does not flicker it away).
const DOCK_SHOW_ZONE_PX = 10;
const DOCK_HIDE_ZONE_PX = 120;

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

// Returns the pointer-move hook that drives the broadcasts.
export function createDockProximity(
  server: IpcServer,
): (event: PointerMoveEvent) => void {
  const dockProximityByMonitor = new Map<string, boolean>();

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
    server.broadcast("dock.proximity", { monitor, inside });
  }

  return (event) => {
    // Update only the monitor the pointer is currently on, and emit "leave"
    // for other monitors that were previously inside. The narrow/wide
    // threshold is hysteretic per current state.
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
  };
}
