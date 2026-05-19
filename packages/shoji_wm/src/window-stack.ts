import { computed, signal, type ReadonlySignal } from "./signals";
import type { WaylandWindow } from "./types";

export type WindowStackPlacement = "front" | "back";

export interface WindowStackOptions {
  baseZIndex?: number;
  step?: number;
}

export interface WindowStackAddOptions {
  at?: WindowStackPlacement;
}

export interface WindowStack {
  add(window: WaylandWindow, options?: WindowStackAddOptions): void;
  remove(window: WaylandWindow): void;
  has(window: WaylandWindow): boolean;
  raise(window: WaylandWindow): void;
  lower(window: WaylandWindow): void;
  moveBefore(window: WaylandWindow, target: WaylandWindow): void;
  moveAfter(window: WaylandWindow, target: WaylandWindow): void;
  zIndex(window: WaylandWindow): ReadonlySignal<number>;
  zIndexValue(window: WaylandWindow): number;
  windows(): readonly WaylandWindow[];
  ids(): readonly string[];
  front(): WaylandWindow | undefined;
  back(): WaylandWindow | undefined;
  clear(): void;
}

export function createWindowStack(options: WindowStackOptions = {}): WindowStack {
  const baseZIndex = options.baseZIndex ?? 0;
  const step = options.step ?? 1;
  const order = signal<string[]>([]);
  const windowsById = new Map<string, WaylandWindow>();
  const zIndexById = new Map<string, ReadonlySignal<number>>();

  const normalize = (window: WaylandWindow): string => {
    windowsById.set(window.id, window);
    return window.id;
  };

  const setOrder = (nextOrder: string[]): void => {
    const seen = new Set<string>();
    const unique = nextOrder.filter((id) => {
      if (seen.has(id)) {
        return false;
      }
      seen.add(id);
      return true;
    });
    order.set(unique);
  };

  const moveTo = (window: WaylandWindow, placement: WindowStackPlacement): void => {
    const id = normalize(window);
    const without = order.peek().filter((candidate) => candidate !== id);
    setOrder(placement === "front" ? [...without, id] : [id, ...without]);
  };

  const indexOf = (window: WaylandWindow): number => order.peek().indexOf(window.id);

  return {
    add(window, addOptions = {}) {
      const at = addOptions.at ?? "front";
      moveTo(window, at);
    },
    remove(window) {
      windowsById.delete(window.id);
      zIndexById.delete(window.id);
      setOrder(order.peek().filter((id) => id !== window.id));
    },
    has(window) {
      return order.peek().includes(window.id);
    },
    raise(window) {
      moveTo(window, "front");
    },
    lower(window) {
      moveTo(window, "back");
    },
    moveBefore(window, target) {
      const id = normalize(window);
      const targetId = normalize(target);
      const without = order.peek().filter((candidate) => candidate !== id);
      const targetIndex = without.indexOf(targetId);
      if (targetIndex < 0) {
        setOrder([...without, id]);
        return;
      }
      setOrder([
        ...without.slice(0, targetIndex),
        id,
        ...without.slice(targetIndex),
      ]);
    },
    moveAfter(window, target) {
      const id = normalize(window);
      const targetId = normalize(target);
      const without = order.peek().filter((candidate) => candidate !== id);
      const targetIndex = without.indexOf(targetId);
      if (targetIndex < 0) {
        setOrder([...without, id]);
        return;
      }
      setOrder([
        ...without.slice(0, targetIndex + 1),
        id,
        ...without.slice(targetIndex + 1),
      ]);
    },
    zIndex(window) {
      normalize(window);
      let existing = zIndexById.get(window.id);
      if (!existing) {
        const id = window.id;
        existing = computed(() => {
          const index = order().indexOf(id);
          return baseZIndex + (index < 0 ? 0 : index * step);
        });
        zIndexById.set(id, existing);
      }
      return existing;
    },
    zIndexValue(window) {
      const index = indexOf(window);
      return baseZIndex + (index < 0 ? 0 : index * step);
    },
    windows() {
      return order.peek()
        .map((id) => windowsById.get(id))
        .filter((window): window is WaylandWindow => window !== undefined);
    },
    ids() {
      return [...order.peek()];
    },
    front() {
      const ids = order.peek();
      return windowsById.get(ids[ids.length - 1] ?? "");
    },
    back() {
      return windowsById.get(order.peek()[0] ?? "");
    },
    clear() {
      windowsById.clear();
      zIndexById.clear();
      order.set([]);
    },
  };
}
