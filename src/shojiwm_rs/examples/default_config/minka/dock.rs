//! Dock proximity: watch the pointer and broadcast enter/leave for the bottom
//! strip of each monitor. The bar uses this in place of a layer-shell trigger
//! surface, which would capture clicks meant for the windows below.
//!
//! ```text
//! dock.proximity  { monitor: string, inside: bool }  (broadcast)
//! ```
//!
//! Two thresholds, with hysteresis: the pointer must reach the bottom 10px to
//! reveal the dock, and once it is visible it must leave the bottom 120px to
//! dismiss it. A precise "reach for the dock" trigger that stays put while
//! the user is interacting with the dock, so brushing the cursor a few dozen
//! pixels above it does not flicker it away.

use std::{cell::RefCell, collections::BTreeMap};

use serde_json::json;
use shojiwm_rs::{prelude::*, ssd::PointerMoveEventSnapshot};

use super::workspace_ipc::WorkspaceIpc;
use crate::window_manager::output_rect;

const DOCK_SHOW_ZONE_PX: f64 = 10.0;
const DOCK_HIDE_ZONE_PX: f64 = 120.0;

fn pointer_in_bottom_strip(monitor: &str, x: f64, y: f64, strip: f64) -> bool {
    output_rect(monitor).is_some_and(|rect| {
        x >= rect.x && x < rect.right() && y >= rect.bottom() - strip && y < rect.bottom()
    })
}

/// The pointer-move hook that drives the broadcasts.
pub fn create_dock_proximity(ipc: &WorkspaceIpc) -> impl Fn(&PointerMoveEventSnapshot) + 'static {
    let ipc = ipc.clone();
    let inside_by_monitor: RefCell<BTreeMap<String, bool>> = RefCell::default();
    move |event| {
        // Only the monitor under the pointer can be "inside"; every other
        // one that was reports leave.
        let (x, y) = (event.position.x, event.position.y);
        for monitor in COMPOSITOR.output.list() {
            let was_inside = inside_by_monitor.borrow().get(&monitor) == Some(&true);
            let on_monitor = event.output_name.as_deref() == Some(monitor.as_str());
            let strip = if was_inside {
                DOCK_HIDE_ZONE_PX
            } else {
                DOCK_SHOW_ZONE_PX
            };
            let inside = on_monitor && pointer_in_bottom_strip(&monitor, x, y, strip);
            if inside_by_monitor.borrow().get(&monitor) == Some(&inside) {
                continue;
            }
            inside_by_monitor
                .borrow_mut()
                .insert(monitor.clone(), inside);
            ipc.broadcast(
                "dock.proximity",
                json!({ "monitor": monitor, "inside": inside }),
            );
        }
    }
}
