import type {
  BackdropBlurOptions,
  BackdropSourceHandle,
  XrayBackdropSourceHandle,
  BlendMode,
  BlendStageHandle,
  CompiledEffectHandle,
  DualKawaseBlurStageHandle,
  EffectInputHandle,
  NoiseKind,
  NoiseStageHandle,
  SaveStageHandle,
  ShaderModuleHandle,
  ShaderStageHandle,
  ShaderInputHandle,
  UnitStageHandle,
  ImageSourceHandle,
  NamedTextureHandle,
  ShaderUniformMap,
  EffectAlphaMode,
  EffectInvalidationPolicyHandle,
  EffectOutsets,
  LayerEffectHandle,
  LayerEffectInputHandle,
  LayerSourceHandle,
  PopupEffectHandle,
  PopupEffectInputHandle,
  PopupSourceHandle,
  WindowEffectHandle,
  WindowSourceHandle,
} from "./types";

let assetBaseDir = "/";

export interface CompileEffectOptions {
  input: EffectInputHandle;
  invalidate?: EffectInvalidationPolicyHandle;
  pipeline: Array<
    | ShaderStageHandle
    | NoiseStageHandle
    | DualKawaseBlurStageHandle
    | SaveStageHandle
    | BlendStageHandle
    | UnitStageHandle
  >;
  /**
   * Output alpha handling. Defaults to `"opaque"`, which forces the result
   * to full opacity to hide capture/blur alpha noise at the edges — the
   * right choice for plain backdrop blurs. Declare `"preserve"` when the
   * pipeline intentionally produces transparency (e.g. masking the blur
   * against a layer's own alpha); the pipeline is then responsible for the
   * alpha of every pixel, including the blur edge regions.
   * See {@link EffectAlphaMode}.
   */
  alpha?: EffectAlphaMode;
}

export interface CompileWindowEffectOptions extends CompileEffectOptions {
  input: WindowSourceHandle;
  outsets?: EffectOutsets;
}

export interface CompileLayerEffectOptions extends CompileEffectOptions {
  input: LayerEffectInputHandle;
  outsets?: EffectOutsets;
}

// Base directory for relative asset paths (shaders, images, fonts). Callers
// pass the already-resolved config package root - typically the directory
// containing the nearest ancestor package.json of the entry config file.
export function installAssetResolverBridge(configRoot: string): void {
  assetBaseDir = normalizePath(
    isAbsolutePath(configRoot) ? configRoot : resolvePath("/", configRoot),
  );
}

export function installShaderResolverBridge(configPath: string): void {
  assetBaseDir = dirnamePath(resolvePath(assetBaseDir, configPath));
}

export function resolveAssetPath(path: string): string {
  return isAbsolutePath(path) ? path : resolvePath(assetBaseDir, path);
}

export function loadShader(path: string): ShaderModuleHandle {
  return {
    kind: "shader-module",
    path: resolveAssetPath(path),
  };
}

export function backdropSource(): BackdropSourceHandle {
  return { kind: "backdrop-source" };
}

export function xrayBackdropSource(): XrayBackdropSourceHandle {
  return { kind: "xray-backdrop-source" };
}

export function windowSource(
  options: { include?: "full" | "root-surface" } = {},
): WindowSourceHandle {
  return {
    kind: "window-source",
    include: options.include ?? "full",
  };
}

export function layerSource(
  options: { include?: "full" | "root-surface" } = {},
): LayerSourceHandle {
  return {
    kind: "layer-source",
    include: options.include ?? "full",
  };
}

/** The popup's own rendered content as an effect input. */
export function popupSource(
  options: { include?: "full" | "root-surface" } = {},
): PopupSourceHandle {
  return {
    kind: "popup-source",
    include: options.include ?? "full",
  };
}

export function imageSource(path: string): ImageSourceHandle {
  return {
    kind: "image-source",
    path: resolveAssetPath(path),
  };
}

export function get(name: string): NamedTextureHandle {
  return {
    kind: "named-texture",
    name,
  };
}

export function shaderStage(
  shader: string | ShaderModuleHandle,
  options: {
    uniforms?: ShaderUniformMap;
    textures?: Record<string, EffectInputHandle>;
  } = {},
): ShaderStageHandle {
  return {
    kind: "shader-stage",
    shader: typeof shader === "string" ? loadShader(shader) : shader,
    uniforms: options.uniforms,
    textures: options.textures,
  };
}

export function shaderInput(
  shader: string | ShaderModuleHandle,
  options: {
    uniforms?: ShaderUniformMap;
    textures?: Record<string, EffectInputHandle>;
  } = {},
): ShaderInputHandle {
  return {
    kind: "shader-input",
    shader: typeof shader === "string" ? loadShader(shader) : shader,
    uniforms: options.uniforms,
    textures: options.textures,
  };
}

export function noise(
  options: { kind?: NoiseKind; amount?: number } = {},
): NoiseStageHandle {
  return {
    kind: "noise",
    noiseKind: options.kind ?? "salt",
    amount: options.amount,
  };
}

export function dualKawaseBlur(
  options: BackdropBlurOptions = {},
): DualKawaseBlurStageHandle {
  return {
    kind: "dual-kawase-blur",
    radius: options.radius,
    passes: options.passes,
  };
}

export function save(name: string): SaveStageHandle {
  return {
    kind: "save",
    name,
  };
}

export function blend(
  input: EffectInputHandle,
  options: { mode?: BlendMode; alpha?: number } = {},
): BlendStageHandle {
  return {
    kind: "blend",
    input,
    mode: options.mode,
    alpha: options.alpha,
  };
}

export function unit(effect: CompiledEffectHandle): UnitStageHandle {
  return {
    kind: "unit",
    effect,
  };
}

function isAbsolutePath(path: string): boolean {
  return path.startsWith("/");
}

function dirnamePath(path: string): string {
  const normalized = normalizePath(path);
  if (normalized === "/") {
    return "/";
  }
  const index = normalized.lastIndexOf("/");
  return index <= 0 ? "/" : normalized.slice(0, index);
}

function resolvePath(...paths: string[]): string {
  return normalizePath(paths.filter(Boolean).join("/"));
}

function normalizePath(path: string): string {
  const absolute = path.startsWith("/");
  const parts = path
    .split("/")
    .filter((part) => part.length > 0 && part !== ".");
  const stack: string[] = [];

  for (const part of parts) {
    if (part === "..") {
      if (stack.length > 0) {
        stack.pop();
      }
      continue;
    }
    stack.push(part);
  }

  const joined = stack.join("/");
  if (absolute) {
    return joined ? `/${joined}` : "/";
  }
  return joined || ".";
}

export function compileEffect(
  options: CompileEffectOptions,
): CompiledEffectHandle {
  return {
    kind: "compiled-effect",
    input: options.input,
    invalidate: options.invalidate ?? {
      kind: "on-source-damage-box",
      antiArtifactMargin: 0,
    },
    pipeline: options.pipeline,
    alpha: options.alpha ?? "opaque",
  };
}

export function compileWindowEffect(
  options: CompileWindowEffectOptions,
): WindowEffectHandle {
  return {
    kind: "window-effect",
    effect: compileEffect(options),
    outsets: options.outsets,
  };
}

export function compileLayerEffect(
  options: CompileLayerEffectOptions,
): LayerEffectHandle {
  return {
    kind: "layer-effect",
    effect: compileEffect(options),
    outsets: options.outsets,
  };
}

export interface CompilePopupEffectOptions extends CompileEffectOptions {
  input: PopupEffectInputHandle;
  outsets?: EffectOutsets;
}

export function compilePopupEffect(
  options: CompilePopupEffectOptions,
): PopupEffectHandle {
  return {
    kind: "popup-effect",
    effect: compileEffect(options),
    outsets: options.outsets,
  };
}
