//! External IPC: the workspace layout for MinkaShell, MinkaMon, MinkaShot and
//! MinkaFX.
//!
//! ```text
//! workspaces.get           { rectsLease?: string } -> WorkspacesView (the token
//!                          renews a 2 s windows.rects lease for this client)
//! workspaces.switch        { direction: -1 | 1 }                 (command)
//! workspaces.activate      { monitor: string, index: number }    (command)
//! workspaces.toggleTiling  { monitor?: string }                  (command)
//! workspaces.changed       -> WorkspacesView                     (broadcast)
//! windows.activate         { windowId: string }                  (command)
//! windows.close            { windowId: string }                  (command)
//! windows.reorder          { windowId, beforeId: string|null }   -> { ok, changed }
//! windows.identify         { windowId, role: string|null }       (command)
//! windows.maximize         { windowId, maximized?: bool }        (command)
//! windows.minimize         { windowId: string }                  (command)
//! windows.setRect          { windowId, x, y, width, height }     -> { ok }
//! windows.rects            -> { windows: [...] }                 (event, lease holders only)
//! snap.preview             -> { monitor, rect|null, kind }       (broadcast)
//! debug.geometry           -> { outputs, usable, insets, layers }
//! ```
//!
//! Served elsewhere on the same socket: `dock.proximity` (dock), `ui.startMenu`
//! and `ui.minkashot` (keybinds), `minka.revision` and `settings.*`
//! (settings).

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde_json::{Map, Value, json};
use shojiwm_rs::{
    ipc::{IpcClient, IpcServer},
    prelude::*,
    runtime_workspace::{
        RuntimeWorkspaceConfigUpdate, RuntimeWorkspaceEntry, RuntimeWorkspaceGroupConfig,
    },
};

use super::{js_numbers, rect_json};
use crate::window_manager::{ReorderOutcome, WINDOW_STATE_RECT, WindowManager, get};

/// Live rect stream for MinkaMon's leader lines, pushed on every window move
/// and resize so the lines track drags at event rate instead of the client's
/// fallback poll. Sent only to clients holding a lease, not broadcast:
/// MinkaShell, MinkaShot and MinkaFX stay connected all session and have no
/// use for it. A client leases the stream by passing `rectsLease: <token>`
/// on its workspaces.get poll, and the lease lapses this long after the last
/// one.
const RECTS_LEASE_MS: f64 = 2000.0;
const RECTS_MAX_SUBSCRIBERS: usize = 8;

/// The geometry of a window's drag tab while one is shown.
pub type DragTabSource = Rc<dyn Fn() -> Option<Rect>>;

struct RectsSubscriber {
    token: String,
    client: IpcClient,
    renewed_at: f64,
}

#[derive(Default)]
struct State {
    last_workspaces: RefCell<String>,
    last_snap: RefCell<String>,
    workspace_broadcast_deferred: Cell<bool>,
    rects_broadcast_deferred: Cell<bool>,
    /// Live drag-tab geometry per window id, registered by the decoration.
    /// Evaluated only when a view is built, so hover and pointer state are
    /// sampled only for MinkaMon's poll.
    drag_tabs: RefCell<HashMap<String, DragTabSource>>,
    /// Oldest renewal first, for eviction.
    rects_subscribers: RefCell<Vec<RectsSubscriber>>,
}

/// The IPC socket and the broadcasts every module sends through it.
#[derive(Clone)]
pub struct WorkspaceIpc {
    pub server: Option<IpcServer>,
    wm: WindowManager,
    state: Rc<State>,
}

fn wall_clock_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

fn rects_lease_live(renewed_at: f64, now: f64) -> bool {
    let age = now - renewed_at;
    // A backwards clock step expires a lease rather than extending it.
    (0.0..=RECTS_LEASE_MS).contains(&age)
}

impl WorkspaceIpc {
    pub fn broadcast(&self, event: &str, payload: Value) {
        if let Some(server) = &self.server {
            server.broadcast(event, js_numbers(payload));
        }
    }

    /// A method whose reply carries no result.
    fn command(&self, method: &str, f: impl Fn(&Value) + 'static) {
        if let Some(server) = &self.server {
            server.handle_with_client(method, move |params, _| {
                f(params);
                Ok(None)
            });
        }
    }

    fn request(&self, method: &str, f: impl Fn(&Value) -> Value + 'static) {
        if let Some(server) = &self.server {
            server.handle_with_client(method, move |params, _| Ok(Some(js_numbers(f(params)))));
        }
    }

    /// The workspaces view as clients see it: the window manager's, plus
    /// the live drag tabs.
    pub fn view(&self) -> Value {
        // As the TypeScript view does: a current monitor that is no longer
        // an output (an activation naming one just unplugged) is put back
        // on a live one before anyone reads it.
        if !self.wm.is_busy() {
            self.wm.with(|wm| wm.sync_workspaces());
        }
        let mut view = self.wm.read(|wm| wm.view_for_ipc()).to_json();
        let mut live = HashSet::new();
        let drag_tabs = self.state.drag_tabs.borrow().clone();
        let windows = view["monitors"]
            .as_array_mut()
            .into_iter()
            .flatten()
            .filter_map(|monitor| monitor["workspaces"].as_array_mut())
            .flatten()
            .filter_map(|workspace| workspace["windows"].as_array_mut())
            .flatten();
        for window in windows {
            let Some(id) = window["id"].as_str().map(str::to_owned) else {
                continue;
            };
            window["dragTab"] = drag_tab_json(drag_tabs.get(&id));
            live.insert(id);
        }
        self.state
            .drag_tabs
            .borrow_mut()
            .retain(|id, _| live.contains(id));
        js_numbers(view)
    }

    /// Stage the protocol workspace model and push the diffed view. The
    /// model is staged before the current reply goes out, so external bars
    /// see a key binding's or activation's change on this turn rather than
    /// on a later, unrelated request.
    pub fn schedule_workspace_broadcast(&self) {
        if self.wm.is_busy() {
            // Called back from inside the window manager: once it is free.
            if !self.state.workspace_broadcast_deferred.replace(true) {
                let ipc = self.clone();
                set_timeout(0.0, move || {
                    ipc.state.workspace_broadcast_deferred.set(false);
                    ipc.schedule_workspace_broadcast();
                });
            }
            return;
        }
        COMPOSITOR.workspace.reconfigure();
        let view = self.view();
        let encoded = view.to_string();
        if *self.state.last_workspaces.borrow() == encoded {
            return;
        }
        *self.state.last_workspaces.borrow_mut() = encoded;
        self.broadcast("workspaces.changed", view);
    }

    /// Send `windows.rects` to the lease holders, if there are any.
    pub fn schedule_rects_broadcast(&self) {
        if self.state.rects_subscribers.borrow().is_empty() {
            return;
        }
        if self.wm.is_busy() {
            if !self.state.rects_broadcast_deferred.replace(true) {
                let ipc = self.clone();
                set_timeout(0.0, move || {
                    ipc.state.rects_broadcast_deferred.set(false);
                    ipc.schedule_rects_broadcast();
                });
            }
            return;
        }
        self.prune_rects_subscribers(wall_clock_ms());
        if self.state.rects_subscribers.borrow().is_empty() {
            return;
        }
        let drag_tabs = self.state.drag_tabs.borrow().clone();
        let windows: Vec<Value> = self
            .wm
            .read(|wm| wm.list_windows())
            .into_iter()
            .map(|window| {
                let id = window.id();
                let rect = get(window, &WINDOW_STATE_RECT);
                json!({
                    "id": id,
                    "x": rect.x,
                    "y": rect.y,
                    "width": rect.width,
                    "height": rect.height,
                    "dragTab": drag_tab_json(drag_tabs.get(&id)),
                })
            })
            .collect();
        let payload = js_numbers(json!({ "windows": windows }));
        for subscriber in self.state.rects_subscribers.borrow().iter() {
            subscriber.client.send("windows.rects", payload.clone());
        }
    }

    /// Registered by the decoration composition, one entry per window.
    pub fn publish_drag_tab(&self, window_id: &str, rect: impl Fn() -> Option<Rect> + 'static) {
        self.state
            .drag_tabs
            .borrow_mut()
            .insert(window_id.to_owned(), Rc::new(rect));
    }

    fn prune_rects_subscribers(&self, now: f64) {
        self.state
            .rects_subscribers
            .borrow_mut()
            .retain(|subscriber| rects_lease_live(subscriber.renewed_at, now));
    }

    fn renew_rects_lease(&self, params: &Value, client: &IpcClient) {
        let Some(token) = params["rectsLease"].as_str() else {
            return;
        };
        if token.is_empty() || token.encode_utf16().count() > 64 {
            return;
        }
        let now = wall_clock_ms();
        {
            let mut subscribers = self.state.rects_subscribers.borrow_mut();
            subscribers.retain(|subscriber| subscriber.token != token);
            subscribers.push(RectsSubscriber {
                token: token.to_owned(),
                client: client.clone(),
                renewed_at: now,
            });
        }
        self.prune_rects_subscribers(now);
        let mut subscribers = self.state.rects_subscribers.borrow_mut();
        let excess = subscribers.len().saturating_sub(RECTS_MAX_SUBSCRIBERS);
        subscribers.drain(..excess);
    }
}

fn drag_tab_json(source: Option<&DragTabSource>) -> Value {
    source
        .and_then(|rect| rect())
        .map_or(Value::Null, rect_json)
}

/// Opens the socket, then registers every workspace and window method on it.
pub fn create_workspace_ipc(wm: &WindowManager) -> WorkspaceIpc {
    let server = match IpcServer::new() {
        Ok(server) => Some(server),
        Err(error) => {
            tracing::warn!(%error, "workspace IPC socket unavailable");
            None
        }
    };
    let ipc = WorkspaceIpc {
        server,
        wm: wm.clone(),
        state: Rc::default(),
    };
    serve_workspaces(&ipc, wm);
    ipc
}

fn serve_workspaces(ipc: &WorkspaceIpc, wm: &WindowManager) {
    {
        let wm = wm.clone();
        COMPOSITOR.workspace.configure(move || {
            let view = wm.read(|wm| wm.view_for_ipc());
            RuntimeWorkspaceConfigUpdate {
                groups: view
                    .monitors
                    .iter()
                    .map(|monitor| RuntimeWorkspaceGroupConfig {
                        id: monitor.name.clone(),
                        outputs: vec![monitor.name.clone()],
                        workspaces: monitor
                            .workspaces
                            .iter()
                            .map(|workspace| RuntimeWorkspaceEntry {
                                id: format!("{}:{}", monitor.name, workspace.index),
                                name: workspace.index.to_string(),
                                coordinates: vec![workspace.index.saturating_sub(1)],
                                active: workspace.active,
                                urgent: false,
                                hidden: !workspace.active && workspace.window_count == 0,
                            })
                            .collect(),
                    })
                    .collect(),
            }
        });
    }

    {
        let ipc = ipc.clone();
        COMPOSITOR.workspace.on_activate(move |event| {
            let Some((monitor, index)) = event.workspace_id.split_once(':') else {
                return;
            };
            let Ok(index) = index.parse::<u32>() else {
                return;
            };
            if !monitor.is_empty() && index >= 1 {
                ipc.wm.with(|wm| wm.activate(monitor, index));
                ipc.schedule_workspace_broadcast();
            }
        });
    }

    if let Some(server) = &ipc.server {
        let ipc = ipc.clone();
        server.handle_with_client("workspaces.get", move |params, client| {
            ipc.renew_rects_lease(params, client);
            Ok(Some(ipc.view()))
        });
    }
    {
        let handle = ipc.clone();
        ipc.command("workspaces.switch", move |params| {
            let direction = if params["direction"].as_f64() == Some(-1.0) {
                -1
            } else {
                1
            };
            handle.wm.with(|wm| wm.switch_workspace(direction));
            handle.schedule_workspace_broadcast();
        });
    }
    {
        let handle = ipc.clone();
        ipc.command("workspaces.activate", move |params| {
            let monitor = params["monitor"]
                .as_str()
                .filter(|monitor| !monitor.is_empty());
            if let (Some(monitor), Some(index)) = (monitor, params["index"].as_f64()) {
                // `activate` refuses indexes below 1 itself.
                if index.fract() == 0.0 && index >= 0.0 {
                    handle.wm.with(|wm| wm.activate(monitor, index as u32));
                    handle.schedule_workspace_broadcast();
                }
            }
        });
    }
    {
        let handle = ipc.clone();
        ipc.command("workspaces.toggleTiling", move |params| {
            match params["monitor"]
                .as_str()
                .filter(|monitor| !monitor.is_empty())
            {
                Some(monitor) => handle
                    .wm
                    .with(|wm| wm.toggle_workspace_tiling_for_monitor(monitor)),
                None => handle.wm.with(|wm| wm.toggle_current_workspace_tiling()),
            }
            handle.schedule_workspace_broadcast();
        });
    }
    {
        let handle = ipc.clone();
        ipc.command("windows.activate", move |params| {
            if let Some(id) = params["windowId"].as_str() {
                handle.wm.with(|wm| wm.activate_window_by_id(id));
                handle.schedule_workspace_broadcast();
            }
        });
    }
    {
        let handle = ipc.clone();
        ipc.command("windows.close", move |params| {
            if let Some(id) = params["windowId"].as_str() {
                handle.wm.with(|wm| wm.close_window_by_id(id));
                handle.schedule_workspace_broadcast();
            }
        });
    }
    // Dock drag-to-reorder (MinkaShell, 15/9/2026): put `windowId` directly
    // before `beforeId` in its own workspace's window order, or last when
    // `beforeId` is null. That order is the tile sequence on a tiled
    // workspace and the Alt+Tab ring everywhere. Never crosses workspaces,
    // never focuses; refused during a pointer tile drag. ok with
    // changed:false is a no-op.
    {
        let handle = ipc.clone();
        ipc.request("windows.reorder", move |params| {
            let refused = json!({ "ok": false, "changed": false });
            let Some(id) = params["windowId"].as_str() else {
                return refused;
            };
            let before = match params.get("beforeId") {
                Some(Value::Null) => None,
                Some(Value::String(before)) => Some(before.as_str()),
                _ => return refused,
            };
            let outcome = handle.wm.with(|wm| wm.reorder_window_by_id(id, before));
            if outcome == ReorderOutcome::Moved {
                handle.schedule_workspace_broadcast();
            }
            json!({
                "ok": outcome != ReorderOutcome::Refused,
                "changed": outcome == ReorderOutcome::Moved,
            })
        });
    }
    // Client-declared semantic window roles ("typed segments", after Arcan's
    // SHMIF): Minka apps claim what a window *is*, e.g. "minkamon.disk", so
    // consumers (leader lines, overview arrangement, MinkaShot's window
    // capture) stop matching on mutable title strings.
    {
        let handle = ipc.clone();
        ipc.command("windows.identify", move |params| {
            if let Some(id) = params["windowId"].as_str() {
                handle
                    .wm
                    .with(|wm| wm.set_window_role(id, params["role"].as_str()));
                handle.schedule_workspace_broadcast();
            }
        });
    }
    // Drives the same maximize path a client CSD button takes, addressable
    // by window id from outside the session.
    {
        let handle = ipc.clone();
        ipc.command("windows.maximize", move |params| {
            let Some(id) = params["windowId"].as_str().filter(|id| !id.is_empty()) else {
                return;
            };
            let Some(window) = handle.wm.read(|wm| wm.find_window_by_id(id)) else {
                return;
            };
            if params["maximized"] == Value::Bool(false) {
                window.unmaximize();
            } else {
                window.maximize();
            }
            handle.schedule_workspace_broadcast();
        });
    }
    // Externally-driven move/resize (MinkaMon's full-overview arrangement).
    {
        let handle = ipc.clone();
        ipc.request("windows.setRect", move |params| {
            let number = |key: &str| params[key].as_f64();
            let (Some(id), Some(x), Some(y), Some(width), Some(height)) = (
                params["windowId"].as_str(),
                number("x"),
                number("y"),
                number("width"),
                number("height"),
            ) else {
                return json!({ "ok": false });
            };
            let ok = handle
                .wm
                .with(|wm| wm.set_window_rect_by_id(id, Rect::new(x, y, width, height)));
            handle.schedule_workspace_broadcast();
            json!({ "ok": ok })
        });
    }
    // Bar window controls: minimize (restoring goes through windows.activate,
    // which unminimizes and focuses).
    {
        let handle = ipc.clone();
        ipc.command("windows.minimize", move |params| {
            let Some(id) = params["windowId"].as_str().filter(|id| !id.is_empty()) else {
                return;
            };
            let Some(window) = handle.wm.read(|wm| wm.find_window_by_id(id)) else {
                return;
            };
            window.minimize();
            handle.schedule_workspace_broadcast();
        });
    }
    // Diagnostic dump from the window-sizing investigation (7/2026):
    // everything the config believes about outputs, layer exclusive zones,
    // and the usable areas derived from them. Queryable from another VT
    // while the compositor runs:
    //   printf '{"id":1,"method":"debug.geometry"}\n' \
    //     | socat - UNIX-CONNECT:$XDG_RUNTIME_DIR/shojiwm-<display>.sock
    ipc.request("debug.geometry", |_| {
        let mut usable = Map::new();
        let mut insets = Map::new();
        for name in COMPOSITOR.output.list() {
            let area = untrack(|| COMPOSITOR.layer.usable_area(&name));
            usable.insert(name.clone(), area.map_or(Value::Null, rect_json));
            let reserved = untrack(|| COMPOSITOR.layer.reserved_insets(&name));
            insets.insert(
                name,
                json!({
                    "top": reserved.top,
                    "right": reserved.right,
                    "bottom": reserved.bottom,
                    "left": reserved.left,
                }),
            );
        }
        let to_json = |value: Result<Value, serde_json::Error>| value.unwrap_or(Value::Null);
        json!({
            "outputs": to_json(serde_json::to_value(COMPOSITOR.output.state().get_untracked())),
            "usable": usable,
            "insets": insets,
            "layers": to_json(serde_json::to_value(COMPOSITOR.layer.state().get_untracked())),
        })
    });

    // Snap-zone preview: the active snap rect (floating edge zones, or the
    // opened tiling slot), which the bar renders as the rounded overlay.
    //   snap.preview  { monitor, rect: {x,y,width,height} | null, kind: "floating"|"tiling" }
    {
        let ipc = ipc.clone();
        wm.set_snap_preview_broadcaster(move |preview| {
            let payload = js_numbers(json!({
                "monitor": preview.monitor,
                "rect": preview.rect.map_or(Value::Null, rect_json),
                "kind": preview.kind,
            }));
            let encoded = payload.to_string();
            if *ipc.state.last_snap.borrow() == encoded {
                return;
            }
            *ipc.state.last_snap.borrow_mut() = encoded;
            ipc.broadcast("snap.preview", payload);
        });
    }
    {
        let ipc = ipc.clone();
        wm.set_workspace_change_broadcaster(move || ipc.schedule_workspace_broadcast());
    }
    {
        let ipc = ipc.clone();
        COMPOSITOR.on_disable(move |_| {
            if let Some(server) = &ipc.server {
                server.close();
            }
        });
    }
}
