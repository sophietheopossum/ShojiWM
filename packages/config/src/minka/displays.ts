import { COMPOSITOR, type DisplayConfigDraft } from "shoji_wm";
import type { OutputSubpixel } from "shoji_wm/types";
import { currentSettings } from "./settings";

const OUTPUT_SUBPIXELS: ReadonlySet<string> = new Set<OutputSubpixel>([
  "unknown",
  "none",
  "horizontal-rgb",
  "horizontal-bgr",
  "vertical-rgb",
  "vertical-bgr",
]);

// The compositor parses the layout strictly, and one unrecognised value would
// fail the whole runtime response rather than just this key. MinkaConf only
// writes valid names, so this guards hand edits: drop the value and say so.
function subpixelSetting(name: string, value: unknown): OutputSubpixel | undefined {
  if (value === undefined || value === null) {
    return undefined;
  }
  if (typeof value === "string" && OUTPUT_SUBPIXELS.has(value)) {
    return value as OutputSubpixel;
  }
  console.warn(`minka-settings: ignoring unknown subpixel layout for ${name}:`, value);
  return undefined;
}

// Displays are fully user-managed through MinkaConf (minka-settings.json):
// scale, explicit position, mode, enable/disable, mirroring, and the HDR
// opt-in (HDR only engages when the sink's EDID advertises PQ support).
// Connectors with no entry get KDE-parity defaults: best mode, auto
// position, scale 1.0.
export function configureDisplays(): void {
  COMPOSITOR.output.configure((context) => {
    const display: DisplayConfigDraft = {};
    const settings = currentSettings();

    const names = new Set<string>(Object.keys(settings.displays));
    for (const output of context.connected) {
      names.add(output.name);
    }

    for (const name of names) {
      const entry = settings.displays[name];
      if (entry?.enabled === false) {
        display[name] = { mode: "disabled" };
        continue;
      }
      if (entry?.mirror) {
        display[name] = {
          mode: "mirror",
          source: entry.mirror,
          subpixel: subpixelSetting(name, entry.subpixel),
        };
        continue;
      }
      display[name] = {
        mode: "extend",
        // COPIED, never aliased. `entry` is a live reference into the session's
        // active settings, loaded once from minka-settings.json by settings.ts
        // — and the origin-anchoring block below MUTATES the position
        // objects it collects (`position.x -= minX`). Handing it the settings'
        // own object meant an unplug rewrote the saved layout in memory.
        //
        // 10/9/2026: the TV dropped out for three seconds. With only the two
        // built-in panels connected, the anchoring pass renormalised them from
        // (192,1080)/(0,1944) to (192,0)/(0,864) *inside activeSettings*. When
        // the TV came back the pass re-derived from those corrupted values, saw
        // minX/minY already 0, and had nothing left to undo — so the TV returned
        // to its own untouched (0,0) and sat on top of both panels. The file on
        // disk was correct the whole time; only the in-memory copy was wrong,
        // which is why it survived until the next Super+Shift+R.
        //
        // The same trap applies to `resolution`: nothing mutates it today, but it
        // is one edit away, so copy it too.
        resolution:
          typeof entry?.resolution === "object"
            ? { ...entry.resolution }
            : (entry?.resolution ?? "best"),
        position: entry?.position
          ? { x: entry.position.x, y: entry.position.y }
          : "auto",
        scale: entry?.scale ?? 1.0,
        hdr: entry?.hdr === true,
        // Omitted when unset: the compositor falls back to EDID, then to its
        // own constants, so an absent key must stay absent rather than become 0.
        hdrMaxLuminance: entry?.hdrMaxLuminance,
        hdrMinLuminance: entry?.hdrMinLuminance,
        // Omitted when unset, like the luminance fields: the compositor then
        // keeps the layout the kernel reported.
        subpixel: subpixelSetting(name, entry?.subpixel),
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
      // `DisplayConfigDraft` values are nullable, and only the extend variant
      // carries a position at all — narrow both before reading it.
      if (entry == null || entry.mode !== "extend" || !connectedNames.has(name)) {
        continue;
      }
      // "auto" is left to the compositor; only explicit coordinates translate.
      const position = entry.position;
      if (typeof position === "object") {
        positioned
            .push(position);
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
}
