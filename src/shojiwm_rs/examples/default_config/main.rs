//! Minka's ShojiWM config (`packages/config/src/index.tsx` and its `minka/`
//! modules on Sophie's branches), ported to Rust on top of `shojiwm_rs`'s
//! reactive API. The window manager is upstream's port of
//! `window-manager.ts` with Minka's changes; the rest lives in [`minka`].
//!
//! ```sh
//! cargo run -p shojiwm_rs --example default_config -- --dev          # nested
//! cargo run -p shojiwm_rs --example default_config --release -- --tty
//! ```
//!
//! Shaders and icons are shared with the TypeScript config: relative asset
//! paths resolve against `packages/config`.

mod minka;
mod window_animation;
mod window_manager;
mod workspace;

use shojiwm_rs::prelude::*;

use crate::{
    minka::{decoration, displays, events, input, keybinds, rendering, session, settings, workspace_ipc},
    window_manager::WindowManager,
};

fn main() -> std::process::ExitCode {
    ConfigBuilder::new(setup)
        .name("minka")
        // Ends a frozen session; the SDK's default only logs.
        .hang_watchdog(HangWatchdog::default())
        .asset_root(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/config"))
        .run()
}

/// The order matters: the settings cursor must replace the default one, and
/// the window manager must exist before anything that drives it.
fn setup() {
    session::configure_session();

    let wm = WindowManager::new(decoration::natural_root_rect);
    wm.with(|wm| wm.set_workspaces_enabled(settings::workspaces_enabled()));

    let ipc = workspace_ipc::create_workspace_ipc(&wm);
    session::start_session_apps();
    session::report_previous_hangs();
    let desktop_keys = keybinds::bind_keys(&wm, &ipc);

    // settings.apply from MinkaConf: re-run everything the settings feed.
    // Takes effect immediately.
    if let Some(server) = &ipc.server {
        let (wm, ipc) = (wm.clone(), ipc.clone());
        settings::serve_settings(server, move |workspaces_toggled| {
            if workspaces_toggled {
                let enabled = settings::workspaces_enabled();
                wm.with(|wm| wm.set_workspaces_enabled(enabled));
                desktop_keys.sync(enabled);
                ipc.schedule_workspace_broadcast();
            }
            COMPOSITOR.input.reconfigure();
            COMPOSITOR.output.reconfigure();
            session::apply_cursor_settings();
        });
    }

    displays::configure_displays();
    input::configure_input(&wm);
    rendering::configure_rendering();

    let pointer = events::wire_window_events(&wm, &ipc);
    COMPOSITOR
        .window
        .composition(decoration::create_window_composition(wm, ipc, pointer));
}

#[cfg(test)]
mod tests {
    //! Drives the ported config through the compositor's `RuntimeHandle`,
    //! the same calls `shojiwm_lib` makes, to exercise the window manager.

    use std::collections::BTreeMap;

    use shojiwm_rs::{
        RuntimeBoot, RuntimeHandle, RuntimeHost,
        cli::CommonArgs,
        ssd::{
            OutputModeSnapshot, OutputPositionSnapshot, WaylandOutputSnapshot, WaylandWindowSnapshot,
            WindowPositionSnapshot,
        },
    };

    use super::*;

    /// The TypeScript config's scenario tests, ported.
    mod parity;

    pub(crate) fn output(name: &str, x: i32) -> WaylandOutputSnapshot {
        WaylandOutputSnapshot {
            name: name.into(),
            description: None,
            make: None,
            model: None,
            serial: None,
            connector: Some(name.into()),
            enabled: true,
            resolution: Some(OutputModeSnapshot {
                width: 1920,
                height: 1080,
                refresh_rate: 60.0,
                clock_khz: None,
            }),
            position: OutputPositionSnapshot { x, y: 0 },
            scale: 1.0,
            transform: Default::default(),
            available_modes: Vec::new(),
            subpixel: Default::default(),
            detected_subpixel: Default::default(),
            hdr_supported: false,
            hdmi: None,
        }
    }

    fn window(id: &str, focused: bool) -> WaylandWindowSnapshot {
        WaylandWindowSnapshot {
            id: id.into(),
            title: format!("window {id}"),
            app_id: Some("org.example.App".into()),
            position: WindowPositionSnapshot {
                x: 100.0,
                y: 100.0,
                width: 800.0,
                height: 600.0,
            },
            rect: Default::default(),
            is_focused: focused,
            is_floating: true,
            is_maximized: false,
            is_fullscreen: false,
            is_xwayland: false,
            decoration: Default::default(),
            size_constraints: Default::default(),
            is_resizable: true,
            is_transient: false,
            parent_id: None,
            icon: None,
            interaction: Default::default(),
        }
    }

    fn start() -> RuntimeHandle {
        let dir = std::env::temp_dir().join(format!("shojiwm-default-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Held until the config is enabled, which binds its IPC socket from
        // these: unlocked, a parity session could bind here, or this config
        // in a parity session's directory.
        let _environment = parity::lock_environment();
        // SAFETY: every test sets the environment under that lock.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
            std::env::set_var("WAYLAND_DISPLAY", "test-display");
        }
        let args = CommonArgs::parse(&[], &[]);
        let launcher = ConfigBuilder::new(setup)
            .hang_watchdog(HangWatchdog::log_only())
            .asset_root(concat!(env!("CARGO_MANIFEST_DIR"), "/../../packages/config"));
        let mut runtime = RuntimeBoot::new(Box::new(launcher), &args).launch(RuntimeHost::detached());
        runtime.preload().unwrap();
        runtime.enable().unwrap();
        runtime.sync_display_state(BTreeMap::from([("DP-1".to_owned(), output("DP-1", 0))]));
        runtime
    }

    /// Run timers and animations until nothing is pending.
    fn settle(runtime: &mut RuntimeHandle, now: &mut f64, ids: &[&str]) {
        for _ in 0..200 {
            *now += 16.0;
            let tick = runtime.scheduler_tick(*now).unwrap();
            for id in ids {
                runtime.evaluate_cached_window(id, None, *now as u64, false).unwrap();
            }
            if tick.next_poll_in_ms.is_none() {
                return;
            }
        }
    }

    #[test]
    fn floating_then_tiled_layout() {
        let mut runtime = start();
        let mut now = 1000.0;

        // A new window: initial configure (deferred while floating), then its
        // first commit centres it on the output.
        runtime.evaluate_window_preview(&window("w1", true), now as u64).unwrap();
        let evaluation = runtime.evaluate_window(&window("w1", true), now as u64).unwrap();
        let rect = evaluation.managed_window.rect.expect("managed rect");
        let chrome = crate::window_manager::EDGE_DRAG_HALO_PX + crate::window_manager::WINDOW_BORDER_PX;
        assert_eq!(rect.width, 800.0 + chrome * 2.0);
        assert_eq!(rect.x, (1920.0 - rect.width) / 2.0);
        assert!(
            evaluation
                .actions
                .iter()
                .any(|action| action.channel.is_none() && action.animation.is_some()),
            "the open animation rides along: {:?}",
            evaluation.actions
        );

        // Tile the workspace: the window becomes a tile in the viewport.
        let toggled = runtime.invoke_key_binding("toggle-tiling-mode", now as u64).unwrap();
        assert!(toggled.invoked);
        assert!(toggled.dirty_window_ids.contains(&"w1".to_owned()));
        let cached = runtime.evaluate_cached_window("w1", None, now as u64, false).unwrap();
        let tiled = cached.managed_window.rect.unwrap();
        assert_eq!(tiled.y, 12.0);
        assert_eq!(tiled.height, 1080.0 - 24.0);
        assert!(cached.managed_window.tiled);
        settle(&mut runtime, &mut now, &["w1"]);

        // A second window joins the row after the focused one.
        runtime.evaluate_window_preview(&window("w2", false), now as u64).unwrap();
        runtime.evaluate_window(&window("w2", false), now as u64).unwrap();
        settle(&mut runtime, &mut now, &["w1", "w2"]);
        let first = runtime.evaluate_cached_window("w1", None, now as u64, true).unwrap();
        let second = runtime.evaluate_cached_window("w2", None, now as u64, true).unwrap();
        let (first, second) = (first.managed_window.rect.unwrap(), second.managed_window.rect.unwrap());
        assert!(second.x > first.x, "{first:?} {second:?}");

        // Keyboard navigation and workspace switching run without trouble.
        for binding in ["tile-focus-left", "tile-focus-right", "tile-move-left", "workspace-next", "workspace-prev"] {
            assert!(runtime.invoke_key_binding(binding, now as u64).unwrap().invoked);
            settle(&mut runtime, &mut now, &["w1", "w2"]);
        }

        // Closing: the close animation's duration is declared.
        let close = runtime.start_close("w2", now as u64).unwrap();
        assert_eq!(close.close_animation_duration_ms, Some(500));
        settle(&mut runtime, &mut now, &["w1", "w2"]);
        runtime.window_closed("w2").unwrap();
        settle(&mut runtime, &mut now, &["w1"]);
    }

    fn collect<'a>(node: &'a shojiwm_rs::ssd::DecorationNode, out: &mut Vec<&'a shojiwm_rs::ssd::DecorationNode>) {
        out.push(node);
        for child in &node.children {
            collect(child, out);
        }
    }

    #[test]
    fn decoration_reacts_to_focus() {
        let mut runtime = start();
        let focused = runtime.evaluate_window(&window("w1", true), 1).unwrap();
        let unfocused = runtime.evaluate_window(&window("w1", false), 2).unwrap();
        // The window border, inside the drag halo.
        let border = |node: &shojiwm_rs::ssd::DecorationNode| {
            let mut nodes = Vec::new();
            collect(node, &mut nodes);
            nodes
                .iter()
                .find_map(|node| node.style.border)
                .expect("a bordered node")
                .color
        };
        assert_eq!(border(&focused.node), hex("#e0263c"));
        assert_eq!(border(&unfocused.node), hex("#8f1e2d"));
    }

    #[test]
    fn hovering_the_halo_reveals_that_edges_drag_tab() {
        use shojiwm_rs::ssd::{DecorationNode, DecorationNodeKind};

        // Visibility of the four drag tabs: top, bottom, left, right.
        fn tabs(node: &DecorationNode) -> Vec<bool> {
            let mut nodes = Vec::new();
            collect(node, &mut nodes);
            nodes
                .iter()
                .filter(|node| matches!(node.kind, DecorationNodeKind::Image(_)))
                .map(|node| node.style.visible.unwrap_or(true))
                .collect()
        }

        let mut runtime = start();
        let snapshot = window("w1", true);
        let evaluation = runtime.evaluate_window(&snapshot, 1000).unwrap();
        assert_eq!(tabs(&evaluation.node), [false; 4]);
        let mut nodes = Vec::new();
        collect(&evaluation.node, &mut nodes);
        // The four hover strips of the halo, in the same order.
        let strips: Vec<_> = nodes
            .iter()
            .filter_map(|node| node.interaction.hover_change.clone())
            .collect();
        assert_eq!(strips.len(), 4, "one hover strip per edge");

        runtime.invoke_handler("w1", &strips[2].true_handler, 1000).unwrap();
        let hovered = runtime.evaluate_window(&snapshot, 1010).unwrap();
        assert_eq!(tabs(&hovered.node), [false, false, true, false], "only the left tab shows");

        runtime.invoke_handler("w1", &strips[2].false_handler, 1020).unwrap();
        let left = runtime.evaluate_window(&snapshot, 1030).unwrap();
        assert_eq!(tabs(&left.node), [false; 4], "leaving the strip hides the tab");
    }
}
