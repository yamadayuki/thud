use cth::{
    Anchor, CollisionPolicy, Config, IpcMessage, OverlayConfig, ScreenCondition, Session,
    SessionError, WidgetSpec,
};
use std::path::PathBuf;

fn main() {
    if let Err(err) = try_main() {
        eprintln!("cth error: {err}");
        std::process::exit(1);
    }
}

fn try_main() -> Result<(), SessionError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None | Some("run") => {
            let config = parse_run_config(&args)?;
            let code = Session::run(config)?;
            std::process::exit(code);
        }
        Some("set") => {
            // Usage: cth set <key> <value> [--ttl-ms <ms>] [--socket <path>]
            if args.len() < 3 {
                return Err(SessionError::Ipc(
                    "usage: cth set <key> <value> [--ttl-ms <ms>] [--socket <path>]".to_string(),
                ));
            }
            let key = args[1].clone();
            let value = args[2].clone();
            let (ttl_ms, socket) = parse_ipc_flags(&args[3..])?;
            Session::send_ipc_message(socket, &IpcMessage::SetSlot { key, value, ttl_ms })
        }
        Some("clear") => {
            if args.len() < 2 {
                return Err(SessionError::Ipc(
                    "usage: cth clear <key> [--socket <path>]".to_string(),
                ));
            }
            let key = args[1].clone();
            let (_, socket) = parse_ipc_flags(&args[2..])?;
            Session::send_ipc_message(socket, &IpcMessage::ClearSlot { key })
        }
        Some("toast") => {
            // Usage: cth toast <message> [--ttl-ms <ms>] [--socket <path>]
            if args.len() < 2 {
                return Err(SessionError::Ipc(
                    "usage: cth toast <message> [--ttl-ms <ms>] [--socket <path>]".to_string(),
                ));
            }
            let text = format!(" {} ", args[1]);
            let (ttl_ms, socket) = parse_ipc_flags(&args[2..])?;
            let overlay = OverlayConfig {
                id: "toast".to_string(),
                anchor: Anchor::BottomRight,
                offset_x: 0,
                offset_y: 0,
                z_index: 50,
                screen_mode: ScreenCondition::Always,
                collision: CollisionPolicy::AutoHide { margin: 1 },
                fg_ansi: 15,
                bg_ansi: 24,
                bold: true,
                widget: WidgetSpec::Text { text },
                ttl_ms: Some(ttl_ms.unwrap_or(4000)),
            };
            Session::send_ipc_message(socket, &IpcMessage::UpsertOverlay { overlay })
        }
        Some("print-default-config") => {
            let default_cfg = Config::default();
            let serialized =
                toml::to_string_pretty(&default_cfg).expect("default config serializes to TOML");
            print!("{serialized}");
            Ok(())
        }
        Some("--help" | "-h" | "help") => {
            print_help();
            Ok(())
        }
        Some(flag) if flag.starts_with("--") => {
            let config = parse_run_config(&args)?;
            let code = Session::run(config)?;
            std::process::exit(code);
        }
        Some(other) => Err(SessionError::Ipc(format!(
            "unknown subcommand '{other}'. Run `cth --help` for usage."
        ))),
    }
}

fn parse_run_config(args: &[String]) -> Result<Config, SessionError> {
    let mut idx = 0;
    if args.first().map(String::as_str) == Some("run") {
        idx = 1;
    }
    let mut config_path: Option<PathBuf> = None;
    let mut shell_override: Option<String> = None;

    while idx < args.len() {
        match args[idx].as_str() {
            "--config" | "-c" => {
                idx += 1;
                if let Some(p) = args.get(idx) {
                    config_path = Some(PathBuf::from(p));
                }
            }
            "--shell" | "-s" => {
                idx += 1;
                if let Some(sh) = args.get(idx) {
                    shell_override = Some(sh.clone());
                }
            }
            _ => {}
        }
        idx += 1;
    }

    let mut cfg = match config_path {
        Some(p) => Config::from_file(p)?,
        None => Config::load_or_default()?,
    };
    if let Some(sh) = shell_override {
        cfg.shell = Some(sh);
    }
    Ok(cfg)
}

fn parse_ipc_flags(args: &[String]) -> Result<(Option<u64>, Option<PathBuf>), SessionError> {
    let mut ttl_ms = None;
    let mut socket = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--ttl-ms" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    ttl_ms = Some(
                        v.parse::<u64>()
                            .map_err(|e| SessionError::Ipc(format!("invalid --ttl-ms: {e}")))?,
                    );
                }
            }
            "--socket" => {
                i += 1;
                if let Some(p) = args.get(i) {
                    socket = Some(PathBuf::from(p));
                }
            }
            _ => {}
        }
        i += 1;
    }
    Ok((ttl_ms, socket))
}

fn print_help() {
    println!(
        "\
cth - Terminal-emulator-agnostic & shell-agnostic HUD compositor

USAGE:
  cth [run] [--config <path>] [--shell <shell>]   Start interactive terminal HUD session
  cth set <key> <value> [--ttl-ms <ms>]           Update an IPC status slot in the active session
  cth clear <key>                                 Clear an IPC status slot
  cth toast <message> [--ttl-ms <ms>]             Show a floating toast notification overlay
  cth print-default-config                        Print the default TOML configuration"
    );
}
