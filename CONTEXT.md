# thud (Terminal Heads Up Display)

Terminal-emulator-agnostic and shell-agnostic PTY HUD compositor that renders docked status bars, non-destructive floating overlays, and live telemetry widgets over any interactive shell.

## Language

### 2D UI Rendering (`src/ui/`)

**Buffer**:
A 2D grid of styled terminal cells (`Cell`) holding inline scalar characters (`char`) with zero-allocation double-buffering, CJK 2-cell wide-character boundary healing on write, and minimal ANSI SGR/CUP diffs with virtual run-length cursor tracking against a previous `Buffer`.
_Avoid_: FrameGrid, Canvas, ScreenBuffer

**Span**:
A contiguous inline run of characters sharing a single `CellStyle`.
_Avoid_: StyledString, TextFragment

**Line**:
A horizontal sequence of `Span`s with a computed Unicode column display width.
_Avoid_: RowText, SpanList

**Block**:
A rectangular container widget that renders an optional box-drawing border (`Single`, `Rounded`, `Double`), background fill, and styled title around an inner `Rect`.
_Avoid_: BoxBorder, Frame, Panel

**Widget**:
Any visual HUD element that renders itself into a `Rect` region of a `Buffer`.
_Avoid_: Component, View, Control

### Compositor (`src/compositor/`)

**Compositor**:
The pure Sans-I/O state machine that merges virtual terminal screen state, `TelemetryStore` values, and `PlacementSolver` geometry into a `Buffer`, separating event ingestion (`step`) from frame composition (`render`).
_Avoid_: Renderer, Engine, ScreenManager

**TelemetryStore**:
The in-memory state store inside `Compositor` that holds TTL-expiring IPC slots, metric ring buffers, toast notification queues, IPC-injected dynamic overlays, builtin values, and the foreground command timer.
_Avoid_: StateCache, WidgetStore

**HudPainter**:
The internal module inside `Compositor` that paints the HUD (status bar, floating overlays, toast stack) over the projected child screen, evaluating each `Widget` from HUD state and resolving theme colors.
_Avoid_: Renderer, HudLayer, Scene

**InputRouter**:
The internal module inside `Compositor` that decides what each piece of host input means: bytes for the child, scrollback movement, or an opaque bracketed paste, holding incomplete mouse reports until they complete.
_Avoid_: KeyHandler, InputParser, HostInputDecoder

**VtScreenAdapter**:
The internal adapter that wraps the dual-screen (`Primary` / `Alternate`) VT100 parser, copies virtual screen cells into a `Buffer`, and synthesizes responses to terminal capability queries (`DA1`, `DA2`, `DSR`, `XTVERSION`, `OSC`).
_Avoid_: TerminalParser, VtWrapper

**CommandScheduler**:
The internal module inside `Compositor` that decides when each widget or metric shell command is due (interval, CWD change, foreground command finish, `when_files` markers), suppresses overlapping runs, and interprets and keeps command output.
_Avoid_: CommandPool, JobQueue, Cron

**PlacementSolver**:
The pure geometric solver that maps an overlay's `Anchor`, offsets, and `CollisionPolicy` (`Ignore`, `AutoHide`, `DodgeToOppositeCorner`) to a collision-free `Rect` on the `Buffer`.
_Avoid_: LayoutEngine, DodgeCalculator

### Session & OS Adapters (`src/session/`)

**Session**:
The outer runtime module and public entry point (`Session::run`) that builds the OS adapters and runs a `SessionLoop` until the child shell exits or the session is told to quit.
_Avoid_: App, Runner, Daemon

**SessionLoop**:
The crate-internal module that owns every runtime policy of a live session (frame pacing, immediate flush on host input, signal intake, child exit, IPC and quit, periodic command dispatch, telemetry ticks, config reload) and drives `Compositor` one iteration at a time over a `SessionIo`.
_Avoid_: EventLoop, Driver, MainLoop

**SessionIo**:
The single crate-internal port through which `SessionLoop` reaches everything outside the process: host terminal, child PTY, IPC socket, signals, periodic commands, telemetry probes, clocks, and config source. Its adapters are `OsSessionIo` (production) and `MemorySessionIo` (tests).
_Avoid_: Ports, Platform, Backend

**InheritedSession**:
The snapshot of `THUD_*` variables a new `thud` process inherited from its parent shell, assessed against the current terminal to decide whether the launch is fresh, in another terminal, stale (the previous session died), or nested.
_Avoid_: SessionEnv, NestingGuard, ParentSession

**SystemProbes**:
The stateful OS telemetry adapter that samples foreground process CWD, Git branch/dirty status, foreground command name, and cached battery status into `BuiltinValues`.
_Avoid_: OsMetrics, BuiltinCollector

**MemorySessionIo**:
The deterministic in-memory `SessionIo` adapter with a virtual clock, used by tests to enqueue host input, PTY output, IPC messages, signals, and config changes, advance virtual time, and observe what `SessionLoop` wrote and spawned.
_Avoid_: HeadlessHarness, MockSession, TestRunner
