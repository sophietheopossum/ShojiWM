---
sidebar_position: 2
---

# Lifecycle & Events

## Lifecycle hooks

Your config module is loaded when the compositor starts and re-loaded when you
edit it (hot reload). Two hooks let you run setup/teardown code and **carry state
across a reload**.

```ts
COMPOSITOR.onEnable((event) => {
  if (event.isReloading) {
    const saved = event.restore<MyState>('my-state');
    if (saved) applyState(saved);
  }
});

COMPOSITOR.onDisable((event) => {
  if (event.isReloading) {
    event.persist('my-state', snapshotState());
  }
});
```

- **`onEnable(listener)`** — runs after the config is applied. `event.isReloading`
  is `true` on a hot reload (vs. a fresh session). Use `event.restore<T>(key)` to
  read state saved by the previous version.
- **`onDisable(listener)`** — runs before the config is torn down. Use
  `event.persist(key, value)` to save state that the next version can restore.

This persist/restore pair is how the default config preserves its window-manager
layout (workspaces, tiling state) across edits without flicker. Both are
shorthands for `COMPOSITOR.event.onEnable` / `onDisable`.

## Debug toggles

`COMPOSITOR.debug` holds development-only switches that don't affect production
behavior.

```ts
COMPOSITOR.key.bind('fps', 'Super+Shift+F', () => {
  COMPOSITOR.debug.fpsCounter = !COMPOSITOR.debug.fpsCounter;
});
```

- **`fpsCounter: boolean`** — draws a small FPS / frame-time overlay in the
  top-left of every output.

## The event bus

`COMPOSITOR.event` is a bus of `on*` subscriptions covering window, input,
output, and layer activity. Every `on*` method returns an **unsubscribe
function**. Listeners are where you wire windows into your layout logic.

```ts
COMPOSITOR.event.onOpen((window) => {
  console.log('opened', window.id);
});

COMPOSITOR.event.onFocus((window, focused) => {
  window.animation.start(focusVar, {to: focused ? 1 : 0, duration: ms(120)});
});
```

### Window lifecycle

| Event | Fires when |
| --- | --- |
| `onOpen(window)` | A toplevel window is created |
| `onFirstCommit(window)` | The window commits its first buffer (ready to show) |
| `onFocus(window, focused)` | A window gains/loses keyboard focus |
| `onStartClose(window)` | The close sequence begins (good for close animations) |
| `onClose(window)` | The window is destroyed |

### Window requests

These fire when a client asks the compositor to change a window's state. Your
window manager decides how to honor them.

| Event | Fires when |
| --- | --- |
| `onWindowResize(event)` | An interactive resize occurs |
| `onWindowMove(event)` | An interactive move occurs |
| `onWindowMaximizeRequest(event)` | The client requests (un)maximize |
| `onWindowMinimizeRequest(event)` | The client requests minimize |
| `onWindowFullscreenRequest(event)` | The client requests (un)fullscreen |
| `onWindowActivateRequest(event)` | The client requests activation/focus |

### Input, output, and layers

| Event | Fires when |
| --- | --- |
| `onPointerMoveAsync(event)` | The pointer moves (async, see below) |
| `onGestureSwipeAsync(event)` | A multi-finger touchpad swipe progresses |
| `onOutputChange(event)` | An output is added/removed/reconfigured |
| `onInputDeviceChange(...)` | The set of input devices changes (hotplug) |
| `onCreateLayer(...)` / `onUpdateLayer(...)` / `onDestroyLayer(...)` | A layer-shell surface (bar/dock/wallpaper) is mapped/updated/unmapped |

### Async listeners

Pointer-move and gesture events have **async** variants
(`onPointerMoveAsync`, `onGestureSwipeAsync`). The listener may return a
`Promise`, which the compositor awaits before continuing; returning `false`
(or resolving to `false`) suppresses further handling. Use these for handlers
that do non-trivial work per event so you don't block the input path.

```ts
COMPOSITOR.event.onPointerMoveAsync((event) => {
  hybridWM.onPointerMove(event);
});
```
