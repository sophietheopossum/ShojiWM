//! Session environment, cursor, window-decoration policy and the autostarted
//! desktop. Runs first, before the window manager exists.

use shojiwm_rs::{
    prelude::*, runtime_process::RuntimeProcessRestartPolicy, ssd::WindowDecorationModeSnapshot,
};

use super::settings;

pub fn configure_session() {
    COMPOSITOR.env.apply([
        ("QT_QPA_PLATFORM", "wayland;xcb"),
        ("QT_QPA_PLATFORMTHEME", "qt6ct"),
        ("GLFW_IM_MODULE", "ibus"),
        ("ELECTRON_OZONE_PLATFORM_HINT", "wayland"),
        // Firefox/hellfire was running through XWayland here (8/7/2026),
        // where fractional scaling makes click hit-testing unreliable: "some
        // clicks don't work". Force native Wayland for this session only;
        // KDE keeps whatever it was doing.
        ("MOZ_ENABLE_WAYLAND", "1"),
        ("MOZ_DBUS_REMOTE", "1"),
    ]);
    COMPOSITOR.env.publish();

    // Fallback for a settings file with no cursor section; MinkaConf's
    // choice replaces it straight away.
    COMPOSITOR.cursor.configure("Bibata-Modern-Ice", 24);
    apply_cursor_settings();

    COMPOSITOR.process.once(
        "GTK-CSD-control-buttons",
        Command::shell("gsettings set org.gnome.desktop.wm.preferences button-layout ':minimize,maximize,close'"),
    );
    // Upstream removed the Firefox special case in 08b0d50 ("fix to avoid
    // duplicated window control CSD & SSD") and honours the client's stated
    // preference instead. Reinstating the old handler on top of that, plus
    // 55962c9's GTK button-layout, put a border back on Firefox that stayed
    // through maximise, so this defers to upstream.
    COMPOSITOR.window.decoration(|_window, context| {
        context
            .client_preference
            .unwrap_or(WindowDecorationModeSnapshot::Server)
    });
}

/// Cursor theme and size from minka-settings.json (MinkaConf's cursor page).
/// `configure` applies live and also exports XCURSOR_THEME/XCURSOR_SIZE to
/// the systemd and D-Bus activation environments, so apps launched
/// afterwards agree with the compositor.
pub fn apply_cursor_settings() {
    let cursor = settings::with_current(|settings| {
        let cursor = &settings["cursor"];
        let theme = cursor["theme"]
            .as_str()
            .filter(|theme| !theme.is_empty())?
            .to_owned();
        let size = cursor["size"].as_f64().map_or(24, |size| size as u32);
        Some((theme, size))
    });
    if let Some((theme, size)) = cursor {
        COMPOSITOR.cursor.configure(&theme, size);
    }
}

/// The rest of the desktop: shell, Minka apps and session daemons.
///
/// Skipped when `MINKA_NESTED=1`: a nested instance for testing must not
/// start a second MinkaShell (which would truncate `/tmp/minkashell.log`) or
/// a second set of clipboard watchers. Once-per-session is per compositor
/// process, so nothing else stops it.
pub fn start_session_apps() {
    if std::env::var_os("MINKA_NESTED").is_some_and(|value| value == "1") {
        return;
    }
    // MinkaShell (Quickshell-based) is the session shell; shoji-bar-2 is
    // retired. Logs go to /tmp/minkashell.log so warnings and crashes
    // survive the session for later inspection. MINKA_SHELL_DIR overrides
    // the installed location for repo-checkout sessions (set in
    // shojiwm-env.fish); tarball installs land in /usr/share/minka.
    // --log-times so window-creation lines can be matched against kernel
    // events: MinkaShell has been hanging its first GPU job on newly created
    // contexts (xe GT0 engine reset, always seqno 0xFFFFFF81), and without
    // timestamps there is nothing to correlate the reset against.
    COMPOSITOR.process.once(
        "shell",
        Command::shell(
            "qs --log-times -p \"${MINKA_SHELL_DIR:-/usr/share/minka/MinkaShell}\" > /tmp/minkashell.log 2>&1",
        ),
    );
    // MinkaShot: freeze-frame screenshot tool. Runs as a daemon so the
    // Print binding's ui.minkashot broadcast always has a listener.
    COMPOSITOR.process.once(
        "minkashot",
        Command::shell(
            "qs -p \"${MINKA_SHOT_DIR:-/usr/share/minka/MinkaShot}\" > /tmp/minkashot.log 2>&1",
        ),
    );
    // MinkaMon: the system monitor. Started with the session because duo
    // mode designs it in: the ScreenPad's side panel and dock reserve their
    // exclusive zones and MinkaMon's schematic fills whatever is left.
    // Launched via its wrapper so autostarted and start-menu launches share
    // one rolling log (~/.local/state/minka/minkamon.log), hence no redirect.
    //
    // The `sleep` is load-bearing: it MUST bind its layer surface after
    // MinkaShell binds the ScreenPad wallpaper. ShojiWM sorts Background and
    // Bottom into one bucket ordered by bind order rather than by layer, so
    // whichever binds later wins. Remove it only once the compositor orders
    // those two layers properly.
    COMPOSITOR.process.once(
        "minkamon",
        Command::shell(
            "MINKA_MON=\"${MINKA_MON_BIN:-/usr/bin/minkamon}\"; [ -x \"$MINKA_MON\" ] || exit 0; sleep 5; exec \"$MINKA_MON\" \"${MINKA_MON_DIR:-/usr/share/minka/MinkaMon}\"",
        ),
    );
    // MinkaFX: the Guido-style wgpu overlay process (snap preview, future
    // OSDs). A missing or not-yet-built binary is a silent no-op.
    COMPOSITOR.process.once(
        "MinkaFX",
        Command::shell(
            "MINKA_FX=\"${MINKA_FX_BIN:-/usr/bin/MinkaFX}\"; [ -x \"$MINKA_FX\" ] && exec \"$MINKA_FX\" > /tmp/minkafx.log 2>&1",
        ),
    );
    // Polkit authentication agent: without one, anything that needs
    // privilege escalation fails silently. `once` rather than a restarting
    // service: polkit permits one agent per session, so a duplicate exits
    // immediately and a restart-on-exit policy would loop on that.
    COMPOSITOR.process.once(
        "polkit-agent",
        Command::shell("/usr/lib/polkit-kde-authentication-agent-1"),
    );
    // cliphist history watchers, restarted if they ever exit.
    COMPOSITOR.process.service(
        "cliphist-text",
        Command::exec(["wl-paste", "--type", "text", "--watch", "cliphist", "store"]),
        RuntimeProcessRestartPolicy::OnExit,
    );
    COMPOSITOR.process.service(
        "cliphist-image",
        Command::exec([
            "wl-paste", "--type", "image", "--watch", "cliphist", "store",
        ]),
        RuntimeProcessRestartPolicy::OnExit,
    );
}
