# Usage & IPC Guide

This guide covers starting and managing `thud` sessions, keyboard shortcuts, live IPC commands (`set`, `progress`, `toast`, `reload`), raw Unix socket scripting, and integration examples.

---

## 1. Starting a Session

Run `thud` (or `thud run`) to wrap your default `$SHELL` in a transparent HUD session:

```sh
thud
```

### Custom Shell or Config File
```sh
thud --shell /opt/homebrew/bin/fish --config ~/.config/thud/config.toml
```

### Nested Session Protection & Fail-Safe Auto-Healing
`thud` automatically exports `THUD_ACTIVE=1`, `THUD_SOCKET=/tmp/thud-<pid>.sock`, and `THUD_TTY=<device>` inside the child shell, always replacing any values inherited from a parent session.

- **Socket Liveness Verification**: `thud` verifies that `$THUD_SOCKET` is genuinely reachable before asserting a nested session. If a previous session terminated abnormally or was killed externally, `thud` removes the dead socket (only if the path really is a socket file) and launches normally.
- **TTY Differentiation**: If a new terminal window or VS Code integrated terminal inherits `$THUD_ACTIVE` from a parent environment, `thud` compares the current terminal's TTY against `$THUD_TTY`. If they differ, the inherited variables are ignored and `thud` starts without blocking.
- **Explicit Override (`--force` / `-f`)**: If you ever want to bypass nested session checks explicitly, run:
  ```sh
  thud --force
  # or
  thud -f
  ```
- **Nested Session Prevention**: If you run `thud` directly inside an active session on the exact same TTY where the socket is alive, it blocks recursion:
  ```text
  thud error: cannot start a nested thud session inside an active thud session (use 'thud --force' to override, or 'exit' to leave)
  ```
Other CLI subcommands (`thud set`, `thud progress`, `thud toast`, `thud zen`, `thud reload`, `thud quit`, `thud init-config`) work normally inside the session.

---

## 2. Keys, Mouse & Zen Mode

thud is only a HUD: it never intercepts keys or the mouse. Every keystroke reaches your shell, and mouse reporting follows exactly what the app inside requests (vim, htop, …). At the shell prompt your terminal's own text selection and copy work as usual.

### Zen Mode (`thud zen`)
Hide all HUD elements (status bar, floating overlays, and toasts) and give the shell the full terminal height:
```sh
thud zen        # toggle
thud zen on     # hide the HUD
thud zen off    # show the HUD
```
To toggle it from a key, bind the command in your shell, for example:
```sh
# zsh
thud-zen() { thud zen }; zle -N thud-zen; bindkey '^\\' thud-zen
# fish
bind \c\\ 'thud zen'
# bash
bind -x '"\C-\\": thud zen'
```

---

## 3. CLI Subcommands

### Configuration Management
```sh
# Write an annotated default config to ~/.config/thud/config.toml
thud init-config

# Overwrite an existing config file
thud init-config --force

# Write to a custom path
thud init-config --output ~/my-thud.toml

# Print the annotated default config to stdout
thud print-default-config

# Manually reload config.toml in the running session
thud reload
```

### Live IPC Slots (`thud set` / `thud clear`)
Populate any `{ type = "ipc_slot", key = "<key>" }` widget on your status bar or floating overlay:

```sh
# Set the default "status" slot on the status bar
thud set status "🤖 Claude: running tests"

# Set a slot that automatically disappears after 10 seconds (10,000 ms)
thud set status "Deploying to staging..." --ttl-ms 10000

# Clear the slot immediately
thud clear status
```

### Live Progress & Metrics (`thud progress`)
Push numeric percentage samples (`0..=100`) to `gauge` and `sparkline` widgets:
- **Configured key**: If `<key>` matches a `gauge` or `sparkline` widget in `config.toml`, that widget updates in-place.
- **Ad-hoc key**: If `<key>` is **not** referenced anywhere in `config.toml`, `thud` automatically pops up a floating progress gauge in your `[toast]` stack until you mark it `done` (or it expires)!

```sh
# Drive an ad-hoc or configured progress bar
for pct in 10 35 70 100; do
  thud progress build "$pct" --label "cargo test"
  sleep 0.3
done

# Clear the progress bar when finished
thud progress build done
```

### Stacked Toast Notifications (`thud toast`)
Trigger transient notifications in the corner configured by `[toast]` (defaults to `bottom_right` floating above the status bar):

```sh
thud toast "Build succeeded!" --level success
thud toast "3 clippy warnings found" --level warn --ttl-ms 6000
thud toast "Deployment failed" --level error --ttl-ms 8000
thud toast "Indexing workspace..." --level info --ttl-ms 3000
```

Available `--level` values:
- `info` *(default, blue)*
- `success` *(green)*
- `warn` *(amber/orange)*
- `error` *(red)*

### Exiting the Session (`thud quit`)
You can exit normally by typing `exit` / pressing `Ctrl+D` in your shell, or terminate the session via IPC:
```sh
thud quit
```
`thud quit` is fully **idempotent**: if no active session is reachable (for example after the session was killed), it outputs `No active thud session is running.` and exits cleanly with exit code `0`.

---

## 4. Environment Variables

Inside a running `thud` session, the following environment variables are available to child processes:

| Variable | Description |
| :--- | :--- |
| `THUD_ACTIVE` | Set to `"1"` inside an active `thud` PTY session. |
| `THUD_SOCKET` | Path to the active session's Unix domain socket (e.g., `/tmp/thud-12345.sock`). Used automatically by `thud set`, `thud progress`, `thud toast`, `thud reload`, and `thud quit`. |
| `THUD_TTY` | TTY device of the outer terminal that launched the session (e.g., `/dev/ttys002`); used to tell a nested launch from a new terminal window. Unset when the outer terminal has no TTY name. |

To send IPC commands from an **external** terminal window or background daemon that did not inherit `$THUD_SOCKET`, pass `--socket`:
```sh
thud --socket /tmp/thud-12345.sock toast "Background job finished!" --level success
```

---

## 5. Raw JSON Unix Socket Protocol

In addition to the `thud` CLI subcommands, any language or tool can send newline-delimited JSON messages directly to `$THUD_SOCKET` (e.g., via `nc -U "$THUD_SOCKET"` or `socat`):

```json
{"action": "set_slot", "key": "status", "value": "READY", "ttl_ms": 5000}
{"action": "clear_slot", "key": "status"}
{"action": "push_metric", "key": "build", "value": 75, "label": "Compiling", "ttl_ms": 15000}
{"action": "toast", "message": "Hello from socket", "level": "info", "ttl_ms": 4000}
{"action": "reload_config"}
{"action": "shutdown"}
```

You can also dynamically add or remove floating overlays at runtime via JSON IPC:
```sh
printf '%s\n' '{
  "action": "upsert_overlay",
  "overlay": {
    "id": "live_panel",
    "anchor": "top_left",
    "border": "rounded",
    "title": " AGENT ",
    "padding_x": 1,
    "widgets": [
      { "type": "text", "text": "Refactoring src/ui..." }
    ],
    "ttl_ms": 8000
  }
}' | nc -U "$THUD_SOCKET"

# Remove it early:
printf '%s\n' '{"action": "remove_overlay", "id": "live_panel"}' | nc -U "$THUD_SOCKET"
```

---

## 6. Practical Integration Recipes

### Recipe 1: Auto-Notify When Long Commands Finish (`zsh` / `fish`)
Notify yourself with a toast whenever a command takes longer than 10 seconds:

**Fish (`~/.config/fish/config.fish`)**:
```fish
if set -q THUD_ACTIVE
    function __thud_postexec --on-event fish_postexec
        if test $CMD_DURATION -gt 10000
            set -l secs (math -s1 "$CMD_DURATION / 1000")
            thud toast "Finished ($secs s): $argv[1]" --level success
        end
    end
end
```

**Zsh (`~/.zshrc`)**:
```zsh
if [[ -n "$THUD_ACTIVE" ]]; then
  autoload -Uz add-zsh-hook
  _thud_preexec() { _thud_cmd_start=$SECONDS; _thud_cmd_name="$1"; }
  _thud_precmd() {
    local exit_code=$?
    if [[ -n "$_thud_cmd_start" ]] && (( SECONDS - _thud_cmd_start >= 10 )); then
      local elapsed=$(( SECONDS - _thud_cmd_start ))
      if (( exit_code == 0 )); then
        thud toast "Done (${elapsed}s): ${_thud_cmd_name}" --level success
      else
        thud toast "Failed [exit ${exit_code}] (${elapsed}s): ${_thud_cmd_name}" --level error
      fi
    fi
    unset _thud_cmd_start
  }
  add-zsh-hook preexec _thud_preexec
  add-zsh-hook precmd _thud_precmd
fi
```

### Recipe 2: Project-Aware Language / Tooling Versions in `config.toml`
Use `type = "command"` with `trigger = "change"` and `when_files` so version commands only run inside relevant repositories and never poll on a timer:

```toml
[status_bar]
left = [
    { type = "cwd", padding = true, bold = true },
    { type = "git_branch", padding = true, bold = true },
    { type = "command", id = "rust", cmd = "rustc -V | cut -d' ' -f2", trigger = "change", when_files = ["Cargo.toml"], icon = "🦀", padding = true },
    { type = "command", id = "node", cmd = "node -v", trigger = "change", when_files = ["package.json", ".nvmrc"], icon = "⬢", padding = true },
]
```
