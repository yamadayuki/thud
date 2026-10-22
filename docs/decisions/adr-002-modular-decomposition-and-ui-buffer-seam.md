---
type: decision
title: "ADR-002: Internal Module Decomposition and Ratatui-Style 2D UI Buffer Seam"
description: Decomposes monolithic compositor/session/config modules into focused submodules and introduces a Ratatui-inspired 2D Buffer/Span/Line/Widget rendering foundation in src/ui/.
tags: [adr, architecture, ui, buffer, widgets, seam, locality]
generated: { by: antigravity/gemini-4, at: "2026-10-22T09:26:07Z" }
status: stable
---

## Context

As `thud` grew to support capsule status bars, bordered floating panels, sparklines, gauges, stacked toast notifications, terminal capability query responses, and OS process telemetry, the three original source files (`src/compositor.rs` at 1,717 lines, `src/session.rs` at 954 lines, and `src/config.rs` at 818 lines) accumulated multiple internal responsibilities:

1. Widget evaluation in `src/compositor.rs` flattened every HUD element into a single-style `String`, making multi-style widgets (such as a `Gauge` with distinct label, filled bar, dimmed track, and percentage styles, or a `Block` with distinct border and title styles) impossible without manual string slicing.
2. Wide-character CJK boundary healing, 2D cell storage, and minimal ANSI SGR/CUP diff generation were split across `FrameGrid` and `Compositor::compose_and_diff`.
3. `src/session.rs` mixed raw `libc` termios/PTY management, Unix domain socket framing, OS process/battery probes, and the deterministic `HeadlessHarness`.
4. `src/config.rs` embedded a 120-line raw TOML template string alongside UI style primitives and IPC wire types.

## Decision

We preserve the existing public crate seams (`Compositor`, `Session`, `Config`) while introducing a dedicated **`src/ui/` 2D cell rendering foundation** and decomposing each top-level module into focused submodules:

1. **`src/ui/` (Ratatui-Inspired 2D Rendering Foundation)**:
   - `style.rs`: `Color`, `CellStyle`, `BorderStyle`, `Anchor`, `TermSize` (re-exported at root and `config` for compatibility).
   - `rect.rs`: `Rect` geometry (`inner`, `intersection`, `contains`).
   - `text.rs`: `Span` and `Line` supporting multi-style inline text and Unicode display width calculation.
   - `buffer.rs`: `Cell` and `Buffer` encapsulating 2D cell storage, automatic CJK 2-cell wide-character boundary healing, and `Buffer::diff(&prev, cursor) -> Vec<u8>` minimal ANSI output generation.
   - `widgets.rs`: `Widget` trait (`render(self, area: Rect, buf: &mut Buffer)`) and concrete components (`Block`, `Gauge`, `Sparkline`, `StatusBar`, `Toast`).
2. **`src/compositor/` Decomposition**:
   - `mod.rs`: Public `Compositor::new` / `Compositor::step`, `Event`, `Transition`, `BuiltinValues`.
   - `store.rs`: `TelemetryStore` managing TTL slots, metric ring buffers, toast stack, command outputs, and foreground command timer.
   - `vt.rs`: `VtScreenAdapter` wrapping `vt100::Parser`, copying screen cells into `Buffer`, and synthesizing responses to terminal queries (`DA1`, `DA2`, `DSR`, `XTVERSION`, Kitty keyboard, `OSC 10/11`).
   - `layout.rs`: `PlacementSolver` resolving anchor coordinates and cursor/text collision dodging (`DodgeToOppositeCorner`, `AutoHide`).
3. **`src/session/` Decomposition**:
   - `mod.rs`: Public `Session` event loop and `SessionError`.
   - `tty.rs`: `OsHostTerminal` and `SIGWINCH` handling.
   - `pty.rs`: `OsChildPty` (`forkpty`, `TIOCSWINSZ`, non-blocking I/O, `waitpid`).
   - `ipc.rs`: `OsIpcServer` and client socket sender.
   - `probes.rs`: Stateful `SystemProbes` encapsulating battery cache and OS process/CWD/Git probes behind `sample(master_fd, child_pid, now) -> BuiltinValues`.
   - `harness.rs`: `HeadlessHarness` test adapter.
4. **`src/config/` Decomposition**:
   - `mod.rs`: Declarative HUD configuration structs and loader/writer methods.
   - `ipc.rs`: `IpcMessage` and `ToastLevel`.
   - `default_config.toml`: Embedded via `include_str!("default_config.toml")`.

# Related Concepts
- [Deep Compositor & Session Module Architecture](../architecture/compositor-module.md): refines the internal module decomposition and introduces the `src/ui/` rendering seam
