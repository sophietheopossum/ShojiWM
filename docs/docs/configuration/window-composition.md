---
sidebar_position: 7
---

# Window composition

`COMPOSITOR.window.composition` is the heart of ShojiWM's customization. Assign a
function that, given a window, returns a TSX tree describing how that window is
placed and decorated. The compositor calls it for every toplevel window and
re-runs it (incrementally) whenever a value it read changes.

```tsx
COMPOSITOR.window.composition = (window) => (
  <ManagedWindow rect={window.position} zIndex={1}>
    <WindowBorder
      style={{borderRadius: 10, border: {px: 2, color: window.isFocused((f) => (f ? '#d7ba7d' : '#4f5666'))}}}
    >
      <Box direction="column">
        <Box direction="row" style={{height: 28, paddingX: 8, gap: 8, alignItems: 'center'}}>
          <AppIcon icon={window.icon} style={{width: 16, height: 16}} />
          <Label text={window.title} style={{flexGrow: 1, fontSize: 13}} />
        </Box>
        <ClientWindow />
      </Box>
    </WindowBorder>
  </ManagedWindow>
);
```

Every tree must contain exactly one [`<ManagedWindow/>`](#managedwindow) wrapping
exactly one [`<ClientWindow/>`](#clientwindow). Everything between them — borders,
title bars, buttons — is your decoration, built from the
[SSD components](./components.md).

## The `window` object

The argument is a `WaylandWindow`: a live, reactive handle to one window. Reading
its signals inside composition automatically subscribes you to changes.

### Reactive properties

Each is a `ReadonlySignal` — read it as `window.title()` or `window.title.value`,
or map it as `window.isFocused((f) => f ? 'a' : 'b')`.

| Property | Type | Meaning |
| --- | --- | --- |
| `title` | `string` | Window title |
| `appId` | `string \| undefined` | Application id (e.g. `"org.gnome.Nautilus"`) |
| `icon` | `WindowIcon \| undefined` | Application icon |
| `isFocused` | `boolean` | Holds keyboard focus |
| `isFloating` | `boolean` | Floating (non-tiled) |
| `isMaximized` | `boolean` | Maximized |
| `isFullscreen` | `boolean` | Fullscreen |
| `isResizable` | `boolean` | Client allows interactive resize |
| `isTransient` | `boolean` | A child (dialog) of another window |
| `parentId` | `string \| undefined` | Parent window id, if transient |
| `sizeConstraints` | `WindowSizeConstraints` | Min/max size from the client |
| `interaction` | snapshot | Current pointer/drag interaction state |

Non-reactive helpers: `id` (stable string), `position` / `rect` (current logical
geometry), `state` (per-window store — see [State & Signals](./state-and-signals.md)),
`transform` (GPU transform), `animation` (see [Animations](./animations.md)).

### Methods

| Method | Effect |
| --- | --- |
| `close()` | Ask the client to close |
| `maximize()` / `unmaximize()` | Toggle maximize |
| `minimize()` | Minimize |
| `fullscreen()` / `unfullscreen()` | Toggle fullscreen |
| `focus()` | Give keyboard focus and raise |
| `scheduleAnimation(options)` | Animate managed-window geometry |
| `cancelAnimation(channel?)` | Cancel a running animation |
| `setCloseAnimationDuration(ms)` | Delay surface destruction to fit a close animation |
| `isXWayland()` | `true` if running under XWayland |

## ManagedWindow

`<ManagedWindow/>` is the anchor that binds a window into the layout system.
Place one per window.

| Prop | Type | Meaning |
| --- | --- | --- |
| `rect` | `ManagedWindowRect` | Logical `{x, y, width, height}` of the window |
| `zIndex` | `number` | Stacking order (higher is on top) |
| `workspace` | `string \| number` | Workspace assignment |
| `visibleOutputs` | `string[] \| null` | Restrict to named outputs (`null` = all) |
| `visible` | `boolean` | Show/hide without unmapping |
| `idle` | `boolean` | Exclude from focus cycling; treat as background |
| `interactive` | `boolean` | When `false`, ignore pointer input |
| `forceRectSize` | `boolean` | Force the client to `rect`'s size |
| `tiled` | `boolean` | Send the tiled state to the client |
| `opacity` | `number` | `0.0`–`1.0` |
| `transform` | `ManagedWindowTransform` | Extra GPU transform |
| `allowTearing` | `boolean` | Permit tearing while fullscreen + direct-scanout (games) |

All props accept signals for reactive layout. `rect`, `zIndex`, etc. are usually
driven by your window-manager logic.

## ClientWindow

`<ClientWindow/>` renders the client's actual surface buffer. A leaf node — no
children. Alias: `<Window/>`.

```tsx
<ClientWindow style={{borderRadius: 8}} />
```

The optional `style` clips/styles the surface (typically just `borderRadius`).

:::tip Fullscreen fast path
For fullscreen windows, return **only** a bare `<ClientWindow/>` inside
`<ManagedWindow/>` (no border, no title bar). Rendering nothing else is what lets
the TTY backend promote the client buffer to the primary plane (direct scanout)
for the lowest latency. The default config does exactly this.
:::
