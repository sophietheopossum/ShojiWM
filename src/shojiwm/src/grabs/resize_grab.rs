//! Resize grab is the state of a composer during which the client window is being resized.
//!
//! eg. Usually whenever a user clicks on the app's border and starts dragging, the compositors
//! enters a ResizeSurfaceGrab state.

use crate::ssd::{
    WindowPositionSnapshot, WindowResizeEdgesSnapshot, WindowResizeEventSnapshot,
    WindowResizePhaseSnapshot, WindowResizePointSnapshot, WindowResizeSourceSnapshot,
};
use crate::state::ShojiWM;
use smithay::{
    desktop::{Space, Window},
    input::pointer::{
        AxisFrame, ButtonEvent, CursorIcon, GestureHoldBeginEvent, GestureHoldEndEvent,
        GesturePinchBeginEvent, GesturePinchEndEvent, GesturePinchUpdateEvent,
        GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
        GrabStartData as PointerGrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
        RelativeMotionEvent,
    },
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel,
        wayland_server::protocol::wl_surface::WlSurface,
    },
    utils::{Logical, Point, Rectangle, Size},
    wayland::{compositor, shell::xdg::SurfaceCachedState},
};
use std::cell::RefCell;
use tracing::info;

fn managed_rect_debug_enabled() -> bool {
    std::env::var_os("SHOJI_MANAGED_RECT_DEBUG")
        .is_some_and(|value| value != "0" && !value.is_empty())
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct ResizeEdge: u32 {
        const TOP          = 0b0001;
        const BOTTOM       = 0b0010;
        const LEFT         = 0b0100;
        const RIGHT        = 0b1000;

        const TOP_LEFT     = Self::TOP.bits() | Self::LEFT.bits();
        const BOTTOM_LEFT  = Self::BOTTOM.bits() | Self::LEFT.bits();

        const TOP_RIGHT    = Self::TOP.bits() | Self::RIGHT.bits();
        const BOTTOM_RIGHT = Self::BOTTOM.bits() | Self::RIGHT.bits();
    }
}

impl From<xdg_toplevel::ResizeEdge> for ResizeEdge {
    #[inline]
    fn from(x: xdg_toplevel::ResizeEdge) -> Self {
        Self::from_bits(x as u32).unwrap()
    }
}

pub struct ResizeSurfaceGrab {
    start_data: PointerGrabStartData<ShojiWM>,
    window: Window,

    edges: ResizeEdge,
    source: WindowResizeSourceSnapshot,
    runtime_managed: bool,

    initial_rect: Rectangle<i32, Logical>,
    initial_event_rect: Rectangle<i32, Logical>,
    last_pointer: Point<f64, Logical>,
    last_window_size: Size<i32, Logical>,
}

impl ResizeSurfaceGrab {
    pub fn start(
        start_data: PointerGrabStartData<ShojiWM>,
        window: Window,
        edges: ResizeEdge,
        initial_window_rect: Rectangle<i32, Logical>,
        initial_event_rect: Rectangle<i32, Logical>,
        source: WindowResizeSourceSnapshot,
    ) -> Option<Self> {
        let toplevel = window.toplevel()?;
        let initial_rect = initial_window_rect;
        let last_pointer = start_data.location;

        ResizeSurfaceState::with(toplevel.wl_surface(), |state| {
            *state = ResizeSurfaceState::Resizing {
                edges,
                initial_rect,
            };
        });

        Some(Self {
            start_data,
            window,
            edges,
            source,
            runtime_managed: false,
            initial_rect,
            initial_event_rect,
            last_pointer,
            last_window_size: initial_rect.size,
        })
    }

    pub fn notify_start(&mut self, data: &mut ShojiWM) {
        data.cursor_override = Some(match self.edges {
            e if e.contains(ResizeEdge::TOP | ResizeEdge::LEFT) => CursorIcon::NwResize,
            e if e.contains(ResizeEdge::TOP | ResizeEdge::RIGHT) => CursorIcon::NeResize,
            e if e.contains(ResizeEdge::BOTTOM | ResizeEdge::LEFT) => CursorIcon::SwResize,
            e if e.contains(ResizeEdge::BOTTOM | ResizeEdge::RIGHT) => CursorIcon::SeResize,
            e if e.contains(ResizeEdge::LEFT) => CursorIcon::WResize,
            e if e.contains(ResizeEdge::RIGHT) => CursorIcon::EResize,
            e if e.contains(ResizeEdge::TOP) => CursorIcon::NResize,
            e if e.contains(ResizeEdge::BOTTOM) => CursorIcon::SResize,
            e if e.contains(ResizeEdge::LEFT | ResizeEdge::RIGHT) => CursorIcon::EwResize,
            e if e.contains(ResizeEdge::TOP | ResizeEdge::BOTTOM) => CursorIcon::NsResize,
            _ => CursorIcon::AllResize,
        });
        data.schedule_redraw();

        self.runtime_managed = self.invoke_runtime_event(
            data,
            WindowResizePhaseSnapshot::Start,
            self.start_data.location,
        );
    }

    fn invoke_runtime_event(
        &self,
        data: &mut ShojiWM,
        phase: WindowResizePhaseSnapshot,
        current_pointer: Point<f64, Logical>,
    ) -> bool {
        let window_id = data.snapshot_window(&self.window).id;
        let event = self.runtime_event(data, phase, current_pointer);
        let now_ms = std::time::Duration::from(data.clock.now()).as_millis() as u64;
        data.invoke_window_resize_event(&window_id, &event, now_ms)
    }

    fn runtime_event(
        &self,
        data: &ShojiWM,
        phase: WindowResizePhaseSnapshot,
        current_pointer: Point<f64, Logical>,
    ) -> WindowResizeEventSnapshot {
        let window_id = data.snapshot_window(&self.window).id;
        let start_pointer = self.start_data.location;
        let delta = current_pointer - start_pointer;
        let current_rect = resize_rect_for_delta(self.initial_event_rect, self.edges, delta);
        let output_name = data
            .space
            .outputs()
            .find(|output| {
                data.space
                    .output_geometry(output)
                    .is_some_and(|geometry| geometry.contains(current_pointer.to_i32_floor()))
            })
            .map(|output| output.name());

        if managed_rect_debug_enabled() {
            info!(
                window_id,
                ?phase,
                ?self.source,
                start_pointer_x = start_pointer.x,
                start_pointer_y = start_pointer.y,
                current_pointer_x = current_pointer.x,
                current_pointer_y = current_pointer.y,
                delta_x = delta.x,
                delta_y = delta.y,
                start_rect_x = self.initial_event_rect.loc.x,
                start_rect_y = self.initial_event_rect.loc.y,
                start_rect_width = self.initial_event_rect.size.w,
                start_rect_height = self.initial_event_rect.size.h,
                start_rect_right = self.initial_event_rect.loc.x + self.initial_event_rect.size.w,
                start_rect_bottom = self.initial_event_rect.loc.y + self.initial_event_rect.size.h,
                current_rect_x = current_rect.loc.x,
                current_rect_y = current_rect.loc.y,
                current_rect_width = current_rect.size.w,
                current_rect_height = current_rect.size.h,
                current_rect_right = current_rect.loc.x + current_rect.size.w,
                current_rect_bottom = current_rect.loc.y + current_rect.size.h,
                "managed rect debug: resize event"
            );
        }

        WindowResizeEventSnapshot {
            source: self.source,
            phase,
            edges: resize_edges_snapshot(self.edges),
            start_pointer: point_snapshot(start_pointer),
            current_pointer: point_snapshot(current_pointer),
            delta: point_snapshot(delta),
            start_rect: rect_snapshot(self.initial_event_rect),
            current_rect: rect_snapshot(current_rect),
            output_name,
            timestamp: std::time::Duration::from(data.clock.now()).as_millis() as u64,
        }
    }
}

impl PointerGrab<ShojiWM> for ResizeSurfaceGrab {
    fn motion(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        _focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // While the grab is active, no client has pointer focus
        handle.motion(data, None, event);
        self.last_pointer = event.location;

        if self.runtime_managed {
            self.invoke_runtime_event(data, WindowResizePhaseSnapshot::Update, event.location);
            return;
        }

        let mut delta = event.location - self.start_data.location;

        let mut new_window_width = self.initial_rect.size.w;
        let mut new_window_height = self.initial_rect.size.h;

        if self.edges.intersects(ResizeEdge::LEFT | ResizeEdge::RIGHT) {
            if self.edges.intersects(ResizeEdge::LEFT) {
                delta.x = -delta.x;
            }

            new_window_width = (self.initial_rect.size.w as f64 + delta.x) as i32;
        }

        if self.edges.intersects(ResizeEdge::TOP | ResizeEdge::BOTTOM) {
            if self.edges.intersects(ResizeEdge::TOP) {
                delta.y = -delta.y;
            }

            new_window_height = (self.initial_rect.size.h as f64 + delta.y) as i32;
        }

        let Some(toplevel_surface) = self.window.toplevel() else {
            return;
        };
        let (min_size, max_size) =
            compositor::with_states(toplevel_surface.wl_surface(), |states| {
                let mut guard = states.cached_state.get::<SurfaceCachedState>();
                let data = guard.current();
                (data.min_size, data.max_size)
            });

        let min_width = min_size.w.max(1);
        let min_height = min_size.h.max(1);

        let max_width = if max_size.w == 0 {
            i32::MAX
        } else {
            max_size.w
        };
        let max_height = if max_size.h == 0 {
            i32::MAX
        } else {
            max_size.h
        };

        self.last_window_size = Size::from((
            new_window_width.max(min_width).min(max_width),
            new_window_height.max(min_height).min(max_height),
        ));

        let xdg = toplevel_surface;
        xdg.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Resizing);
            state.size = Some(self.last_window_size);
        });

        xdg.send_pending_configure();
    }

    fn relative_motion(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        focus: Option<(WlSurface, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, focus, event);
    }

    fn button(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);

        // The button is a button code as defined in the
        // Linux kernel's linux/input-event-codes.h header file, e.g. BTN_LEFT.
        const BTN_LEFT: u32 = 0x110;

        if !handle.current_pressed().contains(&BTN_LEFT) {
            // No more buttons are pressed, release the grab.
            handle.unset_grab(self, data, event.serial, event.time, true);

            if self.runtime_managed {
                self.invoke_runtime_event(data, WindowResizePhaseSnapshot::End, self.last_pointer);
                if let Some(xdg) = self.window.toplevel() {
                    xdg.with_pending_state(|state| {
                        state.states.unset(xdg_toplevel::State::Resizing);
                    });
                    xdg.send_pending_configure();
                    ResizeSurfaceState::with(xdg.wl_surface(), |state| {
                        *state = ResizeSurfaceState::Idle;
                    });
                }
            } else if let Some(xdg) = self.window.toplevel() {
                xdg.with_pending_state(|state| {
                    state.states.unset(xdg_toplevel::State::Resizing);
                    state.size = Some(self.last_window_size);
                });

                xdg.send_pending_configure();

                ResizeSurfaceState::with(xdg.wl_surface(), |state| {
                    *state = ResizeSurfaceState::WaitingForLastCommit {
                        edges: self.edges,
                        initial_rect: self.initial_rect,
                    };
                });
            }
        }
    }

    fn axis(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        details: AxisFrame,
    ) {
        handle.axis(data, details)
    }

    fn frame(&mut self, data: &mut ShojiWM, handle: &mut PointerInnerHandle<'_, ShojiWM>) {
        handle.frame(data);
    }

    fn gesture_swipe_begin(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureSwipeBeginEvent,
    ) {
        handle.gesture_swipe_begin(data, event)
    }

    fn gesture_swipe_update(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureSwipeUpdateEvent,
    ) {
        handle.gesture_swipe_update(data, event)
    }

    fn gesture_swipe_end(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureSwipeEndEvent,
    ) {
        handle.gesture_swipe_end(data, event)
    }

    fn gesture_pinch_begin(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GesturePinchBeginEvent,
    ) {
        handle.gesture_pinch_begin(data, event)
    }

    fn gesture_pinch_update(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GesturePinchUpdateEvent,
    ) {
        handle.gesture_pinch_update(data, event)
    }

    fn gesture_pinch_end(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GesturePinchEndEvent,
    ) {
        handle.gesture_pinch_end(data, event)
    }

    fn gesture_hold_begin(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureHoldBeginEvent,
    ) {
        handle.gesture_hold_begin(data, event)
    }

    fn gesture_hold_end(
        &mut self,
        data: &mut ShojiWM,
        handle: &mut PointerInnerHandle<'_, ShojiWM>,
        event: &GestureHoldEndEvent,
    ) {
        handle.gesture_hold_end(data, event)
    }

    fn start_data(&self) -> &PointerGrabStartData<ShojiWM> {
        &self.start_data
    }

    fn unset(&mut self, _data: &mut ShojiWM) {}
}

fn resize_rect_for_delta(
    initial: Rectangle<i32, Logical>,
    edges: ResizeEdge,
    delta: Point<f64, Logical>,
) -> Rectangle<i32, Logical> {
    let mut width = initial.size.w;
    let mut height = initial.size.h;

    if edges.intersects(ResizeEdge::LEFT) {
        width -= delta.x.round() as i32;
    } else if edges.intersects(ResizeEdge::RIGHT) {
        width += delta.x.round() as i32;
    }

    if edges.intersects(ResizeEdge::TOP) {
        height -= delta.y.round() as i32;
    } else if edges.intersects(ResizeEdge::BOTTOM) {
        height += delta.y.round() as i32;
    }

    let width = width.max(1);
    let height = height.max(1);

    // A left or top drag keeps the opposite edge where it was, so the origin is
    // placed from that edge, after the size floor. Moving it by the pointer delta
    // instead carried a 1 px window along once the drag crossed that edge.
    let x = if edges.intersects(ResizeEdge::LEFT) {
        initial.loc.x + initial.size.w - width
    } else {
        initial.loc.x
    };
    let y = if edges.intersects(ResizeEdge::TOP) {
        initial.loc.y + initial.size.h - height
    } else {
        initial.loc.y
    };

    Rectangle::new((x, y).into(), (width, height).into())
}

fn resize_edges_snapshot(edges: ResizeEdge) -> WindowResizeEdgesSnapshot {
    WindowResizeEdgesSnapshot {
        left: edges.intersects(ResizeEdge::LEFT),
        right: edges.intersects(ResizeEdge::RIGHT),
        top: edges.intersects(ResizeEdge::TOP),
        bottom: edges.intersects(ResizeEdge::BOTTOM),
    }
}

fn point_snapshot(point: Point<f64, Logical>) -> WindowResizePointSnapshot {
    WindowResizePointSnapshot {
        x: point.x,
        y: point.y,
    }
}

/// Unlike a move, a resize rect stays quantized upstream: the size it carries
/// is the one configured on the client surface, and `xdg_toplevel.configure`
/// only speaks whole logical pixels. Widening the snapshot here just avoids a
/// second, separate rect type.
fn rect_snapshot(rect: Rectangle<i32, Logical>) -> WindowPositionSnapshot {
    WindowPositionSnapshot {
        x: rect.loc.x as f64,
        y: rect.loc.y as f64,
        width: rect.size.w as f64,
        height: rect.size.h as f64,
    }
}

/// State of the resize operation.
///
/// It is stored inside of WlSurface,
/// and can be accessed using [`ResizeSurfaceState::with`]
#[derive(Debug, Clone, Copy, Eq, PartialEq, Default)]
enum ResizeSurfaceState {
    #[default]
    Idle,
    Resizing {
        edges: ResizeEdge,
        /// The initial window size and location.
        initial_rect: Rectangle<i32, Logical>,
    },
    /// Resize is done, we are now waiting for last commit, to do the final move
    WaitingForLastCommit {
        edges: ResizeEdge,
        /// The initial window size and location.
        initial_rect: Rectangle<i32, Logical>,
    },
}

impl ResizeSurfaceState {
    fn with<F, T>(surface: &WlSurface, cb: F) -> T
    where
        F: FnOnce(&mut Self) -> T,
    {
        compositor::with_states(surface, |states| {
            states.data_map.insert_if_missing(RefCell::<Self>::default);
            let state = states.data_map.get::<RefCell<Self>>().unwrap();

            cb(&mut state.borrow_mut())
        })
    }

    fn commit(&mut self) -> Option<(ResizeEdge, Rectangle<i32, Logical>)> {
        match *self {
            Self::Resizing {
                edges,
                initial_rect,
            } => Some((edges, initial_rect)),
            Self::WaitingForLastCommit {
                edges,
                initial_rect,
            } => {
                // The resize is done, let's go back to idle
                *self = Self::Idle;

                Some((edges, initial_rect))
            }
            Self::Idle => None,
        }
    }
}

/// Should be called on `WlSurface::commit`
pub fn handle_commit(space: &mut Space<Window>, surface: &WlSurface) -> Option<()> {
    let window = space
        .elements()
        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
        .cloned()?;

    let mut window_loc = space.element_location(&window)?;
    let geometry = window.geometry();

    let new_loc: Point<Option<i32>, Logical> = ResizeSurfaceState::with(surface, |state| {
        state
            .commit()
            .and_then(|(edges, initial_rect)| {
                // If the window is being resized by top or left, its location must be adjusted
                // accordingly.
                edges.intersects(ResizeEdge::TOP_LEFT).then(|| {
                    let new_x = edges
                        .intersects(ResizeEdge::LEFT)
                        .then_some(initial_rect.loc.x + (initial_rect.size.w - geometry.size.w));

                    let new_y = edges
                        .intersects(ResizeEdge::TOP)
                        .then_some(initial_rect.loc.y + (initial_rect.size.h - geometry.size.h));

                    (new_x, new_y).into()
                })
            })
            .unwrap_or_default()
    });

    if let Some(new_x) = new_loc.x {
        window_loc.x = new_x;
    }
    if let Some(new_y) = new_loc.y {
        window_loc.y = new_y;
    }

    if new_loc.x.is_some() || new_loc.y.is_some() {
        // If TOP or LEFT side of the window got resized, we have to move it
        space.map_element(window, window_loc, false);
    }

    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, width: i32, height: i32) -> Rectangle<i32, Logical> {
        Rectangle::new((x, y).into(), (width, height).into())
    }

    fn right(rect: Rectangle<i32, Logical>) -> i32 {
        rect.loc.x + rect.size.w
    }

    fn bottom(rect: Rectangle<i32, Logical>) -> i32 {
        rect.loc.y + rect.size.h
    }

    #[test]
    fn xdg_edges_reach_the_runtime_as_the_same_sides() {
        // `From` reinterprets the protocol value as our bits, so the two
        // layouts have to agree: a mismatch would resize the wrong side.
        use xdg_toplevel::ResizeEdge as Xdg;
        let cases = [
            (Xdg::None, ResizeEdge::empty(), (false, false, false, false)),
            (Xdg::Top, ResizeEdge::TOP, (false, false, true, false)),
            (Xdg::Bottom, ResizeEdge::BOTTOM, (false, false, false, true)),
            (Xdg::Left, ResizeEdge::LEFT, (true, false, false, false)),
            (Xdg::Right, ResizeEdge::RIGHT, (false, true, false, false)),
            (
                Xdg::TopLeft,
                ResizeEdge::TOP_LEFT,
                (true, false, true, false),
            ),
            (
                Xdg::TopRight,
                ResizeEdge::TOP_RIGHT,
                (false, true, true, false),
            ),
            (
                Xdg::BottomLeft,
                ResizeEdge::BOTTOM_LEFT,
                (true, false, false, true),
            ),
            (
                Xdg::BottomRight,
                ResizeEdge::BOTTOM_RIGHT,
                (false, true, false, true),
            ),
        ];
        for (xdg, edges, (left, right, top, bottom)) in cases {
            assert_eq!(ResizeEdge::from(xdg), edges, "{xdg:?}");
            assert_eq!(
                resize_edges_snapshot(edges),
                WindowResizeEdgesSnapshot {
                    left,
                    right,
                    top,
                    bottom
                },
                "{xdg:?}"
            );
        }
    }

    #[test]
    fn right_and_bottom_edges_grow_in_place() {
        let initial = rect(100, 50, 400, 300);
        let current = resize_rect_for_delta(
            initial,
            ResizeEdge::BOTTOM_RIGHT,
            Point::from((30.0, -20.0)),
        );
        assert_eq!(current, rect(100, 50, 430, 280));
    }

    #[test]
    fn a_side_handle_ignores_the_other_axis() {
        // The pointer never moves perfectly straight; drift along the axis a
        // side handle does not resize must not leak into the rect.
        let initial = rect(100, 50, 400, 300);
        let drift = Point::from((30.0, 45.0));
        assert_eq!(
            resize_rect_for_delta(initial, ResizeEdge::RIGHT, drift),
            rect(100, 50, 430, 300)
        );
        assert_eq!(
            resize_rect_for_delta(initial, ResizeEdge::TOP, drift),
            rect(100, 95, 400, 255)
        );
    }

    #[test]
    fn left_and_top_edges_keep_the_opposite_edges_anchored() {
        let initial = rect(100, 50, 400, 300);
        assert_eq!(
            resize_rect_for_delta(initial, ResizeEdge::TOP_LEFT, Point::from((-40.0, -25.0))),
            rect(60, 25, 440, 325)
        );
        assert_eq!(
            resize_rect_for_delta(initial, ResizeEdge::TOP_LEFT, Point::from((60.0, 35.0))),
            rect(160, 85, 340, 265)
        );
    }

    #[test]
    fn fractional_deltas_move_origin_and_size_by_the_same_whole_pixel() {
        // At a fractional scale the pointer moves in fractions of a logical
        // pixel, but a resize rect stays whole (`xdg_toplevel.configure` only
        // speaks whole pixels). The origin and the size have to round
        // together, or the anchored edge wobbles by a pixel under the pointer.
        let initial = rect(100, 50, 400, 300);
        for d in [
            0.4,
            0.5,
            0.6,
            10.49,
            10.5,
            -0.4,
            -0.5,
            -10.5,
            1.0 / 3.0,
            -2.0 / 3.0,
        ] {
            let current = resize_rect_for_delta(initial, ResizeEdge::TOP_LEFT, Point::from((d, d)));
            assert_eq!(right(current), right(initial), "delta {d}");
            assert_eq!(bottom(current), bottom(initial), "delta {d}");
            assert_eq!(current.loc.x - initial.loc.x, d.round() as i32, "delta {d}");
        }
    }

    #[test]
    fn dragging_past_the_opposite_edge_floors_the_size_at_one_pixel() {
        let initial = rect(100, 50, 400, 300);
        assert_eq!(
            resize_rect_for_delta(
                initial,
                ResizeEdge::BOTTOM_RIGHT,
                Point::from((-500.0, -400.0))
            ),
            rect(100, 50, 1, 1)
        );
    }

    #[test]
    fn dragging_left_or_top_past_the_opposite_edge_stays_pinned_to_it() {
        let initial = rect(100, 50, 400, 300);
        // Right edge at 500, bottom edge at 350: a 1 px window ends there no
        // matter how far beyond it the pointer goes.
        for delta in [(500.0, 400.0), (900.0, 1000.0)] {
            assert_eq!(
                resize_rect_for_delta(initial, ResizeEdge::TOP_LEFT, Point::from(delta)),
                rect(499, 349, 1, 1),
                "delta {delta:?}"
            );
        }
    }
}
