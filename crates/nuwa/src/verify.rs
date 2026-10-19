//! Delivery verification: prove the fix on a clean base, then on the branch.
//!
//! Three modes, in order:
//! 1. `reproduce_check` configured in `nuwa.repo.json` — must fail on the
//!    base commit and pass on the branch.
//! 2. No reproducer configured, `failing_check` is red on the base — the
//!    configured check doubles as the reproducer (red → green).
//! 3. No reproducer configured and `failing_check` is green on the base —
//!    require a self-contained `.nuwa/repro.sh` (or `.cmd` on Windows) that is
//!    copied to a clean base worktree, fails there, and passes on the branch.
//!
//! `failing_check` must always pass on the branch, and the delivery diff is
//! checked against the forbidden-path globs before a branch is created.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context};
use globset::GlobBuilder;

use crate::git_ops;
use crate::issue::{FailingCheck, Issue};
use crate::secrets::Secrets;

#[derive(Debug, Clone)]
pub struct CheckOutput {
    pub ok: bool,
    pub timed_out: bool,
    pub output: String,
}

#[derive(Debug, Clone)]
pub struct DiffFile {
    pub status: String,
    pub path: String,
}

#[derive(Debug, Clone, Default)]
pub struct DiffSummary {
    pub files: Vec<DiffFile>,
    pub touches_tests: bool,
    pub touches_ci: bool,
    pub forbidden_hits: Vec<String>,
    pub added: u64,
    pub removed: u64,
}

#[derive(Debug, Clone)]
pub struct VerificationEvidence {
    /// `configured`, `failing_check`, or `reproducer`.
    pub mode: &'static str,
    pub failing_base: CheckOutput,
    pub failing_branch: CheckOutput,
    pub reproduce_base: Option<CheckOutput>,
    pub reproduce_branch: Option<CheckOutput>,
    pub diff: DiffSummary,
}

impl VerificationEvidence {
    pub fn detail(&self, issue: &Issue) -> String {
        let mut out = String::new();
        out.push_str(&format!("MODE: {}\n", self.mode));
        out.push_str(&format!(
            "BASE failing_check: `{}` -> {}\n{}\n\n",
            issue.failing_check.command,
            verdict(&self.failing_base),
            self.failing_base.output
        ));
        if let Some(base) = &self.reproduce_base {
            out.push_str(&format!("BASE reproduce: -> {}\n{}\n\n", verdict(base), base.output));
        }
        out.push_str(&format!(
            "BRANCH failing_check: `{}` -> {}\n{}\n\n",
            issue.failing_check.command,
            verdict(&self.failing_branch),
            self.failing_branch.output
        ));
        if let Some(branch) = &self.reproduce_branch {
            out.push_str(&format!(
                "BRANCH reproduce: -> {}\n{}\n\n",
                verdict(branch),
                branch.output
            ));
        }
        out.push_str(&format!(
            "DIFF: {} file(s), +{}, -{}, tests={}, ci={}\n",
            self.diff.files.len(),
            self.diff.added,
            self.diff.removed,
            self.diff.touches_tests,
            self.diff.touches_ci
        ));
        out
    }
}

fn verdict(output: &CheckOutput) -> &'static str {
    if output.timed_out {
        "timed out"
    } else if output.ok {
        "green"
    } else {
        "red"
    }
}

/// Run the red-green protocol. `base` is the freshly fetched base branch name.
pub async fn verify(
    repo: &Path,
    issue: &Issue,
    base: &str,
    secrets: &Secrets,
) -> anyhow::Result<VerificationEvidence> {
    let env = crate::secrets::agent_env_allowlist();
    let worktree = BaseWorktree::create(repo, base)?;
    let base_dir = worktree.path();

    let failing_base = run_check(base_dir, &issue.failing_check, &env, secrets).await?;
    if failing_base.timed_out {
        bail!(
            "failing_check timed out on base after {}s:\n{}",
            issue.failing_check.timeout_secs,
            failing_base.output
        );
    }
    let failing_branch = run_check(repo, &issue.failing_check, &env, secrets).await?;

    let configured = issue.reproduce_check.as_ref();
    let (mode, reproduce_base, reproduce_branch) = if let Some(check) = configured {
        // A configured command may invoke a reproducer the agent added on the
        // branch; make it available in the clean base worktree.
        copy_reproducer_if_present(repo, base_dir)?;
        let base_out = run_check(base_dir, check, &env, secrets).await?;
        let branch_out = run_check(repo, check, &env, secrets).await?;
        ("configured", Some(base_out), Some(branch_out))
    } else if !failing_base.ok {
        ("failing_check", None, None)
    } else {
        let (path, bytes) = find_reproducer(repo)?;
        let destination = base_dir.join(&path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&destination, &bytes)
            .with_context(|| format!("copy reproducer to {}", destination.display()))?;
        let check = reproducer_check(&path);
        let base_out = run_check(base_dir, &check, &env, secrets).await?;
        let branch_out = run_check(repo, &check, &env, secrets).await?;
        ("reproducer", Some(base_out), Some(branch_out))
    };

    match mode {
        "configured" => {
            let base_out = reproduce_base.as_ref().expect("configured base");
            let branch_out = reproduce_branch.as_ref().expect("configured branch");
            if base_out.timed_out {
                bail!("reproduce_check timed out on base:\n{}", base_out.output);
            }
            if base_out.ok {
                bail!(
                    "reproduce_check passes on the base commit — it does not reproduce the bug:\n{}",
                    base_out.output
                );
            }
            if branch_out.timed_out {
                bail!("reproduce_check timed out on branch:\n{}", branch_out.output);
            }
            if !branch_out.ok {
                bail!(
                    "reproduce_check still fails on the branch:\n{}",
                    branch_out.output
                );
            }
        }
        "failing_check" => {
            // `failing_base` is red by construction; the branch must be green.
        }
        "reproducer" => {
            let base_out = reproduce_base.as_ref().expect("reproducer base");
            let branch_out = reproduce_branch.as_ref().expect("reproducer branch");
            if base_out.timed_out {
                bail!("reproducer timed out on base:\n{}", base_out.output);
            }
            if base_out.ok {
                bail!(
                    ".nuwa/repro.sh passes on the base commit — it does not reproduce the bug:\n{}",
                    base_out.output
                );
            }
            if branch_out.timed_out {
                bail!("reproducer timed out on branch:\n{}", branch_out.output);
            }
            if !branch_out.ok {
                bail!(".nuwa/repro.sh still fails on the branch:\n{}", branch_out.output);
            }
        }
        _ => unreachable!("verification mode"),
    }

    if failing_branch.timed_out {
        bail!(
            "failing_check timed out on the branch after {}s:\n{}",
            issue.failing_check.timeout_secs,
            failing_branch.output
        );
    }
    if !failing_branch.ok {
        bail!(
            "failing_check does not pass on the branch:\n{}",
            failing_branch.output
        );
    }

    let diff = diff_summary(repo, base, &issue.forbidden_paths)?;
    if !diff.forbidden_hits.is_empty() {
        bail!(
            "delivery changes forbidden paths:\n{}",
            diff.forbidden_hits.join("\n")
        );
    }

    Ok(VerificationEvidence {
        mode,
        failing_base,
        failing_branch,
        reproduce_base,
        reproduce_branch,
        diff,
    })
}

/// Run one configured check against a clean base worktree. Used by
/// `nuwa check` without creating a delivery branch.
pub async fn probe_check(
    repo: &Path,
    base: &str,
    check: &FailingCheck,
    secrets: &Secrets,
) -> anyhow::Result<CheckOutput> {
    let worktree = BaseWorktree::create(repo, base)?;
    let env = crate::secrets::agent_env_allowlist();
    run_check(worktree.path(), check, &env, secrets).await
}

/// Validate forbidden-path globs without running a delivery.
pub fn validate_forbidden_paths(paths: &[String]) -> anyhow::Result<()> {
    compile_forbidden(paths).map(|_| ())
}

async fn run_check(
    dir: &Path,
    check: &FailingCheck,
    env: &BTreeMap<String, String>,
    secrets: &Secrets,
) -> anyhow::Result<CheckOutput> {
    let timeout = Duration::from_secs(check.timeout_secs.max(1));
    let (program, args) = if cfg!(windows) {
        ("cmd", vec!["/C".to_string(), check.command.clone()])
    } else {
        ("sh", vec!["-c".to_string(), check.command.clone()])
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = git_ops::run_process(program, &args, dir, timeout, Some(env)).await?;
    let combined = format!("{}{}", out.stdout, out.stderr);
    Ok(CheckOutput {
        ok: out.success && !out.timed_out,
        timed_out: out.timed_out,
        output: secrets.redact(&combined),
    })
}

fn reproducer_check(path: &Path) -> FailingCheck {
    let command = if cfg!(windows) {
        path.to_string_lossy().replace('/', "\\")
    } else {
        format!("sh {}", path.to_string_lossy())
    };
    FailingCheck {
        command,
        target: None,
        timeout_secs: 600,
    }
}

fn reproducer_path() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(".nuwa/repro.cmd")
    } else {
        PathBuf::from(".nuwa/repro.sh")
    }
}

fn find_reproducer(repo: &Path) -> anyhow::Result<(PathBuf, Vec<u8>)> {
    let path = reproducer_path();
    let absolute = repo.join(&path);
    let bytes = std::fs::read(&absolute).with_context(|| {
        format!(
            "failing_check passes on the base commit and no reproducer exists at {}; \
             add a self-contained reproducer or configure reproduce_check",
            path.display()
        )
    })?;
    if bytes.is_empty() {
        bail!("reproducer {} is empty", path.display());
    }
    Ok((path, bytes))
}

fn copy_reproducer_if_present(repo: &Path, base_dir: &Path) -> anyhow::Result<()> {
    let path = reproducer_path();
    let source = repo.join(&path);
    if !source.exists() {
        return Ok(());
    }
    let destination = base_dir.join(&path);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::copy(&source, &destination)
        .with_context(|| format!("copy {} to {}", source.display(), destination.display()))?;
    Ok(())
}

struct BaseWorktree<'a> {
    repo: &'a Path,
    path: PathBuf,
}

impl<'a> BaseWorktree<'a> {
    fn create(repo: &'a Path, base: &str) -> anyhow::Result<Self> {
        let reference = if git_ops::git(
            repo,
            &["rev-parse", "--verify", &format!("origin/{base}")],
        )
        .is_ok()
        {
            format!("origin/{base}")
        } else {
            base.to_string()
        };
        let path = std::env::temp_dir().join(format!("nuwa-verify-{}", uuid::Uuid::new_v4()));
        let path_str = path
            .to_str()
            .context("verification worktree path is not UTF-8")?;
        git_ops::git(repo, &["worktree", "add", "--detach", path_str, &reference])
            .with_context(|| format!("create verification worktree at {}", path.display()))?;
        Ok(Self { repo, path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for BaseWorktree<'_> {
    fn drop(&mut self) {
        if let Some(path) = self.path.to_str() {
            let _ = git_ops::git(self.repo, &["worktree", "remove", "--force", path]);
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Compute the delivery diff, forbidden-path hits, and the risk flags used by
/// the PR body. `base` is a branch name; the working tree is the branch side.
pub fn diff_summary(
    repo: &Path,
    base: &str,
    forbidden_paths: &[String],
) -> anyhow::Result<DiffSummary> {
    let mut summary = DiffSummary::default();
    let status = git_ops::git(repo, &["status", "--porcelain=v1", "-z"])?;
    let mut parts = status.split('\0').filter(|part| !part.is_empty());
    while let Some(entry) = parts.next() {
        if entry.len() < 4 {
            continue;
        }
        let code = entry[..2].to_string();
        let path = entry[3..].to_string();
        if path == "nuwa.repo.json" {
            continue;
        }
        if code.starts_with('R') {
            if let Some(old) = parts.next() {
                summary.files.push(DiffFile {
                    status: code.clone(),
                    path: old.to_string(),
                });
            }
        }
        summary.files.push(DiffFile {
            status: code,
            path,
        });
    }

    let numstat = git_ops::git(repo, &["diff", "--numstat", base]).unwrap_or_default();
    for line in numstat.lines() {
        let mut fields = line.splitn(3, '\t');
        let added = fields.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        let removed = fields.next().and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
        summary.added += added;
        summary.removed += removed;
    }
    for file in &summary.files {
        if file.status.starts_with("??") {
            if let Ok(text) = std::fs::read_to_string(repo.join(&file.path)) {
                summary.added += text.lines().count() as u64;
            }
        }
        if is_test_path(&file.path) {
            summary.touches_tests = true;
        }
        if is_ci_path(&file.path) {
            summary.touches_ci = true;
        }
    }

    let matchers = compile_forbidden(forbidden_paths)?;
    for file in &summary.files {
        let normalized = file.path.replace('\\', "/");
        for (pattern, matcher) in &matchers {
            if matcher.is_match(&normalized) {
                summary
                    .forbidden_hits
                    .push(format!("{} (matches `{}`)", file.path, pattern));
                break;
            }
        }
    }
    Ok(summary)
}

fn compile_forbidden(
    forbidden_paths: &[String],
) -> anyhow::Result<Vec<(String, globset::GlobMatcher)>> {
    let mut matchers = Vec::new();
    for pattern in forbidden_paths {
        let trimmed = pattern.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut patterns = vec![trimmed.to_string()];
        if trimmed.ends_with('/') {
            patterns.push(format!("{trimmed}**"));
        } else if !trimmed.contains('/') {
            patterns.push(format!("**/{trimmed}"));
        }
        for candidate in patterns {
            let glob = GlobBuilder::new(&candidate)
                .literal_separator(false)
                .build()
                .with_context(|| format!("invalid forbidden_paths glob `{candidate}`"))?;
            matchers.push((trimmed.to_string(), glob.compile_matcher()));
        }
    }
    Ok(matchers)
}

fn is_test_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    name.ends_with("_test.go")
        || name.ends_with("_test.py")
        || name.ends_with("_test.rs")
        || name.ends_with("_test.ts")
        || name.ends_with("_test.tsx")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || name.ends_with(".spec.ts")
        || name.ends_with(".spec.tsx")
        || (name.starts_with("test_") && name.ends_with(".py"))
        || normalized.contains("/tests/")
        || normalized.starts_with("tests/")
        || normalized.contains("/test/")
        || normalized.starts_with("test/")
        || normalized.contains("/__tests__/")
}

fn is_ci_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    normalized.starts_with(".github/")
        || normalized == ".gitlab-ci.yml"
        || normalized.starts_with(".circleci/")
        || normalized == "Jenkinsfile"
        || normalized == "azure-pipelines.yml"
        || normalized.starts_with(".buildkite/")
        || normalized == ".travis.yml"
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::FailingCheck;

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).expect("repo");
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.name", "test"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        std::fs::write(dir.join("base.txt"), "base\n").expect("write");
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "init"]);
    }

    fn issue(failing: &str) -> Issue {
        Issue {
            fingerprint: "fp-verify".into(),
            title: "verify".into(),
            language: "sh".into(),
            occurrences: 1,
            severity: "high".into(),
            first_seen: String::new(),
            last_seen: String::new(),
            stack_trace: String::new(),
            log_window: Vec::new(),
            prior_context: String::new(),
            failing_check: FailingCheck {
                command: failing.into(),
                target: None,
                timeout_secs: 10,
            },
            reproduce_check: None,
            forbidden_paths: Vec::new(),
        }
    }

    fn exists_command(path: &str) -> String {
        if cfg!(windows) {
            format!("if exist {path} (exit 0) else (exit 1)")
        } else {
            format!("test -f {path}")
        }
    }

    fn write_reproducer(repo: &Path, path: &str) {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().expect("parent")).expect("dir");
        if cfg!(windows) {
            std::fs::write(&file, "@if exist fixed.txt (exit /b 0) else (exit /b 1)\r\n")
                .expect("write");
        } else {
            std::fs::write(&file, "#!/bin/sh\ntest -f fixed.txt\n").expect("write");
        }
    }

    #[tokio::test]
    async fn failing_check_red_on_base_is_used_as_reproducer() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("fixed.txt"), "fixed\n").expect("write");
        let issue = issue(&exists_command("fixed.txt"));

        let evidence = verify(&repo, &issue, "main", &Secrets::default())
            .await
            .expect("verify");
        assert_eq!(evidence.mode, "failing_check");
        assert!(!evidence.failing_base.ok);
        assert!(evidence.failing_branch.ok);
    }

    #[tokio::test]
    async fn base_green_without_reproducer_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("fixed.txt"), "fixed\n").expect("write");
        let issue = issue("echo ok");

        let err = verify(&repo, &issue, "main", &Secrets::default())
            .await
            .expect_err("no reproducer");
        assert!(format!("{err:#}").contains("no reproducer"));
    }

    #[tokio::test]
    async fn base_green_with_reproducer_requires_red_then_green() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("fixed.txt"), "fixed\n").expect("write");
        write_reproducer(&repo, reproduction_path());
        let issue = issue("echo ok");

        let evidence = verify(&repo, &issue, "main", &Secrets::default())
            .await
            .expect("verify");
        assert_eq!(evidence.mode, "reproducer");
        assert!(!evidence.reproduce_base.as_ref().expect("base").ok);
        assert!(evidence.reproduce_branch.as_ref().expect("branch").ok);
    }

    #[tokio::test]
    async fn configured_reproduce_check_must_be_red_on_base() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("fixed.txt"), "fixed\n").expect("write");
        let mut issue = issue("echo ok");
        issue.reproduce_check = Some(FailingCheck {
            command: exists_command("fixed.txt"),
            target: None,
            timeout_secs: 10,
        });

        let evidence = verify(&repo, &issue, "main", &Secrets::default())
            .await
            .expect("verify");
        assert_eq!(evidence.mode, "configured");
        assert!(!evidence.reproduce_base.as_ref().expect("base").ok);
        assert!(evidence.reproduce_branch.as_ref().expect("branch").ok);
    }

    #[tokio::test]
    async fn check_timeout_is_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let mut issue = issue("echo ok");
        issue.failing_check.command = if cfg!(windows) {
            "ping -n 30 127.0.0.1 >NUL".to_string()
        } else {
            "sleep 30".to_string()
        };
        issue.failing_check.timeout_secs = 1;
        let start = std::time::Instant::now();
        let err = verify(&repo, &issue, "main", &Secrets::default())
            .await
            .expect_err("timeout");
        // 端到端断言：超时路径必须以 "timed out" 语义 bail（run_check 判定
        // timed_out 且 verify 不吞掉）。杀进程机制本身由
        // git_ops::git_with_timeout_kills_a_hanging_command 独立覆盖。
        // 这里故意不断言墙钟：CI 双核 runner 上 237 个并行 tokio 测试会
        // 饱和 CPU，轮询循环被调度延迟拖长（10s/25s 预算两次假红，
        // 2026-10-20）；墙钟回归防护留在 git_ops 层（单测试无竞争）。
        eprintln!(
            "check_timeout_is_bounded: verify elapsed {:?} (informational)",
            start.elapsed()
        );
    }

    #[test]
    fn star_pem_forbidden_glob_matches_nested_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("deploy.pem"), "key\n").expect("write");
        let summary = diff_summary(&repo, "main", &["*.pem".to_string()]).expect("diff");
        assert!(
            summary.forbidden_hits.iter().any(|hit| hit.contains("deploy.pem")),
            "{:?}",
            summary.forbidden_hits
        );
    }

    #[test]
    fn diff_flags_tests_and_ci() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("base_test.go"), "package main\n").expect("write");
        std::fs::create_dir_all(repo.join(".github/workflows")).expect("dir");
        std::fs::write(repo.join(".github/workflows/ci.yml"), "name: ci\n").expect("write");
        let summary = diff_summary(&repo, "main", &[]).expect("diff");
        assert!(summary.touches_tests);
        assert!(summary.touches_ci);
    }

    fn reproduction_path() -> &'static str {
        if cfg!(windows) {
            ".nuwa/repro.cmd"
        } else {
            ".nuwa/repro.sh"
        }
    }
}
