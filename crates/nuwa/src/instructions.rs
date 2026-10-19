//! 仓库指令文件（ADR-006 三处语言相关位之三）— v0 machine-readable core.
//!
//! Lives in the target repository (`nuwa.repo.json` at the checkout root by
//! convention). The instruction file self-teaches the runner: which check
//! proves the bug, which paths are off-limits, and any repo-specific notes
//! for the repair prompt. `notes` is free text the model reads; everything
//! else is enforced by Nuwa, not trusted from the model.
//!
//! The file is runner configuration, not agent input. [`TrustedInstructions`]
//! refuses to keep trusting a working-tree copy after the agent could have
//! edited it: tracked configs are read from the base commit, untracked configs
//! from a runner-private copy, and every cycle restores the working file when
//! it drifts.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::git_ops;
use crate::issue::FailingCheck;

const INSTRUCTION_FILE: &str = "nuwa.repo.json";

fn default_max_attempts() -> u32 {
    3
}

#[derive(Debug, Clone, Deserialize)]
pub struct RepoInstructions {
    /// Identity in 山海经 (`Signal.repo`). The runner refuses to work an
    /// issue whose repo field does not match (ADR-004 防越权).
    pub repo: String,
    /// The check that currently fails and must pass after the fix.
    pub failing_check: FailingCheck,
    #[serde(default)]
    pub forbidden_paths: Vec<String>,
    /// Free-text repo-specific guidance appended to the runner system prompt.
    #[serde(default)]
    pub notes: String,
    /// Optional command that must fail on the base and pass on the branch.
    #[serde(default)]
    pub reproduce_check: Option<FailingCheck>,
    /// Consecutive issue-level failures before the issue is ignored. Provider
    /// outages and environment faults do not count.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// Open the first N PRs for this repo as draft (0 keeps ADR-003's direct
    /// non-draft behaviour).
    #[serde(default)]
    pub draft_first_n: u32,
}


#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustSource {
    /// The file is tracked in the base commit; that commit is authoritative.
    Tracked,
    /// The file is untracked; the runner-private copy is authoritative.
    PrivateCopy,
}

/// An instruction file plus the bytes the runner trusts. The working-tree file
/// is verified/restored against these bytes before every claim.
#[derive(Debug, Clone)]
pub struct TrustedInstructions {
    pub instructions: RepoInstructions,
    pub bytes: Vec<u8>,
    pub sha256: String,
    pub source: TrustSource,
    repo_dir: PathBuf,
    path: PathBuf,
    base: String,
    private_path: Option<PathBuf>,
}

impl TrustedInstructions {
    /// Load the authoritative instruction content:
    /// 1. tracked file at `origin/<base>` (or local `<base>` for repos without
    ///    an origin), else
    /// 2. a runner-private copy, seeded from the working tree on first use.
    pub fn load(
        repo_dir: &Path,
        path: &Path,
        base: &str,
        store_dir: &Path,
    ) -> anyhow::Result<Self> {
        let repo_dir = std::path::absolute(repo_dir)
            .with_context(|| format!("resolve repo dir {}", repo_dir.display()))?;
        let path = std::path::absolute(path)
            .with_context(|| format!("resolve instructions {}", path.display()))?;

        if let Some((bytes, source)) = read_tracked(&repo_dir, base)? {
            let instructions = parse(&bytes, &path)?;
            return Ok(Self {
                sha256: sha256_hex(&bytes),
                instructions,
                bytes,
                source,
                repo_dir,
                path,
                base: base.to_string(),
                private_path: None,
            });
        }

        let private_path = private_path(store_dir, &repo_dir);
        let bytes = if private_path.exists() {
            std::fs::read(&private_path)
                .with_context(|| format!("read trusted instructions {}", private_path.display()))?
        } else {
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read instructions {}", path.display()))?;
            if let Some(parent) = private_path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("create {}", parent.display()))?;
            }
            std::fs::write(&private_path, &bytes)
                .with_context(|| format!("seed trusted instructions {}", private_path.display()))?;
            bytes
        };
        let instructions = parse(&bytes, &path)?;
        Ok(Self {
            sha256: sha256_hex(&bytes),
            instructions,
            bytes,
            source: TrustSource::PrivateCopy,
            repo_dir,
            path,
            base: base.to_string(),
            private_path: Some(private_path),
        })
    }

    /// Restore the working-tree file if it no longer matches the authoritative
    /// bytes. Returns `true` when a write happened.
    pub fn verify_and_restore(&self) -> anyhow::Result<bool> {
        let expected = self.expected_bytes();
        let current = std::fs::read(&self.path).ok();
        if current.as_deref() == Some(expected.as_slice()) {
            return Ok(false);
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&self.path, &expected)
            .with_context(|| format!("restore instructed {}", self.path.display()))?;
        Ok(true)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn source(&self) -> TrustSource {
        self.source
    }

    fn expected_bytes(&self) -> Vec<u8> {
        if self.source == TrustSource::Tracked {
            if let Ok(Some((bytes, _))) = read_tracked(&self.repo_dir, &self.base) {
                return bytes;
            }
        }
        if let Some(private) = &self.private_path {
            if let Ok(bytes) = std::fs::read(private) {
                return bytes;
            }
        }
        self.bytes.clone()
    }
}

fn read_tracked(repo_dir: &Path, base: &str) -> anyhow::Result<Option<(Vec<u8>, TrustSource)>> {
    for reference in [format!("origin/{base}"), base.to_string()] {
        let spec = format!("{reference}:{INSTRUCTION_FILE}");
        if let Ok(text) = git_ops::git(repo_dir, &["show", &spec]) {
            return Ok(Some((text.into_bytes(), TrustSource::Tracked)));
        }
    }
    if git_ops::git(repo_dir, &["ls-files", "--error-unmatch", INSTRUCTION_FILE]).is_ok() {
        // Tracked in the index but not readable from the base commit (e.g. a
        // fresh repo with no commit): do not silently trust the worktree.
        bail!("{INSTRUCTION_FILE} is tracked but not readable from {base}");
    }
    Ok(None)
}

fn parse(bytes: &[u8], path: &Path) -> anyhow::Result<RepoInstructions> {
    serde_json::from_slice(bytes)
        .with_context(|| format!("parse repo instructions JSON from {}", path.display()))
}

fn private_path(store_dir: &Path, repo_dir: &Path) -> PathBuf {
    let id = sha256_hex(repo_dir.to_string_lossy().as_bytes());
    store_dir.join(format!("instructions-{}.json", &id[..12]))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn init_repo(dir: &Path, tracked: bool) {
        std::fs::create_dir_all(dir).expect("repo");
        git(dir, &["init", "-b", "main"]);
        git(dir, &["config", "user.name", "test"]);
        git(dir, &["config", "user.email", "test@example.com"]);
        std::fs::write(dir.join("f.txt"), "one\n").expect("write");
        if tracked {
            std::fs::write(dir.join(INSTRUCTION_FILE), config_json("main")).expect("write");
        }
        git(dir, &["add", "."]);
        git(dir, &["commit", "-m", "init"]);
        if !tracked {
            std::fs::write(dir.join(INSTRUCTION_FILE), config_json("main")).expect("write");
        }
    }

    fn config_json(repo: &str) -> String {
        format!(
            r#"{{"repo":"{repo}","failing_check":{{"command":"echo ok"}},"forbidden_paths":["go.sum"]}}"#
        )
    }

    #[test]
    fn tracked_config_is_read_from_base_not_worktree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo, true);
        // The agent edits the working-tree copy.
        std::fs::write(repo.join(INSTRUCTION_FILE), config_json("evil")).expect("write");

        let trusted = TrustedInstructions::load(
            &repo,
            &repo.join(INSTRUCTION_FILE),
            "main",
            &dir.path().join("trust"),
        )
        .expect("load");
        assert_eq!(trusted.source, TrustSource::Tracked);
        assert_eq!(trusted.instructions.repo, "main");
        assert!(trusted.verify_and_restore().expect("restore"));
        let restored = std::fs::read_to_string(repo.join(INSTRUCTION_FILE)).expect("read");
        assert!(restored.contains("\"repo\":\"main\""));
    }

    #[test]
    fn untracked_config_is_copied_to_private_store() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo, false);
        let store = dir.path().join("trust");

        let trusted =
            TrustedInstructions::load(&repo, &repo.join(INSTRUCTION_FILE), "main", &store)
                .expect("load");
        assert_eq!(trusted.source, TrustSource::PrivateCopy);
        assert_eq!(
            std::fs::read_dir(&store).expect("store").count(),
            1,
            "private copy must be seeded"
        );

        std::fs::write(repo.join(INSTRUCTION_FILE), config_json("evil")).expect("write");
        assert!(trusted.verify_and_restore().expect("restore"));
        let restored = std::fs::read_to_string(repo.join(INSTRUCTION_FILE)).expect("read");
        assert!(restored.contains("\"repo\":\"main\""));
    }

    #[test]
    fn invalid_json_is_an_error_with_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo, false);
        std::fs::write(repo.join(INSTRUCTION_FILE), "{not json").expect("write");
        let err = TrustedInstructions::load(
            &repo,
            &repo.join(INSTRUCTION_FILE),
            "main",
            &dir.path().join("trust"),
        )
        .expect_err("invalid config");
        assert!(format!("{err:#}").contains("parse repo instructions JSON"));
    }

    #[test]
    fn config_defaults_are_applied() {
        let raw = r#"{"repo":"x","failing_check":{"command":"true"}}"#;
        let config: RepoInstructions = serde_json::from_str(raw).expect("parse");
        assert_eq!(config.max_attempts, 3);
        assert_eq!(config.draft_first_n, 0);
        assert!(config.reproduce_check.is_none());
    }
}
