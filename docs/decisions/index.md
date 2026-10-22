# Decisions
* [ADR-001: Design It Twice Evaluation for Compositor and Session Seams](adr-001-design-it-twice-compositor-seam.md) - Comparison of four candidate module interfaces and selection of a Sans-I/O Compositor core paired with a Session runner.
* [ADR-002: Internal Module Decomposition and Ratatui-Style 2D UI Buffer Seam](adr-002-modular-decomposition-and-ui-buffer-seam.md) - Decomposes monolithic compositor/session/config modules into focused submodules and introduces a Ratatui-inspired 2D Buffer/Span/Line/Widget rendering foundation in src/ui/.
