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

pub fn configure_rendering() {
    COMPOSITOR.effect.background(
        Effect::new(backdrop_source())
            .capture_padding(24)
            .invalidate(Invalidate::on_source_damage_box(8))
            .stage(dual_kawase_blur(4, 2)),
    );

    let layer_blur_mask = SurfaceEffect::new(masked_blur(layer_source(), 24));
    COMPOSITOR.effect.layer(move |layer| {
        if layer.namespace.as_deref() == Some("no_blur") {
            SurfaceEffects::none()
        } else {
            SurfaceEffects::behind(layer_blur_mask.clone())
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
