---
sidebar_position: 10
---

# Effects

ShojiWM can run GPU shader effects in four places, configured via
`COMPOSITOR.effect`:

| Field | Type | Applies to |
| --- | --- | --- |
| `background_effect` | `CompiledEffectHandle \| null` | Full-screen background, beneath all windows |
| `window` | `(window) => WindowEffectAssignment \| null` | Per toplevel window |
| `layer` | `(layer) => LayerEffectAssignment \| null` | Per layer-shell surface (bars, docks) |
| `popup` | `(popup) => PopupEffectAssignment \| null` | Per popup (menus, tooltips) |

You can also apply an effect to a region inside the composition with
[`<ShaderEffect/>`](./components.md#shadereffect).

## Background effect

Assign a compiled effect that renders behind everything. Set `null` to disable.

```ts
import {COMPOSITOR, compileEffect, backdropSource, dualKawaseBlur} from 'shoji_wm';

COMPOSITOR.effect.background_effect = compileEffect({
  input: backdropSource(),
  invalidate: {kind: 'on-source-damage-box', antiArtifactMargin: 8},
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});
```

## Per-window / layer / popup effects

Each factory is called per surface and returns an assignment, or `null`/`{}` for
no effect. Layer and popup assignments use `behind` to render the effect beneath
the surface (the default config blurs everything behind bars and menus):

```ts
const LAYER_BLUR = compileLayerEffect({
  input: backdropSource(),
  alpha: 'preserve',
  pipeline: [dualKawaseBlur({radius: 4, passes: 2})],
});

COMPOSITOR.effect.layer = (layer) => {
  if (layer.namespace() === 'no_blur') return {};
  return {behind: LAYER_BLUR};
};

COMPOSITOR.effect.popup = (popup) => {
  if (popup.parentKind === 'window') return {};
  return {behind: POPUP_BLUR};
};
```

## Building an effect

An effect is **a source input + a pipeline of stages**. Compile it with the
function matching where it will be used:

| Compiler | Produces | For |
| --- | --- | --- |
| `compileEffect(opts)` | `CompiledEffectHandle` | background, `<ShaderEffect/>` |
| `compileWindowEffect(opts)` | `WindowEffectHandle` | `COMPOSITOR.effect.window` |
| `compileLayerEffect(opts)` | `LayerEffectHandle` | `COMPOSITOR.effect.layer` |
| `compilePopupEffect(opts)` | `PopupEffectHandle` | `COMPOSITOR.effect.popup` |

Options:

| Option | Type | Meaning |
| --- | --- | --- |
| `input` | source handle | What the pipeline reads from (e.g. `backdropSource()`) |
| `pipeline` | stage array | Stages applied in order |
| `invalidate` | policy | When to re-render (see below) |
| `alpha` | `"opaque" \| "preserve"` | Keep transparency through to display (default `"opaque"`) |
| `outsets` | `EffectOutsets` | (window effects) render beyond the window bounds |

### Sources

| Source | Reads |
| --- | --- |
| `backdropSource()` | The composited scene behind the target |
| `windowSource()` | The window's own surface |
| `layerSource()` | The layer surface's own content |
| `popupSource()` | The popup's own content |
| `imageSource(path)` | A static image file |

### Stages

| Stage | Purpose |
| --- | --- |
| `dualKawaseBlur({radius, passes})` | Fast, wide blur |
| `shaderStage(shader, {uniforms, textures})` | Run a custom GLSL fragment shader |
| `noise({...})` | Add film-grain style noise |
| `save(name)` / `blend(input, {...})` | Save/composite intermediate results |

`shaderStage` takes a shader (a path, or a `loadShader(path)` handle) plus
`uniforms` (numbers/colors passed to the shader) and `textures` (extra source
handles bound by name).

```ts
import {compileEffect, backdropSource, dualKawaseBlur, shaderStage, loadShader} from 'shoji_wm';

const liquidGlass = compileEffect({
  input: backdropSource(),
  invalidate: {kind: 'on-source-damage-box', antiArtifactMargin: 8},
  pipeline: [
    dualKawaseBlur({radius: 4, passes: 2}),
    shaderStage(loadShader('./src/liquid-glass.frag'), {
      uniforms: {
        glass_radius_px: 10.0,
        distortion_strength: 0.15,
        chromatic_shift_px: 3.0,
      },
    }),
  ],
});
```

### Invalidation policy

`invalidate` controls when the effect re-renders, trading freshness for cost:

- `{kind: 'on-source-damage-box', antiArtifactMargin: N}` — re-render only the
  region that changed, padded by `N` px to avoid edge artifacts. The usual choice.
- `'always'` — re-render every frame (expensive; for animated shaders).
- A manual policy you invalidate yourself.

### Alpha

Set `alpha: 'preserve'` when the pipeline's output is meant to be transparent
(e.g. a blur clipped to a layer's own alpha mask), so the transparency survives
to the display pass instead of being forced opaque.
