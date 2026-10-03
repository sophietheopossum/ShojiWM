import { loadShader, shaderStage, type WaylandWindow } from "shoji_wm";
import { createFlip3D } from "../flip-3d";
import { createWindowGrid } from "../window-grid";
import type { WindowSwitcher } from "../window-switcher";
import type { HybridWindowManager } from "../window-manager";

// Window switcher (Super+Tab): "flip-3d" stacks the windows in 3D
// (../flip-3d.tsx), "grid" lays them out side by side (../window-grid.tsx).
// Created by index.tsx once the window manager exists; rendering and the
// decoration read it through `windowSwitcher()` for blur suspension, the
// backdrop fade, and the selected-window glow.
const WINDOW_SWITCHER_STYLE = "flip-3d" as "flip-3d" | "grid";

let switcher: WindowSwitcher | null = null;

export function initWindowSwitcher(
  windowManager: HybridWindowManager,
  stackOrder: (window: WaylandWindow) => number,
): WindowSwitcher {
  switcher = (
    WINDOW_SWITCHER_STYLE === "grid" ? createWindowGrid : createFlip3D
  )({ windowManager, stackOrder });
  return switcher;
}

export function windowSwitcher(): WindowSwitcher {
  if (!switcher) {
    throw new Error("window switcher used before initWindowSwitcher");
  }
  return switcher;
}

// Window backdrop effects end with this stage so they can fade back in after
// the switcher closes.
export const backdropFade = () =>
  shaderStage(loadShader("./src/effect/backdrop-fade.frag"), {
    uniforms: { strength: windowSwitcher().backdropStrength },
  });
