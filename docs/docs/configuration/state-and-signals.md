---
sidebar_position: 9
---

# State & Signals

ShojiWM's composition is **reactive**: when a value your code read changes, the
affected part of the decoration is re-evaluated automatically. The primitive
behind this is the **signal**.

## Signals

A signal is a container for a value. Reading it inside composition subscribes
that composition to changes; writing it notifies subscribers.

### Reading

A `ReadonlySignal<T>` can be read three ways, and mapped into a derived signal:

```ts
window.title.value          // read the value
window.title()              // same, call form
window.title.peek()         // read WITHOUT subscribing
window.isFocused((f) => f ? '#d7ba7d' : '#4f5666')  // derive a mapped signal
```

The mapped form is the workhorse inside TSX — it produces a new reactive value
without manual wiring:

```tsx
<Label text={window.title} style={{color: window.isFocused((f) => (f ? '#fff' : '#aaa'))}} />
```

### Writing

A writable `Signal<T>` adds `.value =`, `.set(...)`, and `.update(...)`:

```ts
count.value = 5;
setCount(5);              // when destructured as [count, setCount]
count.update((n) => n + 1);
```

## Module-scope helpers

Use these at the top level of your config (outside a component function):

| Function | Purpose |
| --- | --- |
| `signal(initial)` | Create a writable signal. Destructures as `[signal, setter]`. |
| `computed(fn)` | Create a derived read-only signal; recomputes when its deps change. |
| `effect(fn)` | Run a side effect when its deps change. Returns a dispose function. |
| `read(maybeSignal)` | Unwrap a value-or-signal to a plain value. |
| `isSignal(x)` | Narrow an `unknown` to a signal. |

```ts
import {signal, computed, effect} from 'shoji_wm';

const [count, setCount] = signal(0);
const doubled = computed(() => count.value * 2);
const dispose = effect(() => console.log('count is', count.value));
setCount(1); // logs "count is 1"
```

## Component-scope hooks

Inside a function component (a TSX component you define), use the hook forms.
They keep stable identity across re-renders, like React hooks.

| Hook | Purpose |
| --- | --- |
| `useState(initial)` | Component-local writable signal (`[signal, setter]`) |
| `useComputed(fn)` | Component-local derived signal |
| `useEffect(fn, deps?)` | Side effect after render; return a cleanup |
| `useLayoutEffect(fn, deps?)` | Like `useEffect`, run synchronously in the render pass |
| `useMemo(fn, deps?)` | Memoize a plain (non-signal) value |
| `useRef(initial)` | Mutable `.current` that persists across renders |
| `onCleanup(fn)` | Register teardown when the component unmounts |

```tsx
const CloseButton = ({window}: {window: WaylandWindow}) => {
  const [hover, setHover] = useState(false);
  return (
    <Button
      onHoverChange={setHover}
      onClick={window.close}
      style={{background: hover((h) => (h ? '#FFFFFF40' : '#FFFFFF20'))}}
    />
  );
};
```

## Per-window state

`createWindowState` declares a named, reactive state slot scoped to each window.
Call it once at module scope to get a key, then read `window.state[key]` (a
`Signal<T>`) inside composition or event handlers.

```ts
import {createWindowState} from 'shoji_wm';

// Module scope — create the key once
const isMinimized = createWindowState('minimized', {default: false});

// In composition: read it (reactive)
COMPOSITOR.window.composition = (window) => {
  const minimized = window.state[isMinimized]; // Signal<boolean>
  return <ManagedWindow visible={minimized((v) => !v)} /* … */ />;
};

// In an event handler: write it
COMPOSITOR.event.onFocus((window) => {
  window.state[isMinimized].set(false);
});
```

The `default` may be a value or a factory `(window) => value` for
window-dependent initial state. The default config uses per-window state
extensively to track tiling, workspace visibility, fullscreen, and animation
offsets.
