use crate::config::{Config, OverlayConfig, WidgetSpec};
use std::fmt::Write as _;

impl Config {
    /// Generates a self-documenting TOML configuration string directly from `Config::default()`.
    pub fn annotated_default_toml() -> String {
        Self::default().to_annotated_toml()
    }

    /// Formats `self` into a self-documenting, human-friendly TOML configuration string
    /// with comments explaining every section, theme, color format, widget kind, and option.
    pub fn to_annotated_toml(&self) -> String {
        let mut out = String::with_capacity(4096);

        out.push_str(
            "# =============================================================================\n\
             # thud - Terminal Heads Up Display Configuration\n\
             # Default path: ~/.config/thud/config.toml (or override via $THUD_CONFIG / --config)\n\
             # =============================================================================\n\n\
             # Color Theme Preset or Custom Theme Table:\n\
             #   \"default\" | \"tokyo_night\" | \"catppuccin\" | \"cyberpunk\" | \"nord\" | \"gruvbox\" | \"github_dark\"\n\
             # Or customize any preset color inline or via `[theme]`:\n\
             #   theme = { preset = \"tokyo_night\", bar_bg = \"transparent\", overlay_bg = \"#24283b@0.8\", cwd_bg = \"#7aa2f7\" }\n\
             # Available theme override keys:\n\
             #   bar_fg, bar_bg, overlay_fg, overlay_bg, border_fg, title_fg, track_fg,\n\
             #   cwd_fg, cwd_bg, git_fg, git_bg, cmd_fg, cmd_bg, metric_fg, metric_bg,\n\
             #   toast_info_fg, toast_info_bg, toast_success_fg, toast_success_bg,\n\
             #   toast_warn_fg, toast_warn_bg, toast_error_fg, toast_error_bg, progress_fg, progress_bg\n",
        );
        let _ = writeln!(out, "theme = {}", format_inline_value(&self.theme));

        out.push_str(
            "\n# Optional: Shell executable to spawn inside the PTY.\n\
             # Defaults to $SHELL (or \"/bin/sh\" if $SHELL is unset).\n",
        );
        match &self.shell {
            Some(sh) => {
                let _ = writeln!(out, "shell = {}", toml_quote(sh));
            }
            None => {
                out.push_str("# shell = \"/opt/homebrew/bin/fish\"\n");
            }
        }

        out.push_str(
            "\n# Optional: Custom Unix domain socket path for IPC commands (`thud set`, `thud toast`, etc.).\n\
             # Defaults to \"/tmp/thud-<pid>.sock\" and is exported as $THUD_SOCKET inside the session.\n",
        );
        match &self.ipc_socket {
            Some(sock) => {
                let _ = writeln!(
                    out,
                    "ipc_socket = {}",
                    toml_quote(&sock.display().to_string())
                );
            }
            None => {
                out.push_str("# ipc_socket = \"/tmp/thud.sock\"\n");
            }
        }

        out.push_str(
            "\n# Hotkey to toggle Zen Mode (temporarily hides all HUD elements and expands the PTY):\n",
        );
        if let Some(key) = &self.zen_toggle_key {
            let _ = writeln!(out, "zen_toggle_key = {}", toml_quote(&key.to_string()));
        } else {
            out.push_str("zen_toggle_key = \"\"\n");
        }

        out.push_str(
            "\n# Hotkey to manually reload config.toml (note: saving config.toml also auto-reloads live):\n",
        );
        if let Some(key) = &self.reload_key {
            let _ = writeln!(out, "reload_key = {}", toml_quote(&key.to_string()));
        } else {
            out.push_str("reload_key = \"\"\n");
        }

        out.push_str(
            "\n# Mouse capture. While thud captures the mouse, your terminal cannot select text:\n\
             #   \"auto\"   - only while the app inside requests the mouse (vim, htop, ...);\n\
             #              select and copy freely at the shell prompt\n\
             #   \"always\" - always; the wheel scrolls thud's scrollback at the shell\n\
             #   \"off\"    - never\n",
        );
        let _ = writeln!(out, "mouse = {}", toml_quote(&self.mouse.to_string()));

        out.push_str(
            "\n# Hotkey to temporarily invert mouse capture (press again to return to `mouse`):\n",
        );
        if let Some(key) = &self.mouse_toggle_key {
            let _ = writeln!(out, "mouse_toggle_key = {}", toml_quote(&key.to_string()));
        } else {
            out.push_str("mouse_toggle_key = \"\"\n");
        }

        out.push_str(
            "\n# Maximum number of scrollback lines retained for mousewheel history scrolling:\n",
        );
        let _ = writeln!(out, "scrollback_lines = {}", self.scrollback_lines);

        // [status_bar] section
        out.push_str(
            "\n# -----------------------------------------------------------------------------\n\
             # Docked Status Bar (Vim/tmux-like status line)\n\
             # Reserves `height` row(s) at `position` (\"bottom\" or \"top\") of the terminal so\n\
             # child applications (including fullscreen TUI apps like vim, htop, fzf, less)\n\
             # never overwrite it.\n\
             #\n\
             # Colors (`fg`, `bg`, `border_fg`, `title_fg`, `track_fg`) accept:\n\
             #   - Transparency:   \"transparent\" (lets Ghostty/WezTerm/Kitty window opacity shine through)\n\
             #   - Semantic names: \"cyan\", \"blue\", \"green\", \"magenta\", \"yellow\", \"red\",\n\
             #                     \"white\", \"black\", \"gray\", \"dark_gray\", \"orange\", \"purple\",\n\
             #                     \"pink\", \"teal\", \"lavender\", \"peach\", \"sky\", \"mauve\", \"emerald\"\n\
             #   - Hex TrueColor:  \"#89b4fa\", \"#1e1e2e\"\n\
             #   - Alpha / Opacity:\"#24283b80\" (8-digit RGBA) or \"#24283b@0.75\" / \"cyan@50%\"\n\
             #   - ANSI 256 index: 0..255 (e.g. 231 = bright white, 236 = dark charcoal)\n\
             # -----------------------------------------------------------------------------\n\
             [status_bar]\n",
        );
        let _ = writeln!(out, "enabled = {}", self.status_bar.enabled);
        let _ = writeln!(
            out,
            "position = {}",
            format_inline_value(&self.status_bar.position)
        );
        let _ = writeln!(out, "height = {}", self.status_bar.height);
        let _ = writeln!(
            out,
            "separator = {}",
            toml_quote(&self.status_bar.separator)
        );
        match self.status_bar.fg {
            Some(c) => {
                let _ = writeln!(out, "fg = {}", format_inline_value(&c));
            }
            None => out.push_str("# fg = \"#c0caf5\"\n"),
        }
        match self.status_bar.bg {
            Some(c) => {
                let _ = writeln!(out, "bg = {}", format_inline_value(&c));
            }
            None => out.push_str("# bg = \"#1a1b26\"\n"),
        }

        out.push_str(
            "\n# Widgets placed on the left, center, and right of the status bar.\n\
             # Every widget supports optional per-segment styling & component decorations:\n\
             #   fg = \"black\", bg = \"cyan\", bold = true, icon = \"📁\", padding = true, pill = true\n\
             #\n\
             # Available widget types (`type = ...`):\n\
             #   1. { type = \"cwd\" }\n\
             #      -> Current working directory of the foreground process (e.g. \"~/dev/thud\")\n\
             #   2. { type = \"git_branch\" }\n\
             #      -> Current Git branch or short commit SHA (e.g. \"git:main\")\n\
             #   3. { type = \"active_command\", min_secs = 2 }\n\
             #      -> Foreground process name & elapsed timer when running >= min_secs (e.g. \"⚙ cargo (5s)\")\n\
             #   4. { type = \"clock\", format = \"%Y-%m-%d %H:%M:%S\" }\n\
             #      -> Local time formatted with strftime syntax (default: \"%H:%M:%S\")\n\
             #   5. { type = \"text\", text = \"PROD\" }\n\
             #      -> Static label string\n\
             #   6. { type = \"command\", id = \"node\", cmd = \"node -v\", trigger = \"change\", when_files = [\"package.json\", \".nvmrc\"] }\n\
             #      -> Event-driven `sh -c` command (`trigger = \"change\" | \"cwd\" | \"interval\"`, optional `when_files` filter)\n\
             #   7. { type = \"ipc_slot\", key = \"status\" }\n\
             #      -> Dynamic slot updated via `thud set status \"BUILDING...\" --ttl-ms 5000`\n\
             #   8. { type = \"sparkline\", key = \"cpu\", width = 8, label = \"CPU\" }\n\
             #      -> Historical mini-graph (`▁▂▃▄▅▆▇█`); built-in keys: \"cpu\", \"battery\", or custom via `cmd` / `thud progress`\n\
             #   9. { type = \"gauge\", key = \"battery\", width = 6 }\n\
             #      -> Progress/metric bar (`━━━━── 85%`); built-in keys: \"battery\", \"cpu\", or custom via `cmd` / `thud progress`\n",
        );

        write_widget_array(&mut out, "left", &self.status_bar.left);
        out.push('\n');
        write_widget_array(&mut out, "center", &self.status_bar.center);
        out.push('\n');
        write_widget_array(&mut out, "right", &self.status_bar.right);

        // [toast] section
        out.push_str(
            "\n# -----------------------------------------------------------------------------\n\
             # Floating Toast & Progress Notification Stack (`thud toast`, `thud progress`)\n\
             # `margin_x` and `margin_y` float notifications away from the terminal edges\n\
             # and the bottom status bar; `gap` adds vertical spacing between stacked toasts.\n\
             # `collision` moves (or hides) the whole stack when the cursor comes near; it\n\
             # takes the same modes as overlays (`ignore` / `auto_hide` / `dodge_to_opposite_corner`).\n\
             # -----------------------------------------------------------------------------\n\
             [toast]\n",
        );
        let _ = writeln!(out, "anchor = {}", format_inline_value(&self.toast.anchor));
        let _ = writeln!(out, "margin_x = {}", self.toast.margin_x);
        let _ = writeln!(out, "margin_y = {}", self.toast.margin_y);
        let _ = writeln!(out, "gap = {}", self.toast.gap);
        let _ = writeln!(out, "pill = {}", self.toast.pill);
        let _ = writeln!(out, "border = {}", format_inline_value(&self.toast.border));
        let _ = writeln!(
            out,
            "collision = {}",
            format_inline_value(&self.toast.collision)
        );

        // [[overlays]] section
        out.push_str(
            "\n# -----------------------------------------------------------------------------\n\
             # Floating Overlays (Non-destructive multi-widget HUD panels)\n\
             # Overlays do not shrink the child PTY or mutate its screen buffer; when moved\n\
             # or hidden, the underlying terminal characters (including wide CJK chars) are\n\
             # restored automatically.\n\
             # -----------------------------------------------------------------------------\n",
        );

        for (idx, ov) in self.overlays.iter().enumerate() {
            if idx > 0 {
                out.push('\n');
            }
            write_overlay(&mut out, ov);
        }

        out
    }
}

fn toml_quote(val: &str) -> String {
    toml::Value::String(val.to_string()).to_string()
}

fn format_inline_value<T: serde::Serialize>(val: &T) -> String {
    const PRIORITY_KEYS: &[&str] = &[
        "type",
        "preset",
        "id",
        "key",
        "cmd",
        "trigger",
        "when_files",
        "text",
        "format",
        "width",
        "label",
        "min_secs",
        "interval_ms",
        "icon",
        "fg",
        "bg",
        "track_fg",
        "bold",
        "pill",
        "padding",
        "prefix",
        "suffix",
    ];
    match toml::Value::try_from(val) {
        Ok(toml::Value::Table(mut map)) => {
            let mut fields: Vec<String> = Vec::with_capacity(map.len());
            for &pk in PRIORITY_KEYS {
                if let Some(v) = map.remove(pk) {
                    fields.push(format!("{pk} = {v}"));
                }
            }
            for (k, v) in map {
                fields.push(format!("{k} = {v}"));
            }
            format!("{{ {} }}", fields.join(", "))
        }
        Ok(other) => other.to_string(),
        Err(_) => "{}".to_string(),
    }
}

fn write_widget_array(out: &mut String, name: &str, widgets: &[WidgetSpec]) {
    if widgets.is_empty() {
        let _ = writeln!(out, "{name} = []");
        return;
    }
    let _ = writeln!(out, "{name} = [");
    for w in widgets {
        let _ = writeln!(out, "    {},", format_inline_value(w));
    }
    out.push_str("]\n");
}

fn write_overlay(out: &mut String, ov: &OverlayConfig) {
    out.push_str("[[overlays]]\n");
    let _ = writeln!(out, "id = {}", toml_quote(&ov.id));
    out.push_str(
        "\n# Anchor position on the child terminal viewport:\n\
         #   \"top_right\" | \"top_left\" | \"top_center\" | \"bottom_right\" | \"bottom_left\" | \"bottom_center\"\n",
    );
    let _ = writeln!(out, "anchor = {}", format_inline_value(&ov.anchor));
    let _ = writeln!(out, "offset_x = {}", ov.offset_x);
    let _ = writeln!(out, "offset_y = {}", ov.offset_y);
    let _ = writeln!(out, "z_index = {}", ov.z_index);

    out.push_str(
        "\n# Which screen buffer mode this overlay is visible in:\n\
         #   \"always\"         -> Both normal shell and fullscreen TUI apps (vim, htop, less)\n\
         #   \"primary_only\"   -> Only normal shell screen; hidden during fullscreen TUI apps\n\
         #   \"alternate_only\" -> Only during fullscreen TUI apps\n",
    );
    let _ = writeln!(
        out,
        "screen_mode = {}",
        format_inline_value(&ov.screen_mode)
    );

    out.push_str(
        "\n# Optional box border around the overlay:\n\
         #   \"none\" | \"single\" | \"rounded\" | \"double\"\n",
    );
    let _ = writeln!(out, "border = {}", format_inline_value(&ov.border));
    match &ov.title {
        Some(title) => {
            let _ = writeln!(out, "title = {}", toml_quote(title));
        }
        None => {
            out.push_str("# title = \" TIME \"\n");
        }
    }

    match ov.fg {
        Some(c) => {
            let _ = writeln!(out, "fg = {}", format_inline_value(&c));
        }
        None => out.push_str("# fg = \"white\"\n"),
    }
    match ov.bg {
        Some(c) => {
            let _ = writeln!(out, "bg = {}", format_inline_value(&c));
        }
        None => out.push_str("# bg = \"dark_gray\"\n"),
    }
    match ov.border_fg {
        Some(c) => {
            let _ = writeln!(out, "border_fg = {}", format_inline_value(&c));
        }
        None => out.push_str("# border_fg = \"cyan\"\n"),
    }
    match ov.title_fg {
        Some(c) => {
            let _ = writeln!(out, "title_fg = {}", format_inline_value(&c));
        }
        None => out.push_str("# title_fg = \"yellow\"\n"),
    }
    let _ = writeln!(out, "padding_x = {}", ov.padding_x);
    let _ = writeln!(out, "bold = {}", ov.bold);
    if let Some(ttl) = ov.ttl_ms {
        let _ = writeln!(out, "ttl_ms = {ttl}");
    }

    out.push_str("\n# Widgets stacked vertically inside this overlay panel:\n");
    write_widget_array(out, "widgets", &ov.widgets);

    out.push_str(
        "\n# Behavior when the child cursor or terminal text reaches the overlay region:\n\
         #   1. { mode = \"dodge_to_opposite_corner\", margin = 1, avoid_text = true }\n\
         #   2. { mode = \"auto_hide\", margin = 1, avoid_text = true }\n\
         #   3. { mode = \"ignore\" }\n\
         [overlays.collision]\n",
    );
    if let Ok(toml::Value::Table(map)) = toml::Value::try_from(ov.collision) {
        for (k, v) in map {
            let _ = writeln!(out, "{k} = {v}");
        }
    }
}
