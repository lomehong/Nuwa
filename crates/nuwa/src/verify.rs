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

mod trusted;
pub use trusted::{prepare, TrustProvenance, VerificationContext};
pub(crate) use trusted::{prepare_workspace, seed_owner_config};

#[derive(Debug, Clone)]
pub struct CheckOutput {
    pub ok: bool,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub output: String,
}

/// A language-level hygiene check (e.g. `go vet`) that runs alongside the
/// configured red/green check. A red baseline is skipped: the default branch
/// was already broken, so the branch cannot be blamed for it.
#[derive(Debug, Clone)]
pub struct DeepCheckResult {
    pub command: String,
    pub base: CheckOutput,
    /// `None` when the baseline was not green and the check was skipped.
    pub branch: Option<CheckOutput>,
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
    pub provenance: TrustProvenance,
    /// `owner_trusted`, or legacy `configured` / `failing_check` / `reproducer`.
    pub mode: &'static str,
    pub failing_base: CheckOutput,
    pub failing_branch: CheckOutput,
    pub reproduce_base: Option<CheckOutput>,
    pub reproduce_branch: Option<CheckOutput>,
    /// Deep checks that must stay green on the branch when they are green on
    /// the base (currently Go: `go vet` + `go build`).
    pub deep_checks: Vec<DeepCheckResult>,
    pub diff: DiffSummary,
}

impl VerificationEvidence {
    pub fn detail(&self, issue: &Issue) -> String {
        let mut out = String::new();
        out.push_str(&format!("MODE: {}\n", self.mode));
        out.push_str(&format!(
            "TRUST: {}; base={}; candidate_tree={}; owner_config={}; manifest={}; source={}; owner_exits={:?}/{:?}\n",
            self.provenance.trust_level,
            self.provenance.base_sha,
            self.provenance.candidate_tree,
            self.provenance.owner_config_sha256.as_deref().unwrap_or("none"),
            self.provenance.manifest_sha256.as_deref().unwrap_or("none"),
            self.provenance.owner_source.unwrap_or("none"),
            self.provenance.oracle_base_exit,
            self.provenance.oracle_candidate_exit,
        ));
        out.push_str(&format!(
            "BASE failing_check: `{}` -> {}\n{}\n\n",
            issue.failing_check.command,
            verdict(&self.failing_base),
            self.failing_base.output
        ));
        if let Some(base) = &self.reproduce_base {
            out.push_str(&format!(
                "BASE reproduce: -> {}\n{}\n\n",
                verdict(base),
                base.output
            ));
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
        for deep in &self.deep_checks {
            match &deep.branch {
                Some(branch) => out.push_str(&format!(
                    "DEEP CHECK `{}`: base {} -> branch {}\n",
                    deep.command,
                    verdict(&deep.base),
                    verdict(branch)
                )),
                None => out.push_str(&format!(
                    "DEEP CHECK `{}`: base {} — skipped\n",
                    deep.command,
                    verdict(&deep.base)
                )),
            }
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

/// Run the red-green protocol. `base` is a base ref or a pinned fetched SHA.
pub async fn verify(
    repo: &Path,
    issue: &Issue,
    base: &str,
    secrets: &Secrets,
    context: &VerificationContext,
) -> anyhow::Result<VerificationEvidence> {
    context.invalidate_verification()?;
    context.assert_delivery_allowed()?;
    context.assert_integrity(repo)?;
    context.assert_candidate_sources(repo)?;
    let base_sha = trusted::resolve_base(repo, base)?;
    let tree = candidate_tree_for_context(repo, context)?;
    let mut evidence = if context.is_trusted() {
        context
            .verify_candidate(repo, issue, &base_sha, &tree, secrets)
            .await?
    } else {
        verify_legacy(
            repo,
            issue,
            &base_sha,
            secrets,
            context.provenance(&base_sha, &tree),
        )
        .await?
    };
    context.assert_integrity(repo)?;
    context.assert_candidate_sources(repo)?;
    if let Ok(tree_now) = candidate_tree_for_context(repo, context) {
        if tree_now != tree {
            // Diagnosis aid: name exactly which paths drifted while the
            // verification command ran, with a content digest per side, so
            // direction (lost fix vs. environment artifact) is observable.
            let describe =
                |index: &CandidateIndex<'_>, which: &str, at: &str, path: &str| -> String {
                    index
                        .git(&["cat-file", "blob", &format!("{at}:{path}")])
                        .map(|bytes| {
                            let text = String::from_utf8_lossy(&bytes);
                            format!("{which}=[{}] ({}B)", text.escape_debug(), bytes.len())
                        })
                        .unwrap_or_else(|_| format!("{which}=absent [{at}]"))
                };
            let index = CandidateIndex::create(repo, &context.excluded_delivery_paths())?;
            let mut report = String::new();
            let raw = index.git(&[
                "diff-tree",
                "-r",
                "--no-commit-id",
                "--name-status",
                "--no-renames",
                &tree,
                &tree_now,
            ])?;
            for path in String::from_utf8_lossy(&raw)
                .lines()
                .filter(|line| !line.is_empty())
                // "M\tapp.py" — status letter and path are tab-separated.
                .map(|line| line.split_once('\t').map(|(_, path)| path).unwrap_or(line))
                .filter(|path| !path.is_empty())
            {
                // Also show what the working tree itself holds for the drifted
                // path: if it matches the snapshot side, only `git add -A`
                // misbehaved; if it matches `now` (or differs from both), the
                // working tree was rewritten while the verification ran.
                let wt = std::fs::read(repo.join(path))
                    .map(|bytes| {
                        let text = String::from_utf8_lossy(&bytes);
                        format!("[{} {}B]", text.escape_debug(), bytes.len())
                    })
                    .unwrap_or_else(|_| "[absent]".into());
                report.push_str(&format!(
                    " [{} vs {}; worktree={wt}]",
                    describe(&index, "snapshot", &tree, path),
                    describe(&index, "now", &tree_now, path),
                ));
            }
            bail!("candidate tree changed during verification: {report}");
        }
    }
    evidence.provenance = context.provenance(&base_sha, &tree);
    context.record_verified(&base_sha, &tree)?;
    Ok(evidence)
}

async fn verify_legacy(
    repo: &Path,
    issue: &Issue,
    base: &str,
    secrets: &Secrets,
    provenance: TrustProvenance,
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
    trusted::require_valid_check(&failing_base, "base failing_check")?;
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
        trusted::write_safe_file(base_dir, path.to_str().context("reproducer path")?, &bytes)?;
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
            trusted::require_valid_check(base_out, "base reproduce_check")?;
            if base_out.ok {
                bail!(
                    "reproduce_check passes on the base commit — it does not reproduce the bug:\n{}",
                    base_out.output
                );
            }
            if branch_out.timed_out {
                bail!(
                    "reproduce_check timed out on branch:\n{}",
                    branch_out.output
                );
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
            trusted::require_valid_check(base_out, "base reproducer")?;
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
                bail!(
                    ".nuwa/repro.sh still fails on the branch:\n{}",
                    branch_out.output
                );
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

    let deep_checks = run_deep_checks(issue, base_dir, repo, &env, secrets).await?;

    let diff = diff_summary(repo, base, &issue.forbidden_paths)?;
    if !diff.forbidden_hits.is_empty() {
        bail!(
            "delivery changes forbidden paths:\n{}",
            diff.forbidden_hits.join("\n")
        );
    }

    Ok(VerificationEvidence {
        provenance,
        mode,
        failing_base,
        failing_branch,
        reproduce_base,
        reproduce_branch,
        deep_checks,
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

/// Run a configured check in the runner's own checkout. The work preflight
/// leaves that checkout at the base commit, so this is the cheap way to learn
/// the verification mode before the agent starts — no base worktree, no extra
/// git traffic. The delivery gate still re-runs everything independently.
pub async fn probe_working_tree(
    repo: &Path,
    check: &FailingCheck,
    secrets: &Secrets,
) -> anyhow::Result<CheckOutput> {
    let env = crate::secrets::agent_env_allowlist();
    run_check(repo, check, &env, secrets).await
}

/// Validate forbidden-path globs without running a delivery.
pub fn validate_forbidden_paths(paths: &[String]) -> anyhow::Result<()> {
    compile_forbidden(paths).map(|_| ())
}

/// Run one check against the current working tree (benchmark scoring).
pub(crate) async fn check_working_tree(
    repo: &Path,
    check: &FailingCheck,
    secrets: &Secrets,
) -> anyhow::Result<CheckOutput> {
    let env = crate::secrets::agent_env_allowlist();
    run_check(repo, check, &env, secrets).await
}

/// Score a base-green benchmark scenario with the same reproducer command the
/// delivery gate runs, instead of the configured check that was already green.
pub(crate) async fn check_reproducer_working_tree(
    repo: &Path,
    secrets: &Secrets,
) -> anyhow::Result<CheckOutput> {
    let (path, _) = find_reproducer(repo)?;
    let check = reproducer_check(&path);
    let env = crate::secrets::agent_env_allowlist();
    run_check(repo, &check, &env, secrets).await
}

/// Go repos get a vet/build triangle alongside the configured test check.
/// Repos without a root `go.mod` (or other languages) are left alone: the
/// runner cannot derive a meaningful command for them.
fn deep_check_specs(issue: &Issue, base_dir: &Path) -> Vec<String> {
    if !issue.language.eq_ignore_ascii_case("go") || !base_dir.join("go.mod").is_file() {
        return Vec::new();
    }
    vec!["go vet ./...".to_string(), "go build ./...".to_string()]
}

async fn run_deep_checks(
    issue: &Issue,
    base_dir: &Path,
    repo: &Path,
    env: &BTreeMap<String, String>,
    secrets: &Secrets,
) -> anyhow::Result<Vec<DeepCheckResult>> {
    let specs = deep_check_specs(issue, base_dir);
    run_deep_checks_with_specs(
        &specs,
        base_dir,
        repo,
        issue.failing_check.timeout_secs.max(1),
        env,
        secrets,
    )
    .await
}

async fn run_deep_checks_with_specs(
    commands: &[String],
    base_dir: &Path,
    repo: &Path,
    timeout_secs: u64,
    env: &BTreeMap<String, String>,
    secrets: &Secrets,
) -> anyhow::Result<Vec<DeepCheckResult>> {
    let mut results = Vec::new();
    for command in commands {
        let check = FailingCheck {
            command: command.clone(),
            target: None,
            timeout_secs,
        };
        // A command that cannot run at all (toolchain absent) is a skipped
        // baseline, not a delivery blocker: the configured failing_check still
        // has to pass on the branch.
        let base = match run_deep_check(base_dir, &check, env, secrets).await {
            Ok(output) => output,
            Err(error) => CheckOutput {
                ok: false,
                exit_code: None,
                timed_out: false,
                output: format!("unavailable: {error:#}"),
            },
        };
        let branch = if base.ok && !base.timed_out {
            let output = run_deep_check(repo, &check, env, secrets).await?;
            if output.timed_out {
                bail!(
                    "deep check `{command}` timed out on the branch after {timeout_secs}s:\n{}",
                    output.output
                );
            }
            if !output.ok {
                bail!(
                    "deep check `{command}` fails on the branch:\n{}",
                    output.output
                );
            }
            Some(output)
        } else {
            None
        };
        results.push(DeepCheckResult {
            command: command.clone(),
            base,
            branch,
        });
    }
    Ok(results)
}

async fn run_deep_check(
    dir: &Path,
    check: &FailingCheck,
    env: &BTreeMap<String, String>,
    secrets: &Secrets,
) -> anyhow::Result<CheckOutput> {
    if check.command != "go build ./..." {
        return run_check(dir, check, env, secrets).await;
    }
    // A main package otherwise writes its executable into the checkout. Keep
    // verifier-created artifacts outside the candidate tree rather than
    // weakening the tree-integrity assertion or ignoring arbitrary new files.
    let output = tempfile::tempdir().context("create Go deep-check output directory")?;
    let destination = output.path().to_str().context("Go output path encoding")?;
    let out = git_ops::run_process(
        "go",
        &["build", "-o", destination, "./..."],
        dir,
        Duration::from_secs(check.timeout_secs.max(1)),
        Some(env),
    )
    .await?;
    Ok(check_output(out, secrets))
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
    Ok(check_output(out, secrets))
}

fn check_output(out: git_ops::ProcessOutput, secrets: &Secrets) -> CheckOutput {
    let combined = format!("{}{}", out.stdout, out.stderr);
    CheckOutput {
        ok: out.success && !out.timed_out,
        exit_code: out.exit_code,
        timed_out: out.timed_out,
        output: secrets.redact(&combined).chars().take(4096).collect(),
    }
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
    let absolute = trusted::safe_file(repo, path.to_str().context("reproducer path")?)
        .context("no reproducer exists at a safe regular-file path")?;
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
    let relative = path.to_str().context("reproducer path")?;
    let source = trusted::safe_destination(repo, relative)?;
    if std::fs::symlink_metadata(&source).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound) {
        return Ok(());
    }
    let source = trusted::safe_file(repo, relative)?;
    let bytes = std::fs::read(source).context("read legacy reproducer")?;
    trusted::write_safe_file(base_dir, relative, &bytes)?;
    Ok(())
}

struct BaseWorktree<'a> {
    repo: &'a Path,
    path: PathBuf,
}

impl<'a> BaseWorktree<'a> {
    fn create(repo: &'a Path, base: &str) -> anyhow::Result<Self> {
        let reference =
            if git_ops::git(repo, &["rev-parse", "--verify", &format!("origin/{base}")]).is_ok() {
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

/// Compute the complete base-to-working-tree delivery diff. `base` may be a
/// branch or a pinned SHA. A private index models exactly what delivery stages,
/// without touching the caller's index, and includes committed changes too.
pub fn diff_summary(
    repo: &Path,
    base: &str,
    forbidden_paths: &[String],
) -> anyhow::Result<DiffSummary> {
    diff_summary_with_exclusions(repo, base, forbidden_paths, &[])
}

fn diff_summary_with_exclusions(
    repo: &Path,
    base: &str,
    forbidden_paths: &[String],
    excluded: &[String],
) -> anyhow::Result<DiffSummary> {
    let mut summary = DiffSummary::default();
    let index = CandidateIndex::create(repo, excluded)?;
    let git = |args: &[&str]| index.git(args);
    // One final tree avoids double-counting staged/unstaged changes or a staged
    // deletion followed by an untracked recreation of the same path.
    let pathspecs = delivery_pathspecs(excluded);
    let mut args = vec![
        "diff",
        "--cached",
        "--name-status",
        "-z",
        "--find-renames",
        base,
        "--",
    ];
    args.extend(pathspecs.iter().map(String::as_str));
    let names = git(&args)?;
    // Parse NUL-delimited bytes before converting paths for display/matching:
    // tabs, newlines, leading spaces and non-UTF-8 bytes are not record syntax.
    let mut parts = names
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty());
    let mut files = BTreeMap::new();
    while let Some(code) = parts.next() {
        let status = std::str::from_utf8(code)
            .context("diff status is not ASCII")?
            .to_string();
        let path = parts.next().context("diff status missing path")?;
        let file = DiffFile {
            status,
            path: String::from_utf8_lossy(path).into_owned(),
        };
        if file.status.starts_with('R') || file.status.starts_with('C') {
            let destination = parts.next().context("diff rename missing destination")?;
            files.insert(
                destination.to_vec(),
                DiffFile {
                    status: file.status.clone(),
                    path: String::from_utf8_lossy(destination).into_owned(),
                },
            );
        }
        // Both ends of a rename must pass forbidden-path and test/CI checks.
        files.insert(path.to_vec(), file);
    }
    summary.files = files.into_values().collect();

    let mut args = vec![
        "diff",
        "--cached",
        "--numstat",
        "-z",
        "--find-renames",
        base,
        "--",
    ];
    args.extend(pathspecs.iter().map(String::as_str));
    let numstat = git(&args)?;
    let mut records = numstat
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty());
    while let Some(record) = records.next() {
        let mut fields = record.splitn(3, |byte| *byte == b'\t');
        let mut count = || -> anyhow::Result<u64> {
            let field = fields.next().context("diff numstat missing count")?;
            if field == b"-" {
                return Ok(0); // Binary files have no line counts.
            }
            std::str::from_utf8(field)?
                .parse()
                .context("invalid diff line count")
        };
        summary.added += count()?;
        summary.removed += count()?;
        if fields
            .next()
            .context("diff numstat missing path")?
            .is_empty()
        {
            // Rename numstat is `added<TAB>removed<TAB>NUL>old<NUL>new<NUL>`.
            records
                .next()
                .context("diff numstat missing rename source")?;
            records
                .next()
                .context("diff numstat missing rename destination")?;
        }
    }
    for file in &summary.files {
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

/// The exact staged/unstaged/untracked delivery tree, without changing the
/// caller's index. Delivery must compare this again immediately before commit.
#[cfg_attr(not(test), allow(dead_code))]
pub fn candidate_tree(repo: &Path) -> anyhow::Result<String> {
    candidate_tree_with_exclusions(repo, &[])
}

pub(crate) fn candidate_tree_for_context(
    repo: &Path,
    context: &VerificationContext,
) -> anyhow::Result<String> {
    candidate_tree_with_exclusions(repo, &context.excluded_delivery_paths())
}

fn candidate_tree_with_exclusions(repo: &Path, excluded: &[String]) -> anyhow::Result<String> {
    let index = CandidateIndex::create(repo, excluded)?;
    Ok(std::str::from_utf8(&index.git(&["write-tree"])?)?
        .trim()
        .to_string())
}

/// Retain the config pathspec contract; private oracle paths are exact, rooted
/// literals, never globs or whole-directory exclusions.
pub(crate) fn delivery_pathspecs(excluded: &[String]) -> Vec<String> {
    let mut paths = vec![":(exclude)nuwa.repo.json".to_string()];
    paths.extend(
        excluded
            .iter()
            .filter(|path| path.as_str() != "nuwa.repo.json")
            .map(|path| format!(":(top,exclude,literal){path}")),
    );
    paths
}

pub(crate) fn delivery_add_args(repo: &Path, excluded: &[String]) -> anyhow::Result<Vec<String>> {
    let mut args = vec!["add".into(), "-A".into(), "--".into()];
    args.push(":(exclude)nuwa.repo.json".into());
    // Interpreter bytecode caches are runtime artifacts, never delivery
    // inputs. Running the verification command (e.g. `python3 checks/…`
    // importing a sibling module) can create or rewrite `__pycache__/*.pyc`
    // between the two candidate-tree snapshots; on Linux a base-checkout
    // round-trip updates the source mtime, Python recompiles the .pyc, and
    // the tree hash changes — a false "candidate tree changed during
    // verification" (CI own-2/own-3, 2026-10-30). Excluded from the snapshot
    // the same way nuwa.repo.json is; tamper protection still covers every
    // tracked/owner-seeded source file.
    args.push(":(exclude,glob)**/__pycache__/**".into());
    args.push(":(exclude,glob)**/*.pyc".into());
    // Git treats an exact negative pathspec beneath an ignored directory as an
    // explicit request to add that directory and fails. Already-ignored seeds
    // need no add exclusion; the reset below still removes any staged copy.
    for path in excluded
        .iter()
        .filter(|path| path.as_str() != "nuwa.repo.json")
    {
        let status = std::process::Command::new("git")
            .args(["check-ignore", "--no-index", "-q", "--", path])
            .current_dir(repo)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .context("check owner seed ignore status")?;
        match status.code() {
            Some(0) => {}
            Some(1) => args.push(format!(":(top,exclude,literal){path}")),
            _ => bail!("cannot determine owner seed ignore status"),
        }
    }
    Ok(args)
}

pub(crate) fn delivery_reset_args(excluded: &[String]) -> Vec<String> {
    let mut args = vec!["reset".into(), "-q".into(), "HEAD".into(), "--".into()];
    args.push("nuwa.repo.json".into());
    args.extend(
        excluded
            .iter()
            .filter(|path| path.as_str() != "nuwa.repo.json")
            .map(|path| format!(":(top,literal){path}")),
    );
    args
}

struct CandidateIndex<'a> {
    repo: &'a Path,
    _scratch: tempfile::TempDir,
    path: PathBuf,
}

impl<'a> CandidateIndex<'a> {
    fn create(repo: &'a Path, excluded: &[String]) -> anyhow::Result<Self> {
        let scratch = tempfile::tempdir().context("create candidate index directory")?;
        let path = scratch.path().join("index");
        let index = Self {
            repo,
            _scratch: scratch,
            path,
        };
        // Start from an empty index and re-hash the whole working tree. A
        // copy of the caller's index makes the snapshot dependent on cached
        // stat data: a same-size rewrite of a tracked file (e.g. the
        // candidate fix) can be skipped as "unchanged" on the second
        // snapshot, which resurrected the pre-fix blob and produced a false
        // "candidate tree changed during verification" on CI
        // (2026-10-30, runs 40984896497+). Re-hashing from scratch makes
        // every snapshot a deterministic function of the working tree.
        index.git(&["read-tree", "--empty"])?;
        let add = delivery_add_args(repo, excluded)?;
        index.git(&add.iter().map(String::as_str).collect::<Vec<_>>())?;
        // Even already staged private owner inputs are not delivery changes.
        let reset = delivery_reset_args(excluded);
        index.git(&reset.iter().map(String::as_str).collect::<Vec<_>>())?;
        Ok(index)
    }

    fn git(&self, args: &[&str]) -> anyhow::Result<Vec<u8>> {
        let out = std::process::Command::new("git")
            .args(args)
            .env("GIT_INDEX_FILE", &self.path)
            .current_dir(self.repo)
            .output()
            .context("git candidate index")?;
        if !out.status.success() {
            bail!(
                "git candidate index {} failed (status {})",
                args.first().unwrap_or(&"operation"),
                out.status
            );
        }
        Ok(out.stdout)
    }
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

    // Legacy compatibility fixtures intentionally have no owner configuration.
    async fn verify(
        repo: &Path,
        issue: &Issue,
        base: &str,
        secrets: &Secrets,
    ) -> anyhow::Result<VerificationEvidence> {
        let context = prepare(repo, base, None, secrets).await?;
        super::verify(repo, issue, base, secrets, &context).await
    }

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

    // Guardrail: interpreter bytecode caches are runtime artifacts and must
    // never enter the candidate-tree snapshot. Reproduces the CI false
    // "candidate tree changed during verification" (own-2/own-3, 2026-10-30):
    // a verification command that writes `__pycache__/*.pyc` between the two
    // snapshots must not change the tree the snapshot reports.
    #[test]
    fn bytecode_cache_never_enters_candidate_tree() {
        let root = tempfile::tempdir().expect("root");
        let repo = root.path().join("repo");
        std::fs::create_dir_all(repo.join("checks")).expect("dirs");
        git(&repo, &["init", "-b", "main"]);
        git(&repo, &["config", "user.name", "test"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        std::fs::write(repo.join("app.py"), "def calculate():\n    return 1\n").expect("bug");
        std::fs::write(
            repo.join("checks/oracle.py"),
            "import helper\nimport sys\nsys.exit(0)\n",
        )
        .expect("oracle");
        std::fs::write(repo.join("checks/helper.py"), "value = 1\n").expect("helper");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "init"]);

        let tree1 = candidate_tree_with_exclusions(&repo, &[]).expect("first snapshot");
        // Simulate the verification command: importing a sibling module makes
        // CPython write `checks/__pycache__/helper.*.pyc` (skipped when the
        // interpreter is told not to write bytecode, e.g. PYTHONDONTWRITEBYTECODE).
        let ran = std::process::Command::new(if cfg!(windows) { "python" } else { "python3" })
            .arg("checks/oracle.py")
            .current_dir(&repo)
            .status()
            .expect("run oracle");
        assert!(ran.success(), "oracle must succeed");
        let pyc_present = std::fs::read_dir(repo.join("checks/__pycache__"))
            .map(|entries| entries.count() > 0)
            .unwrap_or(false);
        if !pyc_present {
            // Interpreter declined to write bytecode; nothing to guard here.
            return;
        }
        let tree2 = candidate_tree_with_exclusions(&repo, &[]).expect("second snapshot");
        assert_eq!(
            tree1, tree2,
            "bytecode cache written by the verification command must not change the candidate tree"
        );
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
            verification_mode: Default::default(),
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
            std::fs::write(
                &file,
                "@if exist fixed.txt (exit /b 0) else (exit /b 1)\r\n",
            )
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
        assert_eq!(evidence.provenance.trust_level, "legacy_untrusted");
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
        assert!(format!("{err:#}").contains("timed out"));
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
            summary
                .forbidden_hits
                .iter()
                .any(|hit| hit.contains("deploy.pem")),
            "{:?}",
            summary.forbidden_hits
        );
    }

    #[test]
    fn forbidden_glob_matches_file_inside_a_new_untracked_directory() {
        // Regression: porcelain folded a wholly-new directory to `cfg/`, so a
        // forbidden `cfg/key.pem` inside it was never matched, yet `git add -A`
        // staged it for delivery. `--untracked-files=all` lists the file itself.
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::create_dir(repo.join("cfg")).expect("mkdir");
        std::fs::write(repo.join("cfg/key.pem"), "key\n").expect("write");
        let summary = diff_summary(&repo, "main", &["*.pem".to_string()]).expect("diff");
        assert!(
            summary
                .forbidden_hits
                .iter()
                .any(|hit| hit.contains("cfg/key.pem")),
            "nested untracked forbidden file must be caught: {:?}",
            summary.forbidden_hits
        );
    }

    #[test]
    fn forbidden_glob_matches_a_modified_tracked_file() {
        // Regression: the shared `git()` helper trimmed porcelain output, so a
        // ` M go.sum` record (leading status space) parsed as path `o.sum` and
        // the forbidden `go.sum` was missed.
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("go.sum"), "module hash\n").expect("write");
        git(&repo, &["add", "go.sum"]);
        git(&repo, &["commit", "-m", "add go.sum"]);
        std::fs::write(repo.join("go.sum"), "module hash CHANGED\n").expect("rewrite");
        let summary = diff_summary(&repo, "main", &["go.sum".to_string()]).expect("diff");
        assert!(
            summary
                .forbidden_hits
                .iter()
                .any(|hit| hit.contains("go.sum")),
            "a modified tracked forbidden file must be caught: {:?}",
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

    #[tokio::test]
    async fn systemic_committed_fix_after_base_merge_keeps_full_risk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        git(&repo, &["checkout", "-b", "fix"]);
        std::fs::write(repo.join("fixed.txt"), "fixed\n").expect("write");
        std::fs::write(repo.join("fix_test.go"), "package main\n").expect("write");
        std::fs::create_dir_all(repo.join(".github/workflows")).expect("mkdir");
        std::fs::write(repo.join(".github/workflows/ci.yml"), "name: ci\n").expect("write");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "fix and tests"]);
        git(&repo, &["checkout", "main"]);
        std::fs::write(repo.join("upstream.txt"), "upstream\n".repeat(20)).expect("write");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "upstream advance"]);
        let tip = git_ops::git(&repo, &["rev-parse", "HEAD"]).expect("tip");
        git(&repo, &["checkout", "fix"]);
        git(&repo, &["merge", "--no-edit", "main"]);

        let evidence = verify(
            &repo,
            &issue(&exists_command("fixed.txt")),
            &tip,
            &Secrets::default(),
        )
        .await
        .expect("reverify merged fix");
        assert_eq!(
            evidence.diff.files.len(),
            3,
            "committed fix must remain visible"
        );
        assert!(evidence.diff.touches_tests);
        assert!(evidence.diff.touches_ci);
        assert_eq!((evidence.diff.added, evidence.diff.removed), (3, 0));
        assert!(!evidence.failing_base.ok);
        assert!(evidence.failing_branch.ok);
    }

    #[tokio::test]
    async fn systemic_committed_forbidden_change_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        git(&repo, &["checkout", "-b", "fix"]);
        std::fs::write(repo.join("fixed.txt"), "fixed\n").expect("write");
        std::fs::write(repo.join("secret.pem"), "not a real key\n").expect("write");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "forbidden fix"]);
        let mut issue = issue(&exists_command("fixed.txt"));
        issue.forbidden_paths = vec!["*.pem".into()];
        let err = verify(&repo, &issue, "main", &Secrets::default())
            .await
            .expect_err("committed forbidden path must block delivery");
        assert!(format!("{err:#}").contains("secret.pem"));
    }

    #[test]
    fn systemic_full_diff_preserves_rename_paths_and_counts_final_tree_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(repo.join("protected.txt"), "keep\n".repeat(20)).expect("write");
        std::fs::write(repo.join("nuwa.repo.json"), "{}\n").expect("write");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "base files"]);
        git(&repo, &["checkout", "-b", "fix"]);
        git(&repo, &["mv", "protected.txt", "renamed_test.go"]);
        git(&repo, &["commit", "-m", "rename"]);
        std::fs::write(repo.join("base.txt"), "base\nstaged\n").expect("write");
        git(&repo, &["add", "base.txt"]);
        std::fs::write(repo.join("base.txt"), "base\nstaged\nunstaged\n").expect("write");
        std::fs::write(repo.join("new file.txt"), "new\n").expect("write");
        std::fs::write(repo.join("nuwa.repo.json"), "ignored\n".repeat(10)).expect("write");
        // A staged deletion followed by an untracked recreation is one final
        // modification, not a deletion plus an unrelated added file.
        git(&repo, &["rm", "--cached", "renamed_test.go"]);
        let before = git_ops::git_raw(&repo, &["status", "--porcelain=v1", "-z"]).expect("status");
        let summary = diff_summary(&repo, "main", &["protected.txt".into()]).expect("diff");
        let mut paths: Vec<_> = summary
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        paths.sort_unstable();
        assert_eq!(
            paths,
            [
                "base.txt",
                "new file.txt",
                "protected.txt",
                "renamed_test.go"
            ]
        );
        assert!(summary.touches_tests);
        assert_eq!(summary.forbidden_hits.len(), 1);
        assert!(summary.forbidden_hits[0].contains("protected.txt"));
        assert_eq!((summary.added, summary.removed), (3, 0));
        assert_eq!(
            git_ops::git_raw(&repo, &["status", "--porcelain=v1", "-z"]).expect("status"),
            before
        );
    }

    #[cfg(unix)]
    #[test]
    fn systemic_diff_parses_nul_paths_without_line_or_utf8_assumptions() {
        use std::os::unix::ffi::OsStringExt;
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path());
        git(dir.path(), &["checkout", "-b", "fix"]);
        let name = std::ffi::OsString::from_vec(b" odd\tname\n\xff_test.go".to_vec());
        std::fs::write(dir.path().join(name), "one\ntwo\n").expect("write");
        git(dir.path(), &["add", "."]);
        git(dir.path(), &["commit", "-m", "odd path"]);
        let summary = diff_summary(dir.path(), "main", &["*_test.go".into()]).expect("diff");
        assert_eq!(summary.files.len(), 1);
        assert_eq!(summary.files[0].path, " odd\tname\n\u{fffd}_test.go");
        assert_eq!((summary.added, summary.removed), (2, 0));
        assert!(summary.touches_tests);
        assert_eq!(summary.forbidden_hits.len(), 1);
    }

    #[test]
    fn deep_check_specs_require_a_go_module() {
        let dir = tempfile::tempdir().expect("tempdir");
        let go_issue = Issue {
            language: "go".into(),
            ..issue("exit 0")
        };
        assert!(deep_check_specs(&go_issue, dir.path()).is_empty());
        std::fs::write(dir.path().join("go.mod"), "module example.com/m\n").expect("go.mod");
        assert_eq!(
            deep_check_specs(&go_issue, dir.path()),
            vec!["go vet ./...".to_string(), "go build ./...".to_string()]
        );
        let python_issue = Issue {
            language: "python".into(),
            ..issue("exit 0")
        };
        assert!(deep_check_specs(&python_issue, dir.path()).is_empty());
    }

    #[tokio::test]
    async fn go_deep_build_does_not_change_the_candidate_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        std::fs::write(
            repo.join("go.mod"),
            "module example.com/deepbuild\n\ngo 1.22\n",
        )
        .expect("module");
        std::fs::write(repo.join("main.go"), "package main\nfunc main() {}\n")
            .expect("main package");
        let tree = candidate_tree(&repo).expect("candidate before build");
        let check = FailingCheck {
            command: "go build ./...".into(),
            target: None,
            timeout_secs: 60,
        };
        let result = run_deep_check(
            &repo,
            &check,
            &crate::secrets::agent_env_allowlist(),
            &Secrets::default(),
        )
        .await
        .expect("Go build");
        assert!(result.ok, "{}", result.output);
        assert_eq!(candidate_tree(&repo).expect("candidate after build"), tree);
    }

    #[tokio::test]
    async fn deep_checks_skip_a_red_baseline() {
        let base = tempfile::tempdir().expect("base");
        let repo = tempfile::tempdir().expect("repo");
        let env = crate::secrets::agent_env_allowlist();
        let results = run_deep_checks_with_specs(
            &["exit 1".to_string()],
            base.path(),
            repo.path(),
            10,
            &env,
            &Secrets::default(),
        )
        .await
        .expect("deep checks");
        assert!(
            results[0].branch.is_none(),
            "a red baseline must be skipped"
        );
    }

    #[tokio::test]
    async fn deep_checks_block_a_red_branch() {
        let base = tempfile::tempdir().expect("base");
        let repo = tempfile::tempdir().expect("repo");
        std::fs::write(base.path().join("deep.txt"), "ok").expect("write");
        let env = crate::secrets::agent_env_allowlist();
        let error = run_deep_checks_with_specs(
            &[exists_command("deep.txt")],
            base.path(),
            repo.path(),
            10,
            &env,
            &Secrets::default(),
        )
        .await
        .expect_err("branch must fail");
        assert!(format!("{error:#}").contains("fails on the branch"));
    }

    #[tokio::test]
    async fn deep_checks_pass_when_both_sides_are_green() {
        let base = tempfile::tempdir().expect("base");
        let repo = tempfile::tempdir().expect("repo");
        let env = crate::secrets::agent_env_allowlist();
        let results = run_deep_checks_with_specs(
            &["exit 0".to_string()],
            base.path(),
            repo.path(),
            10,
            &env,
            &Secrets::default(),
        )
        .await
        .expect("deep checks");
        assert!(results[0].base.ok);
        assert!(results[0].branch.as_ref().is_some_and(|output| output.ok));
    }

    fn reproduction_path() -> &'static str {
        if cfg!(windows) {
            ".nuwa/repro.cmd"
        } else {
            ".nuwa/repro.sh"
        }
    }
}
