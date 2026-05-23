import { animationVariable, createWindowStack, createWindowState, cubicBezier, read, seconds, WINDOW_MANAGER, type PointerMoveEvent, type ReadonlySignal, type WaylandWindow, type WindowActivateRequestEvent, type WindowMaximizeRequestEvent, type WindowMinimizeRequestEvent, type WindowMoveEvent, type WindowResizeEvent, type WindowResizeRect } from "shoji_wm";
import type { ManagedWindowRect, WindowSizeConstraints } from "shoji_wm/types";
import { playRectAnimation, stopRectAnimation } from "./window-animation";

export const WINDOW_STATE_RECT = createWindowState<ManagedWindowRect>("rect", {
    default: (window) => window.rect,
});
export const WINDOW_STATE_RESTORE_RECT = createWindowState<ManagedWindowRect | null>("restoreRect", {
    default: null,
});
export const WINDOW_STATE_MINIMIZED = createWindowState<boolean>("minimized", {
    default: false,
});
export const WINDOW_STATE_MAXIMIZED = createWindowState<boolean>("maximized", {
    default: false,
});

const OPEN_CLOSE_ANIMATION_DURATION = seconds(0.5);
const WINDOW_MANAGEMENT_ANIMATION_DURATION = seconds(0.5);
const UNMAXIMIZE_GRAB_ANIMATION_DURATION = 90;
const WINDOW_MANAGEMENT_EASING = cubicBezier(0.1, 1.1, 0.1, 1.1);
const WINDOW_CLOSE_EASING = cubicBezier(0.3, -0.3, 0, 1);
export const OPEN_ANIMATION = animationVariable("window.open");
export const WINDOW_BORDER_PX = 2;
export const TITLEBAR_HEIGHT = 30;

export class HybridWindowManager {
    private readonly workspaces = new Map<number, Workspace>();
    private readonly windowStack = createWindowStack();
    private readonly naturalRootRect: (rect: WaylandWindow) => ManagedWindowRect;
    private currentMonitor: string;
    private isGrabbing = false;

    public constructor(naturalRootRect: (rect: WaylandWindow) => ManagedWindowRect) {
        this.currentMonitor = "";
        this.syncWorkspaces();
        this.naturalRootRect = naturalRootRect;
    }

    public onPointerMove(event: PointerMoveEvent) {
        this.syncWorkspaces();
        this.currentMonitor = event.outputName ?? this.currentMonitor;
    }

    public onOpen(window: WaylandWindow) {
        window.focus();
        this.windowStack.add(window);

        window.setCloseAnimationDuration(OPEN_CLOSE_ANIMATION_DURATION);
        window.animation.start(OPEN_ANIMATION, {
            duration: OPEN_CLOSE_ANIMATION_DURATION,
            to: 1,
            easing: WINDOW_MANAGEMENT_EASING,
        });
    }

    public onFirstCommit(window: WaylandWindow) {
        const workspace = this.getCurrentWorkspace();
        if (workspace) {
            workspace.addWindow(window);
        } else {
            window.state[WINDOW_STATE_RECT].set(this.naturalRootRect(window));
        }

        if (window.isMaximized()) {
            window.state[WINDOW_STATE_RESTORE_RECT].set(this.initialRestoreRectForMaximizedWindow(window));
            window.state[WINDOW_STATE_RECT].set(this.maximizedRectForWindow(window));
            window.state[WINDOW_STATE_MAXIMIZED].set(true);
        }
    }

    public onStartClose(window: WaylandWindow) {
        window.animation.start(OPEN_ANIMATION, {
            duration: OPEN_CLOSE_ANIMATION_DURATION,
            to: 0,
            easing: WINDOW_CLOSE_EASING,
        });
    }

    public onClose(window: WaylandWindow) {
        this.windowStack.remove(window);
        for (const workspace of this.workspaces.values()) {
            workspace.removeWindow(window);
        }
    }

    public onFocus(window: WaylandWindow, focused: boolean) {
        if (focused) {
            this.windowStack.raise(window);
        }
    }

    public onWindowResize(event: WindowResizeEvent) {
        stopRectAnimation(event.window, WINDOW_STATE_RECT);
        event.window.state[WINDOW_STATE_RECT].set(this.constrainResizeRect(event));
    }

    public onWindowMove(event: WindowMoveEvent) {
        if (event.phase === "start") {
            this.isGrabbing = true;
        }

        const window = event.window;
        if (window.state[WINDOW_STATE_MAXIMIZED]()) {
            const restoreRect = window.state[WINDOW_STATE_RESTORE_RECT]() ?? event.currentRect;
            const width = read(restoreRect.width);
            const height = read(restoreRect.height);
            const nextRect = this.restoreRectForMaximizedMove(event, width, height);
            if (event.phase === "start") {
                playRectAnimation(
                    window,
                    WINDOW_STATE_RECT,
                    nextRect,
                    WINDOW_MANAGEMENT_EASING,
                    UNMAXIMIZE_GRAB_ANIMATION_DURATION,
                );
            } else {
                stopRectAnimation(window, WINDOW_STATE_RECT);
                window.state[WINDOW_STATE_RECT].set(nextRect);
            }
            window.state[WINDOW_STATE_RESTORE_RECT].set(nextRect);

            if (event.phase === "end") {
                this.isGrabbing = false;
                window.unmaximize();
            } else if (event.phase === "cancel") {
                this.isGrabbing = false;
            }
            return;
        }

        stopRectAnimation(window, WINDOW_STATE_RECT);
        window.state[WINDOW_STATE_RECT].set(event.currentRect);

        if (event.phase === "end" || event.phase === "cancel") {
            this.isGrabbing = false;
        }
    }

    public onWindowMaximizeRequest(event: WindowMaximizeRequestEvent) {
        if (this.isGrabbing) {
            return;
        }

        const window = event.window;
        window.state[WINDOW_STATE_MINIMIZED].set(false);

        if (!event.maximized) {
            const restoreRect = window.state[WINDOW_STATE_RESTORE_RECT]();
            if (restoreRect) {
                playRectAnimation(
                    window,
                    WINDOW_STATE_RECT,
                    restoreRect,
                    WINDOW_MANAGEMENT_EASING,
                    WINDOW_MANAGEMENT_ANIMATION_DURATION,
                );
            }
            window.state[WINDOW_STATE_RESTORE_RECT].set(null);
            window.state[WINDOW_STATE_MAXIMIZED].set(false);
            return;
        }

        if (!window.state[WINDOW_STATE_MAXIMIZED]()) {
            const currentRect = window.state[WINDOW_STATE_RECT]();
            const currentWidth = read(currentRect.width);
            const currentHeight = read(currentRect.height);
            if (currentWidth > 1 && currentHeight > 1) {
                window.state[WINDOW_STATE_RESTORE_RECT].set(currentRect);
            }
        }
        playRectAnimation(
            window,
            WINDOW_STATE_RECT,
            this.maximizedRectForWindow(window),
            WINDOW_MANAGEMENT_EASING,
            WINDOW_MANAGEMENT_ANIMATION_DURATION,
        );
        window.state[WINDOW_STATE_MAXIMIZED].set(true);
    }

    public onWindowMinimizeRequest(event: WindowMinimizeRequestEvent) {
        stopRectAnimation(event.window, WINDOW_STATE_RECT);
        event.window.state[WINDOW_STATE_MINIMIZED].set(event.minimized);
    }

    public onWindowActivateRequest(event: WindowActivateRequestEvent) {
        event.window.state[WINDOW_STATE_MINIMIZED].set(false);
        event.window.focus();
    }

    public getCurrentWorkspace(): Workspace | undefined {
        this.syncWorkspaces();
        var currentWorkspace = undefined;
        for (const workspace of this.workspaces.values()) {
            if (this.currentMonitor === workspace.monitor) {
                currentWorkspace = workspace;
                break;
            }
        }
        return currentWorkspace ?? this.workspaces.values().next().value;
    }

    public getWindowZIndex(window: WaylandWindow): ReadonlySignal<number> {
        return this.windowStack.zIndex(window);
    }

    private syncWorkspaces() {
        let index = this.workspaces.size + 1;
        for (const monitor of WINDOW_MANAGER.output.list) {
            const exists = Array.from(this.workspaces.values()).some((workspace) => workspace.monitor === monitor);
            if (exists) {
                continue;
            }
            this.workspaces.set(index, new Workspace(index, monitor, this.naturalRootRect));
            index++;
        }

        if (!this.currentMonitor || !WINDOW_MANAGER.output.list.includes(this.currentMonitor)) {
            this.currentMonitor = WINDOW_MANAGER.output.list.at(0) ?? "";
        }
    }

    private constrainResizeRect(event: WindowResizeEvent): ManagedWindowRect {
        const constraints = event.window.sizeConstraints();
        const extra = this.clientToRootSizeExtra(event.window);
        const minWidth = Math.max(1, constraints.min?.width ?? 1) + extra.width;
        const minHeight = Math.max(1, constraints.min?.height ?? 1) + extra.height;
        const maxWidth = constrainedMax(constraints, "width", extra.width);
        const maxHeight = constrainedMax(constraints, "height", extra.height);

        const width = clamp(event.currentRect.width, minWidth, Math.max(minWidth, maxWidth));
        const height = clamp(event.currentRect.height, minHeight, Math.max(minHeight, maxHeight));

        return {
            x: resizeOriginForAxis(event.startRect, event.currentRect, width, event.edges.left, "x"),
            y: resizeOriginForAxis(event.startRect, event.currentRect, height, event.edges.top, "y"),
            width,
            height,
        };
    }

    private clientToRootSizeExtra(window: WaylandWindow): { width: number; height: number } {
        const natural = this.naturalRootRect(window);
        return {
            width: Math.max(0, read(natural.width) - window.position.width),
            height: Math.max(0, read(natural.height) - window.position.height),
        };
    }

    private maximizedRectForWindow(window: WaylandWindow): ManagedWindowRect {
        const rect = window.state[WINDOW_STATE_RECT]();
        const centerX = read(rect.x) + read(rect.width) / 2;
        const centerY = read(rect.y) + read(rect.height) / 2;
        const outputName = this.outputNameAt(centerX, centerY) ?? this.currentMonitor;
        const output = outputName ? WINDOW_MANAGER.output.current[outputName] : undefined;
        const usable = outputName ? WINDOW_MANAGER.layer.usableArea(outputName) : undefined;

        if (usable) {
            return {
                x: usable.x,
                y: usable.y,
                width: usable.width,
                height: usable.height,
            };
        }
        if (output?.resolution) {
            return {
                x: output.position.x,
                y: output.position.y,
                width: output.resolution.width / output.scale,
                height: output.resolution.height / output.scale,
            };
        }
        return rect;
    }

    private initialRestoreRectForMaximizedWindow(window: WaylandWindow): ManagedWindowRect {
        const maximizedRect = this.maximizedRectForWindow(window);
        const width = Math.max(1, read(maximizedRect.width) * 0.7);
        const height = Math.max(1, read(maximizedRect.height) * 0.7);
        return {
            x: read(maximizedRect.x) + (read(maximizedRect.width) - width) / 2,
            y: read(maximizedRect.y) + (read(maximizedRect.height) - height) / 2,
            width,
            height,
        };
    }

    private restoreRectForMaximizedMove(
        event: WindowMoveEvent,
        width: number,
        height: number,
    ): ManagedWindowRect {
        const pointer = event.currentPointer;
        const titlebarCenterY = WINDOW_BORDER_PX + TITLEBAR_HEIGHT / 2;
        const pointerOffsetY = event.source === "modifier"
            ? height / 2
            : Math.min(height / 2, titlebarCenterY);

        return {
            x: pointer.x - width / 2,
            y: pointer.y - pointerOffsetY,
            width,
            height,
        };
    }

    private outputNameAt(x: number, y: number): string | undefined {
        for (const name of WINDOW_MANAGER.output.list) {
            const output = WINDOW_MANAGER.output.current[name];
            if (!output?.resolution) {
                continue;
            }
            const width = output.resolution.width / output.scale;
            const height = output.resolution.height / output.scale;
            if (
                x >= output.position.x &&
                y >= output.position.y &&
                x < output.position.x + width &&
                y < output.position.y + height
            ) {
                return name;
            }
        }
        return undefined;
    }
}

export class Workspace {
    private readonly index: number;
    private readonly windows: WaylandWindow[] = [];
    private readonly naturalRootRect: (window: WaylandWindow) => ManagedWindowRect;
    monitor: string | null;
    isTiled = false;

    public constructor(
        index: number,
        monitor: string,
        naturalRootRect: (window: WaylandWindow) => ManagedWindowRect
    ) {
        this.index = index;
        this.monitor = monitor;
        this.naturalRootRect = naturalRootRect;
    }

    public addWindow(window: WaylandWindow) {
        if (this.windows.map(window => window.id).includes(window.id)) {
            return;
        }
        this.windows.push(window);

        const monitorName = this.monitor;
        if (monitorName == null || !WINDOW_MANAGER.output.list.includes(monitorName)) {
            return;
        }

        if (this.isTiled) {
            // TODO
        } else {
            const sizeRect = this.naturalRootRect(window);
            const monitor = WINDOW_MANAGER.output.current[monitorName];
            if (!monitor?.resolution) {
                window.state[WINDOW_STATE_RECT].set(sizeRect);
                return;
            }
            const usableRect = WINDOW_MANAGER.layer.usableArea(monitorName);

            const logicalWidth = usableRect?.width ?? monitor.resolution.width / monitor.scale;
            const logicalHeight = usableRect?.height ?? monitor.resolution.height / monitor.scale;
            const logicalX = usableRect?.x ?? monitor.position.x;
            const logicalY = usableRect?.y ?? monitor.position.y;
            const initRect = {
                x: logicalX + (logicalWidth - read(sizeRect.width)) / 2,
                y: logicalY + (logicalHeight - read(sizeRect.height)) / 2,
                width: read(sizeRect.width),
                height: read(sizeRect.height),
            };
            window.state[WINDOW_STATE_RECT].set(initRect);
        }
    }

    public removeWindow(window: WaylandWindow) {
        const index = this.windows.findIndex((current) => current.id === window.id);
        if (index >= 0) {
            this.windows.splice(index, 1);
        }
    }

    public getWindows(): WaylandWindow[] {
        return Array.from(this.windows);
    }
}

function constrainedMax(
    constraints: WindowSizeConstraints,
    axis: "width" | "height",
    extra: number,
): number {
    const max = constraints.max?.[axis];
    return max && max > 0 ? max + extra : Number.POSITIVE_INFINITY;
}

function resizeOriginForAxis(
    start: WindowResizeRect,
    current: WindowResizeRect,
    constrainedSize: number,
    negativeEdge: boolean,
    axis: "x" | "y",
): number {
    if (!negativeEdge) {
        return current[axis];
    }

    const startSize = axis === "x" ? start.width : start.height;
    return start[axis] + startSize - constrainedSize;
}

function clamp(value: number, min: number, max: number): number {
    return Math.min(Math.max(value, min), max);
}
