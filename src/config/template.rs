use crate::config::{Config, OverlayConfig, WidgetSpec};
use std::fmt::Write as _;

impl Config {
    /// Generates a self-documenting TOML configuration string directly from `Config::default()`.
    pub fn annotated_default_toml() -> String {
        Self::default().to_annotated_toml()
    }

    /// Formats `self` into a self-documenting, human-friendly TOML configuration string
    /// with comments explaining every section, widget kind, and option.
    pub fn to_annotated_toml(&self) -> String {
        let mut out = String::with_capacity(4096);

        out.push_str(
            "# =============================================================================\n\
             # thud - Terminal Heads Up Display Configuration\n\
             # Default path: ~/.config/thud/config.toml (or override via $THUD_CONFIG / --config)\n\
             # =============================================================================\n\n\
             # Optional: Shell executable to spawn inside the PTY.\n\
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
            let _ = writeln!(out, "zen_toggle_key = {}", toml_quote(key));
        } else {
            out.push_str("# zen_toggle_key = \"ctrl-\\\\\"\n");
        }

        out.push_str(
            "\n# Hotkey to toggle Dashboard overlays (`screen_mode = \"dashboard_only\"`):\n",
        );
        if let Some(key) = &self.dashboard_toggle_key {
            let _ = writeln!(out, "dashboard_toggle_key = {}", toml_quote(key));
        } else {
            out.push_str("# dashboard_toggle_key = \"ctrl-]\"\n");
        }

        // [status_bar] section
        out.push_str(
            "\n# -----------------------------------------------------------------------------\n\
             # Docked Bottom Status Bar (Vim-like status line)\n\
             # Reserves `height` row(s) at the bottom of the terminal so child applications\n\
             # (including fullscreen TUI apps like vim, htop, fzf, less) never overwrite it.\n\
             # -----------------------------------------------------------------------------\n\
             [status_bar]\n",
        );
        let _ = writeln!(out, "enabled = {}", self.status_bar.enabled);
        let _ = writeln!(out, "height = {}", self.status_bar.height);
        let _ = writeln!(
            out,
            "separator = {}",
            toml_quote(&self.status_bar.separator)
        );
        out.push_str("# Default 256-color ANSI palette index (0-255): 255 = bright white, 236 = dark charcoal\n");
        let _ = writeln!(out, "fg_ansi = {}", self.status_bar.fg_ansi);
        let _ = writeln!(out, "bg_ansi = {}", self.status_bar.bg_ansi);

        out.push_str(
            "\n# Widgets placed on the left, center, and right of the status bar.\n\
             # Every widget supports optional per-segment styling & capsule affixes:\n\
             #   fg_ansi = 231, bg_ansi = 24, bold = true, prefix = \" \", suffix = \" \"\n\
             #\n\
             # Available widget types (`type = ...`):\n\
             #   1. { type = \"cwd\" }\n\
             #      -> Current working directory of the foreground process (e.g. \"~/dev/thud\")\n\
             #   2. { type = \"git_branch\" }\n\
             #      -> Current Git branch or short commit SHA (e.g. \"git:main\")\n\
             #   3. { type = \"active_command\", min_secs = 2 }\n\
             #      -> Foreground process name & elapsed timer when running >= min_secs (e.g. \"⚙ cargo (5s)\")\n\
             #   4. { type = \"battery\" }\n\
             #      -> System battery level (e.g. \"BAT 100%\")\n\
             #   5. { type = \"clock\", format = \"%Y-%m-%d %H:%M:%S\" }\n\
             #      -> Local time formatted with strftime syntax (default: \"%H:%M:%S\")\n\
             #   6. { type = \"text\", text = \"PROD\" }\n\
             #      -> Static label string\n\
             #   7. { type = \"command\", id = \"k8s\", cmd = \"kubectl config current-context\", interval_ms = 5000 }\n\
             #      -> Periodic non-blocking `sh -c` command output refreshed every `interval_ms`\n\
             #   8. { type = \"ipc_slot\", key = \"status\" }\n\
             #      -> Dynamic slot updated via `thud set status \"BUILDING...\" --ttl-ms 5000`\n\
             #   9. { type = \"sparkline\", key = \"cpu\", width = 8 }\n\
             #      -> Historical mini-graph (`▁▂▃▄▅▆▇█`) fed via `thud progress cpu 65`\n\
             #  10. { type = \"gauge\", key = \"build\", width = 10, label = \"Build\" }\n\
             #      -> Progress bar (`━━━─── 60%`) fed via `thud progress build 60 --label \"Compiling\"`\n",
        );

        write_widget_array(&mut out, "left", &self.status_bar.left);
        out.push('\n');
        write_widget_array(&mut out, "center", &self.status_bar.center);
        out.push('\n');
        write_widget_array(&mut out, "right", &self.status_bar.right);

        // [[overlays]] section
        out.push_str(
            "\n# -----------------------------------------------------------------------------\n\
             # Floating Overlays (Non-destructive HUD layers composited over the terminal)\n\
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
    match toml::Value::try_from(val) {
        Ok(toml::Value::Table(map)) => {
            let fields: Vec<String> = map.into_iter().map(|(k, v)| format!("{k} = {v}")).collect();
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
         #   \"alternate_only\" -> Only during fullscreen TUI apps\n\
         #   \"dashboard_only\" -> Only when toggled via `dashboard_toggle_key` (default: Ctrl+])\n",
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

    out.push_str("\n# 256-color ANSI palette index (0-255) and text weight\n");
    let _ = writeln!(out, "fg_ansi = {}", ov.fg_ansi);
    let _ = writeln!(out, "bg_ansi = {}", ov.bg_ansi);
    let _ = writeln!(out, "bold = {}", ov.bold);
    if let Some(ttl) = ov.ttl_ms {
        let _ = writeln!(out, "ttl_ms = {ttl}");
    }

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

    out.push_str("\n[overlays.widget]\n");
    if let Ok(toml::Value::Table(map)) = toml::Value::try_from(&ov.widget) {
        for (k, v) in map {
            let _ = writeln!(out, "{k} = {v}");
        }
    }
}
