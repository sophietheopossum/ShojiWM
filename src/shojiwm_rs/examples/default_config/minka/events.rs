//! Feeds compositor events to the window manager and keeps IPC clients in
//! step.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
};

use shojiwm_rs::{
    prelude::*,
    ssd::{WaylandLayerSnapshot, WindowMovePhaseSnapshot},
};

use super::{dock::create_dock_proximity, workspace_ipc::WorkspaceIpc};
use crate::window_manager::WindowManager;

/// The global pointer position for the decoration's drag tabs. `moves` is
/// bumped only while some window has a hovered edge, as the TypeScript
/// config does (its runtime re-evaluates every window when a signal nothing
/// reads is written). Here a write only reaches the signal's own readers,
/// so the gate just skips an empty flush.
#[derive(Clone)]
pub struct PointerTracking {
    /// Always current, so a tab whose hover just started reads where the
    /// pointer is now.
    latest: Rc<Cell<(f64, f64)>>,
    /// Bumped on motion while an edge is hovered: tells hovered compositions
    /// that the pointer moved.
    moves: Signal<u64>,
    /// Windows with a hovered drag edge: only their compositions read the
    /// pointer.
    edge_hovered: Rc<RefCell<HashSet<String>>>,
}

impl PointerTracking {
    /// Where the pointer is now. Inside a memo, re-runs it on pointer motion.
    pub fn current(&self) -> (f64, f64) {
        // Read for the dependency alone.
        self.moves.get();
        self.latest.get()
    }

    /// A window's drag-edge hover started or ended.
    pub fn set_edge_hovered(&self, window_id: &str, hovered: bool) {
        let mut windows = self.edge_hovered.borrow_mut();
        if hovered {
            windows.insert(window_id.to_owned());
        } else {
            windows.remove(window_id);
        }
    }
}

/// Returns the pointer tracking for the decoration's drag tabs.
pub fn wire_window_events(wm: &WindowManager, ipc: &WorkspaceIpc) -> PointerTracking {
    let pointer = PointerTracking {
        latest: Rc::default(),
        moves: signal(0),
        edge_hovered: Rc::default(),
    };

    // The dock displays live window titles, so a title change must refresh
    // the IPC view. The broadcast is diffed, so noisy title churn
    // (terminals) only goes out when the string actually changed.
    let title_subscriptions: Rc<RefCell<HashMap<Window, Scope>>> = Rc::default();

    {
        let (wm, ipc, titles) = (wm.clone(), ipc.clone(), title_subscriptions.clone());
        COMPOSITOR.event.on_open(move |window| {
            wm.with(|wm| wm.on_open(window));
            let scope = untrack(Scope::root);
            let ipc = ipc.clone();
            let first_run = Cell::new(true);
            scope.effect(move || {
                window.title().get();
                if !first_run.replace(false) {
                    untrack(|| ipc.schedule_workspace_broadcast());
                }
            });
            if let Some(previous) = titles.borrow_mut().insert(window, scope) {
                previous.dispose();
            }
        });
    }
    {
        let wm = wm.clone();
        COMPOSITOR
            .event
            .on_initial_configure(move |window| wm.with(|wm| wm.on_initial_configure(window)));
    }
    {
        let (wm, ipc) = (wm.clone(), ipc.clone());
        COMPOSITOR.event.on_first_commit(move |window| {
            wm.with(|wm| wm.on_first_commit(window));
            ipc.schedule_workspace_broadcast();
        });
    }
    {
        let (wm, ipc) = (wm.clone(), ipc.clone());
        COMPOSITOR.event.on_start_close(move |window| {
            wm.with(|wm| wm.on_start_close(window));
            ipc.schedule_workspace_broadcast();
        });
    }
    {
        let (wm, ipc, pointer, titles) = (
            wm.clone(),
            ipc.clone(),
            pointer.clone(),
            title_subscriptions,
        );
        COMPOSITOR.event.on_close(move |window| {
            wm.with(|wm| wm.on_close(window));
            pointer.set_edge_hovered(&window.id(), false);
            if let Some(scope) = titles.borrow_mut().remove(&window) {
                scope.dispose();
            }
            ipc.schedule_workspace_broadcast();
        });
    }
    {
        let (wm, ipc) = (wm.clone(), ipc.clone());
        COMPOSITOR.event.on_focus(move |window, focused| {
            wm.with(|wm| {
                wm.on_focus(window, focused);
                if focused {
                    wm.record_focus(window);
                }
            });
            // On loss of focus too: when the unfocus lands in a later turn
            // than the gain, a gain-only broadcast snapshots BOTH windows as
            // focused and nothing ever corrects it, so the dock and bar keep
            // highlighting the previously focused window.
            ipc.schedule_workspace_broadcast();
        });
    }
    {
        let (wm, pointer) = (wm.clone(), pointer.clone());
        let track_dock_proximity = create_dock_proximity(ipc);
        COMPOSITOR.event.on_pointer_move_async(move |event| {
            pointer.latest.set((event.position.x, event.position.y));
            if !pointer.edge_hovered.borrow().is_empty() {
                pointer.moves.update(|moves| *moves += 1);
            }
            wm.with(|wm| wm.on_pointer_move(event));
            track_dock_proximity(event);
        });
    }
    {
        let (wm, ipc) = (wm.clone(), ipc.clone());
        COMPOSITOR.event.on_gesture_swipe(move |event| {
            wm.with(|wm| wm.on_gesture_swipe(event));
            ipc.schedule_workspace_broadcast();
        });
    }
    {
        let (wm, ipc) = (wm.clone(), ipc.clone());
        COMPOSITOR.event.on_output_change(move |event| {
            wm.with(|wm| wm.on_output_change(event));
            ipc.schedule_workspace_broadcast();
        });
    }
    for register in [
        |f: Box<dyn Fn(&WaylandLayerSnapshot)>| COMPOSITOR.event.on_create_layer(f),
        |f: Box<dyn Fn(&WaylandLayerSnapshot)>| COMPOSITOR.event.on_update_layer(f),
        |f: Box<dyn Fn(&WaylandLayerSnapshot)>| COMPOSITOR.event.on_destroy_layer(f),
    ] {
        let wm = wm.clone();
        register(Box::new(move |_| {
            wm.with(|wm| wm.refresh_usable_area_layouts())
        }));
    }
    {
        let (wm, ipc) = (wm.clone(), ipc.clone());
        COMPOSITOR.event.on_window_resize(move |window, event| {
            wm.with(|wm| wm.on_window_resize(window, event));
            ipc.schedule_rects_broadcast();
        });
    }
    {
        let (wm, ipc) = (wm.clone(), ipc.clone());
        COMPOSITOR.event.on_window_move(move |window, event| {
            wm.with(|wm| wm.on_window_move(window, event));
            ipc.schedule_rects_broadcast();
            // A drag can hand the window to another monitor's workspace;
            // without a broadcast the dock keeps listing it on the old output
            // until some unrelated event refreshes the view.
            if matches!(
                event.phase,
                WindowMovePhaseSnapshot::End | WindowMovePhaseSnapshot::Cancel
            ) {
                ipc.schedule_workspace_broadcast();
            }
        });
    }
    {
        let (wm, ipc) = (wm.clone(), ipc.clone());
        COMPOSITOR
            .event
            .on_window_maximize_request(move |window, event| {
                wm.with(|wm| wm.on_window_maximize_request(window, event));
                // The view carries maximized/minimized per window (the bar's
                // window controls render from it), so state changes broadcast.
                ipc.schedule_workspace_broadcast();
            });
    }
    {
        let (wm, ipc) = (wm.clone(), ipc.clone());
        COMPOSITOR
            .event
            .on_window_minimize_request(move |window, event| {
                wm.with(|wm| wm.on_window_minimize_request(window, event));
                ipc.schedule_workspace_broadcast();
            });
    }
    {
        let wm = wm.clone();
        COMPOSITOR
            .event
            .on_window_fullscreen_request(move |window, event| {
                wm.with(|wm| wm.on_window_fullscreen_request(window, event))
            });
    }
    {
        let (wm, ipc) = (wm.clone(), ipc.clone());
        COMPOSITOR
            .event
            .on_window_activate_request(move |window, event| {
                wm.with(|wm| wm.on_window_activate_request(window, event));
                ipc.schedule_workspace_broadcast();
            });
    }

    pointer
}
