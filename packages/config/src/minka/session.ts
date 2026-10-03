import { COMPOSITOR } from "shoji_wm";
import { currentSettings } from "./settings";

// Session environment, cursor and window-decoration policy. Runs first,
// before the window manager exists.
export function configureSession(): void {
  COMPOSITOR.env.apply({
    QT_QPA_PLATFORM: "wayland;xcb",
    QT_QPA_PLATFORMTHEME: "qt6ct",
    GLFW_IM_MODULE: "ibus",
    ELECTRON_OZONE_PLATFORM_HINT: "wayland",
    // Firefox/hellfire was running through XWayland here (8/7/2026), where
    // fractional scaling makes click hit-testing unreliable — "some clicks
    // don't work". Force native Wayland for this session only; KDE keeps
    // whatever it was doing.
    MOZ_ENABLE_WAYLAND: "1",
    MOZ_DBUS_REMOTE: "1",
  });
  COMPOSITOR.env.publish();

  // Fallback for a settings file with no cursor section; MinkaConf's choice
  // replaces it straight away.
  COMPOSITOR.cursor.configure({
    theme: "Bibata-Modern-Ice",
    size: 24,
  });
  applyCursorSettings();

  COMPOSITOR.process.once("GTK-CSD-control-buttons", {
    command: "gsettings set org.gnome.desktop.wm.preferences button-layout ':minimize,maximize,close'",
    runPolicy: "once-per-session",
  });
  // Upstream removed the Firefox special case in 08b0d50 ("fix to avoid duplicated
  // window control CSD & SSD") and honours the client's stated preference instead.
  // Reinstating the old handler on top of that — plus 55962c9's GTK button-layout —
  // put a border back on Firefox that stayed through maximise, so this defers to
  // upstream. The old version is in git if Firefox ever renegotiates in a loop again.
  COMPOSITOR.window.decoration.configure((_window, context) => {
    return { mode: context.clientPreference ?? "server" };
  });
}

// Cursor theme + size come from minka-settings.json (MinkaConf's cursor
// page). configure() applies live and also exports XCURSOR_THEME /
// XCURSOR_SIZE to the systemd and D-Bus activation environments, so apps
// launched afterwards agree with the compositor. Feature-detected so this
// config still evaluates on a ShojiWM build without the cursor API.
export function applyCursorSettings() {
  const cursorApi = (
    COMPOSITOR as {
      cursor?: { configure(config: { theme: string; size: number }): void };
    }
  ).cursor;
  const cursor = currentSettings().cursor;
  if (!cursorApi || !cursor?.theme) {
    return;
  }
  cursorApi.configure({
    theme: cursor.theme,
    size: cursor.size ?? 24,
  });
}

// The rest of the desktop: shell, Minka apps and session daemons.
export function startSessionApps(): void {
  // MinkaShell (Quickshell-based) is the session shell now; shoji-bar-2 is retired.
  // Logs go to /tmp/minkashell.log so warnings and crashes survive the session
  // for later inspection.
  // MINKA_SHELL_DIR overrides the installed location for repo-checkout sessions
  // (set in shojiwm-env.fish); tarball installs land in /usr/share/minka.
  // --log-times so window-creation lines can be matched against kernel events:
  // MinkaShell has been hanging its first GPU job on newly created contexts
  // (xe GT0 engine reset, always seqno 0xFFFFFF81), and without timestamps
  // there is nothing to correlate the reset against. MinkaMon already does this.
  COMPOSITOR.process.once("shell", {
    command: "qs --log-times -p \"${MINKA_SHELL_DIR:-/usr/share/minka/MinkaShell}\" > /tmp/minkashell.log 2>&1",
    runPolicy: "once-per-session",
  });
  // MinkaShot: freeze-frame screenshot tool. Runs as a daemon so the Print
  // keybind's ui.minkashot broadcast always has a listener; overlays are
  // pre-declared and hidden until armed, same philosophy as the shell.
  COMPOSITOR.process.once("minkashot", {
    command: "qs -p \"${MINKA_SHOT_DIR:-/usr/share/minka/MinkaShot}\" > /tmp/minkashot.log 2>&1",
    runPolicy: "once-per-session",
  });
  // MinkaMon: the system monitor. Started with the session because duo mode
  // designs it in — the ScreenPad's side panel and dock reserve their exclusive
  // zones and MinkaMon's schematic fills whatever is left, so without it that
  // zone is bare desktop rather than a missing app.
  // Launched via its wrapper rather than qs directly so autostarted and
  // start-menu launches share one rolling log (~/.local/state/minka/minkamon.log);
  // that is the wrapper's whole job, hence no redirect here. Guarded like
  // MinkaFX so an install that predates the wrapper is a no-op, not an error.
  //
  // The `sleep` is load-bearing, not politeness: it MUST bind its layer surface
  // after MinkaShell binds the ScreenPad wallpaper. ShojiWM sorts Background and
  // Bottom into one bucket ordered by bind order rather than by layer
  // (backend/window.rs, layer_surfaces_for_output — smithay's LayerMap is an
  // IndexSet and layers() never sorts by kind), so whichever binds later wins.
  // Start them together and the schematic loses the race and vanishes behind the
  // wallpaper: running, correctly placed, invisible. Remove this only once the
  // compositor orders those two layers properly.
  COMPOSITOR.process.once("minkamon", {
    command: "MINKA_MON=\"${MINKA_MON_BIN:-/usr/bin/minkamon}\"; [ -x \"$MINKA_MON\" ] || exit 0; sleep 5; exec \"$MINKA_MON\" \"${MINKA_MON_DIR:-/usr/share/minka/MinkaMon}\"",
    runPolicy: "once-per-session",
  });
  // MinkaFX: the Guido-style wgpu overlay process (snap preview, future OSDs).
  // Guarded so a missing/not-yet-built binary is a silent no-op instead of a
  // failure. MINKA_FX_BIN overrides the installed path for repo-checkout runs.
  COMPOSITOR.process.once("MinkaFX", {
    command: "MINKA_FX=\"${MINKA_FX_BIN:-/usr/bin/MinkaFX}\"; [ -x \"$MINKA_FX\" ] && exec \"$MINKA_FX\" > /tmp/minkafx.log 2>&1",
    runPolicy: "once-per-session",
  });
  // Polkit authentication agent: without one, anything that needs privilege
  // escalation (pamac, GParted, systemd prompts…) fails silently because
  // polkit has nowhere to send the password dialog. KDE sessions start their
  // own; ours must too. `once` rather than a restarting service: polkit
  // permits one agent per session, so a duplicate spawn exits immediately
  // and a restart-on-exit policy would loop on that.
  // TODO(Minka): replace with a themed MinkaConf-family agent eventually.
  COMPOSITOR.process.once("polkit-agent", {
    command: "/usr/lib/polkit-kde-authentication-agent-1",
    runPolicy: "once-per-session",
  });
  // cliphist clipboard history watchers. Text and image need separate watchers;
  // run as services so they are restarted if they ever exit.
  COMPOSITOR.process.service("cliphist-text", {
    command: ["wl-paste", "--type", "text", "--watch", "cliphist", "store"],
    restart: "on-exit",
  });
  COMPOSITOR.process.service("cliphist-image", {
    command: ["wl-paste", "--type", "image", "--watch", "cliphist", "store"],
    restart: "on-exit",
  });
}
