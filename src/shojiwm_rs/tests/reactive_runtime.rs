//! Drives the reactive Rust runtime through the same `RuntimeHandle` the
//! compositor uses, and checks that signal changes come back as the minimal
//! updates the compositor expects.

use shojiwm_rs::{
    HostMessage, RuntimeBoot, RuntimeHandle, RuntimeHost, cli::CommonArgs, prelude::*,
    runtime_api::CompositionPatch,
    ssd::{
        DecorationNode, DecorationNodeKind, PopupNode, WaylandWindowAction, WaylandWindowSnapshot,
        WindowAction,
    },
};

/// The hang watchdog only logs here: a test must never abort the test binary.
fn config(setup: impl Fn() + 'static) -> ConfigBuilder {
    ConfigBuilder::new(setup).hang_watchdog(HangWatchdog::log_only())
}

static WIDTH: WindowStateKey<f64> = WindowStateKey::new("width", |_| 400.0);

fn snapshot(id: &str, title: &str, focused: bool) -> WaylandWindowSnapshot {
    WaylandWindowSnapshot {
        id: id.into(),
        title: title.into(),
        app_id: Some("test".into()),
        position: Default::default(),
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

fn close_button(window: Window) -> Element {
    let hover = signal(false);
    let border = hover.map(|hover| if *hover { hex("#00000000") } else { hex("#F0808030") });
    Flex::row()
        .style(Style::new().relative())
        .child(
            Button::new()
                .on_hover_change(move |value| hover.set(value))
                .on_click(move || window.close())
                .style(Style::new().size(16.0, 16.0).border(1.0, border)),
        )
        .child_dyn(move || {
            hover
                .get()
                .then(|| Image::new("/assets/x.svg").style(Style::new().absolute()))
        })
}

fn setup() {
    COMPOSITOR.key.bind("grow", "Super+G", || {
        if let Some(window) = COMPOSITOR.window.list().first() {
            window.state(&WIDTH).update(|width| *width += 100.0);
        }
    });
    COMPOSITOR.key.bind("glow", "Super+H", || {
        GLOW.with(|glow| glow.set(1.0));
    });
    COMPOSITOR.window.composition(|window| {
        let width = window.state(&WIDTH);
        let border = window
            .is_focused()
            .map(|focused| if *focused { hex("#d7ba7d") } else { hex("#4f5666") });
        let glow = GLOW.with(|glow| *glow);
        ManagedWindow::new()
            .rect(derive(move || Rect::new(0.0, 0.0, width.get(), 300.0)))
            .child(
                WindowBorder::new()
                    .style(
                        Style::new()
                            .border(2.0, border)
                            .box_shadow(shadow(0.0, 4.0, 12.0, hex("#00000060"))),
                    )
                    .overlay(paint_shader("/ring.frag").uniform("glow", glow))
                    .child(
                        Flex::column()
                            .child(
                                ShaderEffect::new(
                                    Effect::new(backdrop_source())
                                        .stage(dual_kawase_blur(4, 2))
                                        .stage(shader_stage("/glass.frag").uniform("glow", glow)),
                                )
                                .direction(Direction::Row)
                                .child(Label::new(window.title()))
                                .child(close_button(window)),
                            )
                            .child(ClientWindow::new()),
                    ),
            )
    });
}

thread_local! {
    static GLOW: Signal<f64> = signal(0.0);
}

fn start() -> (RuntimeHandle, RuntimeHost) {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime =
        RuntimeBoot::new(Box::new(config(setup)), &args).launch(host.clone());
    runtime.preload().unwrap();
    runtime.enable().unwrap();
    (runtime, host)
}

fn find<'a>(node: &'a DecorationNode, predicate: &dyn Fn(&DecorationNode) -> bool) -> Option<&'a DecorationNode> {
    if predicate(node) {
        return Some(node);
    }
    node.children.iter().find_map(|child| find(child, predicate))
}

fn label_text(node: &DecorationNode) -> Option<String> {
    find(node, &|node| matches!(node.kind, DecorationNodeKind::Label(_))).map(|node| match &node.kind {
        DecorationNodeKind::Label(label) => label.text.clone(),
        _ => unreachable!(),
    })
}

#[test]
fn reactive_config_round_trip() {
    let (mut runtime, host) = start();
    let bindings: Vec<String> = std::iter::from_fn(|| host.pop())
        .filter_map(|message| match message {
            HostMessage::KeyBindings(update) => Some(update.entries.into_iter().map(|entry| entry.id).collect::<Vec<_>>()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(bindings, ["grow", "glow"]);

    // First evaluation: a full tree with the title and the managed rect.
    let evaluation = runtime.evaluate_window(&snapshot("w1", "hello", true), 1).unwrap();
    assert_eq!(label_text(&evaluation.node).as_deref(), Some("hello"));
    assert_eq!(evaluation.managed_window.rect.unwrap().width, 400.0);
    let border = find(&evaluation.node, &|node| matches!(node.kind, DecorationNodeKind::WindowBorder)).unwrap();
    assert_eq!(border.style.border.unwrap().color, hex("#d7ba7d"));
    assert_eq!(border.style.box_shadow, vec![shadow(0.0, 4.0, 12.0, hex("#00000060"))]);
    let overlay = border.style.overlay.as_ref().expect("overlay paint shader");
    assert!(overlay.shader.path.ends_with("ring.frag"));
    let button = find(&evaluation.node, &|node| matches!(node.kind, DecorationNodeKind::Button(_))).unwrap();
    let hover = button.interaction.hover_change.clone().unwrap();
    let DecorationNodeKind::Button(button_node) = &button.kind else {
        unreachable!()
    };
    let WindowAction::RuntimeHandler(click) = &button_node.action else {
        panic!("click should be a runtime handler")
    };
    let click = click.clone();
    assert!(find(&evaluation.node, &|node| matches!(node.kind, DecorationNodeKind::Image(_))).is_none());

    // Hover: only the button row is re-sent, now with the icon.
    let invocation = runtime.invoke_handler("w1", &hover.true_handler, 2).unwrap();
    assert!(invocation.invoked);
    assert_eq!(invocation.dirty_window_ids, ["w1"]);
    let cached = runtime.evaluate_cached_window("w1", None, 3, false).unwrap();
    assert!(cached.node.is_none());
    assert!(!cached.managed_window_only);
    let replaced: Vec<&str> = cached.node_patches.iter().map(CompositionPatch::node_id).collect();
    // The button (border colour) sits inside the row (dynamic icon): one
    // replacement of the row covers both.
    assert_eq!(replaced.len(), 1, "{replaced:?}");
    let row = cached
        .node_patches
        .iter()
        .find_map(|patch| patch.replacement_node().filter(|node| node.children.len() == 2))
        .expect("row patch");
    assert!(matches!(row.children[1].kind, DecorationNodeKind::Image(_)));

    // Nothing changed since: an empty update.
    let cached = runtime.evaluate_cached_window("w1", None, 4, false).unwrap();
    assert!(cached.node_patches.is_empty() && cached.node.is_none());

    // Click: the close action comes back with the reply.
    let invocation = runtime.invoke_handler("w1", &click, 5).unwrap();
    assert!(matches!(invocation.actions.as_slice(), [action] if action.action == WaylandWindowAction::Close));

    // Focus and title flow in through snapshots.
    let evaluation = runtime.evaluate_window(&snapshot("w1", "renamed", false), 6).unwrap();
    assert_eq!(label_text(&evaluation.node).as_deref(), Some("renamed"));
    let border = find(&evaluation.node, &|node| matches!(node.kind, DecorationNodeKind::WindowBorder)).unwrap();
    assert_eq!(border.style.border.unwrap().color, hex("#4f5666"));

    // A key binding that only touches the managed rect.
    let invocation = runtime.invoke_key_binding("grow", 7).unwrap();
    assert!(invocation.invoked);
    assert_eq!(invocation.dirty_managed_window_ids, ["w1"]);
    let cached = runtime.evaluate_cached_window("w1", None, 8, false).unwrap();
    assert!(cached.managed_window_only);
    assert_eq!(cached.managed_window.rect.unwrap().width, 500.0);

    // A reactive uniform: a single-uniform patch, no node rebuild.
    let invocation = runtime.invoke_key_binding("glow", 9).unwrap();
    assert!(invocation.dirty_window_node_ids.contains_key("w1"));
    let cached = runtime.evaluate_cached_window("w1", None, 10, false).unwrap();
    assert_eq!(cached.node_patches.len(), 2, "{:?}", cached.node_patches);
    for stage in [1, shojiwm_rs::runtime_api::OVERLAY_STAGE_INDEX] {
        assert!(
            cached.node_patches.iter().any(|patch| matches!(
                patch,
                CompositionPatch::ShaderUniform {
                    name,
                    stage_index,
                    value: shojiwm_rs::ssd::ShaderUniformValue::Float(value),
                    ..
                } if name == "glow" && *value == 1.0 && *stage_index == stage
            )),
            "uniform patch of stage {stage}: {:?}",
            cached.node_patches
        );
    }

    // Closing.
    let invocation = runtime.start_close("w1", 11).unwrap();
    assert!(invocation.invoked);
    assert!(invocation.actions.iter().any(|action| action.action == WaylandWindowAction::FinalizeClose));
    runtime.window_closed("w1").unwrap();
    assert!(runtime.evaluate_cached_window("w1", None, 12, false).is_err());
}

#[test]
fn timers_and_animations_drive_the_scheduler() {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(
        Box::new(config(|| {
            let progress = Scope::root().run(|| Animation::new(0.0));
            COMPOSITOR.key.bind("animate", "Super+A", move || {
                progress.start(AnimationOptions::to(1.0, 100.0));
            });
            COMPOSITOR.window.composition(move |_| {
                ManagedWindow::new()
                    .opacity(progress)
                    .child(ClientWindow::new())
            });
        })),
        &args,
    )
    .launch(host);
    runtime.preload().unwrap();
    runtime.enable().unwrap();
    runtime.evaluate_window(&snapshot("w1", "a", true), 1000).unwrap();
    let invocation = runtime.invoke_key_binding("animate", 1000).unwrap();
    assert_eq!(invocation.next_poll_in_ms, Some(0));
    let tick = runtime.scheduler_tick(1050.0).unwrap();
    assert_eq!(tick.dirty_managed_window_ids, ["w1"]);
    let cached = runtime.evaluate_cached_window("w1", None, 1050, false).unwrap();
    assert!((cached.transform.opacity - 0.5).abs() < 1e-6);
    let tick = runtime.scheduler_tick(1100.0).unwrap();
    assert_eq!(tick.next_poll_in_ms, None);
}

#[test]
fn hovering_a_button_opens_its_popup() {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(
        Box::new(config(|| {
            COMPOSITOR.window.composition(|_| {
                let hover = signal(false);
                ManagedWindow::new().child(
                    Flex::column()
                        .child(
                            Button::new().on_hover_change(move |on| hover.set(on)).child(
                                Popup::new()
                                    .open(hover)
                                    .placement(PopupPlacement::Top)
                                    .offset(6.0)
                                    .layer(PopupLayer::Window)
                                    .child(Label::new("Maximize")),
                            ),
                        )
                        .child(ClientWindow::new()),
                )
            });
        })),
        &args,
    )
    .launch(host);
    runtime.preload().unwrap();
    runtime.enable().unwrap();

    let popup_of = |node: &DecorationNode| -> PopupNode {
        match find(node, &|node| matches!(node.kind, DecorationNodeKind::Popup(_))).map(|node| &node.kind) {
            Some(DecorationNodeKind::Popup(popup)) => *popup,
            _ => panic!("no popup in {node:?}"),
        }
    };
    let evaluation = runtime.evaluate_window(&snapshot("w1", "a", true), 1).unwrap();
    let popup = popup_of(&evaluation.node);
    assert_eq!(
        popup,
        PopupNode {
            open: false,
            placement: PopupPlacement::Top,
            offset: 6.0,
            layer: PopupLayer::Window,
            ..PopupNode::default()
        }
    );
    let button = find(&evaluation.node, &|node| matches!(node.kind, DecorationNodeKind::Button(_))).unwrap();
    let hover = button.interaction.hover_change.clone().unwrap();

    runtime.invoke_handler("w1", &hover.true_handler, 2).unwrap();
    let cached = runtime.evaluate_cached_window("w1", None, 3, false).unwrap();
    assert_eq!(cached.node_patches.len(), 1, "{:?}", cached.node_patches);
    let patched = cached.node_patches[0].replacement_node().expect("node patch");
    assert!(popup_of(patched).open);
}

#[test]
fn popup_triggers_follow_compositor_events() {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(
        Box::new(config(|| {
            COMPOSITOR.window.composition(|_| {
                ManagedWindow::new().child(
                    Flex::column()
                        .child(
                            Button::new().child(
                                Popup::new()
                                    .mode(PopupMode::Auto)
                                    .trigger(PopupTrigger::hover(300.0, 100.0))
                                    .child(Button::new().on_click(|| {})),
                            ),
                        )
                        .child(ClientWindow::new()),
                )
            });
        })),
        &args,
    )
    .launch(host);
    runtime.preload().unwrap();
    runtime.enable().unwrap();

    fn popup_state(node: &DecorationNode) -> Option<(PopupNode, shojiwm_rs::ssd::PopupHandlers)> {
        find(node, &|node| matches!(node.kind, DecorationNodeKind::Popup(_))).map(|node| match &node.kind {
            DecorationNodeKind::Popup(popup) => (*popup, node.interaction.popup.as_deref().cloned().unwrap_or_default()),
            _ => unreachable!(),
        })
    }
    let open_after = |runtime: &mut RuntimeHandle, now: u64| {
        runtime.scheduler_tick(now as f64).unwrap();
        let cached = runtime.evaluate_cached_window("w1", None, now, false).unwrap();
        cached
            .node
            .iter()
            .chain(cached.node_patches.iter().filter_map(|patch| patch.replacement_node()))
            .find_map(popup_state)
            .map(|(popup, _)| popup.open)
    };

    let evaluation = runtime.evaluate_window(&snapshot("w1", "a", true), 1000).unwrap();
    let (popup, handlers) = popup_state(&evaluation.node).unwrap();
    assert_eq!((popup.mode, popup.open), (PopupMode::Auto, false));
    let interest = handlers.interest_change.expect("hover trigger listens to interest");
    let dismiss = handlers.dismiss.expect("auto popups listen to close requests");

    runtime.invoke_handler("w1", &interest.true_handler, 1000).unwrap();
    assert_ne!(open_after(&mut runtime, 1200), Some(true), "opened before the delay");
    assert_eq!(open_after(&mut runtime, 1300), Some(true));

    // Leaving and coming back within the close delay keeps it open.
    runtime.invoke_handler("w1", &interest.false_handler, 1400).unwrap();
    runtime.invoke_handler("w1", &interest.true_handler, 1450).unwrap();
    assert_ne!(open_after(&mut runtime, 1600), Some(false));

    // A close request closes it at once.
    runtime
        .invoke_handler("w1", dismiss.handler_for(PopupDismissReason::Escape), 1700)
        .unwrap();
    assert_eq!(open_after(&mut runtime, 1700), Some(false));
}

fn layer(id: &str, output: &str, exclusive: u32) -> shojiwm_rs::ssd::WaylandLayerSnapshot {
    use shojiwm_rs::ssd::{
        LayerKindSnapshot, LayerPositionSnapshot,
        window_model::{
            KeyboardInteractivitySnapshot, LayerAnchorSnapshot, LayerExclusiveZoneSnapshot,
            LayerMarginSnapshot,
        },
    };
    shojiwm_rs::ssd::WaylandLayerSnapshot {
        id: id.into(),
        namespace: Some("bar".into()),
        layer: LayerKindSnapshot::Top,
        output_name: output.into(),
        position: LayerPositionSnapshot {
            x: 0,
            y: 0,
            width: 1920,
            height: exclusive as i32,
        },
        anchor: LayerAnchorSnapshot {
            top: true,
            bottom: false,
            left: true,
            right: true,
        },
        exclusive_zone: LayerExclusiveZoneSnapshot::Exclusive { size: exclusive },
        exclusive_edge: None,
        margin: LayerMarginSnapshot {
            top: 0,
            right: 0,
            bottom: 0,
            left: 0,
        },
        keyboard_interactivity: KeyboardInteractivitySnapshot::None,
        desired_size: Default::default(),
    }
}

/// Layer events report what moves the usable area, from every output's
/// layers at once; `layer.state()` still sees every change.
#[test]
fn layer_events_follow_the_usable_area_across_outputs() {
    use std::{cell::RefCell, rc::Rc};

    use shojiwm_rs::ssd::window_model::KeyboardInteractivitySnapshot;

    let events: Rc<RefCell<Vec<String>>> = Rc::default();
    let recorded = events.clone();
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(
        Box::new(config(move || {
            let log = |what: &'static str| {
                let recorded = recorded.clone();
                move |layer: &shojiwm_rs::ssd::WaylandLayerSnapshot| {
                    recorded.borrow_mut().push(format!("{what} {}", layer.id))
                }
            };
            COMPOSITOR.event.on_create_layer(log("create"));
            COMPOSITOR.event.on_update_layer(log("update"));
            COMPOSITOR.event.on_destroy_layer(log("destroy"));
        })),
        &args,
    )
    .launch(host);
    runtime.preload().unwrap();
    runtime.enable().unwrap();
    let take = || std::mem::take(&mut *events.borrow_mut());

    let bar = layer("bar", "DP-1", 30);
    let tv_bar = layer("tv-bar", "HDMI-A-1", 40);
    runtime
        .evaluate_layer_effects("DP-1", &[bar.clone(), tv_bar.clone()], 0)
        .unwrap();
    assert_eq!(take(), ["create bar", "create tv-bar"]);

    // A popover opening resizes the bar and takes the keyboard: the usable
    // area is the same, so no update, but the state follows.
    let mut resized = bar.clone();
    resized.position.width = 1200;
    resized.keyboard_interactivity = KeyboardInteractivitySnapshot::OnDemand;
    runtime
        .evaluate_layer_effects("DP-1", &[resized.clone(), tv_bar.clone()], 0)
        .unwrap();
    assert_eq!(take(), Vec::<String>::new());
    assert_eq!(COMPOSITOR.layer.list().iter().find(|layer| layer.id == "bar").unwrap().position.width, 1200);

    // A taller exclusive zone moves the usable area.
    runtime
        .evaluate_layer_effects("DP-1", &[layer("bar", "DP-1", 48), tv_bar.clone()], 0)
        .unwrap();
    assert_eq!(take(), ["update bar"]);

    // The TV goes away: its bar is gone from a DP-1 request too.
    runtime
        .evaluate_layer_effects("DP-1", &[layer("bar", "DP-1", 48)], 0)
        .unwrap();
    assert_eq!(take(), ["destroy tv-bar"]);
    assert_eq!(COMPOSITOR.layer.list().len(), 1);
}

/// A window the compositor re-seeds after a spurious close (outputs going
/// away) comes back without `on_open`, which a window manager answers by
/// raising and focusing it.
#[test]
fn a_reseeded_window_is_not_opened_again() {
    use std::{cell::RefCell, rc::Rc};

    let events: Rc<RefCell<Vec<String>>> = Rc::default();
    let recorded = events.clone();
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(
        Box::new(config(move || {
            let log = |what: &'static str| {
                let recorded = recorded.clone();
                move |window: Window| recorded.borrow_mut().push(format!("{what} {}", window.id()))
            };
            COMPOSITOR.event.on_open(log("open"));
            COMPOSITOR.event.on_first_commit(log("first-commit"));
            let recorded = recorded.clone();
            COMPOSITOR.event.on_focus(move |window, focused| {
                recorded.borrow_mut().push(format!("focus {} {focused}", window.id()))
            });
            COMPOSITOR.window.composition(|_| ManagedWindow::new().child(ClientWindow::new()));
        })),
        &args,
    )
    .launch(host);
    runtime.preload().unwrap();
    runtime.enable().unwrap();
    let take = || std::mem::take(&mut *events.borrow_mut());

    let window = snapshot("w1", "a", false);
    runtime.evaluate_window(&window, 1).unwrap();
    assert_eq!(take(), ["open w1", "focus w1 false", "first-commit w1"]);

    runtime.window_closed("w1").unwrap();
    runtime.evaluate_cached_window("w1", Some(&window), 2, true).unwrap();
    assert_eq!(take(), ["focus w1 false", "first-commit w1"]);
}

/// What an evaluation's own listeners do reaches the compositor: actions in
/// the reply, and other windows' changes through a tick it asks for.
#[test]
fn changes_made_while_evaluating_are_not_stranded() {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(
        Box::new(config(|| {
            COMPOSITOR.event.on_focus(|window, focused| {
                if !focused {
                    return;
                }
                window.focus();
                for other in COMPOSITOR.window.list() {
                    if other != window {
                        other.state(&WIDTH).update(|width| *width += 10.0);
                    }
                }
            });
            COMPOSITOR.window.composition(|window| {
                let width = window.state(&WIDTH);
                ManagedWindow::new()
                    .rect(derive(move || Rect::new(0.0, 0.0, width.get(), 300.0)))
                    .child(ClientWindow::new())
            });
        })),
        &args,
    )
    .launch(host.clone());
    runtime.preload().unwrap();
    runtime.enable().unwrap();
    runtime.evaluate_window(&snapshot("w1", "a", false), 1).unwrap();
    runtime.evaluate_window(&snapshot("w2", "b", false), 2).unwrap();
    runtime.scheduler_tick(3.0).unwrap();
    host.take_wake_request();

    // The compositor's cached re-evaluation of w1 gaining focus.
    let cached = runtime
        .evaluate_cached_window("w1", Some(&snapshot("w1", "a", true)), 4, true)
        .unwrap();
    assert!(
        cached
            .actions
            .iter()
            .any(|action| action.window_id == "w1" && action.action == WaylandWindowAction::Focus),
        "{:?}",
        cached.actions
    );
    assert!(host.take_wake_request(), "w2 changed, and no reply said so");

    let tick = runtime.scheduler_tick(5.0).unwrap();
    assert_eq!(tick.dirty_managed_window_ids, ["w2"]);
    assert!(!host.take_wake_request(), "the tick carried it all");
}

/// A window's own changes go out in its evaluation's reply, so they ask for
/// no tick.
#[test]
fn an_evaluation_asks_no_tick_for_its_own_window() {
    let host = RuntimeHost::detached();
    let args = CommonArgs::parse(&[], &[]);
    let mut runtime = RuntimeBoot::new(
        Box::new(config(|| {
            COMPOSITOR.window.composition(|window| {
                let focused = window.is_focused();
                ManagedWindow::new()
                    .rect(derive(move || {
                        Rect::new(0.0, 0.0, if focused.get() { 400.0 } else { 300.0 }, 300.0)
                    }))
                    .child(ClientWindow::new())
            });
        })),
        &args,
    )
    .launch(host.clone());
    runtime.preload().unwrap();
    runtime.enable().unwrap();
    runtime.evaluate_window(&snapshot("w1", "a", false), 1).unwrap();
    runtime.scheduler_tick(2.0).unwrap();
    host.take_wake_request();

    runtime.evaluate_cached_window("w1", Some(&snapshot("w1", "a", true)), 3, true).unwrap();
    assert!(!host.take_wake_request(), "the reply carried w1's change");
    let tick = runtime.scheduler_tick(4.0).unwrap();
    assert!(tick.dirty_managed_window_ids.is_empty(), "{:?}", tick.dirty_managed_window_ids);
}
