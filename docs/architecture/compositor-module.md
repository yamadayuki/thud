---
type: Architecture
title: "Deep Compositor & Session Module Architecture"
description: Two-tier deep module design pairing a deterministic Sans-I/O Compositor core with a declarative Session runner.
tags: [architecture, codebase-design, compositor, seam, sans-io]
generated: { by: antigravity/gemini-4, at: "2026-10-22T09:26:27Z" }
status: stable
---

## Architecture Overview

Following the `codebase-design` principles, `thud` preserves deep external seams (`Compositor`, `Session`, `Config`, and `ui`) while organizing implementation responsibilities into focused internal submodules with clean internal seams.

```mermaid
flowchart TB
    subgraph ExternalCallers["Callers & Tests"]
        CLI["main.rs / CLI"]
        UnitTest["Deterministic Unit Tests"]
        IntegTest["HeadlessHarness & PTY E2E Tests"]
    end

    subgraph SessionModule["Module: src/session/ (Common-Caller Seam)"]
        SessionRun["mod.rs: Session::run / run_headless / send_ipc_message"]
        subgraph InternalSeam["Internal OS & Test Adapters"]
            TtyPty["tty.rs & pty.rs: OsHostTerminal / OsChildPty"]
            IpcMod["ipc.rs: OsIpcServer & Client"]
            ProbesMod["probes.rs: SystemProbes::sample()"]
            HarnessMod["harness.rs: HeadlessHarness"]
        end
    end

    subgraph CompositorModule["Module: src/compositor/ (Sans-I/O Deep Core Seam)"]
        Step["mod.rs: Compositor::new / step(Event, Instant) -> Transition"]
        Store["store.rs: TelemetryStore (TTL Slots, Metrics, Toasts, ActiveCmd)"]
        VTE["vt.rs: VtScreenAdapter & Terminal Query Responder"]
        Layout["layout.rs: PlacementSolver (Anchor & Text/Cursor Dodge)"]
    end

    subgraph UiModule["Module: src/ui/ (Ratatui-Style 2D Cell Rendering Seam)"]
        Primitives["style.rs / rect.rs / text.rs (Rect, Span, Line, CellStyle)"]
        Surface["buffer.rs: Buffer (2D Grid + CJK Wide-Char Healing + ANSI Diff)"]
        Widgets["widgets.rs: Widget trait, Block, Gauge, Sparkline, StatusBar, Toast"]
    end

    CLI --> SessionRun
    IntegTest --> SessionRun
    UnitTest --> Step
    UnitTest --> Surface
    SessionRun --> InternalSeam
    SessionRun --> Step
    Step --> Store
    Step --> VTE
    Step --> Layout
    Step --> Widgets
    Widgets --> Surface
    Primitives --> Surface
```

## Module Responsibilities & Seams

### 1. 2D Rendering Foundation: `src/ui/`

- **Interface**:
  - `Rect`, `Span`, `Line`, `Buffer`, and `Widget::render(self, area: Rect, buf: &mut Buffer)`.
- **Hides**:
  - 2D cell coordinate clipping and `Block` border inner-area geometry.
  - Multi-span styled text measurement (`Line::width()`) and rendering.
  - Automatic CJK 2-cell wide-character split boundary healing on every cell write.
  - Double-buffered minimal ANSI SGR/CUP diff emission (`Buffer::diff(&prev, cursor)`).

### 2. Core Module: `src/compositor/` (Sans-I/O Deep Seam)

- **Interface**:
  - `Compositor::new(config: Config, host_size: TermSize) -> Self`
  - `Compositor::step(&mut self, event: Event, now: Instant) -> Transition`
- **Internal Submodules**:
  - `store.rs` (`TelemetryStore`): Manages ephemeral IPC slots, metric ring buffers, stacked toast notifications, periodic command outputs, and foreground command timers.
  - `vt.rs` (`VtScreenAdapter`): Wraps `vt100::Parser` (Primary/Alternate screen), copies virtual terminal cells into `Buffer`, and synthesizes replies to terminal queries (`DA1`, `DA2`, `DSR 5/6`, `XTVERSION`, Kitty keyboard, `OSC 10/11`).
  - `layout.rs` (`PlacementSolver`): Computes child PTY viewport size and resolves floating overlay bounding-box positions with cursor and non-whitespace text collision dodging (`Ignore`, `AutoHide`, `DodgeToOppositeCorner`).

### 3. Outer Module: `src/session/` (Caller Seam + Internal OS Seam)

- **External Interface**:
  - `Session::run(config: Config) -> Result<i32, SessionError>`
  - `Session::run_headless(config: Config, cols: u16, rows: u16) -> Result<HeadlessHarness, SessionError>`
- **Internal Submodules**:
  - `tty.rs` (`OsHostTerminal`): Polls and configures `STDIN_FILENO` (`0`) directly (avoiding macOS XNU `/dev/tty` `POLLNVAL`), writes diffs to `STDOUT_FILENO` (`1`), and tracks `SIGWINCH`.
  - `pty.rs` (`OsChildPty`): Manages `forkpty`, `TIOCSWINSZ`, non-blocking PTY I/O, and child lifecycle.
  - `ipc.rs` (`OsIpcServer`): Non-blocking Unix domain socket listener and client message sender.
  - `probes.rs` (`SystemProbes`): Stateful OS telemetry collector (`sample(master_fd, child_pid, now) -> BuiltinValues`) encapsulating battery cache, CWD, Git branch, and foreground process command inspection.
  - `harness.rs` (`HeadlessHarness`): Deterministic in-memory adapter for multi-step integration tests.

### 4. Configuration Module: `src/config/`

- **Internal Submodules**:
  - `mod.rs`: Declarative TOML schema (`Config`, `StatusBarConfig`, `OverlayConfig`, `WidgetSpec`) and file loader/writer.
  - `ipc.rs`: Wire protocol types (`IpcMessage`, `ToastLevel`).
  - `default_config.toml`: Annotated default configuration template embedded via `include_str!`.

# Related Concepts
- [Core Terminal HUD & Compositor Requirements](../requirements/core-hud-capabilities.md): satisfies core terminal HUD, full-screen overlay, and widget requirements
