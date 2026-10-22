use std::path::PathBuf;
use thud::{Config, IpcMessage, Session, SessionError, ToastLevel};

fn main() {
    if let Err(err) = try_main() {
        eprintln!("thud error: {err}");
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
            // Usage: thud set <key> <value> [--ttl-ms <ms>] [--socket <path>]
            if args.len() < 3 {
                return Err(SessionError::Ipc(
                    "usage: thud set <key> <value> [--ttl-ms <ms>] [--socket <path>]".to_string(),
                ));
            }
            let key = args[1].clone();
            let value = args[2].clone();
            let flags = parse_ipc_flags(&args[3..])?;
            Session::send_ipc_message(
                flags.socket,
                &IpcMessage::SetSlot {
                    key,
                    value,
                    ttl_ms: flags.ttl_ms,
                },
            )
        }
        Some("clear") => {
            if args.len() < 2 {
                return Err(SessionError::Ipc(
                    "usage: thud clear <key> [--socket <path>]".to_string(),
                ));
            }
            let key = args[1].clone();
            let flags = parse_ipc_flags(&args[2..])?;
            Session::send_ipc_message(flags.socket, &IpcMessage::ClearSlot { key })
        }
        Some("progress" | "metric") => {
            // Usage: thud progress <key> <0..100|done> [--label <text>] [--ttl-ms <ms>] [--socket <path>]
            if args.len() < 3 {
                return Err(SessionError::Ipc(
                    "usage: thud progress <key> <0..100|done> [--label <text>] [--ttl-ms <ms>]"
                        .to_string(),
                ));
            }
            let key = args[1].clone();
            let flags = parse_ipc_flags(&args[3..])?;
            if args[2] == "done" || args[2] == "clear" {
                return Session::send_ipc_message(flags.socket, &IpcMessage::ClearSlot { key });
            }
            let value = args[2]
                .parse::<u8>()
                .map_err(|e| SessionError::Ipc(format!("invalid percentage '{}': {e}", args[2])))?;
            Session::send_ipc_message(
                flags.socket,
                &IpcMessage::PushMetric {
                    key,
                    value,
                    label: flags.label,
                    ttl_ms: flags.ttl_ms,
                },
            )
        }
        Some("toast") => {
            // Usage: thud toast <message> [--level info|success|warn|error] [--ttl-ms <ms>] [--socket <path>]
            if args.len() < 2 {
                return Err(SessionError::Ipc(
                    "usage: thud toast <message> [--level info|success|warn|error] [--ttl-ms <ms>]"
                        .to_string(),
                ));
            }
            let message = args[1].clone();
            let flags = parse_ipc_flags(&args[2..])?;
            Session::send_ipc_message(
                flags.socket,
                &IpcMessage::Toast {
                    message,
                    level: flags.level.unwrap_or(ToastLevel::Info),
                    ttl_ms: flags.ttl_ms.unwrap_or(4000),
                },
            )
        }
        Some("reload") => {
            let flags = parse_ipc_flags(&args[1..])?;
            Session::send_ipc_message(flags.socket, &IpcMessage::ReloadConfig)
        }
        Some("quit" | "exit") => {
            let flags = parse_ipc_flags(&args[1..])?;
            Session::send_ipc_message(flags.socket, &IpcMessage::Quit)
        }
        Some("init-config") => {
            let (output_path, force) = parse_init_config_flags(&args[1..]);
            let written_path = Config::write_default_file(output_path, force)?;
            println!(
                "Wrote annotated default config to {}",
                written_path.display()
            );
            Ok(())
        }
        Some("print-default-config") => {
            print!("{}", Config::annotated_default_toml());
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
            "unknown subcommand '{other}'. Run `thud --help` for usage."
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
        Some(p) => {
            unsafe {
                std::env::set_var("THUD_CONFIG", p.as_os_str());
            }
            Config::from_file(p)?
        }
        None => Config::load_or_default()?,
    };
    if let Some(sh) = shell_override {
        cfg.shell = Some(sh);
    }
    Ok(cfg)
}

#[derive(Default)]
struct IpcFlags {
    ttl_ms: Option<u64>,
    socket: Option<PathBuf>,
    label: Option<String>,
    level: Option<ToastLevel>,
}

fn parse_ipc_flags(args: &[String]) -> Result<IpcFlags, SessionError> {
    let mut flags = IpcFlags::default();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--ttl-ms" => {
                i += 1;
                if let Some(v) = args.get(i) {
                    flags.ttl_ms = Some(
                        v.parse::<u64>()
                            .map_err(|e| SessionError::Ipc(format!("invalid --ttl-ms: {e}")))?,
                    );
                }
            }
            "--socket" => {
                i += 1;
                if let Some(p) = args.get(i) {
                    flags.socket = Some(PathBuf::from(p));
                }
            }
            "--label" | "-l" => {
                i += 1;
                if let Some(lbl) = args.get(i) {
                    flags.label = Some(lbl.clone());
                }
            }
            "--level" => {
                i += 1;
                if let Some(lvl) = args.get(i) {
                    flags.level = Some(match lvl.as_str() {
                        "info" => ToastLevel::Info,
                        "success" | "ok" => ToastLevel::Success,
                        "warn" | "warning" => ToastLevel::Warn,
                        "error" | "err" => ToastLevel::Error,
                        other => {
                            return Err(SessionError::Ipc(format!(
                                "invalid --level '{other}' (expected info|success|warn|error)"
                            )));
                        }
                    });
                }
            }
            _ => {}
        }
        i += 1;
    }
    Ok(flags)
}

fn parse_init_config_flags(args: &[String]) -> (Option<PathBuf>, bool) {
    let mut output_path = None;
    let mut force = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--output" | "-o" | "--config" | "-c" => {
                i += 1;
                if let Some(p) = args.get(i) {
                    output_path = Some(PathBuf::from(p));
                }
            }
            "--force" | "-f" => {
                force = true;
            }
            path if !path.starts_with('-') && output_path.is_none() => {
                output_path = Some(PathBuf::from(path));
            }
            _ => {}
        }
        i += 1;
    }
    (output_path, force)
}

fn print_help() {
    println!(
        "\
thud - Terminal Heads Up Display (terminal-emulator-agnostic & shell-agnostic HUD compositor)

USAGE:
  thud [run] [--config <path>] [--shell <shell>]             Start interactive terminal HUD session
  thud init-config [--output <path>] [--force]               Write annotated config file (~/.config/thud/config.toml)
  thud print-default-config                                  Print annotated default TOML configuration
  thud set <key> <value> [--ttl-ms <ms>]                     Update an IPC status slot
  thud clear <key>                                           Clear an IPC status slot or metric
  thud progress <key> <0..100|done> [--label <txt>]          Push progress/metric for gauge or sparkline widgets
  thud toast <msg> [--level info|success|warn|error]         Enqueue a stacked toast notification overlay
  thud reload                                                Reload config.toml in the active thud session
  thud quit                                                  Gracefully exit the active thud session"
    );
}
