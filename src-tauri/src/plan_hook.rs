//! Claude Code plan capture: teach a spawned `claude` to drop the plan it
//! is about to present into the task worktree's `.context/plans/`.
//!
//! Plan mode writes the plan to the GLOBAL `~/.claude/plans/<slug>.md`, which
//! lives outside the worktree and is therefore invisible to termic's file
//! tree. We fix that by injecting a Claude Code hook that calls back into our
//! own bundled CLI (`termic capture-plan`, see `termic-cli/src/plan_capture.rs`)
//! with the plan on stdin.
//!
//! Two deliberate choices, both load-bearing:
//!
//!  1. **Inline JSON, not a settings FILE.** `--settings` takes either a path
//!     or a literal JSON string. A file would have to live in our app-data
//!     dir, and `sandbox.rs` denies read of that dir as the FINAL filesystem
//!     rule of both enforcing branches (it is where the CLI token lives), so
//!     a caged claude could never read it. Inline also means there is no file
//!     to keep in sync with an app update that moves the CLI path.
//!
//!  2. **Both `PreToolUse` and `PostToolUse`.** `PostToolUse` on
//!     `ExitPlanMode` only fires once the user APPROVES, which is too late:
//!     the whole point is to read the plan while deciding. `PreToolUse` fires
//!     before the approval prompt renders and carries `tool_input.plan` +
//!     `tool_input.planFilePath`. `PostToolUse` is kept as an idempotent
//!     confirm (it re-renders identical bytes, which `capture-plan` skips).

use std::path::Path;

/// Wrap `s` for safe use inside a single-quoted POSIX shell word.
///
/// The `command` string inside the hook JSON is run BY A SHELL (claude spawns
/// hooks via `sh -c`), so a bundle path like `/Applications/Termic Beta.app/…`
/// would otherwise split into two words. The JSON blob itself needs no
/// quoting: it is handed to the child as one argv element.
pub fn shell_quote_single(s: &str) -> String {
    // Close the quote, emit an escaped literal quote, reopen. The only
    // character with meaning inside '…' is ' itself.
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The `--settings` payload: one hook command wired to both plan-mode events.
///
/// `timeout` is short on purpose. A hook that hangs stalls the agent's turn,
/// and this one only copies a few KB of markdown; if it cannot manage that in
/// five seconds something is wrong and we would rather lose the capture than
/// make the user wait.
pub fn hook_settings_json(cli_path: &Path) -> String {
    let command = format!("{} capture-plan", shell_quote_single(&cli_path.to_string_lossy()));
    let entry = serde_json::json!([{
        "matcher": "ExitPlanMode",
        "hooks": [{ "type": "command", "command": command, "timeout": 5 }],
    }]);
    serde_json::json!({
        "hooks": { "PreToolUse": entry, "PostToolUse": entry },
    })
    .to_string()
}

/// Is this spawn a `claude`, and therefore the only CLI that understands
/// `--settings` and `ExitPlanMode`?
///
/// EXACT matches only, never a substring. A false positive hands an unknown
/// flag to codex/gemini/a custom CLI, which is a hard startup failure: the
/// one outcome this feature must never produce. `claude-wrapper` and
/// `myclaude` are somebody else's binary and are treated as such.
///
/// Both inputs are checked because the agent registry lets a user give a
/// custom id to an entry whose `command` is still plain `claude`.
pub fn is_claude_agent(agent_id: Option<&str>, cmd: &str) -> bool {
    if agent_id == Some("claude") {
        return true;
    }
    Path::new(cmd).file_name().map(|n| n == "claude").unwrap_or(false)
}

/// Args to PREPEND to a claude spawn, or `None` to leave the spawn alone.
///
/// Returns `None` when the user already passes their own `--settings` (from
/// Settings → Agent CLIs). Two of them would silently clobber one or the
/// other, and between our convenience and their explicit configuration,
/// theirs wins.
pub fn plan_capture_args(
    agent_id: Option<&str>,
    cmd: &str,
    existing: &[String],
    cli_path: &Path,
) -> Option<Vec<String>> {
    if !is_claude_agent(agent_id, cmd) {
        return None;
    }
    let user_owns_settings = existing
        .iter()
        .any(|a| a == "--settings" || a.starts_with("--settings="));
    if user_owns_settings {
        return None;
    }
    Some(vec!["--settings".to_string(), hook_settings_json(cli_path)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn cli() -> PathBuf {
        PathBuf::from("/Applications/Termic.app/Contents/MacOS/termic")
    }

    #[test]
    fn quotes_plain_and_spaced_paths() {
        assert_eq!(shell_quote_single("/usr/bin/termic"), "'/usr/bin/termic'");
        assert_eq!(shell_quote_single("/A B/x"), "'/A B/x'");
    }

    #[test]
    fn quotes_embedded_single_quote() {
        // /it's/termic -> 'it'\''s' so the shell sees one word.
        assert_eq!(shell_quote_single("/it's/x"), "'/it'\\''s/x'");
    }

    #[test]
    fn identifies_claude_by_id_or_basename() {
        assert!(is_claude_agent(Some("claude"), "claude"));
        assert!(is_claude_agent(None, "/usr/local/bin/claude"));
        // Custom registry id, stock binary.
        assert!(is_claude_agent(Some("my-claude"), "claude"));
    }

    #[test]
    fn rejects_every_other_cli() {
        for (id, cmd) in [
            (Some("codex"), "codex"),
            (Some("gemini"), "gemini"),
            (None, "/opt/bin/claude-wrapper"),
            (None, "myclaude"),
            (None, ""),
        ] {
            assert!(!is_claude_agent(id, cmd), "{id:?} {cmd} should not be claude");
        }
    }

    #[test]
    fn hook_json_targets_both_plan_events() {
        let v: serde_json::Value = serde_json::from_str(&hook_settings_json(&cli())).unwrap();
        for event in ["PreToolUse", "PostToolUse"] {
            let h = &v["hooks"][event][0];
            assert_eq!(h["matcher"], "ExitPlanMode");
            assert_eq!(h["hooks"][0]["type"], "command");
            assert_eq!(h["hooks"][0]["timeout"], 5);
            let cmd = h["hooks"][0]["command"].as_str().unwrap();
            assert!(cmd.ends_with(" capture-plan"), "{cmd}");
            assert!(cmd.starts_with('\''), "cli path must be quoted: {cmd}");
        }
    }

    #[test]
    fn injects_for_claude_only() {
        assert!(plan_capture_args(Some("claude"), "claude", &[], &cli()).is_some());
        assert!(plan_capture_args(Some("codex"), "codex", &[], &cli()).is_none());
    }

    #[test]
    fn yields_to_a_user_supplied_settings_flag() {
        for user in [
            vec!["--settings".to_string(), "/x.json".to_string()],
            vec!["--settings=/x.json".to_string()],
        ] {
            assert!(
                plan_capture_args(Some("claude"), "claude", &user, &cli()).is_none(),
                "{user:?}"
            );
        }
    }

    #[test]
    fn prepends_ahead_of_the_resume_block() {
        // Mirrors what spawnArgsForCli composes for a resumed primary tab.
        let mut argv: Vec<String> = ["--resume", "u", "--name", "n"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let extra = plan_capture_args(Some("claude"), "claude", &argv, &cli()).unwrap();
        argv.splice(0..0, extra);
        assert_eq!(argv[0], "--settings");
        assert_eq!(&argv[2..], ["--resume", "u", "--name", "n"]);
    }
}
