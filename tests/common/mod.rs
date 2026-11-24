use std::process::Command;

/// The binary under test, without the environment variables that switch it to
/// agent-oriented output (see `is_llm_environment` in src/util.rs), so tests
/// pass when run from a coding agent.
pub fn treeherder_cli() -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("treeherder-cli"));
    for var in [
        "CLAUDECODE",
        "CODEX_THREAD_ID",
        "CODEX_SANDBOX",
        "GEMINI_CLI",
        "OPENCODE",
    ] {
        cmd.env_remove(var);
    }
    cmd
}
