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

/// Git subcommands reserved for Nuwa's delivery step (`deliver.rs`): the agent
/// must never move branches, rewrite history, commit, or re-point the remote.
const DENIED_GIT_SUBCOMMANDS: &[&str] = &[
    "push",
    "checkout",
    "switch",
    "reset",
    "revert",
    "clean",
    "branch",
    "tag",
    "commit",
    "config",
    "remote",
    "worktree",
    "stash",
    "rebase",
    "cherry-pick",
    "merge",
    "pull",
    "clone",
    "am",
    "apply",
    "update-ref",
];

/// `gh` subcommands reserved for the delivery step.
const DENIED_GH_SUBCOMMANDS: &[&str] = &["pr", "repo", "release", "auth", "api", "workflow"];

/// Credential material the agent's shell must never read. The runner's deploy
/// key and env file live in the same HOME as the agent's shell, so "just cat
/// it" is a real exfiltration path; the delivery step is the only thing that
/// ever needs these files.
const SECRET_PATH_MARKERS: &[&str] = &[
    ".ssh/",
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    "authorized_keys",
    ".aws/credentials",
    ".git-credentials",
    ".netrc",
    "nuwa.env",
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
                // A literal `git push` substring misses the same subcommand
                // behind a global option (`git -C . push`) and behind quoting
                // games (`g""it push`); scan normalized tokens instead.
                if let Some(reserved) = find_reserved_subcommand(cmd) {
                    return Err(format!(
                        "guard: `{reserved}` is reserved for Nuwa's delivery step — the agent must not run it"
                    ));
                }
                let words = shell_words(cmd);
                for marker in SECRET_PATH_MARKERS {
                    if words
                        .iter()
                        .any(|word| word.to_lowercase().contains(marker))
                    {
                        return Err(format!(
                            "guard: reading `{marker}` is not allowed — credentials belong to the runner, not the repair"
                        ));
                    }
                }
                for f in &self.forbidden {
                    if words.iter().any(|word| path_hits(word, f)) {
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
            if path_hits(&slashed, f) {
                return Err(format!(
                    "guard: `{path}` hits the forbidden-path list (`{f}`)"
                ));
            }
        }
        Ok(())
    }
}

/// Does `path` fall under a forbidden pattern? Component-aware, so a forbidden
/// `go.sum` no longer also blocks `mygo.sum.txt`.
fn path_hits(path: &str, pattern: &str) -> bool {
    let path = path.replace('\\', "/");
    let pattern = pattern.replace('\\', "/");
    if pattern.ends_with('/') {
        let dir = pattern.trim_end_matches('/');
        path == dir || path.starts_with(&pattern) || path.contains(&format!("/{pattern}"))
    } else {
        path == pattern || path.ends_with(&format!("/{pattern}"))
    }
}

/// Split a shell command into words after removing the quote and escape
/// characters a shell would drop (`g""it` is `git`, `git\ push` is
/// `git push`). Shell separators become word boundaries.
fn shell_words(command: &str) -> Vec<String> {
    let normalized: String = command
        .chars()
        .filter(|c| !matches!(c, '"' | '\'' | '`' | '^'))
        .map(|c| if c == '\\' { '/' } else { c })
        .collect();
    normalized
        .split(|c: char| c.is_whitespace() || matches!(c, ';' | '&' | '|' | '(' | ')' | '<' | '>'))
        .map(|word| word.trim().to_string())
        .filter(|word| !word.is_empty())
        .collect()
}

/// Mirror rpi-tools' `normalize_tool_path`: fold unicode spaces to ASCII and
/// strip a single leading `@`, so the guard sees the same path the tool opens.
fn normalize_guard_path(path: &str) -> String {
    let trimmed: String = path
        .chars()
        .map(|c| {
            if c.is_whitespace() && c != ' ' {
                ' '
            } else {
                c
            }
        })
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

/// Find a reserved `git`/`gh` subcommand even when global options sit between
/// the command name and the subcommand. Returns `"<tool> <sub>"`, or `None`.
fn find_reserved_subcommand(command: &str) -> Option<String> {
    let words = shell_words(command);
    for (index, word) in words.iter().enumerate() {
        let name = word
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .unwrap_or(word);
        let (tool, denied, opts): (&str, &[&str], &[&str]) =
            if name.eq_ignore_ascii_case("git") || name.eq_ignore_ascii_case("git.exe") {
                ("git", DENIED_GIT_SUBCOMMANDS, GIT_GLOBAL_OPTS_WITH_VALUE)
            } else if name.eq_ignore_ascii_case("gh") || name.eq_ignore_ascii_case("gh.exe") {
                ("gh", DENIED_GH_SUBCOMMANDS, &[])
            } else {
                continue;
            };
        let mut rest = words.iter().skip(index + 1);
        let mut budget = 0;
        while let Some(word) = rest.next() {
            budget += 1;
            if budget > 6 {
                break;
            }
            if opts.iter().any(|opt| opt.eq_ignore_ascii_case(word)) {
                rest.next();
                continue;
            }
            if word.starts_with('-') {
                continue;
            }
            if let Some(matched) = denied.iter().find(|d| d.eq_ignore_ascii_case(word)) {
                return Some(format!("{tool} {matched}"));
            }
            break;
        }
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
            }) as BoxFuture<'static, Option<BeforeToolCallResult>>
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
        assert!(guards()
            .check("write", &json!({"path": "../outside.go"}))
            .is_err());
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
        assert!(guards()
            .check("bash", &json!({"command": "cat go.sum"}))
            .is_err());
        assert!(guards()
            .check("bash", &json!({"command": "cat ./go.sum"}))
            .is_err());
    }

    /// 同名前缀不是禁改路径：`mygo.sum.txt` 不得误伤。
    #[test]
    fn lookalike_path_is_not_blocked() {
        assert!(guards()
            .check("bash", &json!({"command": "cat mygo.sum.txt"}))
            .is_ok());
        assert!(guards()
            .check("edit", &json!({"path": "notes/go.sum.md"}))
            .is_ok());
    }

    #[test]
    fn routine_test_command_is_allowed() {
        assert!(guards()
            .check("bash", &json!({"command": "go test ./..."}))
            .is_ok());
        // `push` as an argument is not the subcommand.
        assert!(guards()
            .check("bash", &json!({"command": "git log --grep push"}))
            .is_ok());
    }

    /// 子串黑名单绕过的回归：多余空格、`-C` 夹带、引号拼接、脚本外置。
    #[test]
    fn git_write_commands_cannot_be_smuggled_past_the_guard() {
        for command in [
            "git  push origin main",
            "git -C . push origin main",
            "git -c user.name=x commit -m y",
            "g\"\"it push",
            "sh -c 'git pu\"\"sh'",
            "git config remote.origin.url https://evil.test/x",
            "git remote set-url origin https://evil.test/x",
            "git stash",
            "git rebase -i HEAD~2",
        ] {
            assert!(
                guards()
                    .check("bash", &json!({"command": command}))
                    .is_err(),
                "{command} must be blocked"
            );
        }
    }

    /// runner 的部署密钥与 env 文件与 agent 的 shell 共享 HOME：读它们
    /// 就等于把凭证交给模型（再随转录/PR 正文外泄）。
    #[test]
    fn credential_files_cannot_be_read_from_the_shell() {
        for command in [
            "cat ~/.ssh/id_ed25519",
            "cp /home/nuwa/.ssh/id_rsa /tmp/k",
            "cat $HOME/.aws/credentials",
            "cat /opt/nuwa/nuwa.env",
        ] {
            assert!(
                guards()
                    .check("bash", &json!({"command": command}))
                    .is_err(),
                "{command} must be blocked"
            );
        }
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
