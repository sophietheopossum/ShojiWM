import { COMPOSITOR } from "shoji_wm";
import type { InputAccelProfile, InputScrollMethod } from "shoji_wm/types";
import type { HybridWindowManager } from "../window-manager";
import { currentSettings } from "./settings";

export function configureInput(windowManager: HybridWindowManager): void {
  // Pointer/touchpad behavior comes from minka-settings.json (MinkaConf).
  // 8/7/2026 defaults per Sophie: adaptive accel at +0.4 (the old flat/0.0
  // was "acceleration too low") and natural scroll off everywhere.
  COMPOSITOR.input.configure((input, _context) => {
    const inputSettings = currentSettings().input;
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

  windowManager.configureWorkspaceGestureSpeed({
    workspaceScrollFactor: 1.5,
    workspaceScrollKineticFactor: 1,
    workspaceSwitchFactor: 1,
    workspaceSwitchVelocityFactor: 1,
    // At or below this scroll speed (logical px/s) the workspace scroll
    // catches on tile snap positions (fully-on-screen edges; center for
    // maximized tiles). 0 disables snapping.
    workspaceScrollSnapMaxVelocity: 600,
    // Finger travel (logical px) needed to break out of a caught position.
    workspaceScrollSnapBreakoutPx: 48,
  });

  COMPOSITOR.pointer.bindWindowMoveModifier("Super");
  COMPOSITOR.pointer.bindWindowResizeModifier("Super");
}
