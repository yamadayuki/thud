---
type: Requirement
title: "Core Terminal HUD & Compositor Requirements"
description: "Terminal-emulator-agnostic and shell-agnostic HUD providing bottom status lines, floating top-right overlays, and multi-source widgets."
tags: [terminal, pty, hud, overlay, ipc, widgets]
generated: { by: antigravity/gemini-4, at: "2026-10-22T05:17:03Z" }
status: stable
---

## Overview

`cth` is a terminal-emulator-agnostic and shell-agnostic terminal customization tool written in Rust. It wraps any shell in a pseudo-terminal (PTY) and composites persistent HUD elements such as a Vim-like bottom status line and a top-right floating clock overlay, remaining functional even when the terminal window is in OS fullscreen mode or running full-screen Alternate Screen TUI applications (`vim`, `htop`, `fzf`, `less`).

## Key Requirements

1. **Terminal Emulator & Shell Agnosticism**:
   - Operates purely over standard ANSI/VT escape sequences on the outer host terminal and spawns any user shell inside a child PTY.
2. **Full Virtual Terminal Compositor**:
   - Maintains an in-memory virtual screen buffer (both Primary Screen and Alternate Screen `?1049h`/`?1049l`).
   - Adjusts child PTY `winsize` (`TIOCSWINSZ`) to reserve rows for docked bars (`rows - docked_rows`) so child applications never overwrite the status bar.
   - Composites floating overlays (such as a top-right clock) non-destructively during frame emission without mutating the child PTY's underlying cell buffer.
   - Resolves East Asian Wide Character (CJK 2-cell width) boundary collisions and preserves child cursor position and visibility (`DECTCEM`).
3. **Three Widget & State Input Channels**:
   - **Built-in Widgets + TOML Configuration**: Clock, battery, CWD, Git status, etc.
   - **Periodic Shell Command Widgets**: Asynchronous `sh -c ...` execution on configurable intervals without blocking rendering.
   - **External IPC via Unix Domain Socket**: Allows external processes and shell hooks to push status updates, variables, or temporary toast overlays dynamically.
