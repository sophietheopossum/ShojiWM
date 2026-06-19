---
sidebar_position: 1
---

# Overview

ShojiWM is configured entirely from TypeScript/TSX. Your config is a normal
TypeScript module that imports from `shoji_wm` and talks to the compositor
through a single root object: **`COMPOSITOR`**.

```tsx
import {COMPOSITOR, ManagedWindow, ClientWindow, WindowBorder} from 'shoji_wm';

// Launch a terminal with Super+T
COMPOSITOR.key.bind('terminal', 'Super+T', () => {
  COMPOSITOR.process.spawn({command: ['kitty']});
});

// Decide how every window is decorated
COMPOSITOR.window.composition = (window) => (
  <ManagedWindow rect={window.position} zIndex={1}>
    <WindowBorder style={{borderRadius: 10, border: {px: 2, color: '#d7ba7d'}}}>
      <ClientWindow />
    </WindowBorder>
  </ManagedWindow>
);
```

:::tip
The examples throughout this section are drawn from the default config
(`packages/config/src/index.tsx`). It is the best end-to-end reference once you
understand the individual pieces below.
:::

## The `COMPOSITOR` object

`COMPOSITOR` groups every configurable area under a named field. Each gets its
own page in this section:

| Field | What it controls | Page |
| --- | --- | --- |
| `event`, `onEnable`, `onDisable` | Lifecycle hooks and the window/input/output event bus | [Lifecycle & Events](./lifecycle-and-events.md) |
| `output` | Monitor resolution, scale, position, mirroring | [Outputs](./outputs.md) |
| `input` | Keyboard, pointer, and touchpad device settings | [Input devices](./input.md) |
| `key`, `pointer` | Keyboard shortcuts and pointer modifiers | [Keybindings & Pointer](./keybindings-and-pointer.md) |
| `process`, `env` | Spawning programs and environment variables | [Processes & Environment](./processes-and-env.md) |
| `window` | Per-window decoration (the composition function) | [Window composition](./window-composition.md) |
| `effect` | GPU effects: background blur, per-window/layer/popup shaders | [Effects](./effects.md) |
| `debug` | Debug overlays such as the FPS counter | [Lifecycle & Events](./lifecycle-and-events.md) |

Building blocks used inside the composition function have their own pages too:

- [SSD Components](./components.md) — `<Box/>`, `<Label/>`, `<Button/>`,
  `<AppIcon/>`, `<Image/>`, `<ShaderEffect/>`, `<WindowBorder/>`,
  `<ManagedWindow/>`, `<ClientWindow/>`, and the full `style` reference.
- [State & Signals](./state-and-signals.md) — the reactive model behind
  automatic updates.
- [Animations](./animations.md) — driving smooth transitions.

## Mental model

1. The **compositor core** (Rust) sends your config a live, reactive view of
   each window, output, and input device.
2. Your config reads those values, registers callbacks, and (for windows)
   returns a small **TSX tree** describing the decoration.
3. When any value your code read changes, only the affected part is
   re-evaluated. This is the **signal** system — see
   [State & Signals](./state-and-signals.md).

If you have not yet, read [the architecture overview](../architecture/shojiwm.md)
for how the two halves fit together.
