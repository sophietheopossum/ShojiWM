import {
  Box,
  ClientWindow,
  Image,
  ManagedWindow,
  ShaderEffect,
  WindowBorder,
  backdropSource,
  compileEffect,
  computed,
  dualKawaseBlur,
  loadShader,
  read,
  shaderStage,
  useState,
  type WaylandWindow,
} from "shoji_wm";
import type {
  ManagedWindowRect,
  WindowCompositionFunction,
} from "shoji_wm/types";
import {
  EDGE_DRAG_HALO_PX,
  WINDOW_BORDER_PX,
  WINDOW_STATE_FULLSCREEN,
  WINDOW_STATE_MAXIMIZED,
  WINDOW_STATE_MINIMIZE_VISUAL_IDLE,
  WINDOW_STATE_RECT,
  WINDOW_STATE_TILE_DRAGGING,
  WINDOW_STATE_TILE_REORDERING,
  WINDOW_STATE_TILED,
  WINDOW_STATE_VISIBLE_OUTPUTS,
  WINDOW_STATE_WORKSPACE_OFFSET_Y,
  WINDOW_STATE_WORKSPACE_OPACITY,
  WINDOW_STATE_WORKSPACE_TILED,
  WINDOW_STATE_WORKSPACE_VISIBLE,
  type HybridWindowManager,
} from "../window-manager";
import type { PointerTracking } from "./events";
import type { WorkspaceIpc } from "./workspace-ipc";

const FULLSCREEN_Z_INDEX = 2_000_000_000;
const FLOATING_WINDOW_Z_INDEX_BASE = 1_500_000_000;
const WINDOW_STACK_Z_INDEX_RANGE = 100_000_000;
const FOCUSED_TILED_WINDOW_Z_INDEX = 1_000_000_000;
const REORDERING_TILED_WINDOW_Z_INDEX = -2_000_000_000;

// Window corner rounding; the drag tabs clamp their travel to the flat part
// of each edge (between the corner arcs).
const WINDOW_CORNER_RADIUS = 10;

// The managed rect around a client: the client plus the drag halo and border
// on every side. The window manager is constructed with this.
export function naturalRootRect(window: WaylandWindow): ManagedWindowRect {
  const client = window.position;
  const chrome = EDGE_DRAG_HALO_PX + WINDOW_BORDER_PX;
  return {
    x: client.x -
        chrome,
    y: client.y -
        chrome,
    width: client.width +
        chrome * 2,
    height: client.height +
        chrome * 2,
  };
}

export function createWindowComposition(
  windowManager: HybridWindowManager,
  ipc: WorkspaceIpc,
  pointer: PointerTracking,
): WindowCompositionFunction {
  return (window: WaylandWindow) => {
    const decoration = window.decoration();
    const useClientDecoration =
      decoration.mode === "client" &&
      !(
        decoration.clientPreference === "server" &&
        decoration.configuredMode === "server"
      );
    const workspaceVisible = window.state[WINDOW_STATE_WORKSPACE_VISIBLE];
    const workspaceOffsetY = window.state[WINDOW_STATE_WORKSPACE_OFFSET_Y];
    const workspaceOpacity = window.state[WINDOW_STATE_WORKSPACE_OPACITY];
    const tileDragging = window.state[WINDOW_STATE_TILE_DRAGGING];
    const managedRect = computed(() => {
      const rect = window.state[WINDOW_STATE_RECT]();
      return {
        x: read(rect.x),
        y: read(rect.y) + workspaceOffsetY(),
        width: read(rect.width),
        height: read(rect.height),
      };
    });
    const forceRectSize = computed(
      () => window.isResizable() && !window.isTransient(),
    );

    // force no corner rounding CSD
    const tiled = true;

    const stackZIndex = windowManager.getWindowZIndex(window);
    const zIndex = computed(() => {
      if (!window.state[WINDOW_STATE_WORKSPACE_TILED]()) {
        return stackZIndex();
      }
      const stackOffset = Math.max(
        -WINDOW_STACK_Z_INDEX_RANGE,
        Math.min(WINDOW_STACK_Z_INDEX_RANGE, stackZIndex()),
      );
      if (!window.state[WINDOW_STATE_TILED]()) {
        return FLOATING_WINDOW_Z_INDEX_BASE + stackOffset;
      }
      if (window.state[WINDOW_STATE_TILE_REORDERING]()) {
        return REORDERING_TILED_WINDOW_Z_INDEX;
      }
      return window.isFocused()
        ? FOCUSED_TILED_WINDOW_Z_INDEX
        : stackOffset;
    });
    const minimizeVisualIdle = window.state[WINDOW_STATE_MINIMIZE_VISUAL_IDLE];
    const inactive = computed(
      () => minimizeVisualIdle() || (!workspaceVisible() && !tileDragging()),
    );

    // Eternal Darkness red (Theme.red / Theme.redDim until shared theme.json).
    const borderColor = window.isFocused((focused) =>
      focused ? "#e0263c" : "#8f1e2d",
    );
    // A soft drop shadow under the frame; the focused window floats a bit higher.
    const windowShadow = window.isFocused((focused) =>
      focused
        ? [{ y: 10, blur: 32, spread: -2, color: "#00000090" }]
        : [{ y: 4, blur: 16, spread: -2, color: "#00000060" }],
    );

    const backgroundShader = compileEffect({
      input: backdropSource(),
      capturePadding: 24,
      invalidate: { kind: "on-source-damage-box", damagePadding: 8 },
      pipeline: [
        dualKawaseBlur({ radius: 4, passes: 2 }),
        shaderStage(loadShader("./src/effect/liquid-glass.frag"), {
          uniforms: {
            // Follow the window's rounded corners.
            glass_radius_px: -1.0,
            distortion_depth: 0.2,
            distortion_strength: 0.15,
            chromatic_shift_px: 3.0,
            glass_tint: 0.9,
          },
        }),
      ],
    });


    let innerComponents = <ClientWindow />;

    const TERMINALS = ["kitty", "ghostty"];

    if (TERMINALS.includes(window.appId() ?? "")) {
      innerComponents = (
        <ShaderEffect shader={backgroundShader} direction="column">
          <ClientWindow />
        </ShaderEffect>
      );
    }

    // Fullscreen: drop all chrome (titlebar, border, rounded corners) and let
    // the client surface fill its managed rect edge to edge. The rect is set to
    // the whole output by onWindowFullscreenRequest. Rendering nothing but the
    // bare ClientWindow is also what lets the tty backend promote the client
    // buffer to the primary plane (direct scanout).
    if (window.state[WINDOW_STATE_FULLSCREEN]()) {
      return (
        <ManagedWindow
          rect={managedRect}
          zIndex={FULLSCREEN_Z_INDEX}
          visibleOutputs={window.state[WINDOW_STATE_VISIBLE_OUTPUTS]}
          opacity={workspaceOpacity}
          forceRectSize={forceRectSize}
          tiled={tiled}
          idle={inactive}
          interactive={inactive((value) => !value)}
          // Low-latency tearing for fullscreen windows, off by default. This was `true` on the
          // reasoning that the compositor only tears once the window is on the direct-scanout
          // fast path AND is committing faster than the refresh rate, so it would be a no-op
          // outside games. Both halves stopped holding on 1/9/2026: direct scanout began
          // engaging routinely (it previously never did), and the TV was pinned to 60Hz, which
          // is far easier to out-commit than the 120Hz it had auto-selected. The result was
          // visible tearing on HDMI-A-3 during ordinary use. Re-enable per app if wanted, e.g.
          // `allowTearing={isGame(window.appId())}`.
          allowTearing={false}
        >
          <ClientWindow />
        </ManagedWindow>
      );
    }

    // use less Server-Side Decoration
    if (useClientDecoration) {
      return (
        <ManagedWindow
          rect={managedRect}
          zIndex={zIndex}
          visibleOutputs={window.state[WINDOW_STATE_VISIBLE_OUTPUTS]}
          opacity={workspaceOpacity}
          forceRectSize={forceRectSize}
          tiled={tiled}
          idle={inactive}
          interactive={inactive((value) => !value)}
        >
          <WindowBorder
            style={{
              // A maximised window fills the output, so a border and rounded corners
              // around it are just inset client area — and upstream's 08b0d50, which
              // added this CSD path, draws them unconditionally. px and borderRadius
              // are both MaybeSignal, so this tracks maximise/restore live rather
              // than being fixed when the window is first composed.
              border: {
                px: computed(() => (window.isMaximized() ? 0 : WINDOW_BORDER_PX)),
                color: borderColor,
              },
              borderRadius: computed(() => (window.isMaximized() ? 0 : 10)),
              // Same for the shadow: a maximised window floats over nothing.
              boxShadow: computed(() => (window.isMaximized() ? [] : windowShadow())),
              background: "#10131900",
              padding: 0,
              paddingX: 0,
              paddingRight: 0,
            }}
            interaction={{
              resizeHitArea: {
                edgePx: 8,
                cornerPx: 14,
              },
            }}
          >
            <ClientWindow />
          </WindowBorder>
        </ManagedWindow>
      );
    }

    // Maximized: no chrome at all
      // — edge to edge in the usable area
    // (maximizedRectForWindow applies no inset to match).
      // Without the halo a maximized window is not pointer-draggable; unmaximize re-centres it, so
    // it can never get stuck. Floating windows below keep the full chrome.
    if (window.state[WINDOW_STATE_MAXIMIZED]()) {
      return (
        <ManagedWindow
          rect={managedRect}
          zIndex={windowManager.getWindowZIndex(window)}
          visibleOutputs={window.state[WINDOW_STATE_VISIBLE_OUTPUTS]}
          opacity={workspaceOpacity}
          forceRectSize={forceRectSize}
          tiled={tiled}
          idle={inactive}
          interactive={inactive((value) => !value)}
        >
          <Box direction="row">{innerComponents}</Box>
        </ManagedWindow>
      );
    }

    // The transparent halo ring around the window is decoration chrome: the SSD
    // hit-test resolves clicks there to Move (outer resizeHitArea band wins for
    // resize), so the whole ring drags the window. Hovering it reveals a tab at
    // that edge as the visible affordance; the tab itself is plain chrome, so
    // grabbing it drags too. Chrome can't render above the client surface,
    // which is why the tab lives outside the window instead of overlapping it.
    const [hoveredEdge, setHoveredEdge] = useState<
      "top" | "bottom" | "left" | "right" | null
    >(null);
    const dragEdgeHover =
      (edge: "top" | "bottom" | "left" | "right") => (inside: boolean) => {
        if (inside) {
          setHoveredEdge(edge);
        } else if (read(hoveredEdge) === edge) {
          setHoveredEdge(null);
        }
        pointer.setEdgeHovered(window.id, read(hoveredEdge) !== null);
      };
    // Trapezium drag tabs (SVG assets, red stipple + border) attached to the
    // window border, centred on the pointer along the hovered edge. The
    // position computeds read the pointer only while their edge is
    // hovered, so idle windows never re-evaluate on mouse motion.
    const DRAG_TAB_LENGTH = 72;
    const DRAG_TAB_THICKNESS = 12;
    // Travel limit: the tab slides along the flat part of the edge and pins at
    // the corner arcs. While pinned it stays visible (visibility follows the
    // hover strip, not the pointer-tab overlap) and stays draggable (the whole
    // halo is move chrome).
    const dragTabMin = EDGE_DRAG_HALO_PX + WINDOW_CORNER_RADIUS;
    const dragTabX = computed(() => {
      const edge = hoveredEdge();
      if (edge !== "top" && edge !== "bottom") {
        return 0;
      }
      const rect = managedRect();
      const max = Math.max(
        dragTabMin,
        read(rect.width) -
          dragTabMin -
          DRAG_TAB_LENGTH,
      );
      const centred = Math.round(
        pointer.current().x - read(rect.x) -
          DRAG_TAB_LENGTH
          / 2,
      );
      return Math.min(max, Math.max(
          dragTabMin,
          centred,
          ));
    });
    const dragTabY = computed(() => {
      const edge = hoveredEdge();
      if (edge !== "left" && edge !== "right") {
        return 0;
      }
      const rect = managedRect();
      const max = Math.max(
        dragTabMin,
        read(rect.height) -
          dragTabMin -
          DRAG_TAB_LENGTH,
      );
      const centred = Math.round(
        pointer.current().y - read(rect.y) - DRAG_TAB_LENGTH / 2,
      );
      return Math.min(max, Math.max(
          dragTabMin,
          centred,
          ));
    });

    // Published to the workspace IPC view (see attachDragTabs): the tab's
    // layout-space rect while an edge is hovered, null otherwise. Lazy — only
    // evaluated when a view is built.
    ipc.publishDragTab(window.id, () => {
      const edge = read(hoveredEdge);
      if (!edge) {
        return null;
      }
      const rect = managedRect();
      switch (edge) {
        case "top":
          return {
            x: rect.x + dragTabX(),
            y: rect.y + EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS,
            width: DRAG_TAB_LENGTH,
            height: DRAG_TAB_THICKNESS,
          };
        case "bottom":
          return {
            x: rect.x + dragTabX(),
            y: rect.y + rect.height - EDGE_DRAG_HALO_PX,
            width: DRAG_TAB_LENGTH,
            height: DRAG_TAB_THICKNESS,
          };
        case "left":
          return {
            x: rect.x + EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS,
            y: rect.y + dragTabY(),
            width: DRAG_TAB_THICKNESS,
            height: DRAG_TAB_LENGTH,
          };
        case "right":
          return {
            x: rect.x + rect.width - EDGE_DRAG_HALO_PX,
            y: rect.y + dragTabY(),
            width: DRAG_TAB_THICKNESS,
            height: DRAG_TAB_LENGTH,
          };
      }
    });

    return (
      <ManagedWindow
        rect={managedRect}
        zIndex={zIndex}
        visibleOutputs={window.state[WINDOW_STATE_VISIBLE_OUTPUTS]}
        opacity={workspaceOpacity}
        forceRectSize={forceRectSize}
        tiled={tiled}
        idle={inactive}
        interactive={inactive((value) => !value)}
      >
        {/* No `position` here: the halo box must NOT establish a containing
            block, so its absolute children (strips + tabs) anchor to the
            decoration root's full rect — the halo's outer edge — instead of
            the padding-inset content box at the window border. */}
        <Box style={{ padding: EDGE_DRAG_HALO_PX }}>
          <WindowBorder
            style={{
              border: { px: WINDOW_BORDER_PX, color: borderColor },
              borderRadius: WINDOW_CORNER_RADIUS,
              boxShadow: windowShadow,
              background: "#10131900",
              padding: 0,
              paddingX: 0,
              paddingRight: 0,
            }}
            interaction={{
              resizeHitArea: {
                edgePx: 8,
                cornerPx: 14,
              },
            }}
          >
            <Box direction="row">{innerComponents}</Box>
          </WindowBorder>
          <Box
            onHoverChange={dragEdgeHover("top")}
            style={{
              position: "absolute",
              top: 0,
              left: 0,
              right: 0,
              height: EDGE_DRAG_HALO_PX,
            }}
          />
          <Box
            onHoverChange={dragEdgeHover("bottom")}
            style={{
              position: "absolute",
              bottom: 0,
              left: 0,
              right: 0,
              height: EDGE_DRAG_HALO_PX,
            }}
          />
          <Box
            onHoverChange={dragEdgeHover("left")}
            style={{
              position: "absolute",
              left: 0,
              top: EDGE_DRAG_HALO_PX,
              bottom: EDGE_DRAG_HALO_PX,
              width: EDGE_DRAG_HALO_PX,
            }}
          />
          <Box
            onHoverChange={dragEdgeHover("right")}
            style={{
              position: "absolute",
              right: 0,
              top: EDGE_DRAG_HALO_PX,
              bottom: EDGE_DRAG_HALO_PX,
              width: EDGE_DRAG_HALO_PX,
            }}
          />
          <Image
            src="./assets/drag-tab-top.svg"
            style={{
              position: "absolute",
              top: EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS,
              left: dragTabX,
              width: DRAG_TAB_LENGTH,
              height: DRAG_TAB_THICKNESS,
              visible: hoveredEdge((edge) => edge === "top"),
            }}
          />
          <Image
            src="./assets/drag-tab-bottom.svg"
            style={{
              position: "absolute",
              bottom: EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS,
              left: dragTabX,
              width: DRAG_TAB_LENGTH,
              height: DRAG_TAB_THICKNESS,
              visible: hoveredEdge((edge) => edge === "bottom"),
            }}
          />
          <Image
            src="./assets/drag-tab-left.svg"
            style={{
              position: "absolute",
              left: EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS,
              top: dragTabY,
              width: DRAG_TAB_THICKNESS,
              height: DRAG_TAB_LENGTH,
              visible: hoveredEdge((edge) => edge === "left"),
            }}
          />
          <Image
            src="./assets/drag-tab-right.svg"
            style={{
              position: "absolute",
              right: EDGE_DRAG_HALO_PX - DRAG_TAB_THICKNESS,
              top: dragTabY,
              width: DRAG_TAB_THICKNESS,
              height: DRAG_TAB_LENGTH,
              visible: hoveredEdge((edge) => edge === "right"),
            }}
          />
        </Box>
      </ManagedWindow>
    );
  };
}
