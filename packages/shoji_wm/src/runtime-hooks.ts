import type {
  SSDRebuildSuppressionHandle,
  SSDRebuildSuppressionOptions,
  SSDRebuildSuppressionViolationPolicy,
} from "./types";

interface RuntimeHooks {
  markRuntimeDirty(): void;
  markWindowDirty(windowId: string): void;
  markLayerDirty(layerId: string): void;
}

let hooks: RuntimeHooks | null = null;
let activeWindowDependencyScope: string | null = null;
let activeLayerDependencyScope: string | null = null;
let activeWindowNodeDependencyScope: string | null = null;
let activeLayerNodeDependencyScope: string | null = null;
let activeWindowManagedDependencyScope: string | null = null;
const windowSignalDependencies = new WeakMap<object, Set<string>>();
const layerSignalDependencies = new WeakMap<object, Set<string>>();
const windowManagedSignalDependencies = new WeakMap<object, Set<string>>();
const windowStructuralSignalDependencies = new WeakMap<object, Set<string>>();
const layerStructuralSignalDependencies = new WeakMap<object, Set<string>>();
const windowNodeSignalDependencies = new WeakMap<object, Map<string, Set<string>>>();
const layerNodeSignalDependencies = new WeakMap<object, Map<string, Set<string>>>();
const windowDependencies = new Map<string, Set<object>>();
const layerDependencies = new Map<string, Set<object>>();
const windowNodeDependencies = new Map<string, Map<string, Set<object>>>();
const layerNodeDependencies = new Map<string, Map<string, Set<object>>>();
const dirtyWindowNodeIds = new Map<string, Set<string>>();
const dirtyLayerNodeIds = new Map<string, Set<string>>();
const dirtyManagedWindowIds = new Set<string>();
// Windows/layers that received a structural-dep write since the last
// takeDirty*NodeIds call. Tracked separately because a structural write may be
// followed by cascading writes from derived computed signals — those cascades
// would otherwise re-add node-scoped dirty entries and re-enable an unsafe
// node-only patch. We keep the flag set until the runtime collects dirty ids
// so the structural intent always wins.
const windowsWithStructuralWrite = new Set<string>();
const layersWithStructuralWrite = new Set<string>();

interface ActiveSSDRebuildSuppression {
  id: number;
  allowManagedWindowOnly: boolean;
  onViolation: SSDRebuildSuppressionViolationPolicy;
  windowIds?: Set<string>;
  layerIds?: Set<string>;
  warned: Set<string>;
  delayedRuntimeDirty: boolean;
  delayedDirtyWindows: Set<string>;
  delayedDirtyLayers: Set<string>;
}

let nextSuppressionId = 1;
const ssdRebuildSuppressionStack: ActiveSSDRebuildSuppression[] = [];

function debugSSD(message: string, details: Record<string, unknown> = {}): void {
  const env = (globalThis as { process?: { env?: Record<string, string | undefined> } }).process?.env;
  if (!env?.SHOJI_SSD_SUPPRESSION_DEBUG) {
    return;
  }
  console.info(`ssd-suppression ${message}`, JSON.stringify(details));
}

function suppressionForDebug(entry: ActiveSSDRebuildSuppression): Record<string, unknown> {
  return {
    id: entry.id,
    allowManagedWindowOnly: entry.allowManagedWindowOnly,
    onViolation: entry.onViolation,
    windowIds: entry.windowIds ? Array.from(entry.windowIds) : null,
    layerIds: entry.layerIds ? Array.from(entry.layerIds) : null,
    delayedRuntimeDirty: entry.delayedRuntimeDirty,
    delayedDirtyWindows: Array.from(entry.delayedDirtyWindows),
    delayedDirtyLayers: Array.from(entry.delayedDirtyLayers),
    stackDepth: ssdRebuildSuppressionStack.length,
  };
}

export function suppressSSDRebuild(
  options: SSDRebuildSuppressionOptions = {},
): SSDRebuildSuppressionHandle {
  const entry: ActiveSSDRebuildSuppression = {
    id: nextSuppressionId++,
    allowManagedWindowOnly: options.allowManagedWindowOnly ?? true,
    onViolation: options.onViolation ?? "fallback",
    windowIds: options.windowIds ? new Set(options.windowIds) : undefined,
    layerIds: options.layerIds ? new Set(options.layerIds) : undefined,
    warned: new Set(),
    delayedRuntimeDirty: false,
    delayedDirtyWindows: new Set(),
    delayedDirtyLayers: new Set(),
  };
  ssdRebuildSuppressionStack.push(entry);
  debugSSD("runtime-suppress-begin", suppressionForDebug(entry));

  let released = false;
  return {
    release() {
      if (released) {
        return;
      }
      released = true;
      const index = ssdRebuildSuppressionStack.findIndex(
        (candidate) => candidate.id === entry.id,
      );
      if (index >= 0) {
        ssdRebuildSuppressionStack.splice(index, 1);
      }
      debugSSD("runtime-suppress-release", suppressionForDebug(entry));
      releaseDelayedSSDRebuilds(entry);
    },
  };
}

export function withSSDRebuildSuppressed<T>(
  options: SSDRebuildSuppressionOptions | undefined,
  callback: () => T,
): T {
  const handle = suppressSSDRebuild(options);
  try {
    const result = callback();
    if (
      result &&
      typeof (result as { finally?: unknown }).finally === "function"
    ) {
      return (result as unknown as Promise<unknown>).finally(() =>
        handle.release(),
      ) as T;
    }
    handle.release();
    return result;
  } catch (error) {
    handle.release();
    throw error;
  }
}

function activeSSDRebuildSuppression(): ActiveSSDRebuildSuppression | undefined {
  return ssdRebuildSuppressionStack.at(-1);
}

function releaseDelayedSSDRebuilds(entry: ActiveSSDRebuildSuppression): void {
  if (entry.onViolation !== "fallback-last") {
    return;
  }

  if (entry.delayedRuntimeDirty) {
    debugSSD("runtime-suppress-flush-runtime", suppressionForDebug(entry));
    markRuntimeDirty();
    return;
  }
  debugSSD("runtime-suppress-flush-targets", suppressionForDebug(entry));
  for (const windowId of entry.delayedDirtyWindows) {
    markWindowDirty(windowId);
  }
  for (const layerId of entry.delayedDirtyLayers) {
    markLayerDirty(layerId);
  }
}

function recordDelayedSSDRebuild(
  suppression: ActiveSSDRebuildSuppression,
  scope:
    | "runtime"
    | "window-structure"
    | "window-node"
    | "layer-structure"
    | "layer-node",
  id: string,
): void {
  if (scope === "runtime") {
    suppression.delayedRuntimeDirty = true;
    return;
  }
  if (scope === "window-structure" || scope === "window-node") {
    suppression.delayedDirtyWindows.add(id);
    return;
  }
  if (scope === "layer-structure" || scope === "layer-node") {
    suppression.delayedDirtyLayers.add(id);
  }
}

function handleSSDRebuildSuppressionViolation(
  scope:
    | "runtime"
    | "window-structure"
    | "window-node"
    | "layer-structure"
    | "layer-node",
  id: string,
): "fallback" | "suppress" {
  const suppression = activeSSDRebuildSuppression();
  if (!suppression) {
    return "fallback";
  }
  if (!suppressionAppliesToViolation(suppression, scope, id)) {
    debugSSD("runtime-suppress-not-applicable", {
      suppression: suppressionForDebug(suppression),
      scope,
      id,
    });
    return "fallback";
  }
  if (suppression.onViolation === "suppress-unsafe") {
    debugSSD("runtime-suppress-unsafe", {
      suppression: suppressionForDebug(suppression),
      scope,
      id,
    });
    return "suppress";
  }

  const message =
    `SSD rebuild was suppressed, but ${scope} changed for ${id}. ` +
    `policy=${suppression.onViolation}`;
  const key = `${scope}:${id}`;
  if (!suppression.warned.has(key)) {
    suppression.warned.add(key);
    console.warn(message);
  }

  if (suppression.onViolation === "throw") {
    throw new Error(message);
  }
  if (suppression.onViolation === "fallback-last") {
    recordDelayedSSDRebuild(suppression, scope, id);
    debugSSD("runtime-suppress-delay", {
      suppression: suppressionForDebug(suppression),
      scope,
      id,
    });
    return "suppress";
  }
  if (suppression.onViolation === "warn") {
    debugSSD("runtime-suppress-warn-policy", {
      suppression: suppressionForDebug(suppression),
      scope,
      id,
    });
    return "suppress";
  }
  debugSSD("runtime-suppress-fallback", {
    suppression: suppressionForDebug(suppression),
    scope,
    id,
  });
  return "fallback";
}

function suppressionAppliesToViolation(
  suppression: ActiveSSDRebuildSuppression,
  scope:
    | "runtime"
    | "window-structure"
    | "window-node"
    | "layer-structure"
    | "layer-node",
  id: string,
): boolean {
  if (scope === "window-structure" || scope === "window-node") {
    return !suppression.windowIds || suppression.windowIds.has(id);
  }
  if (scope === "layer-structure" || scope === "layer-node") {
    return !suppression.layerIds || suppression.layerIds.has(id);
  }

  // Runtime/unknown writes cannot be safely associated with the scoped
  // window/layer. Let them take the normal dirty path rather than allowing a
  // rect animation on one window to hide unrelated SSD updates elsewhere.
  return !suppression.windowIds && !suppression.layerIds;
}

export function installRuntimeHooks(nextHooks: RuntimeHooks | null): void {
  hooks = nextHooks;
}

export function markRuntimeDirty(): void {
  hooks?.markRuntimeDirty();
}

export function markWindowDirty(windowId: string): void {
  hooks?.markWindowDirty(windowId);
}

export function markLayerDirty(layerId: string): void {
  hooks?.markLayerDirty(layerId);
}

export function enterWindowDependencyScope(windowId: string): void {
  clearWindowDependencies(windowId);
  activeWindowDependencyScope = windowId;
  activeWindowNodeDependencyScope = null;
  activeLayerDependencyScope = null;
  activeLayerNodeDependencyScope = null;
}

export function leaveWindowDependencyScope(): void {
  activeWindowDependencyScope = null;
  activeWindowNodeDependencyScope = null;
  activeWindowManagedDependencyScope = null;
}

export function enterLayerDependencyScope(layerId: string): void {
  clearLayerDependencies(layerId);
  activeLayerDependencyScope = layerId;
  activeLayerNodeDependencyScope = null;
  activeWindowDependencyScope = null;
  activeWindowNodeDependencyScope = null;
}

export function leaveLayerDependencyScope(): void {
  activeLayerDependencyScope = null;
  activeLayerNodeDependencyScope = null;
}

export function enterWindowNodeDependencyScope(nodeId: string): void {
  activeWindowNodeDependencyScope =
    activeWindowDependencyScope ? nodeId : null;
  activeLayerNodeDependencyScope = null;
}

export function leaveWindowNodeDependencyScope(): void {
  activeWindowNodeDependencyScope = null;
}

export function enterWindowManagedDependencyScope(windowId: string): void {
  activeWindowManagedDependencyScope =
    activeWindowDependencyScope === windowId ? windowId : null;
  activeWindowNodeDependencyScope = null;
  activeLayerNodeDependencyScope = null;
}

export function leaveWindowManagedDependencyScope(): void {
  activeWindowManagedDependencyScope = null;
}

export function enterLayerNodeDependencyScope(nodeId: string): void {
  activeLayerNodeDependencyScope =
    activeLayerDependencyScope ? nodeId : null;
  activeWindowNodeDependencyScope = null;
}

export function leaveLayerNodeDependencyScope(): void {
  activeLayerNodeDependencyScope = null;
}

export function dropWindowDependencies(windowId: string): void {
  clearWindowDependencies(windowId);
}

export function dropLayerDependencies(layerId: string): void {
  clearLayerDependencies(layerId);
}

export function takeDirtyWindowNodeIds(windowId: string): string[] {
  if (windowsWithStructuralWrite.has(windowId)) {
    windowsWithStructuralWrite.delete(windowId);
    dirtyWindowNodeIds.delete(windowId);
    return [];
  }
  const dirty = dirtyWindowNodeIds.get(windowId);
  if (!dirty) {
    return [];
  }
  dirtyWindowNodeIds.delete(windowId);
  return Array.from(dirty);
}

export function takeManagedWindowOnlyDirty(windowId: string): boolean {
  if (!isManagedWindowOnlyDirty(windowId)) {
    dirtyManagedWindowIds.delete(windowId);
    return false;
  }
  dirtyManagedWindowIds.delete(windowId);
  return true;
}

export function isManagedWindowOnlyDirty(windowId: string): boolean {
  if (!dirtyManagedWindowIds.has(windowId)) {
    return false;
  }
  if (windowsWithStructuralWrite.has(windowId) || dirtyWindowNodeIds.has(windowId)) {
    return false;
  }
  return true;
}

export function takeDirtyLayerNodeIds(layerId: string): string[] {
  if (layersWithStructuralWrite.has(layerId)) {
    layersWithStructuralWrite.delete(layerId);
    dirtyLayerNodeIds.delete(layerId);
    return [];
  }
  const dirty = dirtyLayerNodeIds.get(layerId);
  if (!dirty) {
    return [];
  }
  dirtyLayerNodeIds.delete(layerId);
  return Array.from(dirty);
}

export function trackSignalRead(signal: object): void {
  const managedWindowId = activeWindowManagedDependencyScope;
  if (managedWindowId) {
    let dependentWindows = windowSignalDependencies.get(signal);
    if (!dependentWindows) {
      dependentWindows = new Set<string>();
      windowSignalDependencies.set(signal, dependentWindows);
    }
    dependentWindows.add(managedWindowId);

    let managedWindows = windowManagedSignalDependencies.get(signal);
    if (!managedWindows) {
      managedWindows = new Set<string>();
      windowManagedSignalDependencies.set(signal, managedWindows);
    }
    managedWindows.add(managedWindowId);

    let dependencies = windowDependencies.get(managedWindowId);
    if (!dependencies) {
      dependencies = new Set<object>();
      windowDependencies.set(managedWindowId, dependencies);
    }
    dependencies.add(signal);
    return;
  }

  const windowId = activeWindowDependencyScope;
  if (windowId) {
    let dependentWindows = windowSignalDependencies.get(signal);
    if (!dependentWindows) {
      dependentWindows = new Set<string>();
      windowSignalDependencies.set(signal, dependentWindows);
    }
    dependentWindows.add(windowId);

    let dependencies = windowDependencies.get(windowId);
    if (!dependencies) {
      dependencies = new Set<object>();
      windowDependencies.set(windowId, dependencies);
    }
    dependencies.add(signal);

    const nodeId = activeWindowNodeDependencyScope;
    if (nodeId) {
      let dependentNodesByWindow = windowNodeSignalDependencies.get(signal);
      if (!dependentNodesByWindow) {
        dependentNodesByWindow = new Map<string, Set<string>>();
        windowNodeSignalDependencies.set(signal, dependentNodesByWindow);
      }
      let dependentNodes = dependentNodesByWindow.get(windowId);
      if (!dependentNodes) {
        dependentNodes = new Set<string>();
        dependentNodesByWindow.set(windowId, dependentNodes);
      }
      dependentNodes.add(nodeId);

      let nodeDependenciesByWindow = windowNodeDependencies.get(windowId);
      if (!nodeDependenciesByWindow) {
        nodeDependenciesByWindow = new Map<string, Set<object>>();
        windowNodeDependencies.set(windowId, nodeDependenciesByWindow);
      }
      let nodeDependencies = nodeDependenciesByWindow.get(nodeId);
      if (!nodeDependencies) {
        nodeDependencies = new Set<object>();
        nodeDependenciesByWindow.set(nodeId, nodeDependencies);
      }
      nodeDependencies.add(signal);
    } else {
      let structuralWindows = windowStructuralSignalDependencies.get(signal);
      if (!structuralWindows) {
        structuralWindows = new Set<string>();
        windowStructuralSignalDependencies.set(signal, structuralWindows);
      }
      structuralWindows.add(windowId);
    }
    return;
  }

  const layerId = activeLayerDependencyScope;
  if (!layerId) {
    return;
  }

  let dependentLayers = layerSignalDependencies.get(signal);
  if (!dependentLayers) {
    dependentLayers = new Set<string>();
    layerSignalDependencies.set(signal, dependentLayers);
  }
  dependentLayers.add(layerId);

  let dependencies = layerDependencies.get(layerId);
  if (!dependencies) {
    dependencies = new Set<object>();
    layerDependencies.set(layerId, dependencies);
  }
  dependencies.add(signal);

  const nodeId = activeLayerNodeDependencyScope;
  if (!nodeId) {
    let structuralLayers = layerStructuralSignalDependencies.get(signal);
    if (!structuralLayers) {
      structuralLayers = new Set<string>();
      layerStructuralSignalDependencies.set(signal, structuralLayers);
    }
    structuralLayers.add(layerId);
    return;
  }

  let dependentNodesByLayer = layerNodeSignalDependencies.get(signal);
  if (!dependentNodesByLayer) {
    dependentNodesByLayer = new Map<string, Set<string>>();
    layerNodeSignalDependencies.set(signal, dependentNodesByLayer);
  }
  let dependentNodes = dependentNodesByLayer.get(layerId);
  if (!dependentNodes) {
    dependentNodes = new Set<string>();
    dependentNodesByLayer.set(layerId, dependentNodes);
  }
  dependentNodes.add(nodeId);

  let nodeDependenciesByLayer = layerNodeDependencies.get(layerId);
  if (!nodeDependenciesByLayer) {
    nodeDependenciesByLayer = new Map<string, Set<object>>();
    layerNodeDependencies.set(layerId, nodeDependenciesByLayer);
  }
  let nodeDependencies = nodeDependenciesByLayer.get(nodeId);
  if (!nodeDependencies) {
    nodeDependencies = new Set<object>();
    nodeDependenciesByLayer.set(nodeId, nodeDependencies);
  }
  nodeDependencies.add(signal);
}

export function trackSignalWrite(signal: object): void {
  const dependentWindows = windowSignalDependencies.get(signal);
  const dependentLayers = layerSignalDependencies.get(signal);
  const managedWindows = windowManagedSignalDependencies.get(signal);
  const structuralWindows = windowStructuralSignalDependencies.get(signal);
  const structuralLayers = layerStructuralSignalDependencies.get(signal);
  const dependentWindowNodes = windowNodeSignalDependencies.get(signal);
  const dependentLayerNodes = layerNodeSignalDependencies.get(signal);
  const hasWindowDeps = !!dependentWindows && dependentWindows.size > 0;
  const hasLayerDeps = !!dependentLayers && dependentLayers.size > 0;
  const hasManagedWindowDeps = !!managedWindows && managedWindows.size > 0;
  const hasWindowNodeDeps = !!dependentWindowNodes && dependentWindowNodes.size > 0;
  const hasLayerNodeDeps = !!dependentLayerNodes && dependentLayerNodes.size > 0;
  const suppression = activeSSDRebuildSuppression();
  if (
    !hasWindowDeps &&
    !hasLayerDeps &&
    !hasManagedWindowDeps &&
    !hasWindowNodeDeps &&
    !hasLayerNodeDeps
  ) {
    if (
      suppression?.allowManagedWindowOnly &&
      handleSSDRebuildSuppressionViolation("runtime", "unknown-signal") ===
        "suppress"
    ) {
      return;
    }
    debugSSD("runtime-track-write-unknown-fallback", {
      hasSuppression: suppression !== undefined,
      suppression: suppression ? suppressionForDebug(suppression) : null,
    });
    markRuntimeDirty();
    return;
  }

  let suppressedWindowDirty: Set<string> | undefined;
  let suppressedLayerDirty: Set<string> | undefined;

  if (suppression?.allowManagedWindowOnly) {
    if (structuralWindows) {
      for (const windowId of structuralWindows) {
        if (
          handleSSDRebuildSuppressionViolation("window-structure", windowId) ===
          "suppress"
        ) {
          (suppressedWindowDirty ??= new Set()).add(windowId);
        }
      }
    }
    if (dependentWindowNodes) {
      for (const windowId of dependentWindowNodes.keys()) {
        if (structuralWindows?.has(windowId)) {
          continue;
        }
        if (
          handleSSDRebuildSuppressionViolation("window-node", windowId) ===
          "suppress"
        ) {
          (suppressedWindowDirty ??= new Set()).add(windowId);
        }
      }
    }
    if (structuralLayers) {
      for (const layerId of structuralLayers) {
        if (
          handleSSDRebuildSuppressionViolation("layer-structure", layerId) ===
          "suppress"
        ) {
          (suppressedLayerDirty ??= new Set()).add(layerId);
        }
      }
    }
    if (dependentLayerNodes) {
      for (const layerId of dependentLayerNodes.keys()) {
        if (structuralLayers?.has(layerId)) {
          continue;
        }
        if (
          handleSSDRebuildSuppressionViolation("layer-node", layerId) ===
          "suppress"
        ) {
          (suppressedLayerDirty ??= new Set()).add(layerId);
        }
      }
    }
  }

  if (dependentWindows) {
    for (const windowId of dependentWindows) {
      if (suppressedWindowDirty?.has(windowId) && !managedWindows?.has(windowId)) {
        continue;
      }
      markWindowDirty(windowId);
    }
  }
  if (dependentLayers) {
    for (const layerId of dependentLayers) {
      if (suppressedLayerDirty?.has(layerId)) {
        continue;
      }
      markLayerDirty(layerId);
    }
  }
  if (managedWindows) {
    for (const windowId of managedWindows) {
      dirtyManagedWindowIds.add(windowId);
    }
  }
  if (structuralWindows) {
    for (const windowId of structuralWindows) {
      if (suppressedWindowDirty?.has(windowId)) {
        continue;
      }
      // A structural dependency may affect tree shape, so node-scoped patches
      // are unsafe for the same update. We also need to suppress dirty entries
      // re-added by derived signals during the cascading notify() — record the
      // intent until the runtime collects dirty ids.
      dirtyWindowNodeIds.delete(windowId);
      dirtyManagedWindowIds.delete(windowId);
      windowsWithStructuralWrite.add(windowId);
    }
  }
  if (structuralLayers) {
    for (const layerId of structuralLayers) {
      if (suppressedLayerDirty?.has(layerId)) {
        continue;
      }
      dirtyLayerNodeIds.delete(layerId);
      layersWithStructuralWrite.add(layerId);
    }
  }
  if (dependentWindowNodes) {
    for (const [windowId, nodeIds] of dependentWindowNodes) {
      if (structuralWindows?.has(windowId)) {
        continue;
      }
      // If a structural write happened earlier in the same cascade, the tree
      // shape is changing — derived signals notifying for the same window must
      // not reintroduce node-scoped patches.
      if (suppressedWindowDirty?.has(windowId)) {
        continue;
      }
      if (windowsWithStructuralWrite.has(windowId)) {
        continue;
      }
      let dirtyNodes = dirtyWindowNodeIds.get(windowId);
      if (!dirtyNodes) {
        dirtyNodes = new Set<string>();
        dirtyWindowNodeIds.set(windowId, dirtyNodes);
      }
      for (const nodeId of nodeIds) {
        dirtyNodes.add(nodeId);
      }
    }
  }
  if (dependentLayerNodes) {
    for (const [layerId, nodeIds] of dependentLayerNodes) {
      if (structuralLayers?.has(layerId)) {
        continue;
      }
      if (layersWithStructuralWrite.has(layerId)) {
        continue;
      }
      if (suppressedLayerDirty?.has(layerId)) {
        continue;
      }
      let dirtyNodes = dirtyLayerNodeIds.get(layerId);
      if (!dirtyNodes) {
        dirtyNodes = new Set<string>();
        dirtyLayerNodeIds.set(layerId, dirtyNodes);
      }
      for (const nodeId of nodeIds) {
        dirtyNodes.add(nodeId);
      }
    }
  }
}

function clearWindowDependencies(windowId: string): void {
  const dependencies = windowDependencies.get(windowId);
  if (!dependencies) {
    return;
  }

  for (const signal of dependencies) {
    const dependentWindows = windowSignalDependencies.get(signal);
    dependentWindows?.delete(windowId);
    const structuralWindows = windowStructuralSignalDependencies.get(signal);
    structuralWindows?.delete(windowId);
    const managedWindows = windowManagedSignalDependencies.get(signal);
    managedWindows?.delete(windowId);
  }

  windowDependencies.delete(windowId);
  dirtyWindowNodeIds.delete(windowId);
  dirtyManagedWindowIds.delete(windowId);
  windowsWithStructuralWrite.delete(windowId);

  const nodeDependenciesByWindow = windowNodeDependencies.get(windowId);
  if (nodeDependenciesByWindow) {
    for (const [nodeId, nodeDependencies] of nodeDependenciesByWindow) {
      for (const signal of nodeDependencies) {
        const dependentNodesByWindow = windowNodeSignalDependencies.get(signal);
        dependentNodesByWindow?.get(windowId)?.delete(nodeId);
        if (dependentNodesByWindow?.get(windowId)?.size === 0) {
          dependentNodesByWindow.delete(windowId);
        }
      }
    }
    windowNodeDependencies.delete(windowId);
  }
}

function clearLayerDependencies(layerId: string): void {
  const dependencies = layerDependencies.get(layerId);
  if (!dependencies) {
    return;
  }

  for (const signal of dependencies) {
    const dependentLayers = layerSignalDependencies.get(signal);
    dependentLayers?.delete(layerId);
    const structuralLayers = layerStructuralSignalDependencies.get(signal);
    structuralLayers?.delete(layerId);
  }

  layerDependencies.delete(layerId);
  dirtyLayerNodeIds.delete(layerId);
  layersWithStructuralWrite.delete(layerId);

  const nodeDependenciesByLayer = layerNodeDependencies.get(layerId);
  if (nodeDependenciesByLayer) {
    for (const [nodeId, nodeDependencies] of nodeDependenciesByLayer) {
      for (const signal of nodeDependencies) {
        const dependentNodesByLayer = layerNodeSignalDependencies.get(signal);
        dependentNodesByLayer?.get(layerId)?.delete(nodeId);
        if (dependentNodesByLayer?.get(layerId)?.size === 0) {
          dependentNodesByLayer.delete(layerId);
        }
      }
    }
    layerNodeDependencies.delete(layerId);
  }
}
