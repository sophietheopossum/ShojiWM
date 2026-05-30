import type { DebugController } from "./types";

interface DebugConfigSnapshot {
  fpsCounter: boolean;
}

let fpsCounterEnabled = false;
let pendingDebugConfig = false;

export const DEBUG_CONTROLLER: DebugController = {
  get fpsCounter(): boolean {
    return fpsCounterEnabled;
  },
  set fpsCounter(enabled: boolean) {
    const next = enabled === true;
    if (next === fpsCounterEnabled) {
      return;
    }
    fpsCounterEnabled = next;
    pendingDebugConfig = true;
  },
};

/**
 * Snapshot the current debug config so the decoration runtime can attach it
 * to the next scheduler tick response. Returns `undefined` when nothing has
 * changed since the last call — callers should omit the field rather than
 * sending a default-valued object every tick (the Rust side only re-applies
 * when the field is present).
 */
export function takePendingDebugConfig(): DebugConfigSnapshot | undefined {
  if (!pendingDebugConfig) {
    return undefined;
  }
  pendingDebugConfig = false;
  return { fpsCounter: fpsCounterEnabled };
}
