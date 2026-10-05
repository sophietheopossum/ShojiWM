//! The TypeScript config's scenario tests (the real-config tests in
//! `src/shojiwm/src/evaluator.rs`), ported so this config is held to the
//! same behaviour. Each drives the config through `RuntimeHandle` the way
//! `shojiwm_lib` does, on one 1920x1080 output named TEST-1.

use std::{
    io::{BufRead, BufReader, ErrorKind, Write},
    ops::{Deref, DerefMut},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{
        Mutex, OnceLock, PoisonError,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use shojiwm_rs::{
    HostMessage,
    runtime_key_binding::RuntimeKeyBindingConfigUpdate,
    ssd::{
        GestureSwipeEventSnapshot, GestureSwipePhaseSnapshot, ManagedWindowRectSnapshot,
        PointerModifierStateSnapshot, RuntimeWindowAction, WaylandWindowAction,
        WindowActivateRequestEventSnapshot, WindowActivateRequestSourceSnapshot,
        WindowMaximizeRequestEventSnapshot, WindowMinimizeRequestEventSnapshot,
        WindowMoveEventSnapshot, WindowMovePhaseSnapshot, WindowMoveSourceSnapshot,
        WindowResizeEdgesSnapshot, WindowResizeEventSnapshot, WindowResizePhaseSnapshot,
        WindowResizePointSnapshot, WindowResizeSourceSnapshot, WindowStateRequestSourceSnapshot,
    },
};

use super::*;

/// The config under test, and the host it publishes to.
struct Session {
    runtime: RuntimeHandle,
    host: RuntimeHost,
    /// The session's XDG_RUNTIME_DIR.
    dir: PathBuf,
    /// Where the config's IPC server listens.
    socket: PathBuf,
}

impl Deref for Session {
    type Target = RuntimeHandle;

    fn deref(&self) -> &RuntimeHandle {
        &self.runtime
    }
}

impl DerefMut for Session {
    fn deref_mut(&mut self) -> &mut RuntimeHandle {
        &mut self.runtime
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.runtime.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The config binds its IPC socket from XDG_RUNTIME_DIR and WAYLAND_DISPLAY
/// once enabled. Tests run in parallel, so each session sets them, with a
/// runtime dir of its own, and binds under this lock.
static ENVIRONMENT: Mutex<()> = Mutex::new(());
static SESSIONS: AtomicUsize = AtomicUsize::new(0);

/// A freshly enabled config. `tiled` tiles the empty workspace first, where
/// the TypeScript tests restore a tiled workspace from persisted state: this
/// runtime has none to restore.
fn session(tiled: bool) -> Session {
    let dir = std::env::temp_dir().join(format!(
        "shojiwm-parity-{}-{}",
        std::process::id(),
        SESSIONS.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("runtime dir should be created");
    let host = RuntimeHost::detached();
    let launcher = ConfigBuilder::new(setup).asset_root(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../packages/config"
    ));
    let runtime = {
        let _environment = ENVIRONMENT.lock().unwrap_or_else(PoisonError::into_inner);
        // SAFETY: every session sets these under the lock, and the socket is
        // bound from them before it is released.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", &dir);
            std::env::set_var("WAYLAND_DISPLAY", "parity");
        }
        let args = CommonArgs::parse(&[], &[]);
        let mut runtime = RuntimeBoot::new(Box::new(launcher), &args).launch(host.clone());
        runtime.preload().expect("the config should load");
        runtime.sync_display_state(BTreeMap::from([("TEST-1".to_owned(), test_output())]));
        runtime.enable().expect("the config should enable");
        runtime
    };
    let mut session = Session {
        runtime,
        host,
        socket: dir.join("shojiwm-parity.sock"),
        dir,
    };
    if tiled {
        let toggled = session
            .invoke_key_binding("toggle-tiling-mode", 0)
            .expect("toggle-tiling-mode should evaluate");
        assert!(
            toggled.invoked,
            "toggle-tiling-mode should be a known binding"
        );
    }
    session
}

/// One 1920x1080 output at the origin.
fn test_output() -> WaylandOutputSnapshot {
    let mut output = output("TEST-1", 0);
    output.connector = None;
    output
}

/// An 800x600 floating client at the origin.
fn named_window(id: &str, app_id: &str, focused: bool, maximized: bool) -> WaylandWindowSnapshot {
    let rect = WindowPositionSnapshot {
        x: 0.0,
        y: 0.0,
        width: 800.0,
        height: 600.0,
    };
    let mut snapshot = window(id, focused);
    snapshot.title = format!("{app_id} window");
    snapshot.app_id = Some(app_id.to_owned());
    snapshot.position = rect;
    snapshot.rect = rect;
    snapshot.is_maximized = maximized;
    snapshot
}

fn has_action(
    actions: &[RuntimeWindowAction],
    window_id: &str,
    expected: WaylandWindowAction,
) -> bool {
    actions
        .iter()
        .any(|action| action.window_id == window_id && action.action == expected)
}

/// A three-finger horizontal swipe on TEST-1.
fn swipe(
    phase: GestureSwipePhaseSnapshot,
    delta_x: f64,
    velocity_x: f64,
    timestamp: u64,
) -> GestureSwipeEventSnapshot {
    GestureSwipeEventSnapshot {
        phase,
        fingers: 3,
        position: None,
        delta_x,
        delta_y: 0.0,
        total_x: delta_x,
        total_y: 0.0,
        velocity_x,
        velocity_y: 0.0,
        output_name: Some("TEST-1".into()),
        device: None,
        timestamp,
    }
}

impl Session {
    /// Last message `pick` accepts among those published so far.
    fn published<T>(&self, pick: impl Fn(HostMessage) -> Option<T>) -> Option<T> {
        std::iter::from_fn(|| self.host.pop())
            .filter_map(pick)
            .last()
    }

    fn published_key_bindings(&self) -> Option<RuntimeKeyBindingConfigUpdate> {
        self.published(|message| match message {
            HostMessage::KeyBindings(config) => Some(config),
            _ => None,
        })
    }

    /// Open `windows` (id, maximized) in turn, each taking focus. The previous
    /// window's unfocus is delivered before the next one takes focus: new
    /// tiles insert after the focused window, so stale focus snapshots would
    /// scramble the tile order. Returns the time after the last one.
    fn open_in_turn(&mut self, app_id: &str, windows: &[(&str, bool)], mut now: u64) -> u64 {
        let mut previous: Option<(&str, bool)> = None;
        for &(id, maximized) in windows {
            self.evaluate_window_preview(&named_window(id, app_id, false, maximized), now)
                .expect("preview should evaluate");
            if let Some((previous, previous_maximized)) = previous {
                self.evaluate_window(
                    &named_window(previous, app_id, false, previous_maximized),
                    now + 25,
                )
                .expect("defocus evaluation should succeed");
            }
            self.evaluate_window(&named_window(id, app_id, true, maximized), now + 50)
                .expect("evaluation should succeed");
            previous = Some((id, maximized));
            now += 100;
        }
        now
    }

    /// A tile's rect as the compositor reads it after a managed-window-only
    /// update: through the cached evaluation path. A full evaluation with a
    /// fresh snapshot would reconcile against the snapshot's stale floating
    /// rect instead.
    fn managed_rect(&mut self, id: &str, now: u64) -> ManagedWindowRectSnapshot {
        let managed = self
            .evaluate_cached_window(id, None, now, false)
            .expect("cached evaluation should succeed")
            .managed_window;
        // Floating windows all open centred, where "flush" and "centred"
        // measure the same: an untiled run would pass vacuously.
        assert!(managed.tiled, "{id} should be a tile");
        managed
            .rect
            .expect("tiled window should have a managed rect")
    }

    fn swipe(&mut self, phase: GestureSwipePhaseSnapshot, delta_x: f64, velocity_x: f64, now: u64) {
        self.gesture_swipe(&swipe(phase, delta_x, velocity_x, now), now)
            .expect("swipe should evaluate");
    }

    /// `count` scheduler ticks `step` ms apart.
    fn tick_for(&mut self, now: &mut u64, count: usize, step: u64) {
        for _ in 0..count {
            *now += step;
            self.scheduler_tick(*now as f64)
                .expect("scheduler tick should evaluate");
        }
    }
}

/// A client of the config's IPC server. Requests reach the config through a
/// channel drained at the start of each compositor turn, so every wait turns
/// the runtime over.
struct Client {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
    /// A line whose end has not arrived yet.
    partial: String,
}

impl Client {
    fn connect(path: &Path) -> Self {
        let stream = UnixStream::connect(path).expect("config IPC should accept");
        stream
            .set_read_timeout(Some(Duration::from_millis(10)))
            .expect("read timeout should be configured");
        Self {
            reader: BufReader::new(stream.try_clone().expect("stream should clone")),
            writer: stream,
            next_id: 1,
            partial: String::new(),
        }
    }

    fn send(&mut self, frame: Value) {
        self.writer
            .write_all(format!("{frame}\n").as_bytes())
            .expect("frame should be written");
    }

    /// The next line, or `None` once `wait` passes without one.
    fn next_line(&mut self, runtime: &mut RuntimeHandle, wait: Duration) -> Option<Value> {
        let deadline = Instant::now() + wait;
        loop {
            runtime
                .scheduler_tick(0.0)
                .expect("scheduler tick should evaluate");
            match self.reader.read_line(&mut self.partial) {
                Ok(0) => return None,
                Ok(_) if self.partial.ends_with('\n') => {
                    let line = std::mem::take(&mut self.partial);
                    return Some(serde_json::from_str(&line).expect("IPC lines should be JSON"));
                }
                Ok(_) => {}
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(error) => panic!("IPC read failed: {error}"),
            }
            if Instant::now() >= deadline {
                return None;
            }
        }
    }

    fn request(&mut self, runtime: &mut RuntimeHandle, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "id": id, "method": method, "params": params }));
        loop {
            let message = self
                .next_line(runtime, Duration::from_secs(3))
                .expect("reply should arrive");
            if message["id"] == json!(id) {
                return message["result"].clone();
            }
        }
    }

    /// Lines until the socket stays quiet for 400 ms.
    fn drain(&mut self, runtime: &mut RuntimeHandle) -> Vec<Value> {
        std::iter::from_fn(|| self.next_line(runtime, Duration::from_millis(400))).collect()
    }
}

fn launch_scenario_z_indices(second_window_maximized: bool, tiled: bool) -> (i32, i32) {
    let mut s = session(tiled);

    // First app opens unmaximized and takes focus.
    let editor = named_window("0xa", "org.gnome.TextEditor", false, false);
    s.evaluate_window_preview(&editor, 0)
        .expect("editor preview should evaluate");
    let editor_focused = named_window("0xa", "org.gnome.TextEditor", true, false);
    s.evaluate_window(&editor_focused, 100)
        .expect("editor evaluation should succeed");

    // Second app launches; when maximized it sends set_maximized before its
    // window joins any workspace (observed live: the maximize request is
    // dispatched before hybrid-initial-configure).
    let chrome = named_window("0xb", "google-chrome", false, second_window_maximized);
    if second_window_maximized {
        s.window_maximize_request(
            &chrome,
            &WindowMaximizeRequestEventSnapshot {
                maximized: true,
                source: WindowStateRequestSourceSnapshot::ClientCsd,
                timestamp: 150,
            },
            150,
        )
        .expect("maximize request should evaluate");
    }
    s.evaluate_window_preview(&chrome, 200)
        .expect("chrome preview should evaluate");
    let chrome_focused = named_window("0xb", "google-chrome", true, second_window_maximized);
    let chrome_result = s
        .evaluate_window(&chrome_focused, 300)
        .expect("chrome evaluation should succeed");

    let editor_unfocused = named_window("0xa", "org.gnome.TextEditor", false, false);
    let editor_result = s
        .evaluate_window(&editor_unfocused, 400)
        .expect("editor re-evaluation should succeed");

    let chrome_z = chrome_result
        .managed_window
        .z_index
        .expect("chrome should have a z index");
    let editor_z = editor_result
        .managed_window
        .z_index
        .expect("editor should have a z index");
    (editor_z, chrome_z)
}

#[test]
fn plain_second_window_launches_above_existing_window() {
    let (editor_z, chrome_z) = launch_scenario_z_indices(false, false);
    assert!(
        chrome_z > editor_z,
        "second (plain) window should stack above: editor={editor_z} chrome={chrome_z}"
    );
}

#[test]
fn maximized_second_window_launches_above_existing_window() {
    let (editor_z, chrome_z) = launch_scenario_z_indices(true, false);
    assert!(
        chrome_z > editor_z,
        "second (maximized) window should stack above: editor={editor_z} chrome={chrome_z}"
    );
}

#[test]
fn plain_second_window_launches_above_existing_window_tiled() {
    let (editor_z, chrome_z) = launch_scenario_z_indices(false, true);
    assert!(
        chrome_z > editor_z,
        "second (plain, tiled ws) window should stack above: editor={editor_z} chrome={chrome_z}"
    );
}

#[test]
fn maximized_second_window_launches_above_existing_window_tiled() {
    let (editor_z, chrome_z) = launch_scenario_z_indices(true, true);
    assert!(
        chrome_z > editor_z,
        "second (maximized, tiled ws) window should stack above: editor={editor_z} chrome={chrome_z}"
    );
}

fn activate_toggle_fixture(
    focused_at_activate: bool,
    source: WindowActivateRequestSourceSnapshot,
) -> Vec<RuntimeWindowAction> {
    let mut s = session(false);

    let window = named_window("0xa", "kitty-float", false, false);
    s.evaluate_window_preview(&window, 0)
        .expect("preview should evaluate");
    let focused = named_window("0xa", "kitty-float", true, false);
    s.evaluate_window(&focused, 100)
        .expect("evaluation should succeed");
    let at_activate = named_window("0xa", "kitty-float", focused_at_activate, false);
    if !focused_at_activate {
        s.evaluate_window(&at_activate, 150)
            .expect("defocus evaluation should succeed");
    }

    s.window_activate_request(
        &at_activate,
        &WindowActivateRequestEventSnapshot {
            source,
            timestamp: 200,
        },
        200,
    )
    .expect("activate request should evaluate")
    .actions
}

#[test]
fn reactivating_focused_floating_window_requests_minimize() {
    let actions = activate_toggle_fixture(true, WindowActivateRequestSourceSnapshot::Api);
    assert!(
        has_action(&actions, "0xa", WaylandWindowAction::Minimize),
        "dock activation of the focused floating window should minimize it: {actions:?}"
    );
}

#[test]
fn xdg_activation_of_focused_window_does_not_minimize() {
    let actions = activate_toggle_fixture(true, WindowActivateRequestSourceSnapshot::XdgActivation);
    assert!(
        !has_action(&actions, "0xa", WaylandWindowAction::Minimize),
        "xdg-activation must never trigger the minimize toggle: {actions:?}"
    );
}

#[test]
fn activating_unfocused_floating_window_focuses_it() {
    let actions = activate_toggle_fixture(false, WindowActivateRequestSourceSnapshot::Api);
    assert!(
        !has_action(&actions, "0xa", WaylandWindowAction::Minimize),
        "activating an unfocused window must not minimize it: {actions:?}"
    );
    assert!(
        has_action(&actions, "0xa", WaylandWindowAction::Focus),
        "activating an unfocused window should focus it: {actions:?}"
    );
}

/// A dock that focuses the app it just launched (noctalia's dock arms a
/// pending-launch-focus and activates as soon as a matching toplevel
/// appears) sends `activate` before the client has committed a single
/// buffer — the foreign-toplevel handle exists from `xdg_toplevel`
/// creation. Only preview evaluations have run at that point, so the
/// window is focused but has never presented. Treating that hand-off as a
/// taskbar re-click minimized apps straight into the taskbar on launch
/// (issue #68), visible only for maximized windows because those skip the
/// deferred initial layout and so already belong to a workspace.
#[test]
fn launch_focus_activation_before_first_commit_does_not_minimize() {
    let mut s = session(false);

    // Maximized launch: preview evaluations only — no `evaluate_window`,
    // so `onFirstCommit` has not fired yet.
    let opening = named_window("0xb", "google-chrome", false, true);
    s.evaluate_window_preview(&opening, 0)
        .expect("preview should evaluate");
    let opening_focused = named_window("0xb", "google-chrome", true, true);
    s.evaluate_window_preview(&opening_focused, 100)
        .expect("focused preview should evaluate");

    let actions = s
        .window_activate_request(
            &opening_focused,
            &WindowActivateRequestEventSnapshot {
                source: WindowActivateRequestSourceSnapshot::Api,
                timestamp: 150,
            },
            150,
        )
        .expect("activate request should evaluate")
        .actions;

    assert!(
        !has_action(&actions, "0xb", WaylandWindowAction::Minimize),
        "a dock focusing the window it just launched must not minimize it: {actions:?}"
    );
}

#[test]
fn reactivating_toggled_minimized_window_restores_it() {
    // The noctalia regression: minimize via the dock toggle, then click
    // the icon again while the (hidden) window still holds keyboard
    // focus. The second activation must restore the window, not bounce
    // it back into minimized.
    let mut s = session(false);

    let window = named_window("0xa", "kitty-float", false, false);
    s.evaluate_window_preview(&window, 0)
        .expect("preview should evaluate");
    let focused = named_window("0xa", "kitty-float", true, false);
    s.evaluate_window(&focused, 100)
        .expect("evaluation should succeed");

    let event = WindowActivateRequestEventSnapshot {
        source: WindowActivateRequestSourceSnapshot::Api,
        timestamp: 200,
    };
    let first = s
        .window_activate_request(&focused, &event, 200)
        .expect("first activate should evaluate");
    assert!(
        has_action(&first.actions, "0xa", WaylandWindowAction::Minimize),
        "first activation should toggle the focused window into minimize: {:?}",
        first.actions
    );

    // Mirror `apply_runtime_window_actions`: the queued `window.minimize()`
    // action round-trips through the compositor as a minimize request,
    // which is what flips the window's minimized state in the config.
    s.window_minimize_request(
        &focused,
        &WindowMinimizeRequestEventSnapshot {
            minimized: true,
            source: WindowStateRequestSourceSnapshot::Api,
            timestamp: 250,
        },
        250,
    )
    .expect("minimize request should evaluate");

    // No focus change is delivered in between — the focused snapshot is
    // intentionally stale, mirroring the live race.
    let second = s
        .window_activate_request(&focused, &event, 300)
        .expect("second activate should evaluate");
    assert!(
        !has_action(&second.actions, "0xa", WaylandWindowAction::Minimize),
        "re-activating the minimized window must not re-minimize it: {:?}",
        second.actions
    );
    assert!(
        has_action(&second.actions, "0xa", WaylandWindowAction::Focus),
        "re-activating the minimized window should restore and focus it: {:?}",
        second.actions
    );
}

/// The sfwbar regression: its taskbar click sends `unset_minimized` and
/// `activate` as separate requests in one flush. The unset_minimized
/// restores the window before the activate handler runs, so `wasMinimized`
/// no longer shields the minimize-raise toggle — and since focus never
/// leaves a minimized window, the toggle read the activate as a re-click
/// of a visible focused window and bounced it straight back into
/// minimized (a one-frame flash). A restore and an activate this close
/// together are one gesture and must never toggle.
#[test]
fn restore_then_activate_in_one_gesture_does_not_reminimize() {
    let mut s = session(false);

    let window = named_window("0xa", "kitty-float", false, false);
    s.evaluate_window_preview(&window, 0)
        .expect("preview should evaluate");
    let focused = named_window("0xa", "kitty-float", true, false);
    s.evaluate_window(&focused, 100)
        .expect("evaluation should succeed");

    // Minimize from the taskbar; the window keeps keyboard focus.
    s.window_minimize_request(
        &focused,
        &WindowMinimizeRequestEventSnapshot {
            minimized: true,
            source: WindowStateRequestSourceSnapshot::Api,
            timestamp: 200,
        },
        200,
    )
    .expect("minimize request should evaluate");

    // sfwbar's click: unset_minimized (twice, in fact), then activate.
    for timestamp in [300, 301] {
        s.window_minimize_request(
            &focused,
            &WindowMinimizeRequestEventSnapshot {
                minimized: false,
                source: WindowStateRequestSourceSnapshot::Api,
                timestamp,
            },
            timestamp,
        )
        .expect("restore request should evaluate");
    }
    let activate = s
        .window_activate_request(
            &focused,
            &WindowActivateRequestEventSnapshot {
                source: WindowActivateRequestSourceSnapshot::Api,
                timestamp: 302,
            },
            302,
        )
        .expect("activate request should evaluate");

    assert!(
        !has_action(&activate.actions, "0xa", WaylandWindowAction::Minimize),
        "a restore+activate taskbar click must not re-minimize the window: {:?}",
        activate.actions
    );
    assert!(
        has_action(&activate.actions, "0xa", WaylandWindowAction::Focus),
        "the restored window should be focused: {:?}",
        activate.actions
    );
}

/// Super+Left/Right on a tiled workspace: when the focused tile sticks out
/// of the viewport on the side the key is heading, the press pans the tile
/// fully into view; only a fully-visible tile advances focus to the
/// neighbor. Repro: resize the middle of three tiles wider than the
/// screen — it ends left-aligned (the resize-end `scrollToWindow` flips a
/// wider-than-viewport tile to its left edge), overflowing to the right —
/// then press right: the old behavior jumped straight to the neighbor.
#[test]
fn focus_key_pans_overflowing_tile_into_view_before_advancing() {
    let mut s = session(true);

    let mut now = s.open_in_turn(
        "kitty",
        &[("0xa", false), ("0xb", false), ("0xc", false)],
        0,
    );
    // Focus the middle tile.
    s.evaluate_window(&named_window("0xc", "kitty", false, false), now)
        .expect("defocus evaluation should succeed");
    s.evaluate_window(&named_window("0xb", "kitty", true, false), now + 50)
        .expect("focus evaluation should succeed");
    now += 100;

    // Interactively resize the middle tile wider than the 1920px viewport.
    // `resizeTile` right-aligns the tile afterwards, so it overflows the
    // viewport on the left.
    let rect = |width: f64| WindowPositionSnapshot {
        x: 0.0,
        y: 0.0,
        width,
        height: 600.0,
    };
    for (phase, width) in [
        (WindowResizePhaseSnapshot::Start, 800.0),
        (WindowResizePhaseSnapshot::Update, 2400.0),
        (WindowResizePhaseSnapshot::End, 2400.0),
    ] {
        let resize = WindowResizeEventSnapshot {
            source: WindowResizeSourceSnapshot::Ssd,
            phase,
            edges: WindowResizeEdgesSnapshot {
                left: false,
                right: true,
                top: false,
                bottom: false,
            },
            start_pointer: WindowResizePointSnapshot { x: 800.0, y: 300.0 },
            current_pointer: WindowResizePointSnapshot { x: width, y: 300.0 },
            delta: WindowResizePointSnapshot {
                x: width - 800.0,
                y: 0.0,
            },
            start_rect: rect(800.0),
            current_rect: rect(width),
            output_name: Some("TEST-1".into()),
            timestamp: now,
        };
        s.window_resize("0xb", &resize, now)
            .expect("resize should evaluate");
        now += 10;
    }

    // First press: the tile overflows right, so the key pans it into view
    // and focus must stay on the same window.
    let first = s
        .invoke_key_binding("tile-focus-right-quick", now)
        .expect("first focus-right should evaluate");
    assert!(
        first.invoked,
        "tile-focus-right-quick should be a known binding"
    );
    assert!(
        !has_action(&first.actions, "0xc", WaylandWindowAction::Focus),
        "an overflowing tile must be panned into view, not skipped: {:?}",
        first.actions
    );
    assert!(
        has_action(&first.actions, "0xb", WaylandWindowAction::Focus),
        "the overflowing tile should keep focus while panning: {:?}",
        first.actions
    );

    // Second press: the tile's right edge is now flush with the viewport,
    // so focus advances to the neighbor.
    let second = s
        .invoke_key_binding("tile-focus-right-quick", now + 100)
        .expect("second focus-right should evaluate");
    assert!(
        has_action(&second.actions, "0xc", WaylandWindowAction::Focus),
        "a fully-visible tile should advance focus to the neighbor: {:?}",
        second.actions
    );
}

/// Maximized tiles are wider than the inset tile viewport by design
/// (MAXIMIZED_WINDOW_PADDING 8 < TILE_MARGIN 12), so when centered they
/// poke 4px past the viewport on both sides while being fully on screen.
/// Measuring the focus-key overflow against the inset viewport burned the
/// first key press on that invisible 4px pan — every focus move between
/// maximized tiles needed two presses. Fully-visible tiles must advance
/// on the first press.
#[test]
fn focus_key_advances_from_fully_visible_maximized_tile_on_first_press() {
    let mut s = session(true);
    let now = s.open_in_turn("kitty", &[("0xa", true), ("0xb", true), ("0xc", true)], 0);

    // Focus sits on 0xc, centered by the maximized scrollToWindow branch.
    // Each left press must advance immediately: 0xc → 0xb → 0xa.
    let first = s
        .invoke_key_binding("tile-focus-left-quick", now)
        .expect("first focus-left should evaluate");
    assert!(
        has_action(&first.actions, "0xb", WaylandWindowAction::Focus),
        "a fully-visible maximized tile must advance on the first press: {:?}",
        first.actions
    );

    let second = s
        .invoke_key_binding("tile-focus-left-quick", now + 100)
        .expect("second focus-left should evaluate");
    assert!(
        has_action(&second.actions, "0xa", WaylandWindowAction::Focus),
        "every subsequent press must advance one tile as well: {:?}",
        second.actions
    );
}

/// These tests assert where a tile settles, which depends on the config's
/// tile margin, gap and window chrome. Those are the config's to choose, so
/// a literal here only tests the config that happened to ship.
#[derive(Clone, Copy, Debug)]
struct TileMetrics {
    /// Left inset of the tile viewport: where a tile sits at scroll 0.
    margin: f64,
    /// Width of a tile holding an 800px client.
    width: f64,
    /// Space between adjacent tiles.
    gap: f64,
}

impl TileMetrics {
    fn viewport_width(self) -> f64 {
        1920.0 - self.margin * 2.0
    }

    /// Distance from one tile's left edge to the next one's.
    fn pitch(self) -> f64 {
        self.width + self.gap
    }

    /// A tile flush against the left edge of the viewport.
    fn flush_left_x(self) -> f64 {
        self.margin
    }

    /// A tile flush against the right edge of the viewport.
    fn flush_right_x(self) -> f64 {
        self.margin + self.viewport_width() - self.width
    }

    /// A tile of `width` centered in the viewport.
    fn centered_x(self, width: f64) -> f64 {
        self.margin + (self.viewport_width() - width) / 2.0
    }
}

/// Open one tile in a fresh runtime and read the geometry back: a single
/// tile is narrower than the viewport, so it sits unscrolled at the
/// viewport's left edge. A second tile then gives the gap.
fn probe_tile_metrics() -> TileMetrics {
    let mut s = session(true);
    s.open_in_turn("kitty", &[("0x1", false)], 0);
    let rect = s.managed_rect("0x1", 100);

    // Measured against the first tile at the same instant, well after any
    // open animation, so the gap holds even if the second tile scrolls
    // the strip.
    s.evaluate_window_preview(&named_window("0x2", "kitty", false, false), 200)
        .expect("preview should evaluate");
    s.evaluate_window(&named_window("0x1", "kitty", false, false), 225)
        .expect("defocus evaluation should succeed");
    s.evaluate_window(&named_window("0x2", "kitty", true, false), 250)
        .expect("evaluation should succeed");
    let first = s.managed_rect("0x1", 5_000).x;
    let second = s.managed_rect("0x2", 5_000).x;

    TileMetrics {
        margin: rect.x,
        width: rect.width,
        gap: second - first - rect.width,
    }
}

/// Probed once per test binary: the geometry cannot change between tests.
fn tile_metrics() -> TileMetrics {
    static METRICS: OnceLock<TileMetrics> = OnceLock::new();
    // A thread runs one config at a time, and the calling test's is live.
    *METRICS.get_or_init(|| {
        std::thread::spawn(probe_tile_metrics)
            .join()
            .expect("the tile geometry probe should succeed")
    })
}

/// Three-finger workspace scrolling catches on tile snap positions (the
/// offsets where a tile is fully on screen at the viewport edge) when the
/// gesture moves at or below workspaceScrollSnapMaxVelocity, holds the
/// catch until the finger travels workspaceScrollSnapBreakoutPx further,
/// then continues to the next snap position — while a fast gesture passes
/// straight through.
#[test]
fn workspace_scroll_gesture_snaps_to_tile_edges_at_low_speed() {
    use GestureSwipePhaseSnapshot::{Begin, End, Update};

    let mut s = session(true);

    // Four tiles; opening 0xd last scrolls the strip to its maximum, 0xd
    // flush at the viewport right edge. The scroll offsets this test
    // crosses, in strip coordinates, follow from the config's tile
    // geometry, so they are derived rather than written in.
    let metrics = tile_metrics();
    let viewport = metrics.viewport_width();
    let max_scroll = 3.0 * metrics.pitch() + metrics.width - viewport;
    // Snap offset with 0xb flush at the viewport left edge.
    let b_flush_left = metrics.pitch();
    // Snap offset with 0xc flush at the viewport right edge.
    let c_flush_right = 2.0 * metrics.pitch() + metrics.width - viewport;
    assert!(
        0.0 < c_flush_right && c_flush_right < b_flush_left && b_flush_left < max_scroll,
        "the scenario needs a scrolled strip with the 0xc snap below the 0xb one: \
         {c_flush_right} < {b_flush_left} < {max_scroll} for {metrics:?}"
    );
    let mut now = s.open_in_turn(
        "kitty",
        &[
            ("0xa", false),
            ("0xb", false),
            ("0xc", false),
            ("0xd", false),
        ],
        0,
    );

    // The config maps scroll delta as -delta_x * 1.5 and compares
    // -velocity_x * 1.5 against the 300 px/s snap threshold.
    const SLOW_STEP: f64 = 30.0; // delta_x 20 at 150 px/s: catchable
    const FAST_STEP: f64 = 60.0; // delta_x -40 at 3000 px/s: too fast to catch
    const BREAKOUT: f64 = 48.0; // the config's workspaceScrollSnapBreakoutPx

    // Slow drag towards lower offsets: 30px of scroll per event at
    // 150 px/s. Crossing the 0xb snap offset must catch and hold there,
    // tile 0xb exactly at the viewport left edge. The crossing event is
    // caught on the offset itself; its overshoot is dropped, not carried.
    s.swipe(Begin, 0.0, 0.0, now);
    let slow_updates = ((max_scroll - b_flush_left) / SLOW_STEP).ceil() as usize;
    for _ in 0..slow_updates {
        now += 10;
        s.swipe(Update, 20.0, 100.0, now);
    }
    assert_eq!(
        s.managed_rect("0xb", now + 1).x,
        metrics.flush_left_x(),
        "slow scroll should catch with tile 0xb flush at the viewport left edge"
    );

    // One more event stays within the 48px breakout: still caught.
    now += 10;
    s.swipe(Update, 20.0, 100.0, now);
    assert_eq!(
        s.managed_rect("0xb", now + 1).x,
        metrics.flush_left_x(),
        "movement within the breakout distance must not move the caught scroll"
    );

    // Keep dragging: the accumulated travel exceeds the breakout, the
    // catch releases, and the scroll then catches the next snap offset,
    // where tile 0xc is flush at the viewport right edge.
    // The first event exceeds the breakout and releases with the excess
    // (two steps of travel less the breakout); the rest scroll on until
    // the 0xc snap offset is crossed and caught.
    let release_excess = 2.0 * SLOW_STEP - BREAKOUT;
    let updates_to_c =
        1 + ((b_flush_left - release_excess - c_flush_right) / SLOW_STEP).ceil() as usize;
    for _ in 0..updates_to_c {
        now += 10;
        s.swipe(Update, 20.0, 100.0, now);
    }
    assert_eq!(
        s.managed_rect("0xc", now + 1).x,
        metrics.flush_right_x(),
        "after breaking out the scroll should catch the next snap position \
         (0xc flush at the viewport right edge)"
    );

    // Lift while caught: no kinetic glide, the catch holds.
    now += 10;
    s.swipe(End, 0.0, -100.0, now);
    assert_eq!(
        s.managed_rect("0xc", now + 1).x,
        metrics.flush_right_x(),
        "lifting the fingers while caught must stay on the snap position"
    );

    // Fast drag back up: crossing the 0xb snap offset at 3000 px/s must
    // pass straight through (0xb ends past the viewport edge, not flush).
    // Just enough events to carry the scroll past it.
    let fast_updates = ((b_flush_left - c_flush_right) / FAST_STEP).floor() as usize + 1;
    now += 10;
    s.swipe(Begin, 0.0, 0.0, now);
    for _ in 0..fast_updates {
        now += 10;
        s.swipe(Update, -40.0, -2000.0, now);
    }
    assert_eq!(
        s.managed_rect("0xb", now + 1).x,
        metrics.flush_left_x() + b_flush_left - (c_flush_right + fast_updates as f64 * FAST_STEP),
        "a fast scroll must pass through the snap position without catching"
    );
}

/// Frame-driven kinetic scrolling: ticks stamped with fractional frame
/// times (the compositor's predicted presentation times) move the glide by
/// exactly one frame's worth each, even with wall-clock requests that run
/// behind them in between. Whole-millisecond ticks used to step 16, 16, 17
/// ms and the timer landed them anywhere between two frames.
#[test]
fn workspace_kinetic_scroll_steps_once_per_fractional_frame_tick() {
    use GestureSwipePhaseSnapshot::{Begin, End, Update};

    let mut s = session(true);
    let mut now = s.open_in_turn(
        "kitty",
        &[
            ("0xa", false),
            ("0xb", false),
            ("0xc", false),
            ("0xd", false),
        ],
        0,
    );

    const RELEASE_VELOCITY: f64 = 1500.0;
    s.swipe(Begin, 0.0, 0.0, now);
    for _ in 0..4 {
        now += 10;
        s.swipe(Update, 15.0, RELEASE_VELOCITY, now);
    }
    now += 10;
    // The workspace starts scrolled to its end, so the glide runs back
    // toward the start: the tiles move right.
    s.swipe(End, 0.0, RELEASE_VELOCITY, now);

    // The test output runs at 60 Hz: one frame is 16.67 ms, which no whole
    // millisecond interval matches.
    let frame_ms = 1000.0 / 60.0;
    let time_constant_ms = 360.0;
    let release_ms = now as f64;
    let mut last_x = s.managed_rect("0xa", now).x;
    for frame in 1..=15 {
        let frame_time = release_ms + frame as f64 * frame_ms;
        // A wall-clock request from just before this frame's presentation
        // time; it must not pull the scheduler clock back.
        s.managed_rect("0xa", frame_time.floor() as u64 - 4);
        s.scheduler_tick(frame_time)
            .expect("scheduler tick should evaluate");
        let x = s.managed_rect("0xa", frame_time.floor() as u64).x;
        let velocity = RELEASE_VELOCITY * (-(frame as f64) * frame_ms / time_constant_ms).exp();
        let expected_step = velocity * frame_ms / 1000.0;
        let step = x - last_x;
        assert!(
            (step - expected_step).abs() <= 1.01,
            "frame {frame}: the glide moved {step} px, one frame at this \
             velocity is {expected_step} px"
        );
        last_x = x;
    }
}

/// Kinetic settle, non-maximized anchor leaning the other way: the
/// center-closest tile snaps flush to the LEFT edge when it sits left of
/// the screen center.
#[test]
fn workspace_kinetic_scroll_snaps_center_window_flush_to_leaning_left_edge() {
    use GestureSwipePhaseSnapshot::{Begin, End, Update};

    let mut s = session(true);
    let mut now = s.open_in_turn(
        "kitty",
        &[
            ("0xa", false),
            ("0xb", false),
            ("0xc", false),
            ("0xd", false),
        ],
        0,
    );

    // Drag to scroll ~516 and release at 150 px/s so the settle engages
    // at the release position. There the screen center (~1462) is
    // closest to 0xb's center (1218); 0xb leans left of it, so it must
    // snap flush to the viewport left edge (scroll 816).
    s.swipe(Begin, 0.0, 0.0, now);
    for _ in 0..28 {
        now += 10;
        s.swipe(Update, 20.0, 2000.0, now);
    }
    now += 10;
    s.swipe(End, 0.0, 150.0, now);
    s.tick_for(&mut now, 250, 8);

    assert_eq!(
        s.managed_rect("0xb", now + 1).x,
        tile_metrics().flush_left_x(),
        "the center-closest tile must snap flush to the edge it leans \
         toward (0xb at the viewport left edge)"
    );
}

/// A maximized tile hanging off one edge must not yank a smaller
/// neighbor that already sits fully on screen out of view: the neighbor
/// is closer to the screen center, so it is the anchor, and its
/// leaning-edge position is exactly where the scroll already rests.
#[test]
fn workspace_kinetic_scroll_never_yanks_fully_visible_tile_for_maximized_neighbor() {
    use GestureSwipePhaseSnapshot::{Begin, End, Update};

    let mut s = session(true);
    // 0xa is maximized (1904px), 0xb a normal 804px tile to its right.
    let mut now = s.open_in_turn("kitty", &[("0xa", true), ("0xb", false)], 0);

    // Opening 0xb scrolled it fully into view at the right end (scroll
    // 824, flush at the viewport right edge); the maximized 0xa pokes off
    // the left edge at ~57% visible. 0xb's center is nearer the screen
    // center, so a slow flick further rightwards anchors on 0xb, whose
    // leaning-edge target is the current position — NOT 0xa's center,
    // which would push 0xb completely off screen.
    s.swipe(Begin, 0.0, 0.0, now);
    for _ in 0..3 {
        now += 10;
        s.swipe(Update, -20.0, -2000.0, now);
    }
    now += 10;
    s.swipe(End, 0.0, -150.0, now);
    s.tick_for(&mut now, 250, 8);

    assert_eq!(
        s.managed_rect("0xb", now + 1).x,
        tile_metrics().flush_right_x(),
        "the fully-visible tile must stay on screen (flush at the \
         viewport right edge), not be yanked away to center the cut \
         maximized neighbor"
    );
}

/// Maximized tiles settle on their center once the glide decays below
/// the snap threshold.
#[test]
fn workspace_kinetic_scroll_settles_maximized_tile_at_center() {
    use GestureSwipePhaseSnapshot::{Begin, End, Update};

    let mut s = session(true);
    let mut now = s.open_in_turn("kitty", &[("0xa", true), ("0xb", true), ("0xc", true)], 0);

    // Three maximized tiles, 1904px wide with centers 1916px apart:
    // 0xb spans [1916, 3820] and is centered at scroll offset 1920.
    // Read the current scroll from 0xb's on-screen position and release
    // a flick whose natural landing point (start - v * 360ms) falls just
    // past 0xb's center, so the settle must center 0xb (x = the 8px
    // maximized padding).
    let scroll_start = 1928.0 - s.managed_rect("0xb", now).x;
    // Three fast updates below scroll the workspace by 90px first.
    let velocity = (scroll_start - 90.0 - 1930.0) / 0.36;
    assert!(
        (120.0..=5000.0).contains(&velocity),
        "flick velocity {velocity} out of kinetic range; adjust the setup"
    );

    s.swipe(Begin, 0.0, 0.0, now);
    for _ in 0..3 {
        now += 10;
        s.swipe(Update, 20.0, 2000.0, now);
    }
    now += 10;
    s.swipe(End, 0.0, velocity, now);
    s.tick_for(&mut now, 250, 8);

    let rect = s.managed_rect("0xb", now + 1);
    assert_eq!(
        rect.x,
        tile_metrics().centered_x(rect.width),
        "the maximized tile must settle centered on screen"
    );
}

/// Kinetic settle, non-maximized anchor: the tile whose center is
/// closest to the screen center is the anchor, and it snaps flush to the
/// screen edge on the side it leans toward — here the right edge.
#[test]
fn workspace_kinetic_scroll_snaps_center_window_flush_to_leaning_right_edge() {
    use GestureSwipePhaseSnapshot::{Begin, End, Update};

    let mut s = session(true);
    let mut now = s.open_in_turn(
        "kitty",
        &[
            ("0xa", false),
            ("0xb", false),
            ("0xc", false),
            ("0xd", false),
        ],
        0,
    );

    // Drag to scroll ~996 and release at 150 px/s — below any realistic
    // snap threshold, so the settle engages right at the release
    // position. There the screen center (~1942) is closest to 0xc's
    // center (2034); 0xc leans right of it, so it must snap flush to the
    // viewport right edge (scroll 540).
    s.swipe(Begin, 0.0, 0.0, now);
    for _ in 0..12 {
        now += 10;
        s.swipe(Update, 20.0, 2000.0, now);
    }
    now += 10;
    s.swipe(End, 0.0, 150.0, now);
    s.tick_for(&mut now, 250, 8);

    assert_eq!(
        s.managed_rect("0xc", now + 1).x,
        tile_metrics().flush_right_x(),
        "the center-closest tile must snap flush to the edge it leans \
         toward (0xc at the viewport right edge)"
    );
}

/// Each session's IPC socket is its own, so unlike the TypeScript test this
/// needs no scrubbed environment and runs with the rest.
#[test]
fn real_config_sends_window_rects_only_to_lease_holders() {
    let mut s = session(false);
    let window = named_window("1", "kitty", true, false);
    s.evaluate_window(&window, 1)
        .expect("window should evaluate");

    let has_rects = |lines: &[Value]| {
        lines
            .iter()
            .any(|line| line.get("event").and_then(Value::as_str) == Some("windows.rects"))
    };

    let mut holder = Client::connect(&s.socket);
    let mut bystander = Client::connect(&s.socket);
    holder.send(
        json!({ "id": 1, "method": "workspaces.get", "params": { "rectsLease": "test-lease" } }),
    );
    bystander.send(json!({ "id": 1, "method": "workspaces.get" }));
    holder.drain(&mut s);
    bystander.drain(&mut s);

    let point = WindowResizePointSnapshot { x: 10.0, y: 20.0 };
    let move_event = |timestamp: u64| WindowMoveEventSnapshot {
        source: WindowMoveSourceSnapshot::Modifier,
        phase: WindowMovePhaseSnapshot::Update,
        start_pointer: point,
        current_pointer: WindowResizePointSnapshot { x: 30.0, y: 40.0 },
        delta: WindowResizePointSnapshot { x: 20.0, y: 20.0 },
        start_rect: window.rect,
        current_rect: window.rect,
        output_name: Some("output-1".into()),
        modifiers: PointerModifierStateSnapshot {
            logo: true,
            alt: false,
            ctrl: false,
            shift: false,
        },
        timestamp,
    };
    s.window_move(&window.id, &move_event(2), 2)
        .expect("window move should complete");
    assert!(
        has_rects(&holder.drain(&mut s)),
        "the lease holder should get rects"
    );
    assert!(
        !has_rects(&bystander.drain(&mut s)),
        "a client without a lease should not get rects"
    );

    // The lease lapses 2 s after the last renewal.
    std::thread::sleep(Duration::from_millis(2300));
    s.window_move(&window.id, &move_event(3), 3)
        .expect("window move should complete");
    assert!(
        !has_rects(&holder.drain(&mut s)),
        "an expired lease should get no rects"
    );
}

// Reads $HOME/.config/minka-settings.json through the config, so it needs a
// scratch HOME. Build with `cargo test --no-run`, then run the test binary
// directly:
//   env -u WAYLAND_DISPLAY HOME=<scratch>/home \
//     <test binary> real_config_virtual_desktops_off --ignored --test-threads=1
#[test]
#[ignore = "needs a scratch HOME; run alone with --ignored"]
fn real_config_virtual_desktops_off_folds_and_releases_keys() {
    let home = PathBuf::from(std::env::var("HOME").expect("HOME should be set"));
    let settings_path = home.join(".config/minka-settings.json");
    // Never overwrite a real settings file, only this test's own fixture.
    if let Ok(existing) = std::fs::read_to_string(&settings_path)
        && !existing.contains("__shojiTestFixture")
    {
        eprintln!("skipping: {settings_path:?} is not a test fixture");
        return;
    }
    std::fs::create_dir_all(settings_path.parent().expect("settings dir"))
        .expect("settings dir should be created");
    let fixture = |enabled: bool| {
        json!({
            "__shojiTestFixture": true,
            "input": {
                "pointerAccel": 0.4, "accelProfile": "adaptive", "naturalScroll": false,
                "touchpad": {
                    "naturalScroll": false, "tapToClick": true, "scrollMethod": "twoFinger",
                    "scrollFactor": 1, "disableWhileTyping": false
                },
                "keyboard": { "layout": "us", "variant": "" }
            },
            "displays": {},
            "workspaces": { "enabled": enabled }
        })
    };
    std::fs::write(&settings_path, fixture(true).to_string()).expect("fixture should be written");
    const DESKTOP_KEYS: [&str; 4] = [
        "window-move-workspace-prev",
        "window-move-workspace-next",
        "workspace-prev",
        "workspace-next",
    ];
    let bound = |update: Option<RuntimeKeyBindingConfigUpdate>| -> Vec<String> {
        update
            .expect("the runtime should have published the binding set")
            .entries
            .into_iter()
            .map(|entry| entry.id)
            .collect()
    };
    let has_desktop_keys = |ids: &[String]| {
        DESKTOP_KEYS
            .iter()
            .all(|key| ids.iter().any(|id| id == key))
    };
    let no_desktop_keys = |ids: &[String]| {
        DESKTOP_KEYS
            .iter()
            .all(|key| !ids.iter().any(|id| id == key))
    };

    // TEST-1's desktops as (index, window ids).
    let desktops = |view: &Value| -> Vec<(u64, Vec<String>)> {
        view["monitors"]
            .as_array()
            .expect("monitors")
            .iter()
            .filter(|monitor| monitor["name"] == "TEST-1")
            .flat_map(|monitor| monitor["workspaces"].as_array().expect("workspaces").iter())
            .map(|workspace| {
                let ids = workspace["windows"]
                    .as_array()
                    .expect("windows")
                    .iter()
                    .map(|window| window["id"].as_str().expect("id").to_owned())
                    .collect();
                (workspace["index"].as_u64().expect("index"), ids)
            })
            .collect()
    };
    let open = |s: &mut Session, id: &str, now: u64| {
        s.evaluate_window_preview(&named_window(id, "kitty", false, false), now)
            .expect("preview should evaluate");
        s.evaluate_window(&named_window(id, "kitty", true, false), now + 10)
            .expect("window should evaluate");
    };

    // Desktops on: a window on desktop 1 and one on desktop 2.
    let mut s = session(false);
    assert!(has_desktop_keys(&bound(s.published_key_bindings())));
    open(&mut s, "0xa", 100);
    assert!(
        s.invoke_key_binding("workspace-next", 200)
            .expect("desktop key should run")
            .invoked
    );
    open(&mut s, "0xb", 300);
    let mut ipc = Client::connect(&s.socket);
    let before = desktops(&ipc.request(&mut s, "workspaces.get", json!({})));
    assert!(
        before
            .iter()
            .any(|(index, ids)| *index == 2 && ids == &["0xb"]),
        "0xb should open on desktop 2: {before:?}"
    );

    // Off, live, as MinkaConf does it (save, then apply): one desktop
    // holding both windows, and the keys released.
    std::fs::write(&settings_path, fixture(false).to_string()).expect("fixture should be written");
    assert_eq!(
        ipc.request(&mut s, "settings.apply", fixture(false)),
        json!({ "ok": true })
    );
    let folded = desktops(&ipc.request(&mut s, "workspaces.get", json!({})));
    assert_eq!(folded.len(), 1, "one desktop after folding: {folded:?}");
    assert_eq!(folded[0].0, 1);
    let mut ids = folded[0].1.clone();
    ids.sort();
    assert_eq!(ids, ["0xa", "0xb"], "no window lost: {folded:?}");
    s.scheduler_tick(400.0).expect("tick should succeed");
    assert!(no_desktop_keys(&bound(s.published_key_bindings())));
    assert!(
        !s.invoke_key_binding("workspace-next", 410)
            .expect("an unbound key should not fail")
            .invoked
    );
    ipc.request(&mut s, "workspaces.switch", json!({ "direction": 1 }));
    ipc.request(
        &mut s,
        "workspaces.activate",
        json!({ "monitor": "TEST-1", "index": 2 }),
    );
    let still = desktops(&ipc.request(&mut s, "workspaces.get", json!({})));
    assert!(
        still.iter().all(|(index, _)| *index == 1),
        "no way back to desktop 2: {still:?}"
    );

    // A restart with desktops off binds no desktop keys. (The TypeScript
    // test reloads, and also checks the persisted state holds one desktop;
    // this runtime persists nothing.)
    drop(ipc);
    drop(s);
    let mut s = session(false);
    assert!(no_desktop_keys(&bound(s.published_key_bindings())));

    // On again, live: the keys come back.
    let mut ipc = Client::connect(&s.socket);
    std::fs::write(&settings_path, fixture(true).to_string()).expect("fixture should be written");
    assert_eq!(
        ipc.request(&mut s, "settings.apply", fixture(true)),
        json!({ "ok": true })
    );
    s.scheduler_tick(600.0).expect("tick should succeed");
    assert!(has_desktop_keys(&bound(s.published_key_bindings())));

    drop(ipc);
    drop(s);
    let _ = std::fs::remove_file(&settings_path);
}
