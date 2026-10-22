---
type: Requirement
title: "Core Terminal HUD & Compositor Requirements"
description: "Terminal-emulator-agnostic and shell-agnostic HUD providing bottom status lines, floating top-right overlays, and multi-source widgets."
tags: [terminal, pty, hud, overlay, ipc, widgets]
generated: { by: agent/cli, at: "2026-10-22T10:22:50Z" }
status: stable
---

## Overview

`thud` (Terminal Heads Up Display) is a terminal-emulator-agnostic and shell-agnostic terminal customization tool written in Rust. It wraps any shell in a pseudo-terminal (PTY) and composites persistent HUD elements such as a Vim-like bottom status line, multi-widget bordered panels, sparkline/gauge metrics, stacked toast notifications, and a top-right floating clock overlay, remaining functional even when the terminal window is in OS fullscreen mode or running full-screen Alternate Screen TUI applications (`vim`, `htop`, `fzf`, `less`).

## Key Requirements

1. **Terminal Emulator & Shell Agnosticism**:
   - Operates purely over standard ANSI/VT escape sequences on the outer host terminal and spawns any user shell inside a child PTY.
   - Responds to terminal capability/status queries (`DA1`, `DA2`, `DSR 5/6`, `XTVERSION`, Kitty keyboard, `OSC 10/11`) so modern shells like `fish` start without delay or warnings.
2. **Full Virtual Terminal Compositor & Rich 2D HUD Chrome**:
   - Maintains an in-memory virtual screen buffer (both Primary Screen and Alternate Screen `?1049h`/`?1049l`).
   - Adjusts child PTY `winsize` (`TIOCSWINSZ`) to reserve rows for docked bars (`rows - docked_rows`) and dynamically expands to full height during Zen Mode (`Ctrl+\`).
   - Supports semantic colors (named colors like `"cyan"`, `"transparent"`, hex RGB `"#89b4fa"`, 8-digit hex RGBA `"#24283b80"`, `@<opacity>` suffix like `"#24283b@0.7"`, or ANSI 256 numbers) with native terminal transparency (`Color::Default` / `"transparent"`, default for `bar_bg` in all theme presets so Ghostty/WezTerm/Kitty window opacity shines through) and in-buffer RGB alpha blending against underlying cells.
   - Supports both string theme presets (`theme = "tokyo_night"`) and customizable `[theme]` tables (`preset = "tokyo_night"` with per-field overrides for `bar_fg`, `bar_bg`, `overlay_fg`, `overlay_bg`, `border_fg`, `title_fg`, `track_fg`, and widget accents).
   - Supports per-widget `icon`, `pill` (Nerd Font `...` capsules), `padding`, `track_fg`, and multi-widget bordered `[[overlays]]` (`widgets = [...]`, `border_fg`, `title_fg`, `padding_x`).
   - Composites floating overlays non-destructively with cursor and non-whitespace text collision dodging (`DodgeToOppositeCorner` / `AutoHide`) and CJK 2-cell wide character boundary healing.
3. **Widget, Telemetry & IPC Channels**:
   - **Built-in & Telemetry Widgets**: Clock, CWD, Git branch, foreground process duration telemetry (`active_command` showing `⚙ <cmd> (<secs>s)` when running >= `min_secs`), and built-in OS metric streams (`key = "cpu"` and `key = "battery"` with automatic `"<pct>%"` label) consumable by `gauge` and `sparkline` widgets.
   - **Event-Driven & Periodic Shell Command Widgets (`type = "command"`)**:
     - Supports `trigger = "change"` (default when `interval_ms` is omitted: runs on CWD change and foreground command completion), `trigger = "cwd"` (runs only on CWD change), and `trigger = "interval"` (runs every `interval_ms`).
     - Supports `when_files = ["package.json", ".node-version", ".nvmrc"]` ancestor file detection so project-specific runtime versions (e.g. `node -v`, `rustc -V`) are only executed and displayed when inside a matching project directory.
     - Executes commands asynchronously with `current_dir` set to the child shell's active CWD.
   - **Metrics, Progress Bars & Stacked Toast Queue via Unix Domain Socket**:
     - `thud set` / `thud clear` for named status slots with optional TTL.
     - `thud progress <key> <0..100|done> [--label <txt>]` feeding `sparkline` (`▁▂▃▄▅▆▇█`) and `gauge` (`━━━─── 60%`) widgets.
     - `thud toast <msg> [--level info|success|warn|error]` pushing to a vertical toast stack at the bottom-right.
   - **Hotkeys**: `Ctrl+\` to toggle Zen Mode (hide/show all HUD chrome), and `Ctrl+]` to toggle `dashboard_only` overlays.
