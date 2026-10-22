## 2026-10-22
* **Update**: Linked `decisions/adr-008-input-router-and-host-mode-mirroring.md` to `decisions/adr-004-session-loop-and-session-io-seam.md` (relies on the reload error toast and OS adapter exit restore defined by ADR-004).
* **Update**: Linked `decisions/adr-008-input-router-and-host-mode-mirroring.md` to `architecture/compositor-module.md` (adds InputRouter to the Compositor module and mirrors child terminal modes to the host).
* **Creation**: Documented concept `decisions/adr-008-input-router-and-host-mode-mirroring.md` (ADR-008: InputRouter and Host Terminal Mode Mirroring).
* **Update**: Linked `decisions/adr-007-hud-painter-and-closed-telemetry-store.md` to `decisions/adr-005-command-scheduler-and-typed-command-keys.md` (provides the single effective-widget walk consumed by CommandScheduler).
* **Update**: Linked `decisions/adr-007-hud-painter-and-closed-telemetry-store.md` to `architecture/compositor-module.md` (adds HudPainter and narrows TelemetryStore inside the Compositor module).
* **Creation**: Documented concept `decisions/adr-007-hud-painter-and-closed-telemetry-store.md` (ADR-007: HudPainter, Closed TelemetryStore, and Themed Toast Placement).
* **Update**: Linked `decisions/adr-006-inherited-session-and-thud-env.md` to `decisions/adr-004-session-loop-and-session-io-seam.md` (passes the --config path to the SessionIo config source as a value instead of THUD_CONFIG).
* **Update**: Linked `decisions/adr-006-inherited-session-and-thud-env.md` to `architecture/compositor-module.md` (redefines the Session module's fail-safe nested-session lifecycle around InheritedSession).
* **Creation**: Documented concept `decisions/adr-006-inherited-session-and-thud-env.md` (ADR-006: InheritedSession Assessment and Explicit THUD_* Environment Handling).
* **Update**: Updated concept `decisions/adr-005-command-scheduler-and-typed-command-keys.md`.
* **Update**: Updated concept `decisions/adr-004-session-loop-and-session-io-seam.md`.
* **Update**: Linked `decisions/adr-005-command-scheduler-and-typed-command-keys.md` to `decisions/adr-004-session-loop-and-session-io-seam.md` (routes command dispatch, completion, and marker-file checks through the SessionIo seam).
* **Update**: Linked `decisions/adr-005-command-scheduler-and-typed-command-keys.md` to `architecture/compositor-module.md` (adds CommandScheduler as an internal submodule of Compositor and restores its Sans-I/O invariant).
* **Creation**: Documented concept `decisions/adr-005-command-scheduler-and-typed-command-keys.md` (ADR-005: CommandScheduler, Typed Command Keys, and Filesystem Checks Behind SessionIo).
* **Update**: Linked `decisions/adr-004-session-loop-and-session-io-seam.md` to `decisions/adr-001-design-it-twice-compositor-seam.md` (implements the internal OS port seam decided in ADR-001 without leaking generic port traits).
* **Update**: Linked `decisions/adr-004-session-loop-and-session-io-seam.md` to `architecture/compositor-module.md` (realizes the internal OS seam of the Session module as SessionLoop over SessionIo).
* **Creation**: Documented concept `decisions/adr-004-session-loop-and-session-io-seam.md` (ADR-004: SessionLoop and SessionIo Internal Seam).
* **Update**: Updated concept `decisions/adr-003-performance-deepening-and-frame-pacing.md`.
* **Update**: Linked `decisions/adr-003-performance-deepening-and-frame-pacing.md` to `architecture/compositor-module.md` (deepens compositor and session seams for wire-speed PTY throughput and 144 FPS pacing).
* **Creation**: Documented concept `decisions/adr-003-performance-deepening-and-frame-pacing.md` (Performance Deepening, Frame Pacing, and Zero-Allocation Cell Grid).
* **Update**: Updated concept `decisions/adr-002-modular-decomposition-and-ui-buffer-seam.md`.
* **Creation**: Documented concept `decisions/adr-002-modular-decomposition-and-ui-buffer-seam.md` (ADR-002: Internal Module Decomposition and Ratatui-Style 2D UI Buffer Seam).
* **Update**: Updated concept `decisions/adr-001-design-it-twice-compositor-seam.md`.
* **Update**: Updated concept `requirements/core-hud-capabilities.md`.
* **Update**: Updated concept `architecture/compositor-module.md`.
* **Update**: Linked `architecture/compositor-module.md` to `requirements/core-hud-capabilities.md` (satisfies core terminal HUD, full-screen overlay, and widget requirements).
* **Update**: Linked `decisions/adr-001-design-it-twice-compositor-seam.md` to `architecture/compositor-module.md` (selects and justifies the two-tier Compositor and Session seam design).
* **Creation**: Documented concept `decisions/adr-001-design-it-twice-compositor-seam.md` (ADR-001: Design It Twice Evaluation for Compositor and Session Seams).
* **Creation**: Documented concept `architecture/compositor-module.md` (Deep Compositor & Session Module Architecture).
* **Creation**: Documented concept `requirements/core-hud-capabilities.md` (Core Terminal HUD & Compositor Requirements).
* **Creation**: Initialized OKF v0.2 knowledge bundle.
