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
        let windows_drive = path.len() >= 2 && path.as_bytes()[1] == b':';
        if path.starts_with('/') || path.starts_with('\\') || windows_drive {
            return Err(
                "guard: absolute paths are not allowed — edit inside the repository".to_string(),
            );
        }
        let traverses_up = std::path::Path::new(path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
        if traverses_up {
            return Err("guard: path traversal (`..`) is not allowed".to_string());
        }
        for f in &self.forbidden {
            if path.contains(f.as_str()) {
                return Err(format!(
                    "guard: `{path}` hits the forbidden-path list (`{f}`)"
                ));
            }
        }
        Ok(())
    }
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
}
