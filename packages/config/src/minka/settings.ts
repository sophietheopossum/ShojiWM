import { readTextFile } from "shoji_wm";
import type { IpcServer } from "shoji_wm/ipc";
import type { OutputSubpixel } from "shoji_wm/types";

// Full per-display schema MinkaConf's visual page writes.
export interface MinkaDisplaySettings {
  scale?: number;
  position?: { x: number; y: number };
  resolution?: { width: number; height: number; refreshRate?: number } | "best";
  enabled?: boolean;
  mirror?: string | null;
  hdr?: boolean;
  /// Real peak luminance of the panel, cd/m². Only needed when the EDID
  /// advertises PQ but omits its luminance fields — common, and the reason
  /// the compositor would otherwise assume 1000. Ignored outside 50..=10000.
  hdrMaxLuminance?: number;
  /// Real black level of the panel, cd/m². Ignored outside 0..=10.
  hdrMinLuminance?: number;
  /// Physical subpixel layout of the panel. `null` (MinkaConf's "detected")
  /// and absent both keep what the kernel reported.
  subpixel?: OutputSubpixel | null;
}

export interface MinkaInputSettings {
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

export interface MinkaSettings {
  input: MinkaInputSettings;
  displays: Record<string, MinkaDisplaySettings | undefined>;
  // XCursor theme + size, owned by MinkaConf's cursor page. Optional so
  // settings files from before revision 3 still parse.
  cursor?: { theme?: string; size?: number };
  // Shell-side keys (e.g. shell.layout) ride along untouched: MinkaConf owns
  // the whole file and MinkaShell reads it directly.
  shell?: { layout?: string };
  // Virtual desktops (MinkaConf layout page). Only an explicit false turns
  // them off; MinkaShell reads the same key to hide its desktop pills.
  workspaces?: { enabled?: boolean };
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
    return JSON.parse(readTextFile(MINKA_SETTINGS_PATH)) as MinkaSettings;
  } catch (error) {
    console.warn(
      `minka-settings: using defaults (cannot read ${MINKA_SETTINGS_PATH}: ${error})`,
    );
    return MINKA_SETTINGS_DEFAULTS;
  }
}

let activeSettings: MinkaSettings = loadMinkaSettings();

// The settings in effect. Factories must call this when they run rather than
// keep the result: `settings.apply` replaces the whole object.
export function currentSettings(): MinkaSettings {
  return activeSettings;
}

export function workspacesEnabled(): boolean {
  return activeSettings.workspaces?.enabled !== false;
}

// Config-schema revision handshake. Bump whenever the settings schema or
// the factories consuming it change, so MinkaConf can tell the user the
// running session predates the edit ("reload with Super+Shift+R") instead
// of silently half-applying. History: 1 = input + display scale;
// 2 = full display schema (position/mode/enabled/mirror/hdr);
// 3 = cursor theme + size; 4 = display subpixel layout;
// 5 = workspaces.enabled (virtual desktops on/off).
const MINKA_CONFIG_REVISION = 5;

// MinkaConf's side of the IPC socket. `onApply` runs after every
// `settings.apply` swap and re-runs whatever consumes the settings;
// `workspacesToggled` says whether virtual desktops flipped on or off.
export function serveSettings(
  server: IpcServer,
  onApply: (workspacesToggled: boolean) => void,
): void {
  server.handle("minka.revision", () => ({
    revision: MINKA_CONFIG_REVISION,
  }));

  // Effective user settings, for MinkaConf to display.
  server.handle("settings.get", () => activeSettings);
  // Live-apply from MinkaConf: swap the active settings and re-run the input
  // and output factories. No config reload; takes effect immediately.
  server.handle("settings.apply", (params) => {
    if (!params || typeof params !== "object") {
      return { ok: false, error: "expected a settings object" };
    }
    const desktopsWereEnabled = workspacesEnabled();
    activeSettings = params as MinkaSettings;
    // MinkaConf sends the whole file on every edit: act only on a change.
    onApply(workspacesEnabled() !== desktopsWereEnabled);
    return { ok: true };
  });
}
