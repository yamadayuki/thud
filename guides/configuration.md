# Configuration Reference (`config.toml`)

This document describes every configuration option supported by `thud`.

---

## 1. File Location & Live Reload

### Resolution Order
`thud` resolves its configuration file in the following order:
1. `--config <path>` (or `-c <path>`) CLI option
2. `$THUD_CONFIG` environment variable
3. `~/.config/thud/config.toml`
4. Built-in default configuration (`Config::default()`)

To generate an annotated default configuration file at `~/.config/thud/config.toml`:
```sh
thud init-config
# Use --force to overwrite an existing file:
thud init-config --force
```

### Live Configuration Reload
Changes to `config.toml` are applied to the running `thud` session **without restarting your shell**:
- **Automatic Reload on Save**: `thud` checks the file modification timestamp (`mtime`) every 250ms and automatically reloads when the file is saved.
- **Hotkey Reload**: Press `Ctrl+]` (configurable via `reload_key`) to trigger a manual reload at any time.
- **CLI Reload**: Run `thud reload` inside or outside the session.
- **Error Handling**: Reloads are silent when valid. If the TOML file contains a syntax or type error, `thud` keeps your previous working configuration active and displays an error toast in the corner with the parse failure details.

---

## 2. Top-Level Options

Top-level keys must appear at the very top of `config.toml` (before any `[table]` or `[[array_of_tables]]` headers).

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `theme` | `String` or `Table` | `"tokyo_night"` | Built-in theme preset name or inline/table palette overrides (see [Themes & Palette Overrides](#4-themes--palette-overrides)). |
| `shell` | `String` (optional) | `$SHELL` (`"/bin/sh"`) | Default interactive shell executable to spawn when `--shell` is not passed on the CLI. |
| `ipc_socket` | `String` (optional) | `/tmp/thud-<pid>.sock` | Explicit Unix domain socket path for IPC commands. |
| `zen_toggle_key` | `String` (optional) | `"ctrl-\\"` | Hotkey to toggle **Zen Mode** (hides all HUD elements and expands the child PTY to full terminal height). Set to `""` to disable. |
| `reload_key` | `String` (optional) | `"ctrl-]"` | Hotkey to manually reload `config.toml`. Set to `""` to disable. |
| `scrollback_lines` | `Integer` | `10000` | Maximum number of scrollback lines retained in memory for mousewheel history scrolling. |

### Supported Hotkey Syntax
Hotkeys intercept a single raw ASCII control character before it reaches the child PTY:
- `"ctrl-a"` through `"ctrl-z"` (`0x01`..`0x1a`)
- `"ctrl-\\"` or `"^\""` (`0x1c`)
- `"ctrl-]"` or `"^]"` (`0x1d`)
- `"ctrl-^"` (`0x1e`)
- `"ctrl-_"` (`0x1f`)
- `""` (empty string disables the hotkey)

---

## 3. Colors & Opacity (Alpha Blending)

Every color field (`fg`, `bg`, `border_fg`, `title_fg`, `track_fg`, and `[theme]` overrides) accepts any of the following formats:

| Format | Examples | Description |
| :--- | :--- | :--- |
| **Native Terminal Transparency** | `"transparent"`, `"default"`, `"none"`, `"reset"` | Emits default terminal SGR (`\x1b[49m` / `\x1b[39m`) so your terminal emulator's native window transparency and blur (e.g., Ghostty `background-opacity`, WezTerm, Kitty, Alacritty) shines directly through. |
| **24-bit Hex RGB** | `"#7aa2f7"`, `"#1a1b26"` | TrueColor 24-bit RGB hex string (`#rrggbb`). |
| **32-bit Hex RGBA** | `"#24283b80"`, `"#7aa2f7cc"` | TrueColor RGB with an 8-bit hex alpha channel (`00` = fully transparent, `ff` = opaque). Composited in-buffer against underlying cells. |
| **Opacity Suffix (`@`)** | `"#24283b@0.75"`, `"#24283b@75%"`, `"blue@50%"` | Appends `@<0.0..=1.0>` or `@<0..=100>%` to any hex, named, or ANSI color to alpha-blend it over the underlying terminal or status bar background. |
| **Semantic Color Names** | `"cyan"`, `"blue"`, `"green"`, `"magenta"`, `"purple"`, `"mauve"`, `"yellow"`, `"red"`, `"orange"`, `"peach"`, `"pink"`, `"teal"`, `"sky"`, `"lavender"`, `"emerald"`, `"white"`, `"gray"`, `"dark_gray"`, `"black"` | Built-in curated palette names. |
| **256-Color ANSI Index** | `231`, `236`, `39` | Integer `0..=255` (or numeric string `"0"`..`"255"`). |

---

## 4. Themes & Palette Overrides

### Using a Built-in Preset
Set `theme` at the top of `config.toml` to one of the 6 built-in presets:
- `"tokyo_night"` *(default)*
- `"catppuccin"`
- `"cyberpunk"`
- `"nord"`
- `"gruvbox"`
- `"default"` *(minimal grayscale ANSI)*

```toml
theme = "tokyo_night"
```

All built-in presets default `bar_bg` to `"transparent"` so unused space on the bottom status line preserves your terminal's native background and opacity.

### Customizing Theme Colors (`[theme]`)
You can override individual palette slots on top of any preset using either an inline table (`theme = { preset = "tokyo_night", ... }`) or a `[theme]` section:

```toml
[theme]
preset = "tokyo_night"
bar_bg = "transparent"        # or "#1a1b26@80%" for a semi-transparent bar
overlay_bg = "#24283b@85%"    # translucent floating overlays
cwd_bg = "#7aa2f7@70%"
git_bg = "#bb9af7@70%"
```

| `[theme]` Key | Description |
| :--- | :--- |
| `preset` | Base preset (`"tokyo_night"`, `"catppuccin"`, `"cyberpunk"`, `"nord"`, `"gruvbox"`, `"default"`). |
| `bar_fg` | Default foreground color for unstyled status bar text and separators. |
| `bar_bg` | Background fill color for the status bar (`"transparent"` by default). |
| `overlay_fg` | Default text color inside floating `[[overlays]]`. |
| `overlay_bg` | Default background fill color inside floating `[[overlays]]`. |
| `border_fg` | Default box-drawing border color for `[[overlays]]`. |
| `title_fg` | Default title text color for `[[overlays]]`. |
| `track_fg` | Default unfilled track color (`─`) for `gauge` widgets. |
| `cwd_fg` / `cwd_bg` | Automatic foreground/background capsule accent for `type = "cwd"` widgets. |
| `git_fg` / `git_bg` | Automatic foreground/background capsule accent for `type = "git_branch"` widgets. |
| `cmd_fg` / `cmd_bg` | Automatic foreground/background capsule accent for `type = "active_command"` and `type = "ipc_slot"` widgets. |
| `metric_fg` / `metric_bg` | Automatic foreground/background capsule accent for `type = "sparkline"` and `type = "gauge"` widgets. |

---

## 5. Docked Status Bar (`[status_bar]`)

The status bar docks to the bottom or top of the terminal and resizes the child PTY height (`rows - status_bar.height`) so shells and full-screen TUI apps (`vim`, `nvim`, `less`, `fzf`) never get covered by the bar.

```toml
[status_bar]
enabled = true
position = "bottom"    # "bottom" | "top"
height = 1
separator = ""
fg = "#c0caf5"         # optional override for theme.bar_fg
bg = "transparent"     # optional override for theme.bar_bg
left = [ ... ]
center = [ ... ]
right = [ ... ]
```

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `enabled` | `bool` | `true` | Enable or disable the docked status bar. |
| `position` | `String` | `"bottom"` | Vertical dock position: `"bottom"` or `"top"`. When set to `"top"`, the child terminal viewport shifts down by `height` rows and floating overlays/toasts automatically position inside the child viewport below the top bar. |
| `height` | `u16` | `1` | Number of terminal rows reserved for the status bar (`1` recommended). |
| `separator` | `String` | `""` | String inserted between adjacent visible widgets in the same alignment group (e.g., `""`, `" "`, or `" \| "`). |
| `fg` | `Color` (optional) | `theme.bar_fg` | Default foreground color for the status bar. |
| `bg` | `Color` (optional) | `theme.bar_bg` | Background fill color for empty cells on the status bar. |
| `left` | `Array<Widget>` | `[cwd, git_branch, active_command, ipc_slot("status")]` | Left-aligned widgets. |
| `center` | `Array<Widget>` | `[]` | Center-aligned widgets (automatically hidden if `left` + `right` leave insufficient room). |
| `right` | `Array<Widget>` | `[sparkline("cpu"), gauge("battery")]` | Right-aligned widgets. |

---

## 6. Widgets & Per-Segment Styling

Widgets can be placed in `[status_bar].left`, `[status_bar].center`, `[status_bar].right`, or inside `[[overlays]].widgets`.
Every widget combines a **widget type** (`type = "..."`) with optional **per-segment styling** keys.

### Per-Segment Styling Keys (Available on Every Widget)

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `fg` | `Color` (optional) | Theme accent | Foreground color override for this widget. |
| `bg` | `Color` (optional) | Theme accent | Background color override for this widget. |
| `bold` | `bool` (optional) | `false` | Render text in bold weight. |
| `icon` | `String` (optional) | `None` | Icon or prefix glyph prepended inside the capsule (e.g., `icon = "📁"` or Nerd Font glyph). |
| `padding` | `bool` (optional) | `false` (`true` when `pill = true`) | Adds a leading and trailing space (`" ... "`) inside the segment background so adjacent capsules look clean even with `separator = ""`. |
| `pill` | `bool` (optional) | `false` | Wraps the segment with Nerd Font rounded half-circle caps (`\u{e0b6}` left cap and `\u{e0b4}` right cap) colored with the segment's `bg`. Requires a Nerd Font in your terminal. |
| `track_fg` | `Color` (optional) | `theme.track_fg` | Custom color for the unfilled portion (`─`) of a `gauge` widget. |
| `prefix` | `String` (optional) | `None` | Raw text prepended before the segment. |
| `suffix` | `String` (optional) | `None` | Raw text appended after the segment. |

---

### Widget Types (`type = "..."`)

#### 1. `cwd` — Current Working Directory
Automatically tracks the working directory of the child shell via OS process inspection (`proc_pidinfo` on macOS, `/proc/<pid>/cwd` on Linux) with `$HOME` abbreviated as `~`. No shell hooks required.
```toml
{ type = "cwd", padding = true, bold = true }
```

#### 2. `git_branch` — Git Branch & Worktree Status
Inspects the `.git` directory of the active working directory (walking up parent directories). Hidden automatically when outside a Git repository.
```toml
{ type = "git_branch", icon = "", padding = true, bold = true }
```

#### 3. `active_command` — Slow Foreground Command Timer
Monitors the foreground process group running inside the PTY. Once a foreground command has been running for at least `min_secs` seconds, renders `⚙ <command> (<elapsed>s)`. Automatically disappears as soon as the command finishes.
```toml
{ type = "active_command", min_secs = 2, padding = true, bold = true }
```
| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `min_secs` | `u64` | `0` | Minimum elapsed seconds before the active command indicator appears (`2` in default config). |

#### 4. `clock` — Local Wall-Clock Time
Renders the current local time formatted with `chrono` `strftime` syntax.
```toml
{ type = "clock", format = " %H:%M:%S " }
```
| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `format` | `String` | `"%H:%M:%S"` | `strftime` format string (e.g., `"%Y-%m-%d %H:%M:%S"`, `"%H:%M"`). |

#### 5. `text` — Static Text Label
Renders a static string.
```toml
{ type = "text", text = "DEV", padding = true, bold = true, fg = "#1a1b26", bg = "cyan" }
```

#### 6. `ipc_slot` — Dynamic IPC Text Slot
Displays a named string slot populated via `thud set <key> <value> [--ttl-ms <ms>]`. Automatically hidden when the slot is unset or expired.
```toml
{ type = "ipc_slot", key = "status", padding = true, bold = true }
```
| Key | Type | Required | Description |
| :--- | :--- | :--- | :--- |
| `key` | `String` | Yes | Slot identifier matched by `thud set <key> <value>` and `thud clear <key>`. |

#### 7. `sparkline` — Historical Metric Sparkline (`▁▂▃▄▅▆▇█`)
Renders a rolling Unicode block sparkline for a numeric metric (`0..=100`).
```toml
{ type = "sparkline", key = "cpu", width = 8, label = "CPU", padding = true }
```
| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `key` | `String` | *(required)* | Metric identifier. Built-in keys: `"cpu"` (system CPU load `0..100`) and `"battery"` (battery percentage `0..100`). Any custom key can be pushed via `thud progress <key> <0..100>`. |
| `width` | `usize` | `8` | Number of historical bar columns to display. |
| `label` | `String` (optional) | `None` | Text label rendered before the sparkline bars (e.g., `label = "CPU"` -> `CPU ▁▂▃▄▅▆▇█`). |
| `cmd` | `String` (optional) | `None` | Optional shell command whose stdout (`0..100` or `<num>%`) is sampled periodically into this metric. |
| `interval_ms` | `u64` (optional) | `2000` | Polling interval in milliseconds when `cmd` is specified. |

#### 8. `gauge` — Horizontal Progress & Metric Bar (`━━━━──  75%`)
Renders a horizontal bar with a 3-character left-padded percentage (`  0%` .. `100%`) so changing digits never shift the layout width.
```toml
{ type = "gauge", key = "battery", width = 6, padding = true }
```
| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `key` | `String` | *(required)* | Metric identifier. Built-in keys: `"battery"` (OS battery level `0..100`) and `"cpu"` (OS CPU load `0..100`). Custom keys can be pushed via `thud progress <key> <0..100>`. |
| `width` | `usize` | `10` | Width of the bar track (`━` / `─`) in character cells. |
| `label` | `String` (optional) | `None` | Optional label rendered before the bar (can also be set dynamically via `thud progress <key> <pct> --label "..."`). |
| `cmd` | `String` (optional) | `None` | Optional shell command whose stdout (`0..100` or `<num>%`) is sampled periodically into this gauge. |
| `interval_ms` | `u64` (optional) | `2000` | Polling interval in milliseconds when `cmd` is specified. |

#### 9. `command` — Event-Driven or Periodic Shell Command
Executes `sh -c "<cmd>"` in the **child shell's current working directory** and renders the first line of stdout. Hidden when stdout is empty.
```toml
# Event-driven (default): runs only when entering/changing a Rust repo or after a command finishes
{ type = "command", id = "rust_ver", cmd = "rustc -V | cut -d' ' -f2", trigger = "change", when_files = ["Cargo.toml"], icon = "🦀", padding = true }

# Periodic polling: runs every 5000ms
{ type = "command", id = "k8s_ctx", cmd = "kubectl config current-context 2>/dev/null", trigger = "interval", interval_ms = 5000, padding = true }
```
| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `id` | `String` | *(required)* | Unique identifier for caching the command's output. |
| `cmd` | `String` | *(required)* | Shell command executed via `sh -c` with its working directory set to the child shell's active `cwd`. |
| `trigger` | `String` | `"change"` (or `"interval"` if `interval_ms` is set) | Execution policy:<br>- `"change"`: Runs on startup, whenever `cwd` changes, and whenever an interactive foreground command in the shell finishes.<br>- `"cwd"`: Runs only on startup and whenever `cwd` changes.<br>- `"interval"`: Runs periodically every `interval_ms` milliseconds. |
| `interval_ms` | `u64` (optional) | `5000` (when `trigger = "interval"`) | Polling interval in milliseconds when using periodic execution. |
| `when_files` | `Array<String>` | `[]` | Project file guard. When non-empty (e.g., `["Cargo.toml"]` or `[".nvmrc", "package.json"]`), the command is only executed if at least one listed file/directory exists in `cwd` or any of its parent directories. When leaving a matching directory, the cached output is cleared immediately. |

---

## 7. Floating Toast & Progress Notification Stack (`[toast]`)

Controls the positioning and appearance of transient toast notifications (`thud toast ...`), config reload error toasts, and unreferenced ad-hoc progress bars (`thud progress <key> <0..100>` when `<key>` is not placed on the status bar).

```toml
[toast]
anchor = "bottom_right"
margin_x = 2
margin_y = 1
gap = 0
pill = false
border = "none"
```

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `anchor` | `Anchor` | `"bottom_right"` | Corner/edge anchor (`"top_right"`, `"top_left"`, `"top_center"`, `"bottom_right"`, `"bottom_left"`, `"bottom_center"`). |
| `margin_x` | `u16` | `2` | Horizontal margin (in cells) from the left/right terminal edge so toasts float cleanly inward. |
| `margin_y` | `u16` | `1` | Vertical margin (in rows) from the top/bottom of the child viewport (leaves 1 row of breathing room above the bottom status bar by default). |
| `gap` | `u16` | `0` | Vertical spacing (in rows) between multiple stacked toasts. |
| `pill` | `bool` | `false` | Wrap each toast in Nerd Font rounded caps (`\u{e0b6}` / `\u{e0b4}`). |
| `border` | `BorderStyle` | `"none"` | Box border around each toast (`"none"`, `"single"`, `"rounded"`, `"double"`). |

---

## 8. Floating Overlays (`[[overlays]]`)

You can define any number of non-destructive floating overlays (`[[overlays]]`) that composite over the child terminal without corrupting scrollback or terminal state.

```toml
[[overlays]]
id = "clock"
anchor = "top_right"
offset_x = 0
offset_y = 0
z_index = 10
screen_mode = "always"
border = "none"
padding_x = 0
bold = true
collision = { mode = "dodge_to_opposite_corner", margin = 1, avoid_text = true }
widgets = [
    { type = "clock", format = " %H:%M:%S " },
]
```

| Key | Type | Default | Description |
| :--- | :--- | :--- | :--- |
| `id` | `String` | *(required)* | Unique overlay identifier. |
| `anchor` | `Anchor` | `"top_right"` | `"top_right"`, `"top_left"`, `"top_center"`, `"bottom_right"`, `"bottom_left"`, or `"bottom_center"`. |
| `offset_x` | `i16` | `0` | Horizontal offset in cells inward from the anchor corner. |
| `offset_y` | `i16` | `0` | Vertical offset in rows inward from the anchor corner. |
| `z_index` | `i32` | `0` | Rendering stacking order (higher `z_index` renders on top). |
| `screen_mode` | `ScreenCondition` | `"always"` | When to show the overlay:<br>- `"always"`: Show in both normal shell and alternate-screen TUI apps.<br>- `"primary_only"`: Automatically hide when full-screen TUI apps (`vim`, `less`, `htop`) enter alternate screen.<br>- `"alternate_only"`: Show only inside full-screen TUI apps. |
| `collision` | `CollisionPolicy` | `{ mode = "dodge_to_opposite_corner", margin = 1, avoid_text = true }` | How the overlay reacts when the cursor or terminal text reaches its bounding box:<br>- `{ mode = "dodge_to_opposite_corner", margin = 1, avoid_text = true }`: Moves to the horizontally opposite corner (`top_right` ↔ `top_left`), or hides if both corners are occupied.<br>- `{ mode = "auto_hide", margin = 1, avoid_text = true }`: Hides in-place until the cursor/text moves away.<br>- `{ mode = "ignore" }`: Always renders at its anchor. |
| `border` | `BorderStyle` | `"none"` | `"none"`, `"single"` (`┌─┐`), `"rounded"` (`╭─╮`), or `"double"` (`╔═╗`). |
| `title` | `String` (optional) | `None` | Optional title rendered inside the top border line (requires `border != "none"`). |
| `fg` | `Color` (optional) | `theme.overlay_fg` | Default text color inside the overlay. |
| `bg` | `Color` (optional) | `theme.overlay_bg` | Background fill color inside the overlay. |
| `border_fg` | `Color` (optional) | `theme.border_fg` | Box border color override. |
| `title_fg` | `Color` (optional) | `theme.title_fg` | Title text color override. |
| `padding_x` | `u16` | `0` | Extra horizontal inner padding on the left and right of the overlay content. |
| `bold` | `bool` | `true` | Default bold text attribute for widgets inside the overlay. |
| `ttl_ms` | `u64` (optional) | `None` | Optional time-to-live in milliseconds (primarily used by dynamic IPC overlays). |
| `widgets` | `Array<Widget>` | *(required)* | List of widgets rendered vertically (one per row) inside the overlay. |
