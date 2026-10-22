//! CLI subcommands that talk to a session over IPC. Kept apart from the PTY E2E harness,
//! whose post-`fork` child must not race with other threads spawning processes.

#[test]
fn ipc_commands_report_missing_session_and_quit_stays_idempotent() {
    let thud = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_thud"))
            .args(args)
            .env_remove("THUD_SOCKET")
            .env_remove("THUD_ACTIVE")
            .env_remove("THUD_TTY")
            .output()
            .unwrap()
    };

    let quit = thud(&["quit"]);
    assert!(quit.status.success());
    assert_eq!(
        String::from_utf8_lossy(&quit.stdout).trim(),
        "No active thud session is running."
    );

    let set = thud(&[
        "set",
        "mode",
        "NORMAL",
        "--socket",
        "/tmp/thud-e2e-missing.sock",
    ]);
    assert!(!set.status.success());
    assert!(
        String::from_utf8_lossy(&set.stderr)
            .contains("no active thud session reachable at /tmp/thud-e2e-missing.sock"),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
}
