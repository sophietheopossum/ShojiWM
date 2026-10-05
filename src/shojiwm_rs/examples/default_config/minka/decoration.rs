//! Window chrome: a transparent drag halo with hover-revealed drag tabs, no
//! titlebar; a plain border for client-decorated windows; nothing at all
//! when maximized or fullscreen.

use shojiwm_rs::{prelude::*, ssd::WindowDecorationModeSnapshot};

use super::{events::PointerTracking, workspace_ipc::WorkspaceIpc};
use crate::window_manager::{
    EDGE_DRAG_HALO_PX, WINDOW_BORDER_PX, WINDOW_STATE_FULLSCREEN, WINDOW_STATE_MAXIMIZED,
    WINDOW_STATE_MINIMIZE_VISUAL_IDLE, WINDOW_STATE_RECT, WINDOW_STATE_TILE_DRAGGING,
    WINDOW_STATE_TILE_REORDERING, WINDOW_STATE_TILED, WINDOW_STATE_VISIBLE_OUTPUTS,
    WINDOW_STATE_WORKSPACE_OFFSET_Y, WINDOW_STATE_WORKSPACE_OPACITY, WINDOW_STATE_WORKSPACE_TILED,
    WINDOW_STATE_WORKSPACE_VISIBLE, WindowManager, js_round,
};

const FULLSCREEN_Z_INDEX: i32 = 2_000_000_000;
const FLOATING_WINDOW_Z_INDEX_BASE: i32 = 1_500_000_000;
const WINDOW_STACK_Z_INDEX_RANGE: i32 = 100_000_000;
const FOCUSED_TILED_WINDOW_Z_INDEX: i32 = 1_000_000_000;
const REORDERING_TILED_WINDOW_Z_INDEX: i32 = -2_000_000_000;

/// Window corner rounding; the drag tabs keep their travel to the flat part
/// of each edge, between the corner arcs.
const WINDOW_CORNER_RADIUS: f64 = 10.0;
const DRAG_TAB_LENGTH: f64 = 72.0;
const DRAG_TAB_THICKNESS: f64 = 12.0;

/// Translucent terminals sit on liquid glass.
const TERMINALS: [&str; 2] = ["kitty", "ghostty"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

/// The managed rect around a client: the client plus the drag halo and
/// border on every side. The window manager is constructed with this.
pub fn natural_root_rect(window: Window) -> Rect {
    let client = window.position().get_untracked();
    let chrome = EDGE_DRAG_HALO_PX + WINDOW_BORDER_PX;
    Rect::new(
        client.x - chrome,
        client.y - chrome,
        client.width + chrome * 2.0,
        client.height + chrome * 2.0,
    )
}

fn liquid_glass() -> Effect {
    Effect::new(backdrop_source())
        .capture_padding(24)
        .invalidate(Invalidate::on_source_damage_box(8))
        .stage(dual_kawase_blur(4, 2))
        .stage(
            shader_stage("./src/effect/liquid-glass.frag")
                // Follow the window's rounded corners.
                .uniform("glass_radius_px", -1.0)
                .uniform("distortion_depth", 0.2)
                .uniform("distortion_strength", 0.15)
                .uniform("chromatic_shift_px", 3.0)
                .uniform("glass_tint", 0.9),
        )
}

/// `min(max, max(min, value))`, as the TypeScript wrote it: `min` wins when
/// the range is empty.
fn clamp_to(value: f64, min: f64, max: f64) -> f64 {
    max.min(min.max(value))
}

pub fn create_window_composition(
    wm: WindowManager,
    ipc: WorkspaceIpc,
    pointer: PointerTracking,
) -> impl Fn(Window) -> ManagedWindow + 'static {
    move |window| {
        // Read in the body on purpose: these switch the whole structure, so a
        // change re-runs this function.
        let decoration = window.decoration().get();
        let use_client_decoration = decoration.mode == WindowDecorationModeSnapshot::Client
            && !(decoration.client_preference == Some(WindowDecorationModeSnapshot::Server)
                && decoration.configured_mode == WindowDecorationModeSnapshot::Server);
        let fullscreen = window.state(&WINDOW_STATE_FULLSCREEN).get();
        let maximized = window.state(&WINDOW_STATE_MAXIMIZED).get();
        let terminal = TERMINALS.contains(&window.app_id().get().as_deref().unwrap_or_default());
        let id = window.id();

        let rect = window.state(&WINDOW_STATE_RECT);
        let workspace_offset_y = window.state(&WINDOW_STATE_WORKSPACE_OFFSET_Y);
        let managed_rect = memo(move || {
            let rect = rect.get();
            Rect {
                y: rect.y + workspace_offset_y.get(),
                ..rect
            }
        });
        let force_rect_size =
            memo(move || window.is_resizable().get() && !window.is_transient().get());

        let stack_z_index = wm.window_z_index(window);
        let workspace_tiled = window.state(&WINDOW_STATE_WORKSPACE_TILED);
        let window_tiled = window.state(&WINDOW_STATE_TILED);
        let reordering = window.state(&WINDOW_STATE_TILE_REORDERING);
        let z_index = memo(move || {
            if !workspace_tiled.get() {
                return stack_z_index.get();
            }
            let offset = stack_z_index
                .get()
                .clamp(-WINDOW_STACK_Z_INDEX_RANGE, WINDOW_STACK_Z_INDEX_RANGE);
            if !window_tiled.get() {
                FLOATING_WINDOW_Z_INDEX_BASE + offset
            } else if reordering.get() {
                REORDERING_TILED_WINDOW_Z_INDEX
            } else if window.is_focused().get() {
                FOCUSED_TILED_WINDOW_Z_INDEX
            } else {
                offset
            }
        });
        let minimize_visual_idle = window.state(&WINDOW_STATE_MINIMIZE_VISUAL_IDLE);
        let workspace_visible = window.state(&WINDOW_STATE_WORKSPACE_VISIBLE);
        let tile_dragging = window.state(&WINDOW_STATE_TILE_DRAGGING);
        let inactive = memo(move || {
            minimize_visual_idle.get() || (!workspace_visible.get() && !tile_dragging.get())
        });

        let managed = ManagedWindow::new()
            .rect(managed_rect)
            .visible_outputs(window.state(&WINDOW_STATE_VISIBLE_OUTPUTS))
            .opacity(window.state(&WINDOW_STATE_WORKSPACE_OPACITY))
            .force_rect_size(force_rect_size)
            // Forces no corner rounding by CSD.
            .tiled(true)
            .idle(inactive)
            .interactive(inactive.map(|inactive| !inactive));

        // Eternal Darkness red (Theme.red / Theme.redDim until a shared
        // theme.json).
        let border_color = window.is_focused().map(|focused| {
            if *focused {
                hex("#e0263c")
            } else {
                hex("#8f1e2d")
            }
        });
        // A soft drop shadow under the frame; the focused window floats a
        // bit higher.
        let window_shadow = window.is_focused().map(|focused| {
            if *focused {
                vec![BoxShadow {
                    spread: -2.0,
                    ..shadow(0.0, 10.0, 32.0, hex("#00000090"))
                }]
            } else {
                vec![BoxShadow {
                    spread: -2.0,
                    ..shadow(0.0, 4.0, 16.0, hex("#00000060"))
                }]
            }
        });
        let inner = || {
            if terminal {
                ShaderEffect::new(liquid_glass())
                    .direction(Direction::Column)
                    .child(ClientWindow::new())
            } else {
                ClientWindow::new()
            }
        };

        // The branches without the halo never get its hover-leave, so they
        // drop a stale hover themselves. Otherwise pointer motion would stay
        // signalled (every motion re-evaluating every window again), MinkaMon
        // would keep a ghost drag tab, and the old tab would come back with
        // the halo.
        let leave_halo = || {
            pointer.set_edge_hovered(&id, false);
            ipc.publish_drag_tab(&id, || None);
        };

        // Fullscreen: no chrome at all; the client fills its managed rect
        // (the whole output) edge to edge, which is also what lets the tty
        // backend promote its buffer to the primary plane (direct scanout).
        if fullscreen {
            leave_halo();
            return managed
                .z_index(FULLSCREEN_Z_INDEX)
                // Low-latency tearing for fullscreen windows, off by default.
                // It was on, on the reasoning that the compositor only tears
                // on direct scanout while the client outruns the refresh rate,
                // so it would be a no-op outside games. Both halves stopped
                // holding on 1/9/2026: direct scanout began engaging
                // routinely, and the TV was pinned to 60Hz, far easier to
                // out-commit than its auto-selected 120Hz. The result was
                // visible tearing on HDMI-A-3 in ordinary use. Re-enable per
                // app if wanted.
                .allow_tearing(false)
                .child(ClientWindow::new());
        }

        // Client-side decorations: only a border. A maximized window fills
        // the output, so a border and rounded corners around it are just
        // inset client area; they track maximize live.
        if use_client_decoration {
            leave_halo();
            let client_maximized = window.is_maximized();
            let border = memo(move || {
                let width = if client_maximized.get() {
                    0.0
                } else {
                    WINDOW_BORDER_PX
                };
                Border::new(width, border_color.get())
            });
            let radius = client_maximized.map(|maximized| {
                if *maximized {
                    0.0
                } else {
                    WINDOW_CORNER_RADIUS
                }
            });
            // A maximized window floats over nothing.
            let csd_shadow = memo(move || {
                if client_maximized.get() {
                    Vec::new()
                } else {
                    window_shadow.get()
                }
            });
            return managed.z_index(z_index).child(
                WindowBorder::new()
                    .style(
                        Style::new()
                            .border_all(border)
                            .border_radius(radius)
                            .box_shadow(csd_shadow)
                            .background(hex("#10131900"))
                            .padding(0.0),
                    )
                    .resize_hit_area(8, 14)
                    .child(ClientWindow::new()),
            );
        }

        // Maximized: no chrome at all, edge to edge in the usable area.
        // Without the halo a maximized window cannot be dragged by the
        // pointer; unmaximize re-centres it, so it can never get stuck.
        if maximized {
            leave_halo();
            return managed
                .z_index(stack_z_index)
                .child(Flex::row().child(inner()));
        }

        // The transparent halo ring around the window is decoration chrome:
        // the SSD hit-test resolves clicks there to Move (the outer resize
        // band wins for resize), so the whole ring drags the window. Hovering
        // it reveals a tab at that edge as the visible affordance; the tab is
        // plain chrome, so grabbing it drags too. Chrome can't render above
        // the client surface, which is why the tab lives outside the window
        // instead of overlapping it.
        let hovered: Signal<Option<Edge>> = signal(None);
        let edge_hover = |edge: Edge| {
            let (pointer, id) = (pointer.clone(), id.clone());
            move |inside: bool| {
                if inside {
                    hovered.set(Some(edge));
                } else if hovered.get_untracked() == Some(edge) {
                    hovered.set(None);
                }
                pointer.set_edge_hovered(&id, hovered.get_untracked().is_some());
            }
        };

        // Trapezium drag tabs (SVG assets, red stipple + border) on the
        // window border, centred on the pointer along the hovered edge. The
        // positions read the pointer only while their edge is hovered, so
        // idle windows never re-evaluate on mouse motion. Travel stops at the
        // corner arcs; a pinned tab stays visible (visibility follows the
        // hover strip, not the pointer) and draggable (the whole halo is
        // move chrome).
        let drag_tab_min = EDGE_DRAG_HALO_PX + WINDOW_CORNER_RADIUS;
        let drag_tab_x = {
            let pointer = pointer.clone();
            memo(move || {
                if !matches!(hovered.get(), Some(Edge::Top | Edge::Bottom)) {
                    return 0.0;
                }
                let rect = managed_rect.get();
                let max = drag_tab_min.max(rect.width - drag_tab_min - DRAG_TAB_LENGTH);
                let centred = js_round(pointer.current().0 - rect.x - DRAG_TAB_LENGTH / 2.0);
                clamp_to(centred, drag_tab_min, max)
            })
        };
        let drag_tab_y = {
            let pointer = pointer.clone();
            memo(move || {
                if !matches!(hovered.get(), Some(Edge::Left | Edge::Right)) {
                    return 0.0;
                }
                let rect = managed_rect.get();
                let max = drag_tab_min.max(rect.height - drag_tab_min - DRAG_TAB_LENGTH);
                let centred = js_round(pointer.current().1 - rect.y - DRAG_TAB_LENGTH / 2.0);
                clamp_to(centred, drag_tab_min, max)
            })
        };

        // Published to the workspace IPC view: the tab's layout-space rect
        // while an edge is hovered. Evaluated only when a view is built, and
        // read defensively, because the view can outlive this composition.
        ipc.publish_drag_tab(&id, move || {
            let edge = hovered.try_with(|edge| *edge).flatten()?;
            let rect = managed_rect.try_with(|rect| *rect)?;
            let tab_x = || drag_tab_x.try_with(|x| *x);
            let tab_y = || drag_tab_y.try_with(|y| *y);
            Some(match edge {
                Edge::Top => Rect::new(
                    rect.x + tab_x()?,
                    rect.y + EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS,
                    DRAG_TAB_LENGTH,
                    DRAG_TAB_THICKNESS,
                ),
                Edge::Bottom => Rect::new(
                    rect.x + tab_x()?,
                    rect.y + rect.height - EDGE_DRAG_HALO_PX,
                    DRAG_TAB_LENGTH,
                    DRAG_TAB_THICKNESS,
                ),
                Edge::Left => Rect::new(
                    rect.x + EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS,
                    rect.y + tab_y()?,
                    DRAG_TAB_THICKNESS,
                    DRAG_TAB_LENGTH,
                ),
                Edge::Right => Rect::new(
                    rect.x + rect.width - EDGE_DRAG_HALO_PX,
                    rect.y + tab_y()?,
                    DRAG_TAB_THICKNESS,
                    DRAG_TAB_LENGTH,
                ),
            })
        });

        let strip = |edge: Edge, style: Style| {
            Flex::column()
                .on_hover_change(edge_hover(edge))
                .style(style.absolute())
        };
        let tab = |src: &str, edge: Edge, style: Style| {
            Image::new(src).style(
                style
                    .absolute()
                    .visible(hovered.map(move |hovered| *hovered == Some(edge))),
            )
        };
        let halo = EDGE_DRAG_HALO_PX;
        let tab_inset = EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS;

        managed.z_index(z_index).child(
            // No `position` here: the halo box must NOT establish a
            // containing block, so its absolute children (strips and tabs)
            // anchor to the decoration root's full rect, the halo's outer
            // edge, instead of the padding-inset content box at the border.
            Flex::column()
                .style(Style::new().padding(halo))
                .child(
                    WindowBorder::new()
                        .style(
                            Style::new()
                                .border(WINDOW_BORDER_PX, border_color)
                                .border_radius(WINDOW_CORNER_RADIUS)
                                .box_shadow(window_shadow)
                                .background(hex("#10131900"))
                                .padding(0.0),
                        )
                        .resize_hit_area(8, 14)
                        .child(Flex::row().child(inner())),
                )
                .child(strip(
                    Edge::Top,
                    Style::new().top(0.0).left(0.0).right(0.0).height(halo),
                ))
                .child(strip(
                    Edge::Bottom,
                    Style::new().bottom(0.0).left(0.0).right(0.0).height(halo),
                ))
                .child(strip(
                    Edge::Left,
                    Style::new().left(0.0).top(halo).bottom(halo).width(halo),
                ))
                .child(strip(
                    Edge::Right,
                    Style::new().right(0.0).top(halo).bottom(halo).width(halo),
                ))
                .child(tab(
                    "./assets/drag-tab-top.svg",
                    Edge::Top,
                    Style::new()
                        .top(tab_inset)
                        .left(drag_tab_x)
                        .width(DRAG_TAB_LENGTH)
                        .height(DRAG_TAB_THICKNESS),
                ))
                .child(tab(
                    "./assets/drag-tab-bottom.svg",
                    Edge::Bottom,
                    Style::new()
                        .bottom(tab_inset)
                        .left(drag_tab_x)
                        .width(DRAG_TAB_LENGTH)
                        .height(DRAG_TAB_THICKNESS),
                ))
                .child(tab(
                    "./assets/drag-tab-left.svg",
                    Edge::Left,
                    Style::new()
                        .left(tab_inset)
                        .top(drag_tab_y)
                        .width(DRAG_TAB_THICKNESS)
                        .height(DRAG_TAB_LENGTH),
                ))
                .child(tab(
                    "./assets/drag-tab-right.svg",
                    Edge::Right,
                    Style::new()
                        .right(tab_inset)
                        .top(drag_tab_y)
                        .width(DRAG_TAB_THICKNESS)
                        .height(DRAG_TAB_LENGTH),
                )),
        )
    }
}
