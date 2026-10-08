//! Backdrop blur for layers and popups, plus the surface policies that keep
//! blending correct. Shader paths are relative to the config package root.

use shojiwm_rs::{
    prelude::*,
    ssd::{OpaqueRegionPolicy, PopupParentKindSnapshot, SurfacePolicy},
};

use crate::window_manager::{WINDOW_STATE_MINIMIZE_VISUAL_IDLE, WINDOW_STATE_MINIMIZED};

/// Chromium-family clients repaint their CSD shadow margins as transparent
/// black, while still declaring the whole surface opaque, the moment they
/// send set_minimized, assuming the surface will never be shown again.
/// Honoring that declaration skips blending and paints the margins as a
/// solid black ring during the minimize animation.
fn is_chromium_family(app_id: &str) -> bool {
    let id = app_id.to_lowercase();
    ["chrome", "chromium", "electron"]
        .iter()
        .any(|name| id.contains(name))
}

/// The blur clipped to the surface's own alpha: the mask stage outputs
/// transparency, so the pipeline's alpha has to survive the finish and
/// display passes instead of being forced opaque.
fn masked_blur(mask: Source, capture_padding: i32) -> Effect {
    Effect::new(backdrop_source())
        .capture_padding(capture_padding)
        .invalidate(Invalidate::on_source_damage_box(8))
        .preserve_alpha()
        .stage(dual_kawase_blur(4, 2))
        .stage(
            shader_stage("./src/effect/layer-blur-mask.frag")
                .texture("layer_mask", mask)
                .uniform("opacity_threshold", 0.25)
                .uniform("mask_feather", 0.04),
        )
}

/// Whether a blur behind `layer` can show. It costs a backdrop capture and a
/// 2-pass blur on every commit of that layer, so it is skipped where nothing
/// shows through: the Background layer, which has only the clear colour behind
/// it, and surfaces that cover a whole output (the wallpaper, MenuBackdrop's
/// click catcher, the start menu's 0.96 sheet, MinkaMon's opaque pad, the
/// leader-line and capture overlays). MinkaFX keeps it: its snap preview is a
/// translucent fill meant to frost what it covers, and it only gets here while
/// shown, since it sinks to the Background layer when idle.
fn wants_layer_blur(layer: &shojiwm_rs::ssd::WaylandLayerSnapshot) -> bool {
    let namespace = layer.namespace.as_deref();
    if namespace == Some("no_blur") || layer.layer == shojiwm_rs::ssd::LayerKindSnapshot::Background {
        return false;
    }
    let anchor = layer.anchor;
    let covers_output = anchor.top && anchor.bottom && anchor.left && anchor.right;
    !covers_output || namespace == Some("minka-fx")
}

pub fn configure_rendering() {
    COMPOSITOR.effect.background(
        Effect::new(backdrop_source())
            .capture_padding(24)
            .invalidate(Invalidate::on_source_damage_box(8))
            .stage(dual_kawase_blur(4, 2)),
    );

    let layer_blur_mask = SurfaceEffect::new(masked_blur(layer_source(), 24));
    COMPOSITOR.effect.layer(move |layer| {
        if wants_layer_blur(layer) {
            SurfaceEffects::behind(layer_blur_mask.clone())
        } else {
            SurfaceEffects::none()
        }
    });

    let popup_blur = SurfaceEffect::new(masked_blur(popup_source(), 4 * 2 * 2 + 24 + 32));
    COMPOSITOR.effect.popup(move |popup| {
        if popup.parent_kind == PopupParentKindSnapshot::Window {
            SurfaceEffects::none()
        } else {
            SurfaceEffects::behind(popup_blur.clone())
        }
    });

    // GTK3 tooltips (waybar) declare their whole rect opaque despite
    // transparent rounded corners, which paints the corners as a solid fill
    // and culls the behind-blur: ignore the declaration for layer-shell
    // popups.
    COMPOSITOR.rendering.surface_policy(|surface| {
        let ignore = Some(SurfacePolicy {
            opaque_region: OpaqueRegionPolicy::Ignore,
        });
        match surface {
            SurfaceRef::Popup {
                parent_kind: PopupParentKindSnapshot::Layer,
                ..
            } => ignore,
            // Minimized only: the restore animation fades in from opacity 0,
            // so the few frames where a stale black-margin buffer could still
            // be on screen after unminimize are effectively invisible.
            SurfaceRef::Toplevel(window) => {
                let chromium = is_chromium_family(&window.app_id().get().unwrap_or_default());
                let minimized = window.state(&WINDOW_STATE_MINIMIZED).get()
                    || window.state(&WINDOW_STATE_MINIMIZE_VISUAL_IDLE).get();
                (chromium && minimized).then_some(ignore).flatten()
            }
            SurfaceRef::Popup { .. } => None,
        }
    });
}

#[cfg(test)]
mod tests {
    use super::wants_layer_blur;
    use shojiwm_rs::ssd::{
        LayerKindSnapshot, LayerPositionSnapshot, WaylandLayerSnapshot,
        window_model::{
            KeyboardInteractivitySnapshot, LayerAnchorSnapshot, LayerExclusiveZoneSnapshot,
            LayerMarginSnapshot,
        },
    };

    fn layer(namespace: &str, kind: LayerKindSnapshot, edges: [bool; 4]) -> WaylandLayerSnapshot {
        let [top, bottom, left, right] = edges;
        WaylandLayerSnapshot {
            id: "layer".into(),
            namespace: Some(namespace.into()),
            layer: kind,
            output_name: "eDP-1".into(),
            position: LayerPositionSnapshot {
                x: 0,
                y: 0,
                width: 1536,
                height: 864,
            },
            anchor: LayerAnchorSnapshot {
                top,
                bottom,
                left,
                right,
            },
            exclusive_zone: LayerExclusiveZoneSnapshot::Neutral,
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

    const ALL_EDGES: [bool; 4] = [true; 4];

    #[test]
    fn blurs_panels_and_popover_layers() {
        // A bar or side panel: anchored to three edges at most.
        assert!(wants_layer_blur(&layer("quickshell", LayerKindSnapshot::Top, [true, false, true, true])));
        assert!(wants_layer_blur(&layer("quickshell", LayerKindSnapshot::Overlay, [false; 4])));
    }

    #[test]
    fn skips_layers_nothing_shows_through() {
        // The wallpaper, and anything else on the Background layer.
        assert!(!wants_layer_blur(&layer("quickshell", LayerKindSnapshot::Background, ALL_EDGES)));
        // Full-output click catchers, sheets and overlays.
        assert!(!wants_layer_blur(&layer("quickshell", LayerKindSnapshot::Overlay, ALL_EDGES)));
        assert!(!wants_layer_blur(&layer("minkamon-leaderlines", LayerKindSnapshot::Top, ALL_EDGES)));
        // The explicit opt-out still wins.
        assert!(!wants_layer_blur(&layer("no_blur", LayerKindSnapshot::Top, [true, false, true, true])));
    }

    #[test]
    fn minka_fx_keeps_its_frost_only_while_shown() {
        assert!(wants_layer_blur(&layer("minka-fx", LayerKindSnapshot::Overlay, ALL_EDGES)));
        assert!(!wants_layer_blur(&layer("minka-fx", LayerKindSnapshot::Background, ALL_EDGES)));
    }
}
