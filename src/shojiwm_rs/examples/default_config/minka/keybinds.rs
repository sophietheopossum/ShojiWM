//! Key bindings, including the virtual-desktop keys.

use std::{cell::Cell, rc::Rc};

use serde_json::json;
use shojiwm_rs::prelude::*;

use super::{settings, workspace_ipc::WorkspaceIpc};
use crate::window_manager::{HybridWindowManager, WindowManager};

/// Backlight device for Fn+F4 / Fn+F5 (the XF86MonBrightness* keysyms).
///
/// The Duo has two panels with independent backlights, so the keys act on
/// whichever one currently holds focus: eDP-1 is the 1920x1080 main panel,
/// while the ScreenPad Plus enumerates as an internal DisplayPort output
/// (DP-1, 1920x515) rather than a second eDP. Anything else (an external
/// monitor, which has no sysfs backlight anyway) falls back to the main
/// panel so the keys always do something predictable.
fn backlight_for(connector: &str) -> &'static str {
    match connector {
        "DP-1" => "asus_screenpad",
        _ => "intel_backlight",
    }
}

fn spawn(command: &str) {
    COMPOSITOR.process.spawn(Command::shell(command));
}

type WmAction = fn(&mut HybridWindowManager);

/// Virtual-desktop keys, bound only while desktops are on: an unbound
/// shortcut is not intercepted, so with desktops off these reach the
/// focused app.
const DESKTOP_KEY_BINDINGS: [(&str, &str, WmAction); 4] = [
    ("window-move-workspace-prev", "Super+Shift+Up", |wm| {
        wm.move_focused_window_to_workspace(-1)
    }),
    ("window-move-workspace-next", "Super+Shift+Down", |wm| {
        wm.move_focused_window_to_workspace(1)
    }),
    ("workspace-prev", "Super+Ctrl+Up", |wm| {
        wm.switch_workspace(-1)
    }),
    ("workspace-next", "Super+Ctrl+Down", |wm| {
        wm.switch_workspace(1)
    }),
];

/// Binds or releases the virtual-desktop keys when desktops flip.
#[derive(Clone)]
pub struct DesktopKeyBindings {
    ipc: WorkspaceIpc,
    wm: WindowManager,
}

impl DesktopKeyBindings {
    pub fn sync(&self, enabled: bool) {
        for (id, shortcut, run) in DESKTOP_KEY_BINDINGS {
            if enabled {
                let (wm, ipc) = (self.wm.clone(), self.ipc.clone());
                COMPOSITOR.key.bind(id, shortcut, move || {
                    if !settings::workspaces_enabled() {
                        return;
                    }
                    wm.with(run);
                    ipc.schedule_workspace_broadcast();
                });
            } else {
                COMPOSITOR.key.unbind(id);
            }
        }
    }
}

pub fn bind_keys(wm: &WindowManager, ipc: &WorkspaceIpc) -> DesktopKeyBindings {
    COMPOSITOR.key.bind("terminal", "Super+T", || {
        COMPOSITOR.process.spawn(Command::exec(["kitty"]));
    });
    // With kwallet6 as the password store, add --password-store=kwallet6.
    COMPOSITOR.key.bind("chrome", "Super+B", || {
        spawn("google-chrome-stable --enable-features=OzonePlatform --ozone-platform=wayland");
    });
    COMPOSITOR.key.bind("discord", "Super+D", || {
        spawn(
            "discord --enable-features=UseOzonePlatform --ozone-platform=wayland --enable-wayland-ime --disable-gpu",
        );
    });
    COMPOSITOR
        .key
        .bind("dolphin", "Super+E", || spawn("dolphin"));
    COMPOSITOR
        .key
        .bind("play", "XF86AudioPlay", || spawn("playerctl play-pause"));
    COMPOSITOR
        .key
        .bind("pause", "XF86AudioPause", || spawn("playerctl play-pause"));
    COMPOSITOR
        .key
        .bind("next", "XF86AudioNext", || spawn("playerctl next"));
    COMPOSITOR
        .key
        .bind("prev", "XF86AudioPrev", || spawn("playerctl previous"));

    // brightnessctl writes through logind's D-Bus session interface, so no
    // setuid binary or udev rule is needed. `-n` keeps a floor of one step:
    // with no OSD yet, a fully black panel is indistinguishable from a crash.
    // It must stay without a value: the flag takes an *optional* argument,
    // so a spaced `-n 1` leaves the 1 as a positional, which brightnessctl
    // reads as an unknown operation and quietly downgrades the whole run to
    // `info` (exit 0, no write).
    for (id, shortcut, delta) in [
        ("brightness-up", "XF86MonBrightnessUp", "5%+"),
        ("brightness-down", "XF86MonBrightnessDown", "5%-"),
    ] {
        let wm = wm.clone();
        COMPOSITOR.key.bind(id, shortcut, move || {
            let monitor = wm.with(|wm| wm.current_monitor_name());
            spawn(&format!(
                "brightnessctl -d {} -n set {delta}",
                backlight_for(&monitor)
            ));
        });
    }

    // The start menu on the monitor under the cursor: MinkaShell listens for
    // the ui.startMenu broadcast on the IPC socket.
    let toggle_start_menu = {
        let (wm, ipc) = (wm.clone(), ipc.clone());
        move || {
            let monitor = wm.with(|wm| wm.current_monitor_name());
            ipc.broadcast(
                "ui.startMenu",
                json!({ "connector": monitor, "action": "toggle" }),
            );
        }
    };
    COMPOSITOR
        .key
        .bind("start-menu", "Super+A", toggle_start_menu.clone());
    // A Super tap: fires on release when nothing else was pressed meanwhile.
    COMPOSITOR
        .key
        .bind_release("start-menu-tap", "Super", toggle_start_menu);
    // The clipboard UI went with shoji-bar-2 (Sophie's call); the cliphist
    // watchers keep collecting history for a future picker, so Super+V is
    // intentionally unbound for now.
    COMPOSITOR.key.bind("screenshot", "Super+P", || {
        spawn("hyprshot -m region --raw | swappy -f -")
    });
    COMPOSITOR
        .key
        .bind("screenshot-freeze", "Super+Ctrl+P", || {
            spawn("hyprshot -m region --freeze --raw | swappy -f -");
        });
    // MinkaShot: the running app listens for this broadcast on the IPC
    // socket, the same pattern as the start menu.
    {
        let ipc = ipc.clone();
        COMPOSITOR.key.bind("minkashot", "Print", move || {
            ipc.broadcast("ui.minkashot", json!({ "action": "interactive" }));
        });
    }

    let bind = |id: &str, shortcut: &str, broadcast: bool, f: WmAction| {
        let (wm, ipc) = (wm.clone(), ipc.clone());
        COMPOSITOR.key.bind(id, shortcut, move || {
            wm.with(f);
            if broadcast {
                ipc.schedule_workspace_broadcast();
            }
        });
    };
    bind("cycle-windows", "Alt+Tab", true, |wm| {
        wm.cycle_workspace_focus(1)
    });
    bind("cycle-windows-back", "Alt+Shift+Tab", true, |wm| {
        wm.cycle_workspace_focus(-1)
    });
    bind("toggle-tiling-mode", "Super+S", true, |wm| {
        wm.toggle_current_workspace_tiling()
    });
    bind("close-focused-window", "Super+Q", false, |wm| {
        wm.close_focused_window()
    });
    bind("close-focused-window-alt-f4", "Alt+F4", false, |wm| {
        wm.close_focused_window()
    });
    bind("toggle-focused-window-maximize", "Super+M", false, |wm| {
        wm.toggle_focused_window_maximize()
    });
    bind("toggle-focused-window-fullscreen", "Super+F", false, |wm| {
        wm.toggle_focused_window_fullscreen()
    });
    bind("tile-focus-left-quick", "Super+Left", false, |wm| {
        wm.focus_tile(-1)
    });
    bind("tile-focus-right-quick", "Super+Right", false, |wm| {
        wm.focus_tile(1)
    });
    bind("tile-focus-left", "Super+Ctrl+Left", false, |wm| {
        wm.focus_tile(-1)
    });
    bind("tile-focus-right", "Super+Ctrl+Right", false, |wm| {
        wm.focus_tile(1)
    });
    bind("tile-move-left", "Super+Shift+Left", true, |wm| {
        wm.move_focused_tile(-1)
    });
    bind("tile-move-right", "Super+Shift+Right", true, |wm| {
        wm.move_focused_tile(1)
    });

    let desktop_keys = DesktopKeyBindings {
        ipc: ipc.clone(),
        wm: wm.clone(),
    };
    desktop_keys.sync(settings::workspaces_enabled());

    let fps_counter = Rc::new(Cell::new(false));
    COMPOSITOR.key.bind("fps", "Super+Shift+F", move || {
        fps_counter.set(!fps_counter.get());
        COMPOSITOR.debug.set_fps_counter(fps_counter.get());
    });
    let profile = Rc::new(Cell::new(false));
    COMPOSITOR.key.bind("profile", "Super+Shift+T", move || {
        profile.set(!profile.get());
        COMPOSITOR.debug.enable_profile(profile.get());
    });

    desktop_keys
}
