---
sidebar_position: 2
---

# ShojiWM Architecture

In one sentence: **ShojiWM is a Wayland compositor with a fast core written in
Rust, whose look and behavior you describe in TypeScript/TSX.**

## The big picture

```mermaid
flowchart LR
  apps["Apps<br/>(Firefox, terminal, games...)"]
  core["ShojiWM core<br/>(Rust + Smithay)"]
  config["Config runtime<br/>(TypeScript / TSX on Node.js)"]
  gpu["GPU & Display<br/>(OpenGL · DRM/KMS)"]

  apps -- "Wayland protocol" --> core
  core -- "window state" --> config
  config -- "decoration tree (JSON)" --> core
  core -- "render" --> gpu
```

- **Apps** talk to ShojiWM through the standard **Wayland protocol**.
- The **Rust core** handles input, windows, and rendering — the parts that must
  be fast and reliable.
- The **TypeScript config runtime** decides how windows look and behave. You
  write this part.
- The core draws the final frame on the **GPU**.

## Two worlds: Rust core and TypeScript config

ShojiWM splits responsibilities into two processes:

| Layer | Language | Responsibility |
| --- | --- | --- |
| Core | Rust + Smithay | Wayland protocol, input, layout, GPU rendering |
| Config | TypeScript/TSX | Window decorations, layout rules, effects, keybindings |

They communicate over a Unix socket. See the Japanese page for a more detailed,
beginner-friendly walkthrough with sequence diagrams.

## Server-Side Decoration (SSD) flow

```mermaid
sequenceDiagram
  participant App as App
  participant Core as Rust core
  participant TS as TS runtime
  App->>Core: Window changes (title, focus, size)
  Core->>TS: Window snapshot
  TS->>TS: Evaluate composition(window)
  TS-->>Core: Decoration tree (JSON)
  Core->>Core: Layout + render
```

## Directory layout

```
src/        Rust core (compositor, IPC, protocol, portal)
packages/   TypeScript SDK (shoji_wm) and user config
```
