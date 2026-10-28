//! Frozen owner oracle. This is provenance/integrity enforcement, NOT a sandbox:
//! commands and candidate application code still have host-user privileges.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Context};
use sha2::{Digest, Sha256};

use super::{CheckOutput, VerificationEvidence};
use crate::git_ops;
use crate::instructions::{
    validate_protected_path, OwnerOracleSpec, TrustSource, TrustedInstructions, VerificationPolicy,
};
use crate::issue::Issue;
use crate::secrets::Secrets;

/// Safe to persist. Contains no source bytes, command text, paths or raw output.
#[derive(Debug, Clone)]
pub struct TrustProvenance {
    pub trust_level: &'static str,
    pub base_sha: String,
    pub candidate_tree: String,
    pub owner_config_sha256: Option<String>,
    pub owner_source: Option<&'static str>,
    pub manifest_sha256: Option<String>,
    pub oracle_base_exit: Option<i32>,
    pub oracle_candidate_exit: Option<i32>,
}

// Deliberately not Debug: private source bytes must never enter evidence/logs.
struct ProtectedInput {
    path: String,
    bytes: Vec<u8>,
    digest: String,
    git_mode: String,
    original_mode: u32,
    indexed_mode: Option<String>,
    tracked: bool,
}

struct FrozenOracle {
    spec: OwnerOracleSpec,
    command: FrozenCommand,
    inputs: Vec<ProtectedInput>,
    manifest: String,
    private: tempfile::TempDir,
    env: BTreeMap<String, String>,
    // Serializes exports to the SAME physical target path, including rebase.
    execution: tokio::sync::Mutex<()>,
}

/// Created before the first repair write. Retain this SAME instance across a
/// merge/rebase; verify() reruns its original oracle against the new base.
pub struct VerificationContext {
    repo: PathBuf,
    owner_root: PathBuf,
    base_sha: String,
    policy: VerificationPolicy,
    owner_digest: Option<String>,
    owner_source: Option<TrustSource>,
    config: Option<ProtectedInput>,
    oracle: Option<FrozenOracle>,
    verified: Mutex<Option<(String, String)>>,
}

impl std::fmt::Debug for VerificationContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerificationContext")
            .field("base_sha", &self.base_sha)
            .field("policy", &self.policy)
            .field("owner_digest", &self.owner_digest)
            .field("trusted", &self.is_trusted())
            .finish_non_exhaustive()
    }
}

/// Snapshot and prove owner-red BEFORE repair. Strict without an oracle fails
/// here, before any model/branch/commit/push effects. Never pass issue config.
pub async fn prepare(
    repo: &Path,
    base: &str,
    owner: Option<&TrustedInstructions>,
    secrets: &Secrets,
) -> anyhow::Result<VerificationContext> {
    prepare_workspace(repo, repo, base, owner, secrets).await
}

/// Freeze authority at the config root, then provision only missing untracked
/// inputs into a separate pool checkout. Never re-adopt the workspace's bytes.
pub(crate) async fn prepare_workspace(
    repo: &Path,
    owner_root: &Path,
    base: &str,
    owner: Option<&TrustedInstructions>,
    secrets: &Secrets,
) -> anyhow::Result<VerificationContext> {
    let repo = checked_root(repo)?;
    let owner_root = checked_root(owner_root)?;
    let base_sha = resolve_base(&repo, base)?;
    let mut context = VerificationContext {
        repo,
        owner_root,
        base_sha,
        policy: VerificationPolicy::Compatibility,
        owner_digest: None,
        owner_source: None,
        config: None,
        oracle: None,
        verified: Mutex::new(None),
    };
    if let Some(owner) = owner {
        let config = owner.pinned_instructions(&context.owner_root, &context.base_sha)?;
        context.policy = config.verification_policy;
        context.owner_digest = Some(owner.sha256.clone());
        context.owner_source = Some(owner.source());
        context.config = Some(snapshot_input(
            &context.owner_root,
            &context.base_sha,
            "nuwa.repo.json",
            owner.source() == TrustSource::PrivateCopy,
        )?);
        if context
            .config
            .as_ref()
            .context("owner config snapshot")?
            .bytes
            != owner.bytes
        {
            bail!("working owner config differs from pinned authority");
        }
        if let Some(spec) = config.trusted_verification {
            crate::instructions::validate_owner_oracle(&spec)?;
            let mut inputs = Vec::new();
            for path in &spec.protected_inputs {
                inputs.push(snapshot_input(
                    &context.owner_root,
                    &context.base_sha,
                    path,
                    owner.source() == TrustSource::PrivateCopy,
                )?);
            }
            inputs.sort_by(|a, b| a.path.cmp(&b.path));
            let manifest = manifest_digest(&inputs);
            let private = tempfile::Builder::new().prefix("nuwa-oracle-").tempdir()?;
            let private_root = checked_root(private.path())?;
            if private_root.starts_with(&context.repo)
                || private_root.starts_with(&context.owner_root)
            {
                bail!("trusted oracle directory must be outside the checkout");
            }
            std::fs::create_dir(private.path().join("oracle"))?;
            for input in &inputs {
                write_safe_file(&private.path().join("oracle"), &input.path, &input.bytes)?;
                set_git_mode(
                    &private.path().join("oracle").join(&input.path),
                    &input.git_mode,
                )?;
            }
            let env = oracle_environment(&private_root, &context.repo)?;
            let command = FrozenCommand::prepare(&spec, &private_root, &context.repo, &env)?;
            context.oracle = Some(FrozenOracle {
                spec,
                command,
                inputs,
                manifest,
                private,
                env,
                execution: tokio::sync::Mutex::new(()),
            });
        }
    } else if std::fs::symlink_metadata(context.repo.join("nuwa.repo.json")).is_ok()
        || std::fs::symlink_metadata(context.owner_root.join("nuwa.repo.json")).is_ok()
        || !git_ops::git_raw(
            &context.repo,
            &["ls-tree", &context.base_sha, "--", "nuwa.repo.json"],
        )?
        .is_empty()
    {
        bail!("existing owner config requires authoritative TrustedInstructions; cannot downgrade");
    }
    context.assert_delivery_allowed()?;
    if context.owner_root != context.repo {
        // Every input is already frozen before any destination is written.
        for input in context
            .config
            .iter()
            .chain(context.oracle.iter().flat_map(|o| &o.inputs))
        {
            provision_input(&context.repo, input)?;
        }
    }
    context.assert_integrity(&context.repo)?;
    if let Some(oracle) = &context.oracle {
        let _execution = oracle.execution.lock().await;
        context.assert_base_inputs(&context.repo, context.base_sha())?;
        let out = oracle
            .run_tree(&context.repo, &context.base_sha, secrets)
            .await?;
        require_exit(&out, 1, "owner oracle baseline")?;
        context.assert_integrity(&context.repo)?;
    }
    Ok(context)
}

/// Pre-model config restoration retains existing native permissions and uses
/// the same ancestor/link checks as protected-input provisioning.
pub(crate) fn seed_owner_config(repo: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let destination = safe_destination(repo, "nuwa.repo.json")?;
    let permissions = match std::fs::symlink_metadata(&destination) {
        Ok(_) => {
            safe_file(repo, "nuwa.repo.json")?;
            if std::fs::read(&destination)? == bytes {
                return Ok(());
            }
            Some(safe_metadata(&destination)?.permissions())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    write_safe_file(repo, "nuwa.repo.json", bytes)?;
    if let Some(permissions) = permissions {
        std::fs::set_permissions(&destination, permissions)?;
    }
    Ok(())
}

impl VerificationContext {
    pub fn is_trusted(&self) -> bool {
        self.oracle.is_some()
    }

    pub fn base_sha(&self) -> &str {
        &self.base_sha
    }

    /// Policy readiness only. At the final effect boundary also call
    /// assert_verified_candidate(repo, base) to require the successful proof.
    pub fn assert_delivery_allowed(&self) -> anyhow::Result<()> {
        if self.policy == VerificationPolicy::Strict && self.oracle.is_none() {
            bail!("strict verification requires an owner oracle before repair/delivery");
        }
        Ok(())
    }

    pub fn protected_paths(&self) -> Vec<String> {
        let mut paths = BTreeSet::new();
        if self.config.is_some() {
            paths.insert("nuwa.repo.json".to_string());
        }
        if let Some(oracle) = &self.oracle {
            paths.extend(oracle.inputs.iter().map(|input| input.path.clone()));
        }
        paths.into_iter().collect()
    }

    /// Exact non-delivery files. Tracked oracle inputs are NEVER excluded.
    /// Config keeps its established exclusion regardless of its Git status.
    pub fn excluded_delivery_paths(&self) -> Vec<String> {
        let mut paths = BTreeSet::from(["nuwa.repo.json".to_string()]);
        if let Some(oracle) = &self.oracle {
            paths.extend(
                oracle
                    .inputs
                    .iter()
                    .filter(|input| !input.tracked)
                    .map(|input| input.path.clone()),
            );
        }
        paths.into_iter().collect()
    }

    /// Check original bytes/type/native mode AND staged Git executable mode.
    /// No tampered input is restored and then accepted.
    pub fn assert_integrity(&self, repo: &Path) -> anyhow::Result<()> {
        if checked_root(repo)? != self.repo {
            bail!("verification checkout changed");
        }
        if let Some(config) = &self.config {
            assert_original(repo, config)?;
            if self.owner_root != self.repo {
                assert_original(&self.owner_root, config)?;
            }
        }
        if let Some(oracle) = &self.oracle {
            for input in &oracle.inputs {
                assert_original(repo, input)?;
                if self.owner_root != self.repo {
                    assert_original(&self.owner_root, input)?;
                }
            }
            oracle.assert_snapshot()?;
            let target = oracle.private.path().join("target");
            if std::fs::symlink_metadata(target).is_ok() {
                oracle.assert_target_inputs()?;
            }
        }
        Ok(())
    }

    /// Required immediately before staging/commit/push, including local-only
    /// delivery. Both the base and actual tree must equal the last green proof.
    pub fn assert_verified_candidate(&self, repo: &Path, base: &str) -> anyhow::Result<()> {
        self.assert_delivery_allowed()?;
        self.assert_integrity(repo)?;
        self.assert_candidate_sources(repo)?;
        let base = resolve_base(repo, base)?;
        let tree = super::candidate_tree_for_context(repo, self)?;
        let verified = self
            .verified
            .lock()
            .map_err(|_| anyhow::anyhow!("verification state poisoned"))?;
        if verified.as_ref() != Some(&(base, tree)) {
            bail!("delivery base/candidate tree lacks matching verification");
        }
        Ok(())
    }

    pub(super) fn assert_candidate_sources(&self, repo: &Path) -> anyhow::Result<()> {
        if self.is_trusted() {
            // Inspect actual source ancestors BEFORE git add reads any bytes.
            // In particular Git on Windows may traverse directory junctions.
            let paths = git_ops::git_raw(
                repo,
                &[
                    "ls-files",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                    "-z",
                    "--",
                    ":(exclude)nuwa.repo.json",
                ],
            )?;
            for path in paths.split(|b| *b == 0).filter(|p| !p.is_empty()) {
                safe_destination(
                    repo,
                    std::str::from_utf8(path).context("trusted source path encoding")?,
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn invalidate_verification(&self) -> anyhow::Result<()> {
        *self
            .verified
            .lock()
            .map_err(|_| anyhow::anyhow!("verification state poisoned"))? = None;
        Ok(())
    }

    pub(super) fn record_verified(&self, base: &str, tree: &str) -> anyhow::Result<()> {
        *self
            .verified
            .lock()
            .map_err(|_| anyhow::anyhow!("verification state poisoned"))? =
            Some((base.to_string(), tree.to_string()));
        Ok(())
    }

    pub(super) fn provenance(&self, base: &str, tree: &str) -> TrustProvenance {
        TrustProvenance {
            trust_level: if self.is_trusted() {
                "owner_trusted"
            } else {
                "legacy_untrusted"
            },
            base_sha: base.to_string(),
            candidate_tree: tree.to_string(),
            owner_config_sha256: self.owner_digest.clone(),
            owner_source: self.owner_source.map(|s| match s {
                TrustSource::Tracked => "tracked_base",
                TrustSource::PrivateCopy => "owner_seeded",
            }),
            manifest_sha256: self.oracle.as_ref().map(|o| o.manifest.clone()),
            oracle_base_exit: self.is_trusted().then_some(1),
            oracle_candidate_exit: self.is_trusted().then_some(0),
        }
    }

    /// Never adopts new instructions or seeds missing inputs during rebase.
    fn assert_base_inputs(&self, repo: &Path, base: &str) -> anyhow::Result<()> {
        let tree = tree_entries(repo, base)?;
        for input in self
            .config
            .iter()
            .chain(self.oracle.iter().flat_map(|o| &o.inputs))
        {
            if let Some(entry) = tree.iter().find(|entry| entry.path == input.path) {
                if !input.tracked {
                    bail!("owner-seeded input unexpectedly tracked in verification tree");
                }
                if entry.mode != input.git_mode || read_blob(repo, &entry.oid)? != input.bytes {
                    bail!("pinned owner input changed on verification base");
                }
            } else if input.tracked {
                bail!("pinned owner input missing from verification base");
            }
        }
        Ok(())
    }

    pub(super) async fn verify_candidate(
        &self,
        repo: &Path,
        issue: &Issue,
        base: &str,
        tree: &str,
        secrets: &Secrets,
    ) -> anyhow::Result<VerificationEvidence> {
        let oracle = self
            .oracle
            .as_ref()
            .context("missing frozen owner oracle")?;
        let _execution = oracle.execution.lock().await;
        self.assert_integrity(repo)?;
        self.assert_base_inputs(repo, base)?;
        // Require the delivered tree to contain unchanged tracked owner inputs.
        self.assert_base_inputs(repo, tree)?;
        let base_out = oracle.run_tree(repo, base, secrets).await?;
        require_exit(&base_out, 1, "owner oracle baseline")?;
        let target = oracle.private.path().join("target");
        let env = &oracle.env;
        let failing_base = private_check(&target, &issue.failing_check, env, secrets).await?;
        require_valid_check(&failing_base, "base failing_check")?;
        let specs = super::deep_check_specs(issue, &target);
        let mut deep_base = Vec::new();
        for command in &specs {
            let check = crate::issue::FailingCheck {
                command: command.clone(),
                target: None,
                timeout_secs: issue.failing_check.timeout_secs.max(1),
            };
            let output = private_check(&target, &check, env, secrets).await?;
            require_valid_check(&output, "base deep check")?;
            deep_base.push(output);
        }
        oracle.assert_target_inputs()?;
        self.assert_integrity(repo)?;
        let candidate_out = oracle.run_tree(repo, tree, secrets).await?;
        require_exit(&candidate_out, 0, "owner oracle candidate")?;
        let failing_branch = private_check(&target, &issue.failing_check, env, secrets).await?;
        require_exit(&failing_branch, 0, "candidate failing_check")?;
        let mut deep_checks = Vec::new();
        for (command, base) in specs.into_iter().zip(deep_base) {
            let branch = if base.ok && !base.timed_out {
                let check = crate::issue::FailingCheck {
                    command: command.clone(),
                    target: None,
                    timeout_secs: issue.failing_check.timeout_secs.max(1),
                };
                let out = private_check(&target, &check, env, secrets).await?;
                require_exit(&out, 0, "candidate deep check")?;
                Some(out)
            } else {
                None
            };
            deep_checks.push(super::DeepCheckResult {
                command,
                base,
                branch,
            });
        }
        oracle.assert_target_inputs()?;
        self.assert_integrity(repo)?;
        self.assert_candidate_sources(repo)?;
        let diff = super::diff_summary_with_exclusions(
            repo,
            base,
            &issue.forbidden_paths,
            &self.excluded_delivery_paths(),
        )?;
        if !diff.forbidden_hits.is_empty() {
            bail!("delivery changes forbidden paths");
        }
        Ok(VerificationEvidence {
            provenance: self.provenance(base, tree),
            mode: "owner_trusted",
            failing_base,
            failing_branch,
            reproduce_base: Some(base_out),
            reproduce_branch: Some(candidate_out),
            deep_checks,
            diff,
        })
    }
}

impl FrozenOracle {
    fn assert_snapshot(&self) -> anyhow::Result<()> {
        let private = checked_root(self.private.path())?;
        // A harness cannot carry phase/branch hints in an unreset sibling file.
        for entry in std::fs::read_dir(&private)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .context("private oracle layout")?;
            if !matches!(name, "oracle" | "target" | "home" | "tmp" | "cache")
                || !safe_metadata(&path)?.is_dir()
            {
                bail!("unexpected private verification artifact");
            }
        }
        let root = self.private.path().join("oracle");
        checked_root(&root)?;
        let allowed: BTreeSet<_> = self.inputs.iter().map(|i| i.path.as_str()).collect();
        assert_snapshot_entries(&root, &root, &allowed)?;
        for input in &self.inputs {
            assert_frozen_file(&root, input)?;
        }
        Ok(())
    }

    fn assert_target_inputs(&self) -> anyhow::Result<()> {
        assert_export_safety(&self.private.path().join("target"))?;
        for input in &self.inputs {
            assert_frozen_file(&self.private.path().join("target"), input)?;
        }
        Ok(())
    }

    async fn run_tree(
        &self,
        repo: &Path,
        tree: &str,
        _secrets: &Secrets,
    ) -> anyhow::Result<CheckOutput> {
        self.assert_snapshot()?;
        let root = checked_root(self.private.path())?;
        for name in ["target", "home", "tmp", "cache"] {
            clear_directory(&root, name)?;
        }
        let target = root.join("target");
        export_tree(repo, tree, &target)?;
        // Seeded owner files may be absent from Git; only frozen bytes can be
        // overlaid. Tracked inputs have already been checked against the tree.
        for input in &self.inputs {
            write_safe_file(&target, &input.path, &input.bytes)?;
            set_git_mode(&target.join(&input.path), &input.git_mode)?;
        }
        self.assert_target_inputs()?;
        let args: Vec<_> = self.command.args.iter().map(String::as_str).collect();
        let output = git_ops::run_process(
            self.command
                .program
                .to_str()
                .context("oracle toolchain path encoding")?,
            &args,
            &root.join("oracle"),
            std::time::Duration::from_secs(self.spec.check.timeout_secs),
            Some(&self.env),
        )
        .await;
        self.assert_snapshot()?;
        self.assert_target_inputs()?;
        let out = output.map_err(|_| anyhow::anyhow!("owner oracle could not execute"))?;
        Ok(CheckOutput {
            ok: out.success && !out.timed_out,
            exit_code: out.exit_code,
            timed_out: out.timed_out,
            output: format!("exit={:?}; timeout={}", out.exit_code, out.timed_out),
        })
    }
}

struct FrozenCommand {
    program: PathBuf,
    args: Vec<String>,
}

impl FrozenCommand {
    fn prepare(
        spec: &OwnerOracleSpec,
        root: &Path,
        repo: &Path,
        env: &BTreeMap<String, String>,
    ) -> anyhow::Result<Self> {
        // Deliberately a file launcher, not arbitrary shell evaluation. Owners
        // put setup/assertions in a listed harness (unexpected faults exit 2+).
        let words = command_words(&spec.check.command)?;
        let first = words.first().context("missing oracle launcher")?;
        let relative = first.strip_prefix("./").unwrap_or(first);
        if spec.protected_inputs.iter().any(|path| path == relative) {
            let program = safe_file(&root.join("oracle"), relative)?;
            #[cfg(unix)]
            if git_mode(&program)? != "100755" {
                bail!("direct oracle entrypoint is not executable");
            }
            return Ok(Self {
                program,
                args: words[1..].to_vec(),
            });
        }
        let name = first.to_ascii_lowercase();
        if !matches!(
            name.as_str(),
            "python"
                | "python3"
                | "python.exe"
                | "python3.exe"
                | "node"
                | "node.exe"
                | "sh"
                | "bash"
                | "ruby"
                | "perl"
                | "cmd"
                | "cmd.exe"
        ) {
            bail!("oracle requires a protected executable or explicit file interpreter");
        }
        if words.iter().skip(1).any(|arg| {
            matches!(
                arg.to_ascii_lowercase().as_str(),
                "-c" | "-m" | "-e" | "--eval" | "--command"
            )
        }) {
            bail!("oracle must launch a protected file, not evaluated code/discovery");
        }
        let is_cmd = name == "cmd" || name == "cmd.exe";
        let entrypoint = words
            .iter()
            .skip(1)
            .find(|arg| !(arg.starts_with('-') || is_cmd && arg.starts_with('/')))
            .context("oracle entrypoint must be listed in protected_inputs")?;
        let relative = entrypoint.strip_prefix("./").unwrap_or(entrypoint);
        if !spec.protected_inputs.iter().any(|path| path == relative) {
            bail!("oracle entrypoint must be listed in protected_inputs");
        }
        safe_file(&root.join("oracle"), relative)?;
        let names = launcher_names(first);
        let mut program = None;
        for dir in std::env::split_paths(env.get("PATH").context("oracle toolchain PATH missing")?)
        {
            for name in &names {
                let path = dir.join(name);
                if path.is_file() {
                    let path = std::fs::canonicalize(path)?;
                    if path.starts_with(repo) || path.starts_with(root) {
                        bail!("candidate-controlled oracle launcher");
                    }
                    program = Some(path);
                    break;
                }
            }
            if program.is_some() {
                break;
            }
        }
        let program = program.context("owner oracle toolchain is unavailable")?;
        let mut args = words[1..].to_vec();
        if is_cmd {
            args.insert(0, "/D".to_string());
        }
        Ok(Self { program, args })
    }
}

fn command_words(command: &str) -> anyhow::Result<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    for c in command.chars() {
        if c.is_control() || "\\$%;&|<>`*?(){}".contains(c) {
            bail!("unsafe oracle launcher: use a literal protected harness file");
        }
        if c == '\'' || c == '"' {
            if quote == Some(c) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(c);
            } else {
                word.push(c);
            }
        } else if c.is_whitespace() && quote.is_none() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(c);
        }
    }
    if quote.is_some() {
        bail!("unterminated oracle launcher quote");
    }
    if !word.is_empty() {
        words.push(word);
    }
    Ok(words)
}

fn launcher_names(first: &str) -> Vec<String> {
    #[cfg(windows)]
    if Path::new(first).extension().is_none() {
        return vec![format!("{first}.exe"), first.to_string()];
    }
    vec![first.to_string()]
}

// Oracle/application output can contain protected source or fixtures. Do not
// persist it even after known-secret redaction; publish outcomes only.
async fn private_check(
    dir: &Path,
    check: &crate::issue::FailingCheck,
    env: &BTreeMap<String, String>,
    secrets: &Secrets,
) -> anyhow::Result<CheckOutput> {
    let mut out = super::run_check(dir, check, env, secrets)
        .await
        .map_err(|_| anyhow::anyhow!("trusted verification command could not execute"))?;
    out.output = format!("exit={:?}; timeout={}", out.exit_code, out.timed_out);
    Ok(out)
}

fn assert_frozen_file(root: &Path, input: &ProtectedInput) -> anyhow::Result<()> {
    let path = safe_file(root, &input.path)?;
    if std::fs::read(&path)? != input.bytes {
        bail!("frozen owner input changed");
    }
    #[cfg(unix)]
    if git_mode(&path)? != input.git_mode {
        bail!("frozen owner executable mode changed");
    }
    Ok(())
}

fn require_exit(out: &CheckOutput, expected: i32, label: &str) -> anyhow::Result<()> {
    if out.timed_out {
        bail!("{label} timed out");
    }
    if out.exit_code != Some(expected) {
        bail!(
            "{label}: expected exit {expected}, got {:?}; no trusted proof",
            out.exit_code
        );
    }
    Ok(())
}

pub(super) fn require_valid_check(out: &CheckOutput, label: &str) -> anyhow::Result<()> {
    if out.timed_out {
        bail!("{label} timed out");
    }
    // Any completed process is a red/green outcome: 0 is green, every other
    // exit is red. Cargo uses 101 for a failed check/test, so restricting this
    // to 0/1 misclassified ordinary red baselines as setup errors.
    if out.exit_code.is_none() {
        bail!("{label} setup/termination error; not a red/green outcome");
    }
    Ok(())
}

pub(super) fn resolve_base(repo: &Path, base: &str) -> anyhow::Result<String> {
    // Prefer an exact supplied object/ref; preparation normally supplies a SHA.
    let reference = if git_ops::git(
        repo,
        &[
            "rev-parse",
            "--verify",
            &format!("origin/{base}^{{commit}}"),
        ],
    )
    .is_ok()
    {
        format!("origin/{base}")
    } else {
        base.to_string()
    };
    git_ops::git(
        repo,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
    )
}

struct TreeEntry {
    path: String,
    mode: String,
    oid: String,
}

fn tree_entries(repo: &Path, tree: &str) -> anyhow::Result<Vec<TreeEntry>> {
    let bytes = git_ops::git_raw(repo, &["ls-tree", "-r", "-z", tree])?;
    let mut entries = Vec::new();
    let mut seen = BTreeSet::new();
    for record in bytes.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let split = record
            .iter()
            .position(|b| *b == b'\t')
            .context("invalid tree entry")?;
        let path = std::str::from_utf8(&record[split + 1..])
            .context("trusted export requires UTF-8 paths")?;
        validate_protected_path(path)?;
        if !seen.insert(path.to_ascii_lowercase()) {
            bail!("ambiguous case-alias in trusted export");
        }
        let header: Vec<_> = std::str::from_utf8(&record[..split])?.split(' ').collect();
        if header.len() != 3 || header[1] != "blob" || !matches!(header[0], "100644" | "100755") {
            bail!("trusted exports refuse links, submodules and special entries");
        }
        entries.push(TreeEntry {
            path: path.to_string(),
            mode: header[0].to_string(),
            oid: header[2].to_string(),
        });
    }
    Ok(entries)
}

fn read_blob(repo: &Path, oid: &str) -> anyhow::Result<Vec<u8>> {
    git_ops::git_raw(repo, &["cat-file", "blob", oid])
}

fn snapshot_input(
    repo: &Path,
    base: &str,
    path: &str,
    allow_seed: bool,
) -> anyhow::Result<ProtectedInput> {
    let original =
        safe_file(repo, path).context("protected owner input missing/unsafe before repair")?;
    let working = std::fs::read(&original)?;
    let entries = git_ops::git_raw(repo, &["ls-tree", "-r", "-z", base, "--", path])?;
    let entry = entries
        .split(|b| *b == 0)
        .find(|r| !r.is_empty())
        .map(|record| -> anyhow::Result<(String, String)> {
            let split = record
                .iter()
                .position(|b| *b == b'\t')
                .context("invalid protected tree entry")?;
            let header: Vec<_> = std::str::from_utf8(&record[..split])?.split(' ').collect();
            if &record[split + 1..] != path.as_bytes()
                || header.len() != 3
                || header[1] != "blob"
                || !matches!(header[0], "100644" | "100755")
            {
                bail!("protected owner input must be a regular Git blob");
            }
            Ok((header[0].to_string(), header[2].to_string()))
        })
        .transpose()?;
    let tracked = entry.is_some();
    let (bytes, mode) = if let Some((mode, oid)) = entry {
        (read_blob(repo, &oid)?, mode)
    } else if allow_seed {
        (working.clone(), git_mode(&original)?)
    } else {
        bail!("protected owner input is not tracked at pinned base");
    };
    if working != bytes {
        bail!("protected owner input differs from pinned base before repair");
    }
    #[cfg(unix)]
    if git_mode(&original)? != mode {
        bail!("protected owner executable mode differs from base");
    }
    let indexed_mode = indexed_mode(repo, path, &bytes)?;
    if !tracked && indexed_mode.is_some() {
        bail!("owner-seeded input must be genuinely untracked before repair");
    }
    if indexed_mode
        .as_ref()
        .is_some_and(|indexed| indexed != &mode)
    {
        bail!("protected owner staged executable mode differs from base");
    }
    Ok(ProtectedInput {
        path: path.to_string(),
        digest: digest(&bytes),
        bytes,
        git_mode: mode,
        original_mode: native_mode(&original)?,
        indexed_mode,
        tracked,
    })
}

fn provision_input(repo: &Path, input: &ProtectedInput) -> anyhow::Result<()> {
    let destination = safe_destination(repo, &input.path)?;
    match std::fs::symlink_metadata(&destination) {
        Ok(_) => {
            // Existing bytes/mode are checked, never overwritten or restored.
            assert_original(repo, input)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if input.tracked || indexed_mode(repo, &input.path, &input.bytes)?.is_some() {
                bail!("cannot seed a missing tracked/staged owner input");
            }
            write_safe_file(repo, &input.path, &input.bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &destination,
                    std::fs::Permissions::from_mode(input.original_mode),
                )?;
            }
            #[cfg(not(unix))]
            {
                let mut permissions = safe_metadata(&destination)?.permissions();
                permissions.set_readonly(input.original_mode != 0);
                std::fs::set_permissions(&destination, permissions)?;
            }
            assert_original(repo, input)?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn assert_original(repo: &Path, input: &ProtectedInput) -> anyhow::Result<()> {
    let path =
        safe_file(repo, &input.path).context("protected owner input deleted/replaced/linked")?;
    if !input.tracked
        && !git_ops::git_raw(
            repo,
            &[
                "ls-tree",
                "HEAD",
                "--",
                &format!(":(literal){}", input.path),
            ],
        )?
        .is_empty()
    {
        bail!("owner-seeded input unexpectedly committed");
    }
    let bytes = std::fs::read(&path)?;
    let indexed = indexed_mode(repo, &input.path, &input.bytes)?;
    let index_ok = match &input.indexed_mode {
        Some(mode) => indexed.as_ref() == Some(mode),
        None => indexed.as_ref().is_none_or(|mode| mode == &input.git_mode),
    };
    if bytes != input.bytes
        || digest(&bytes) != input.digest
        || native_mode(&path)? != input.original_mode
        || !index_ok
    {
        bail!("protected owner input integrity violation");
    }
    Ok(())
}

fn indexed_mode(repo: &Path, path: &str, expected: &[u8]) -> anyhow::Result<Option<String>> {
    let output = git_ops::git_raw(
        repo,
        &[
            "ls-files",
            "--stage",
            "-z",
            "--",
            &format!(":(literal){path}"),
        ],
    )?;
    let mut entries = output.split(|b| *b == 0).filter(|r| !r.is_empty());
    let mode = entries
        .next()
        .map(|entry| -> anyhow::Result<String> {
            let text = std::str::from_utf8(entry)?;
            let header = text
                .split('\t')
                .next()
                .context("invalid protected index entry")?;
            let parts: Vec<_> = header.split(' ').collect();
            if parts.len() != 3 || parts[2] != "0" || !matches!(parts[0], "100644" | "100755") {
                bail!("protected input has invalid/conflicted index mode");
            }
            if read_blob(repo, parts[1])? != expected {
                bail!("protected staged owner input changed");
            }
            Ok(parts[0].to_string())
        })
        .transpose()?;
    if entries.next().is_some() {
        bail!("ambiguous protected index entry");
    }
    Ok(mode)
}

fn manifest_digest(inputs: &[ProtectedInput]) -> String {
    let mut hash = Sha256::new();
    for input in inputs {
        for field in [&input.path, &input.digest, &input.git_mode] {
            hash.update((field.len() as u64).to_be_bytes());
            hash.update(field.as_bytes());
        }
        hash.update(b"regular-file");
        hash.update(input.original_mode.to_be_bytes());
    }
    format!("{:x}", hash.finalize())
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn native_mode(path: &Path) -> anyhow::Result<u32> {
    let meta = std::fs::symlink_metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Ok(meta.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        Ok(u32::from(meta.permissions().readonly()))
    }
}

fn git_mode(path: &Path) -> anyhow::Result<String> {
    #[cfg(unix)]
    {
        Ok(if native_mode(path)? & 0o111 != 0 {
            "100755"
        } else {
            "100644"
        }
        .to_string())
    }
    #[cfg(not(unix))]
    {
        safe_metadata(path)?;
        Ok("100644".to_string())
    }
}

fn set_git_mode(path: &Path, mode: &str) -> anyhow::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(if mode == "100755" { 0o755 } else { 0o644 }),
        )?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

fn safe_metadata(path: &Path) -> anyhow::Result<std::fs::Metadata> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        bail!("verification paths refuse symbolic links");
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            bail!("verification paths refuse reparse points");
        }
    }
    if !meta.is_file() && !meta.is_dir() {
        bail!("verification paths refuse special files");
    }
    Ok(meta)
}

fn checked_root(root: &Path) -> anyhow::Result<PathBuf> {
    let absolute = std::path::absolute(root)?;
    for ancestor in absolute.ancestors() {
        if !safe_metadata(ancestor)?.is_dir() {
            bail!("verification ancestor is not a directory");
        }
    }
    Ok(std::fs::canonicalize(absolute)?)
}

pub(super) fn safe_destination(root: &Path, relative: &str) -> anyhow::Result<PathBuf> {
    validate_protected_path(relative)?;
    let root = checked_root(root)?;
    let mut path = root.clone();
    let parts: Vec<_> = relative.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let meta = safe_metadata(&path)?;
                if index + 1 != parts.len() && !meta.is_dir() {
                    bail!("unsafe verification ancestor");
                }
                if !std::fs::canonicalize(&path)?.starts_with(&root) {
                    bail!("verification path escapes root");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(path)
}

pub(super) fn safe_file(root: &Path, relative: &str) -> anyhow::Result<PathBuf> {
    let path = safe_destination(root, relative)?;
    if !safe_metadata(&path)?.is_file() {
        bail!("verification input is not a regular file");
    }
    Ok(path)
}

pub(super) fn write_safe_file(root: &Path, relative: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let mut path = checked_root(root)?;
    validate_protected_path(relative)?;
    let parts: Vec<_> = relative.split('/').collect();
    for part in &parts[..parts.len() - 1] {
        path.push(part);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                if !safe_metadata(&path)?.is_dir() {
                    bail!("unsafe copy destination ancestor");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(&path)?,
            Err(e) => return Err(e.into()),
        }
    }
    let destination = safe_destination(root, relative)?;
    if destination.exists() && !safe_metadata(&destination)?.is_file() {
        bail!("unsafe copy destination");
    }
    // Atomic replacement avoids writing through an existing hard link too.
    // Recheck parents immediately before publishing (host-user races remain).
    use std::io::Write;
    let mut file = tempfile::NamedTempFile::new_in(destination.parent().context("copy parent")?)?;
    file.write_all(bytes)?;
    let destination = safe_destination(root, relative)?;
    file.persist(destination)
        .map_err(|_| anyhow::anyhow!("publish safe verification file"))?;
    Ok(())
}

fn export_tree(repo: &Path, tree: &str, target: &Path) -> anyhow::Result<()> {
    for entry in tree_entries(repo, tree)? {
        if safe_destination(target, &entry.path)?.exists() {
            bail!("colliding path alias in trusted export");
        }
        write_safe_file(target, &entry.path, &read_blob(repo, &entry.oid)?)?;
        set_git_mode(&target.join(entry.path), &entry.mode)?;
    }
    Ok(())
}

fn assert_snapshot_entries(
    root: &Path,
    dir: &Path,
    allowed: &BTreeSet<&str>,
) -> anyhow::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = safe_metadata(&path)?;
        let relative = path
            .strip_prefix(root)?
            .to_str()
            .context("oracle path encoding")?
            .replace('\\', "/");
        validate_protected_path(&relative)?;
        if meta.is_dir() {
            if !allowed
                .iter()
                .any(|file| file.starts_with(&format!("{relative}/")))
            {
                bail!("unexpected private oracle directory");
            }
            assert_snapshot_entries(root, &path, allowed)?;
        } else if !allowed.contains(relative.as_str()) {
            bail!("unexpected private oracle file");
        }
    }
    Ok(())
}

fn assert_export_safety(dir: &Path) -> anyhow::Result<()> {
    if !safe_metadata(dir)?.is_dir() {
        bail!("verification target replaced");
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("verification output path encoding")?;
        validate_protected_path(name)?;
        if safe_metadata(&path)?.is_dir() {
            assert_export_safety(&path)?;
        }
    }
    Ok(())
}

// Only called for explicit children of our private temporary directory. Refuse
// links before cleanup instead of traversing a command-created junction.
fn clear_directory(root: &Path, name: &str) -> anyhow::Result<()> {
    let path = safe_destination(root, name)?;
    if path.exists() {
        remove_safe_directory(&path)?;
    }
    std::fs::create_dir(path)?;
    Ok(())
}

fn remove_safe_directory(path: &Path) -> anyhow::Result<()> {
    if !safe_metadata(path)?.is_dir() {
        bail!("private verification directory replaced");
    }
    for entry in std::fs::read_dir(path)? {
        let path = entry?.path();
        if safe_metadata(&path)?.is_dir() {
            remove_safe_directory(&path)?;
        } else {
            std::fs::remove_file(path)?;
        }
    }
    std::fs::remove_dir(path)?;
    Ok(())
}

fn oracle_environment(root: &Path, repo: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    // No proxy credentials, provider keys, Git vars, PYTHONPATH, branch labels,
    // agent extensions or candidate env. Freeze tool lookup before repair and
    // remove relative/current-directory search entries.
    let vars = crate::env_store::vars();
    let mut env = BTreeMap::new();
    let lookup = |key: &str| {
        vars.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    };
    if let Some(path) = lookup("PATH") {
        let entries: Vec<_> = std::env::split_paths(path)
            .filter(|p| p.is_absolute())
            .filter_map(|p| std::fs::canonicalize(p).ok())
            .filter(|p| !p.starts_with(repo) && !p.starts_with(root))
            .collect();
        env.insert(
            "PATH".to_string(),
            std::env::join_paths(entries)?
                .into_string()
                .map_err(|_| anyhow::anyhow!("toolchain PATH encoding"))?,
        );
    }
    for key in ["SystemRoot", "WINDIR", "PATHEXT"] {
        if let Some(value) = lookup(key) {
            env.insert(key.to_string(), value.clone());
        }
    }
    for (key, value) in [
        ("NUWA_VERIFY_ROOT", root.join("target")),
        ("HOME", root.join("home")),
        ("USERPROFILE", root.join("home")),
        ("TMPDIR", root.join("tmp")),
        ("TMP", root.join("tmp")),
        ("TEMP", root.join("tmp")),
        ("XDG_CACHE_HOME", root.join("cache")),
    ] {
        env.insert(
            key.to_string(),
            value.to_str().context("private path encoding")?.to_string(),
        );
    }
    for (key, value) in [
        ("LC_ALL", "C"),
        ("LANG", "C"),
        ("TZ", "UTC"),
        ("PYTHONDONTWRITEBYTECODE", "1"),
        ("PYTHONNOUSERSITE", "1"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        (
            "GIT_CEILING_DIRECTORIES",
            root.to_str().context("private path encoding")?,
        ),
    ] {
        env.insert(key.to_string(), value.to_string());
    }
    Ok(env)
}

#[cfg(test)]
mod tests;
