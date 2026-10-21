//! 禁改路径 + git/PR 命令护栏 — the day-one guardrails (ADR-003 五件套).
//!
//! Enforcement point is the harness `before_tool_call` hook: a vetoed call
//! never executes, and the model sees the reason as an error tool result, so
//! it can route around the block instead of derailing.

use std::sync::Arc;

use futures::future::BoxFuture;
use rpi_agent::hooks::BeforeToolCall;
use rpi_agent::types::{BeforeToolCallContext, BeforeToolCallResult};
use tokio_util::sync::CancellationToken;

/// Git operations reserved for Nuwa's delivery step (`deliver.rs`); the agent
/// must never move branches, rewrite history, or touch PRs itself.
const DENIED_COMMANDS: &[&str] = &[
    "git push",
    "git checkout",
    "git switch",
    "git reset",
    "git revert",
    "git clean",
    "git branch",
    "git tag",
    "gh pr",
    "gh repo",
    "gh release",
    "gh auth",
];

/// Tools whose `path` argument mutates the tree.
const PATH_MUTATING_TOOLS: &[&str] = &["edit", "write"];

/// Tools that run shell commands.
const SHELL_TOOLS: &[&str] = &["bash", "powershell"];

#[derive(Debug, Clone)]
pub struct Guards {
    forbidden: Vec<String>,
}

impl Guards {
    pub fn new(mut forbidden: Vec<String>) -> Self {
        // The instruction file is runner configuration. It is never a valid
        // agent edit target, even when the repo owner forgot to list it.
        if !forbidden.iter().any(|path| path == "nuwa.repo.json") {
            forbidden.push("nuwa.repo.json".to_string());
        }
        Self { forbidden }
    }

    /// `Err(reason)` = block the call; the reason is surfaced to the model.
    pub fn check(&self, tool: &str, args: &serde_json::Value) -> Result<(), String> {
        if PATH_MUTATING_TOOLS.contains(&tool) {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                self.check_path(path)?;
            }
        }
        if SHELL_TOOLS.contains(&tool) {
            if let Some(cmd) = args.get("command").and_then(|v| v.as_str()) {
                for denied in DENIED_COMMANDS {
                    if cmd.contains(denied) {
                        return Err(format!(
                            "guard: `{denied}` is reserved for Nuwa's delivery step — the agent must not run it"
                        ));
                    }
                }
                // A literal `git checkout` substring misses the same subcommand
                // behind a global option (`git -C . checkout -b probe`,
                // `git -c x=y reset`), which still moves branches / rewrites
                // history. Scan tokens so the reserved-subcommand contract holds
                // regardless of global flags.
                if let Some(sub) = find_denied_git_subcommand(cmd) {
                    return Err(format!(
                        "guard: `git {sub}` is reserved for Nuwa's delivery step — the agent must not run it"
                    ));
                }
                for f in &self.forbidden {
                    if cmd.contains(f.as_str()) {
                        return Err(format!("guard: command references forbidden path `{f}`"));
                    }
                }
            }
        }
        Ok(())
    }

    fn check_path(&self, path: &str) -> Result<(), String> {
        // Judge the path the tool will actually open, not the raw argument:
        // rpi-tools strips a leading `@` and expands `~` to the home dir before
        // resolving. Without mirroring that, `@/tmp/x` and `~/x` slip past the
        // absolute-path check and escape the repository.
        let normalized = normalize_guard_path(path);
        if normalized.starts_with('~') {
            return Err(
                "guard: `~` (home-relative) paths are not allowed — edit inside the repository"
                    .to_string(),
            );
        }
        let windows_drive = normalized.len() >= 2 && normalized.as_bytes()[1] == b':';
        if normalized.starts_with('/') || normalized.starts_with('\\') || windows_drive {
            return Err(
                "guard: absolute paths are not allowed — edit inside the repository".to_string(),
            );
        }
        let traverses_up = std::path::Path::new(&normalized)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
        if traverses_up {
            return Err("guard: path traversal (`..`) is not allowed".to_string());
        }
        // Forbidden globs are written with `/`; a Windows-style `\` separator
        // must not slip through (`.github\ci.yml` vs forbidden `.github/`).
        let slashed = normalized.replace('\\', "/");
        for f in &self.forbidden {
            if slashed.contains(f.as_str()) {
                return Err(format!(
                    "guard: `{path}` hits the forbidden-path list (`{f}`)"
                ));
            }
        }
        Ok(())
    }
}

/// Mirror rpi-tools' `normalize_tool_path`: fold unicode spaces to ASCII and
/// strip a single leading `@`, so the guard sees the same path the tool opens.
fn normalize_guard_path(path: &str) -> String {
    let trimmed: String = path
        .chars()
        .map(|c| if c.is_whitespace() && c != ' ' { ' ' } else { c })
        .collect();
    let trimmed = trimmed.trim_start();
    trimmed.strip_prefix('@').unwrap_or(trimmed).to_string()
}

/// Git global options that consume the following token as their value.
const GIT_GLOBAL_OPTS_WITH_VALUE: &[&str] = &[
    "-C",
    "-c",
    "--git-dir",
    "--work-tree",
    "--namespace",
    "--exec-path",
];

/// Reserved git subcommands (the `git `-prefixed entries of [`DENIED_COMMANDS`]).
const DENIED_GIT_SUBCOMMANDS: &[&str] = &[
    "push", "checkout", "switch", "reset", "revert", "clean", "branch", "tag",
];

/// Find a reserved git subcommand even when global options sit between `git`
/// and the subcommand. Returns the subcommand name, or `None`.
fn find_denied_git_subcommand(cmd: &str) -> Option<&'static str> {
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    let mut i = 0;
    while i < tokens.len() {
        if tokens[i] == "git" {
            let mut j = i + 1;
            while j < tokens.len() {
                let token = tokens[j];
                if GIT_GLOBAL_OPTS_WITH_VALUE.contains(&token) {
                    j += 2; // skip the option and its value
                    continue;
                }
                if token.starts_with('-') {
                    j += 1; // a valueless global flag
                    continue;
                }
                break;
            }
            if let Some(sub) = tokens.get(j) {
                if let Some(matched) = DENIED_GIT_SUBCOMMANDS
                    .iter()
                    .find(|denied| *denied == sub)
                {
                    return Some(matched);
                }
            }
        }
        i += 1;
    }
    None
}

/// Build the harness hook. `Some(blocked)` prevents execution; the model gets
/// `reason` back as the tool result.
pub fn before_tool_hook(guards: Arc<Guards>) -> BeforeToolCall {
    Arc::new(
        move |ctx: BeforeToolCallContext<'_>, _signal: CancellationToken| {
            let verdict = guards.check(&ctx.tool_call.name, ctx.args).err();
            Box::pin(async move {
                verdict.map(|reason| BeforeToolCallResult {
                    block: true,
                    reason: Some(reason),
                    ..BeforeToolCallResult::default()
                })
            })
                as BoxFuture<'static, Option<BeforeToolCallResult>>
        },
    )
}

#[cfg(test)]
mod tests {
    use super::Guards;
    use serde_json::json;

    fn guards() -> Guards {
        Guards::new(vec![".github/".to_string(), "go.sum".to_string()])
    }

    #[test]
    fn edit_of_forbidden_path_is_blocked() {
        match guards().check("edit", &json!({"path": ".github/workflows/ci.yml"})) {
            Err(err) => assert!(err.contains(".github/")),
            Ok(()) => panic!("forbidden-path edit must be blocked"),
        }
    }

    #[test]
    fn write_with_absolute_path_is_blocked() {
        assert!(guards()
            .check("write", &json!({"path": "C:/Windows/evil.go"}))
            .is_err());
    }

    #[test]
    fn traversal_is_blocked() {
        assert!(guards().check("write", &json!({"path": "../outside.go"})).is_err());
    }

    #[test]
    fn ordinary_edit_is_allowed() {
        assert!(guards()
            .check("edit", &json!({"path": "calc.go", "edits": []}))
            .is_ok());
    }

    #[test]
    fn git_push_is_blocked_in_shell() {
        match guards().check("bash", &json!({"command": "git push origin main"})) {
            Err(err) => assert!(err.contains("delivery step")),
            Ok(()) => panic!("git push must be blocked"),
        }
    }

    #[test]
    fn gh_pr_create_is_blocked() {
        assert!(guards()
            .check("bash", &json!({"command": "gh pr create --title x"}))
            .is_err());
    }

    #[test]
    fn forbidden_path_in_command_is_blocked() {
        assert!(guards().check("bash", &json!({"command": "cat go.sum"})).is_err());
    }

    #[test]
    fn routine_test_command_is_allowed() {
        assert!(guards().check("bash", &json!({"command": "go test ./..."})).is_ok());
    }

    #[test]
    fn read_only_tools_pass_through() {
        assert!(guards().check("read", &json!({"path": "go.sum"})).is_ok());
        assert!(guards().check("grep", &json!({})).is_ok());
    }

    #[test]
    fn instruction_file_is_implicitly_forbidden() {
        for tool in ["edit", "write"] {
            assert!(
                guards()
                    .check(tool, &json!({"path": "nuwa.repo.json"}))
                    .is_err(),
                "{tool} must not touch the instruction file"
            );
        }
        assert!(guards()
            .check("bash", &json!({"command": "printf x > nuwa.repo.json"}))
            .is_err());
    }

    #[test]
    fn at_prefixed_and_home_relative_paths_are_blocked() {
        // rpi-tools strips a leading `@` and expands `~`; the guard must judge
        // the resolved target, so both escape attempts are rejected.
        assert!(guards()
            .check("write", &json!({"path": "@/tmp/escape.go"}))
            .is_err());
        assert!(guards()
            .check("write", &json!({"path": "~/escape.go"}))
            .is_err());
    }

    #[test]
    fn windows_separator_does_not_slip_past_forbidden_prefix() {
        assert!(guards()
            .check("write", &json!({"path": r".github\workflows\ci.yml"}))
            .is_err());
    }

    #[test]
    fn git_subcommand_behind_global_options_is_blocked() {
        for cmd in [
            "git -C . checkout -b probe",
            "git -c user.email=a@b reset --hard HEAD~1",
            "git --no-pager push origin main",
        ] {
            match guards().check("bash", &json!({"command": cmd})) {
                Err(err) => assert!(err.contains("delivery step"), "{cmd}: {err}"),
                Ok(()) => panic!("`{cmd}` must be blocked"),
            }
        }
    }

    #[test]
    fn ordinary_git_read_commands_are_allowed() {
        for cmd in ["git status", "git --no-pager log -1", "git diff"] {
            assert!(
                guards().check("bash", &json!({"command": cmd})).is_ok(),
                "`{cmd}` is read-only and must pass"
            );
        }
    }
}
