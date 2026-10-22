---
type: Decision
title: "ADR-001: Design It Twice Evaluation for Compositor and Session Seams"
description: Comparison of four candidate module interfaces and selection of a Sans-I/O Compositor core paired with a Session runner.
tags: [adr, design-it-twice, codebase-design, seam, depth]
generated: { by: antigravity/gemini-4, at: "2026-10-22T09:09:24Z" }
status: stable
---

## Context

We evaluated four radically different designs using the `codebase-design` "Design It Twice" methodology to determine where to place the seams for `thud`.

## Candidate Designs Evaluated

1. **Design 1: Minimal Interface / Sans-I/O Core (`Compositor::new` + `Compositor::step`)**
   - **Constraint**: Minimize the interface to 1-3 entry points; maximize leverage per entry point.
   - **Strengths**: Pure in-process state machine (`Event` in, `Transition` out). Zero mocks or traits needed to test VT emulation, CJK wide-character healing, alternate screen switching, and ANSI diffing.
   - **Weaknesses**: Leaves OS event loop wiring outside the seam; basic widget spec lacks cursor-collision policies.
2. **Design 2: Maximum Flexibility (`HudCompositor` + `StateSource` Seam)**
   - **Constraint**: Maximize layout flexibility and reactive/pull-push extensibility.
   - **Strengths**: Introduces declarative `RegionSpec` (`Placement::Docked` vs `Placement::Floating`), `CollisionPolicy` (`Ignore`, `AutoHide`, `DodgeToOppositeCorner` when child cursor approaches the overlay), and `ttl` on `StatePayload` (unifying persistent widgets and temporary toast notifications).
   - **Weaknesses**: Slightly larger method surface (`feed_child_output`, `resize_host`, `apply_state_updates`, `render_diff`) requiring callers to obey ordering constraints per tick.
3. **Design 3: Common Caller Optimization (`Session::run` + `Session::run_headless`)**
   - **Constraint**: Make the default production and test callers trivial.
   - **Strengths**: 3-line `main.rs` (`Config::load_or_default()` -> `Session::run(config)`) and fluent `HeadlessHarness` for integration tests; hides OS ports behind an **Internal Seam** (`RuntimeEnv`).
   - **Weaknesses**: If used alone without an inner Sans-I/O seam, the event loop and pure rendering logic would be coupled inside one struct.
4. **Design 4: Explicit Ports & Adapters at OS Edge (`Session<H: HostTerminal, C: ChildPty, E: EnvironmentPort>`)**
   - **Constraint**: Design around explicit ports and adapters for cross-seam dependencies.
   - **Strengths**: Allows deterministic injection of `FakeHostTerminal`, `FakeChildPty`, and `FakeEnvironment`.
   - **Weaknesses**: Leaks 3 generic port traits into the public caller interface even though production callers only ever use the OS adapters.

## Decision

We adopt a **Hybrid Architecture combining Design 1, Design 2, and Design 3**:

- **From Design 1**: The core module is a pure, infallible Sans-I/O state machine (`Compositor::new` and `Compositor::step(Event, Instant) -> Transition`).
- **From Design 2**: The configuration and state data types passed across `Compositor`'s seam incorporate `Placement` (`Docked` vs `Floating`), `CollisionPolicy` (`Ignore` / `AutoHide` / `DodgeToOppositeCorner`), and `ttl: Option<Duration>` for ephemeral IPC overlays.
- **From Design 3 (and Design 4's internal equivalent)**: The outer `Session` module provides `Session::run(config)` and `Session::run_headless(config, cols, rows)`, keeping the OS ports (`HostTerminal`, `ChildPty`, `UnixSocket`, `CommandPool`) as an **internal seam** rather than polluting the public signature with generic traits.

# Related Concepts
- [Deep Compositor & Session Module Architecture](../architecture/compositor-module.md): selects and justifies the two-tier Compositor and Session seam design
