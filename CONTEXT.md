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
The in-memory state store inside `Compositor` that holds TTL-expiring IPC slots, metric ring buffers, toast notification queues, periodic command outputs, and foreground command timers.
_Avoid_: StateCache, WidgetStore

**VtScreenAdapter**:
The internal adapter that wraps the dual-screen (`Primary` / `Alternate`) VT100 parser, copies virtual screen cells into a `Buffer`, and synthesizes responses to terminal capability queries (`DA1`, `DA2`, `DSR`, `XTVERSION`, `OSC`).
_Avoid_: TerminalParser, VtWrapper

**PlacementSolver**:
The pure geometric solver that maps an overlay's `Anchor`, offsets, and `CollisionPolicy` (`Ignore`, `AutoHide`, `DodgeToOppositeCorner`) to a collision-free `Rect` on the `Buffer`.
_Avoid_: LayoutEngine, DodgeCalculator

### Session & OS Adapters (`src/session/`)

**Session**:
The outer runtime module that drives `Compositor` across either OS file descriptors (`Session::run`) or an in-memory test harness (`Session::run_headless`).
_Avoid_: App, Runner, Daemon

**SystemProbes**:
The stateful OS telemetry adapter that samples foreground process CWD, Git branch/dirty status, foreground command name, and cached battery status into `BuiltinValues`.
_Avoid_: OsMetrics, BuiltinCollector

**HeadlessHarness**:
The deterministic in-memory adapter at the `Session` seam used by integration tests to drive PTY output, IPC messages, and ticks without spawning an OS terminal.
_Avoid_: MockSession, TestRunner
