---
type: Requirement
title: "Core Terminal HUD & Compositor Requirements"
description: "Terminal-emulator-agnostic and shell-agnostic HUD providing bottom status lines, floating top-right overlays, and multi-source widgets."
tags: [terminal, pty, hud, overlay, ipc, widgets]
generated: { by: antigravity/gemini-4, at: "2026-10-22T09:08:01Z" }
status: stable
---

## Overview

`thud` (Terminal Heads Up Display) is a terminal-emulator-agnostic and shell-agnostic terminal customization tool written in Rust. It wraps any shell in a pseudo-terminal (PTY) and composites persistent HUD elements such as a Vim-like bottom status line, bordered panels, sparkline/gauge metrics, stacked toast notifications, and a top-right floating clock overlay, remaining functional even when the terminal window is in OS fullscreen mode or running full-screen Alternate Screen TUI applications (`vim`, `htop`, `fzf`, `less`).

## Key Requirements

1. **Terminal Emulator & Shell Agnosticism**:
   - Operates purely over standard ANSI/VT escape sequences on the outer host terminal and spawns any user shell inside a child PTY.
   - Responds to terminal capability/status queries (`DA1`, `DA2`, `DSR 5/6`, `XTVERSION`, Kitty keyboard, `OSC 10/11`) so modern shells like `fish` start without delay or warnings.
2. **Full Virtual Terminal Compositor & Rich HUD Chrome**:
   - Maintains an in-memory virtual screen buffer (both Primary Screen and Alternate Screen `?1049h`/`?1049l`).
   - Adjusts child PTY `winsize` (`TIOCSWINSZ`) to reserve rows for docked bars (`rows - docked_rows`) and dynamically expands to full height during Zen Mode (`Ctrl+\`).
   - Supports per-segment styling (`fg_ansi`, `bg_ansi`, `bold`, `prefix`, `suffix`) and box-bordered multi-line overlays (`border = "none" | "single" | "rounded" | "double"` with optional `title`).
   - Composites floating overlays non-destructively with cursor and non-whitespace text collision dodging (`DodgeToOppositeCorner` / `AutoHide`) and CJK 2-cell wide character boundary healing.
3. **Widget, Telemetry & IPC Channels**:
   - **Built-in & Telemetry Widgets**: Clock, battery, CWD, Git branch, and foreground process duration telemetry (`active_command` showing `[running: <cmd> <secs>s]` when running >= `min_duration_ms`).
   - **Periodic Shell Command Widgets**: Asynchronous `sh -c ...` execution on configurable intervals.
   - **Metrics, Progress Bars & Stacked Toast Queue via Unix Domain Socket**:
     - `thud set` / `thud clear` for named status slots with optional TTL.
     - `thud progress <key> <0..100|done> [--label <txt>]` feeding `sparkline` (`▁▂▃▄▅▆▇█`) and `gauge` (`████▌░░░ 56%`) widgets.
     - `thud toast <msg> [--level info|success|warn|error]` pushing to a vertical toast stack at the top-right.
   - **Hotkeys**: `Ctrl+\` to toggle Zen Mode (hide/show all HUD chrome), and `Ctrl+]` to toggle `toggle_only` dashboard overlays.
