import {
  COMPOSITOR,
  backdropSource,
  compileEffect,
  compileLayerEffect,
  compilePopupEffect,
  dualKawaseBlur,
  layerSource,
  loadShader,
  popupSource,
  shaderStage,
} from "shoji_wm";
import {
  WINDOW_STATE_MINIMIZED,
  WINDOW_STATE_MINIMIZE_VISUAL_IDLE,
} from "../window-manager";

// Chromium-family clients repaint their CSD shadow margins as transparent
// black — while still declaring the whole surface opaque — the moment they
// send set_minimized, assuming the surface will never be shown again. Honoring
// that declaration skips blending and paints the margins as a solid black
// ring during the minimize animation.
const isChromiumFamily = (appId: string): boolean => {
  const id = appId.toLowerCase();
  return (
    id.includes("chrome") || id.includes("chromium") || id.includes("electron")
  );
};

// Backdrop blur for layers and popups, plus the surface policies that keep
// blending correct. Shader paths are relative to the config package root.
export function configureRendering(): void {
  COMPOSITOR.effect.background_effect = compileEffect({
    input: backdropSource(),
    capturePadding: 24,
    invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
    pipeline: [dualKawaseBlur({ radius: 4, passes: 2 })],
  });

  const LAYER_BLUR_MASK = compileLayerEffect({
    input: backdropSource(),
    capturePadding: 24,
    invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
    // The mask stage intentionally outputs transparency (the blur is clipped
    // to the layer's own alpha), so the pipeline's alpha must survive the
    // finish/display passes instead of being forced opaque.
    alpha: "preserve",
    pipeline: [
      dualKawaseBlur({ radius: 4, passes: 2 }),
      shaderStage(loadShader("./src/effect/layer-blur-mask.frag"), {
        textures: {
          layer_mask: layerSource(),
        },
        uniforms: {
          opacity_threshold: 0.25,
          mask_feather: 0.04,
        },
      }),
    ],
  });

  // A blur behind a layer costs a backdrop capture and a 2-pass blur on every
  // commit of that layer, so skip it where nothing can show through: the
  // Background layer, which has only the clear colour behind it, and surfaces
  // that cover a whole output (the wallpaper, MenuBackdrop's click catcher, the
  // start menu's 0.96 sheet, MinkaMon's opaque pad, the leader-line and capture
  // overlays). MinkaFX keeps it: its snap preview is a translucent fill that is
  // meant to frost what it covers, and it only reaches here while it is shown,
  // since it sinks to the Background layer when idle.
  COMPOSITOR.effect.layer = (layer) => {
    if (layer.namespace() === "no_blur" || layer.layer() === "background") {
      return {};
    }
    const anchor = layer.anchor();
    const coversOutput = anchor.top && anchor.bottom && anchor.left && anchor.right;
    if (coversOutput && layer.namespace() !== "minka-fx") {
      return {};
    }

    return {
      behind: LAYER_BLUR_MASK,
    };
  };

  const POPUP_BLUR = compilePopupEffect({
    input: backdropSource(),
    capturePadding: 4 * 2 * 2 + 24 + 32,
    invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
    // The mask stage intentionally outputs transparency (the blur is clipped
    // to the layer's own alpha), so the pipeline's alpha must survive the
    // finish/display passes instead of being forced opaque.
    alpha: "preserve",
    pipeline: [
      dualKawaseBlur({ radius: 4, passes: 2 }),
      shaderStage(loadShader("./src/effect/layer-blur-mask.frag"), {
        textures: {
          layer_mask: popupSource(),
        },
        uniforms: {
          opacity_threshold: 0.25,
          mask_feather: 0.04,
        },
      }),
    ],
  });

  COMPOSITOR.effect.popup = (popup) => {
    if (popup.parentKind === "window") {
      return {};
    }

    return {
      behind: POPUP_BLUR,
    };
  };

  // GTK3 tooltips (waybar) declare their whole rect opaque despite transparent
  // rounded corners, which paints the corners as a solid fill and culls the
  // behind-blur. Ignore the declaration for layer-shell popups.
  COMPOSITOR.rendering.surfacePolicy = (surface) => {
    if (surface.kind === "popup" && surface.parentKind === "layer") {
      return { opaqueRegion: "ignore" };
    }
    if (surface.kind === "toplevel") {
      const window = surface.window;
      // Minimized only: the restore animation fades in from opacity 0, so the
      // few frames where a stale black-margin buffer could still be on screen
      // after unminimize are effectively invisible.
      if (
        isChromiumFamily(window.appId() ?? "") &&
        (window.state[WINDOW_STATE_MINIMIZED]() ||
          window.state[WINDOW_STATE_MINIMIZE_VISUAL_IDLE]())
      ) {
        return { opaqueRegion: "ignore" };
      }
    }
    return null;
  };
}
