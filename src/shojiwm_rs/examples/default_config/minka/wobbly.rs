//! Wobbly windows while dragging: a port of dcrey7's TypeScript
//! `jelly-drag.ts`, which is itself a line-for-line port of KWin's
//! wobblywindows effect (kwin/src/plugins/wobblywindows/wobblywindows.cpp),
//! with KDE's defaults. The shader (`packages/config/src/effect/
//! jelly-drag.frag`) is his, unchanged.
//!
//! A 4x4 grid of points sits on the window. Each point is joined to its
//! neighbours by springs that want to keep the grid spacing. The point you
//! grab is pulled towards its place on the window; the others only follow
//! their neighbours. So the window trails the pointer and wobbles to a
//! stop. After every step, speed and acceleration are averaged with the
//! eight points around each point: that keeps the wobble soft and clean.
//!
//! The jelly-drag shader bends the window's pixels with the grid. Nothing
//! runs while no window moves: the effect is only assigned while a drag
//! (and the settle after it) is live, and the poll cancels itself at rest.
//!
//! Two of dcrey7's helpers are not ported: his automatic quality gate
//! (this machine is known, and the config has no quality levels) and his
//! animation-activity signal that pauses his glass blur while animations
//! run. If the wobble ever drops frames against the liquid glass, that
//! second one is the knob to add.

use std::{cell::RefCell, collections::HashMap, rc::Rc};

use shojiwm_rs::{
    prelude::*,
    ssd::{WindowMovePhaseSnapshot, WindowPositionSnapshot},
};

const GRID: usize = 4;
const COUNT: usize = GRID * GRID;
/// KDE's "Wobbliness" slider, 0 (stiff, KDE default) to 4 (very wobbly).
/// Levels 3 and 4 stop with a small visible jump.
const WOBBLINESS: usize = 2;
/// KWin's parameter sets: stiffness, drag (speed kept after each step),
/// move factor.
const LEVELS: [[f64; 3]; 5] = [
    [0.15, 0.8, 0.1],
    [0.1, 0.85, 0.1],
    [0.06, 0.9, 0.1],
    [0.03, 0.92, 0.2],
    [0.01, 0.97, 0.25],
];
const STIFFNESS: f64 = LEVELS[WOBBLINESS][0];
const DRAG: f64 = LEVELS[WOBBLINESS][1];
const MOVE_FACTOR: f64 = LEVELS[WOBBLINESS][2];
const MIN_VELOCITY: f64 = 0.0;
const MAX_VELOCITY: f64 = 1000.0;
const STOP_VELOCITY: f64 = 0.5;
const MIN_ACCELERATION: f64 = 0.0;
const MAX_ACCELERATION: f64 = 1000.0;
const STOP_ACCELERATION: f64 = 0.5;
/// The physics step, in ms: one frame at 144 Hz. Every step is this long,
/// and the drawing blends the last two steps, so a late frame does not
/// make the window jump. (KWin steps up to 10 ms of whatever the frame
/// took.)
const STEP_MS: f64 = 1000.0 / 144.0;
/// The farthest the drawing may bend, in logical px. Also the effect margin.
const MAX_BEND_PX: f64 = 250.0;
/// Window opacity while dragged, like KDE's Translucency effect (80%).
const DRAG_OPACITY: f64 = 0.8;
/// How long the fade to and from DRAG_OPACITY takes, in ms (KDE: 250 ms).
const FADE_MS: f64 = 250.0;

type Rect = WindowPositionSnapshot;

/// The rest place of grid point `i` on `rect`.
fn origin_x(rect: Rect, i: usize) -> f64 {
    rect.x + (i % GRID) as f64 * rect.width / (GRID - 1) as f64
}
fn origin_y(rect: Rect, i: usize) -> f64 {
    rect.y + (i / GRID) as f64 * rect.height / (GRID - 1) as f64
}

/// Average every value with the up to eight points around it
/// (KWin: heightRingLinearMean over the "height ring").
fn ring_mean(x: &mut [f64; COUNT], y: &mut [f64; COUNT]) {
    let (mut out_x, mut out_y) = ([0.0; COUNT], [0.0; COUNT]);
    for i in 0..COUNT {
        let (col, row) = ((i % GRID) as i32, (i / GRID) as i32);
        let mut count = 0.0;
        let (mut sum_x, mut sum_y) = (0.0, 0.0);
        for dr in -1..=1 {
            for dc in -1..=1 {
                let (r, c) = (row + dr, col + dc);
                if (dr != 0 || dc != 0)
                    && (0..GRID as i32).contains(&r)
                    && (0..GRID as i32).contains(&c)
                {
                    let j = (r * GRID as i32 + c) as usize;
                    sum_x += x[j];
                    sum_y += y[j];
                    count += 1.0;
                }
            }
        }
        out_x[i] = (count * x[i] + sum_x) / (2.0 * count);
        out_y[i] = (count * y[i] + sum_y) / (2.0 * count);
    }
    *x = out_x;
    *y = out_y;
}

/// Zero values under `min`, cap values over `max` (KWin: fixVectorBounds).
fn bound(value: f64, min: f64, max: f64) -> f64 {
    let size = value.abs();
    if size < min {
        0.0
    } else if size > max {
        value.signum() * max
    } else {
        value
    }
}

/// The spring grid over one window, in absolute logical coordinates,
/// KWin style. Pure physics: nothing here knows about signals or windows,
/// which is what the tests pin.
#[derive(Debug, Clone)]
struct Grid {
    pos_x: [f64; COUNT],
    pos_y: [f64; COUNT],
    vel_x: [f64; COUNT],
    vel_y: [f64; COUNT],
    acc_x: [f64; COUNT],
    acc_y: [f64; COUNT],
    /// The positions one step earlier, to blend with the current ones.
    prev_x: [f64; COUNT],
    prev_y: [f64; COUNT],
    /// Points pulled towards their place on the window (the grab points).
    constraint: [bool; COUNT],
}

impl Default for Grid {
    fn default() -> Self {
        Self {
            pos_x: [0.0; COUNT],
            pos_y: [0.0; COUNT],
            vel_x: [0.0; COUNT],
            vel_y: [0.0; COUNT],
            acc_x: [0.0; COUNT],
            acc_y: [0.0; COUNT],
            prev_x: [0.0; COUNT],
            prev_y: [0.0; COUNT],
            constraint: [false; COUNT],
        }
    }
}

impl Grid {
    /// Put every point at its rest place, at rest (KWin: initWobblyInfo).
    fn reset(&mut self, rect: Rect) {
        for i in 0..COUNT {
            self.pos_x[i] = origin_x(rect, i);
            self.pos_y[i] = origin_y(rect, i);
            self.vel_x[i] = 0.0;
            self.vel_y[i] = 0.0;
            self.constraint[i] = false;
        }
        self.prev_x = self.pos_x;
        self.prev_y = self.pos_y;
    }

    /// One physics step of `time` ms (KWin: updateWindowWobblyDatas).
    /// Returns true while wobbling.
    fn step(&mut self, rect: Rect, time: f64) -> bool {
        let x_length = rect.width / (GRID - 1) as f64;
        let y_length = rect.height / (GRID - 1) as f64;

        for i in 0..COUNT {
            if self.constraint[i] {
                // A grab point: pulled towards its place on the window.
                self.acc_x[i] = (origin_x(rect, i) - self.pos_x[i]) * STIFFNESS;
                self.acc_y[i] = (origin_y(rect, i) - self.pos_y[i]) * STIFFNESS;
                continue;
            }
            // Springs to the neighbours, which want the grid spacing
            // between them.
            let (col, row) = (i % GRID, i / GRID);
            let mut springs = [(0usize, 0.0f64, 0.0f64); 4];
            let mut count = 0;
            if col > 0 {
                springs[count] = (i - 1, -1.0, 0.0);
                count += 1;
            }
            if col < GRID - 1 {
                springs[count] = (i + 1, 1.0, 0.0);
                count += 1;
            }
            if row > 0 {
                springs[count] = (i - GRID, 0.0, -1.0);
                count += 1;
            }
            if row < GRID - 1 {
                springs[count] = (i + GRID, 0.0, 1.0);
                count += 1;
            }
            let (mut ax, mut ay) = (0.0, 0.0);
            for &(j, dx, dy) in &springs[..count] {
                ax += (self.pos_x[j] - self.pos_x[i] - dx * x_length) * STIFFNESS;
                ay += (self.pos_y[j] - self.pos_y[i] - dy * y_length) * STIFFNESS;
            }
            self.acc_x[i] = ax / count as f64;
            self.acc_y[i] = ay / count as f64;
        }
        ring_mean(&mut self.acc_x, &mut self.acc_y);

        let mut acc_sum = 0.0;
        for i in 0..COUNT {
            let ax = bound(self.acc_x[i], MIN_ACCELERATION, MAX_ACCELERATION);
            let ay = bound(self.acc_y[i], MIN_ACCELERATION, MAX_ACCELERATION);
            self.vel_x[i] = ax * time + self.vel_x[i] * DRAG;
            self.vel_y[i] = ay * time + self.vel_y[i] * DRAG;
            acc_sum += ax.abs() + ay.abs();
        }
        ring_mean(&mut self.vel_x, &mut self.vel_y);

        let mut vel_sum = 0.0;
        for i in 0..COUNT {
            self.vel_x[i] = bound(self.vel_x[i], MIN_VELOCITY, MAX_VELOCITY);
            self.vel_y[i] = bound(self.vel_y[i], MIN_VELOCITY, MAX_VELOCITY);
            self.pos_x[i] += self.vel_x[i] * time * MOVE_FACTOR;
            self.pos_y[i] += self.vel_y[i] * time * MOVE_FACTOR;
            vel_sum += self.vel_x[i].abs() + self.vel_y[i].abs();
        }

        !(acc_sum < STOP_ACCELERATION && vel_sum < STOP_VELOCITY)
    }

    /// The bend of every point for the shader, packed two points per vec4
    /// (d0 = points 0 and 1, d1 = points 2 and 3, ...). `blend` (0 to 1) is
    /// how far the drawing is between the previous step and the current
    /// one; `still` draws the window flat, as KWin does at rest.
    fn bends(&self, rect: Rect, still: bool, blend: f64) -> [[f64; 4]; 8] {
        let (mut bend_x, mut bend_y) = ([0.0; COUNT], [0.0; COUNT]);
        let mut biggest = 0.0f64;
        if !still {
            for i in 0..COUNT {
                let x = self.prev_x[i] + (self.pos_x[i] - self.prev_x[i]) * blend;
                let y = self.prev_y[i] + (self.pos_y[i] - self.prev_y[i]) * blend;
                bend_x[i] = x - origin_x(rect, i);
                bend_y[i] = y - origin_y(rect, i);
                biggest = biggest.max(bend_x[i].hypot(bend_y[i]));
            }
        }
        // Keep the drawing inside the effect margin. Scale the whole bend,
        // so the shape stays smooth; the physics itself keeps the full bend.
        let scale = if biggest > MAX_BEND_PX {
            MAX_BEND_PX / biggest
        } else {
            1.0
        };
        let mut packed = [[0.0; 4]; 8];
        for i in 0..COUNT {
            packed[i / 2][(i % 2) * 2] = bend_x[i] * scale;
            packed[i / 2][(i % 2) * 2 + 1] = bend_y[i] * scale;
        }
        packed
    }
}

/// One window's wobble: the effect and its on/off switch on the outside
/// (the effect callback reads them without borrowing the physics), the
/// mutable drag state inside.
struct Wobble {
    active: Signal<bool>,
    effect: SurfaceEffect,
    inner: RefCell<Inner>,
}

struct Inner {
    window: Window,
    /// 8 vec4 bends for the shader, two grid points each.
    bend: [Signal<[f64; 4]>; 8],
    opacity: Signal<f64>,
    dragging: bool,
    /// The window rect from the last move event.
    drag_rect: Option<Rect>,
    grid: Grid,
    /// Frame time not yet used by a physics step, in ms.
    left_ms: f64,
    /// False once the springs came to rest.
    wobbling: bool,
    /// Set once the springs have reset for a fresh wobble.
    started: bool,
    poll: Option<PollHandle>,
    last_ms: Option<f64>,
}

type Wobbles = Rc<RefCell<HashMap<Window, Rc<Wobble>>>>;

fn wobble_for(wobbles: &Wobbles, window: Window) -> Rc<Wobble> {
    if let Some(wobble) = wobbles.borrow().get(&window) {
        return wobble.clone();
    }
    let bend: [Signal<[f64; 4]>; 8] = std::array::from_fn(|_| signal([0.0; 4]));
    let opacity = signal(1.0_f64);
    let mut stage = shader_stage("./src/effect/jelly-drag.frag").uniform("opacity", opacity);
    for (i, signal) in bend.iter().enumerate() {
        stage = stage.uniform(&format!("d{i}"), *signal);
    }
    let effect = Effect::new(window_source())
        .invalidate(Invalidate::always())
        .preserve_alpha()
        .stage(stage);
    let wobble = Rc::new(Wobble {
        active: signal(false),
        effect: SurfaceEffect::new(effect).outsets(MAX_BEND_PX as i32),
        inner: RefCell::new(Inner {
            window,
            bend,
            opacity,
            dragging: false,
            drag_rect: None,
            grid: Grid::default(),
            left_ms: 0.0,
            wobbling: true,
            started: false,
            poll: None,
            last_ms: None,
        }),
    });
    wobbles.borrow_mut().insert(window, wobble.clone());
    wobble
}

impl Inner {
    /// The window rect now: the move event while dragging, else the live rect.
    fn rect(&self) -> Rect {
        if self.dragging
            && let Some(rect) = self.drag_rect
        {
            return rect;
        }
        let rect = self.window.rect();
        Rect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }

    /// Fade towards the drag opacity while dragging, back to opaque after.
    fn fade(&mut self, ms: f64) -> f64 {
        let target = if self.dragging { DRAG_OPACITY } else { 1.0 };
        let change = (1.0 - DRAG_OPACITY) * ms / FADE_MS;
        let current = self.opacity.get_untracked();
        let next = if current < target {
            (current + change).min(target)
        } else {
            (current - change).max(target)
        };
        self.opacity.set(next);
        next
    }

    /// Run the physics for one frame (KWin: prePaintWindow). Returns false
    /// once done.
    fn frame(&mut self, delta_ms: f64) -> bool {
        let rect = self.rect();
        // Whole steps only; the rest of the frame time waits for the next frame.
        self.left_ms += delta_ms;
        while self.left_ms >= STEP_MS {
            self.left_ms -= STEP_MS;
            self.grid.prev_x = self.grid.pos_x;
            self.grid.prev_y = self.grid.pos_y;
            self.wobbling = self.grid.step(rect, STEP_MS);
            if !self.wobbling {
                self.left_ms = 0.0;
                break;
            }
        }
        let opacity = self.fade(delta_ms);
        let done = !self.dragging && !self.wobbling && opacity == 1.0;
        // Like KWin, draw the window flat while the springs are at rest.
        let packed = self
            .grid
            .bends(rect, !self.wobbling, self.left_ms / STEP_MS);
        for (signal, value) in self.bend.iter().zip(packed) {
            signal.set(value);
        }
        !done
    }
}

fn start_poll(wobble: &Rc<Wobble>) {
    let window = {
        let mut inner = wobble.inner.borrow_mut();
        if inner.poll.is_some() {
            return;
        }
        inner.last_ms = None;
        inner.window
    };
    let poll_wobble = wobble.clone();
    let poll = window.create_poll(1.0, move |handle| {
        let running = {
            let mut inner = poll_wobble.inner.borrow_mut();
            // Cap a stalled frame, so the window does not jump.
            let now = handle.now_ms();
            let delta = inner.last_ms.map_or(0.0, |last| (now - last).min(100.0));
            inner.last_ms = Some(now);
            inner.frame(delta)
        };
        if !running {
            {
                let mut inner = poll_wobble.inner.borrow_mut();
                inner.poll = None;
                inner.started = false;
            }
            poll_wobble.active.set(false);
            handle.cancel();
        }
    });
    wobble.inner.borrow_mut().poll = Some(poll);
}

/// Wobbly windows: register the window effect and feed it every
/// interactive window move (KWin: start/step/finish UserMovedResized).
pub fn configure_wobbly() {
    let wobbles: Wobbles = Rc::default();

    {
        let wobbles = wobbles.clone();
        COMPOSITOR.effect.window(move |window| {
            let wobble = wobble_for(&wobbles, window);
            if wobble.active.get() {
                SurfaceEffects {
                    replace: Some(wobble.effect.clone()),
                    ..SurfaceEffects::none()
                }
            } else {
                SurfaceEffects::none()
            }
        });
    }

    {
        let wobbles = wobbles.clone();
        COMPOSITOR.event.on_window_move(move |window, event| {
            let wobble = wobble_for(&wobbles, window);
            let dragging = {
                let mut inner = wobble.inner.borrow_mut();
                inner.drag_rect = Some(event.current_rect);
                if event.phase == WindowMovePhaseSnapshot::Start {
                    if !inner.started {
                        inner.grid.reset(event.start_rect);
                        inner.left_ms = 0.0;
                        inner.wobbling = true;
                        inner.started = true;
                    }
                    // Constrain the grid point nearest to the grab, like KWin.
                    let x_increment = event.start_rect.width / (GRID - 1) as f64;
                    let y_increment = event.start_rect.height / (GRID - 1) as f64;
                    let col =
                        ((event.start_pointer.x - event.start_rect.x) / x_increment + 0.5).floor();
                    let row =
                        ((event.start_pointer.y - event.start_rect.y) / y_increment + 0.5).floor();
                    let picked = (row * GRID as f64 + col).clamp(0.0, (COUNT - 1) as f64) as usize;
                    inner.grid.constraint[picked] = true;
                }
                inner.dragging = matches!(
                    event.phase,
                    WindowMovePhaseSnapshot::Start | WindowMovePhaseSnapshot::Update
                );
                inner.dragging
            };
            if dragging {
                wobble.active.set(true);
                start_poll(&wobble);
            }
        });
    }

    {
        let wobbles = wobbles.clone();
        COMPOSITOR.event.on_close(move |window| {
            if let Some(wobble) = wobbles.borrow_mut().remove(&window)
                && let Some(poll) = wobble.inner.borrow_mut().poll.take()
            {
                poll.cancel();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64) -> Rect {
        Rect {
            x,
            y,
            width: 800.0,
            height: 600.0,
        }
    }

    /// Step the grid against `rect` until it rests; returns the step count.
    /// Panics if it is still wobbling after `max` steps.
    fn settle(grid: &mut Grid, rect: Rect, max: usize) -> usize {
        for step in 1..=max {
            grid.prev_x = grid.pos_x;
            grid.prev_y = grid.pos_y;
            if !grid.step(rect, STEP_MS) {
                return step;
            }
        }
        panic!("grid did not settle within {max} steps");
    }

    #[test]
    fn a_resting_grid_stays_at_rest() {
        let mut grid = Grid::default();
        grid.reset(rect(100.0, 100.0));
        assert!(!grid.step(rect(100.0, 100.0), STEP_MS));
        let bends = grid.bends(rect(100.0, 100.0), false, 1.0);
        assert!(bends.iter().flatten().all(|value| *value == 0.0));
    }

    #[test]
    fn a_moved_window_wobbles_and_then_settles_on_its_new_place() {
        let mut grid = Grid::default();
        grid.reset(rect(0.0, 0.0));
        grid.constraint[0] = true;
        // The grab flings the window 240x90 px away in one go.
        let moved = rect(240.0, 90.0);
        assert!(grid.step(moved, STEP_MS), "a flung grid must be wobbling");

        // Mid-settle the far corner trails well behind the window: that
        // trailing is the whole effect.
        for _ in 0..12 {
            grid.step(moved, STEP_MS);
        }
        let bends = grid.bends(moved, false, 1.0);
        let biggest = bends.iter().flatten().fold(0.0f64, |a, b| a.max(b.abs()));
        assert!(biggest > 20.0, "expected a visible bend, got {biggest}");

        settle(&mut grid, moved, 2_000);
        // KWin's stop thresholds sum over all 16 points, so the springs halt
        // a couple of px short of their places. That never shows: at rest
        // the window is drawn flat (bends() with still = true).
        for i in 0..COUNT {
            assert!(
                (grid.pos_x[i] - origin_x(moved, i)).abs() < 3.0,
                "point {i} settled off its place in x: {} vs {}",
                grid.pos_x[i],
                origin_x(moved, i)
            );
            assert!(
                (grid.pos_y[i] - origin_y(moved, i)).abs() < 3.0,
                "point {i} settled off its place in y: {} vs {}",
                grid.pos_y[i],
                origin_y(moved, i)
            );
        }
        assert!(
            grid.bends(moved, true, 0.0)
                .iter()
                .flatten()
                .all(|value| *value == 0.0)
        );
    }

    #[test]
    fn bends_are_scaled_into_the_effect_margin() {
        let mut grid = Grid::default();
        grid.reset(rect(0.0, 0.0));
        // Far past anything a drag produces: the physics keeps it, the
        // drawing must not.
        let far = rect(4_000.0, 0.0);
        grid.step(far, STEP_MS);
        let bends = grid.bends(far, false, 1.0);
        let biggest = (0..COUNT)
            .map(|i| {
                let packed = bends[i / 2];
                packed[(i % 2) * 2].hypot(packed[(i % 2) * 2 + 1])
            })
            .fold(0.0f64, f64::max);
        assert!(
            (biggest - MAX_BEND_PX).abs() < 0.01,
            "largest drawn bend should sit on the margin, got {biggest}"
        );
        // Drawn flat at rest regardless of where the points are.
        assert!(
            grid.bends(far, true, 1.0)
                .iter()
                .flatten()
                .all(|value| *value == 0.0)
        );
    }

    #[test]
    fn bound_zeroes_and_caps_like_kwin() {
        assert_eq!(bound(0.25, 0.5, 10.0), 0.0);
        assert_eq!(bound(-25.0, 0.0, 10.0), -10.0);
        assert_eq!(bound(3.0, 0.0, 10.0), 3.0);
    }

    #[test]
    fn ring_mean_pulls_a_spike_towards_its_neighbours() {
        let mut x = [0.0; COUNT];
        let mut y = [0.0; COUNT];
        x[5] = 16.0;
        ring_mean(&mut x, &mut y);
        // Point 5 (interior, 8 neighbours): (8*16 + 0) / 16 = 8.
        assert_eq!(x[5], 8.0);
        // A neighbour with 5 ring members (edge point 1): (5*0 + 16) / 10.
        assert!((x[1] - 1.6).abs() < 1e-9, "got {}", x[1]);
        assert!(y.iter().all(|value| *value == 0.0));
    }
}
