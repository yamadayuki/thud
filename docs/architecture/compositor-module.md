---
type: Architecture
title: "Deep Compositor & Session Module Architecture"
description: Two-tier deep module design pairing a deterministic Sans-I/O Compositor core with a declarative Session runner.
tags: [architecture, codebase-design, compositor, seam, sans-io]
generated: { by: agent/cli, at: "2026-10-22T12:27:55Z" }
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
  - Zero-allocation flat 2D `Cell` grid (`char` inline scalar + `Buffer::clear` + double-buffering).
  - Minimal ANSI SGR/CUP diff emission with virtual run-length cursor tracking (`Buffer::diff(&prev, cursor)`), eliminating 60–80% of cursor position escape codes.
  - Automatic CJK 2-cell wide-character split boundary healing on every cell write.

### 2. Core Module: `src/compositor/` (Sans-I/O Deep Seam)

- **Interface**:
  - `Compositor::new(config: Config, host_size: TermSize) -> Self`
  - `Compositor::step(&mut self, event: Event, now: Instant) -> Transition`
  - `Compositor::render(&mut self, now: Instant) -> Option<Vec<u8>>`
  - `Compositor::is_dirty(&self) -> bool`
- **Internal Submodules**:
  - `store.rs` (`TelemetryStore`): Manages ephemeral IPC slots, metric ring buffers, stacked toast notifications, periodic command outputs, and foreground command timers.
  - `vt.rs` (`VtScreenAdapter`): Wraps `vt100::Parser` (Primary/Alternate screen), copies virtual terminal cells into `Buffer`, and synthesizes replies to terminal queries (`DA1`, `DA2`, `DSR 5/6`, `XTVERSION`, Kitty keyboard, `OSC 10/11`).
  - `layout.rs` (`PlacementSolver`): Computes child PTY viewport size and resolves floating overlay bounding-box positions with cursor and non-whitespace text collision dodging (`Ignore`, `AutoHide`, `DodgeToOppositeCorner`).
- **Decoupled Ingestion & Pacing**:
  - Byte ingestion (`Event::PtyOutput`) updates virtual screen state and sets a dirty flag immediately without triggering frame rendering.
  - Frame composition (`render`) is paced by the session loop at ~144 FPS (6.9ms interval), while interactive host keystrokes (`Event::HostInput`) trigger an immediate 0ms flush.

### 3. Outer Module: `src/session/` (Caller Seam + Internal OS Seam)

- **External Interface**:
  - `Session::run(config: Config) -> Result<i32, SessionError>`
  - `Session::run_headless(config: Config, cols: u16, rows: u16) -> Result<HeadlessHarness, SessionError>`
  - `Session::send_ipc_message(socket_path: Option<PathBuf>, msg: &IpcMessage) -> Result<(), SessionError>`
- **Internal Submodules**:
  - `tty.rs` (`OsHostTerminal`): Direct `STDIN_FILENO` (`0`) raw mode configuration and poll descriptor, atomic `SIGWINCH` and `SIGTERM`/`SIGHUP`/`SIGINT` graceful shutdown signal interception, terminal device name resolution (`get_tty_name`), and alternate screen buffer restoration.
  - `pty.rs` (`OsChildPty`): Manages `forkpty`, `TIOCSWINSZ`, non-blocking PTY I/O, child lifecycle cleanup (`SIGHUP` on drop), and exports child session variables (`THUD_SOCKET`, `THUD_ACTIVE`, `THUD_TTY`).
  - `ipc.rs` (`OsIpcServer`): Non-blocking Unix domain socket listener and client message sender with automatic socket file removal on drop.
  - `probes.rs` (`SystemProbes`): Stateful OS telemetry collector (`sample(master_fd, child_pid, now) -> BuiltinValues`) with CWD-memoized Git branch resolution and asynchronous background thread for battery sampling.
  - `harness.rs` (`HeadlessHarness`): Deterministic in-memory adapter for multi-step integration tests.
- **Fail-Safe Session Lifecycle**:
  - Verifies socket liveness and TTY identity before asserting a nested session, automatically purging stale inherited environment variables from dead parent sessions.
  - Supports `--force` (`-f`) override to bypass nested checks explicitly.
  - Idempotent `thud quit` exits cleanly with code 0 and purges stale session variables if no server is running.

### 4. Configuration Module: `src/config/`

- **Internal Submodules**:
  - `mod.rs`: Declarative TOML schema (`Config`, `StatusBarConfig`, `OverlayConfig`, `WidgetSpec`) and file loader/writer.
  - `ipc.rs`: Wire protocol types (`IpcMessage`, `ToastLevel`).
  - `default_config.toml`: Annotated default configuration template embedded via `include_str!`.

# Related Concepts
- [Core Terminal HUD & Compositor Requirements](../requirements/core-hud-capabilities.md): satisfies core terminal HUD, full-screen overlay, and widget requirements
