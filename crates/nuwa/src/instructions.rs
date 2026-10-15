//! 仓库指令文件（ADR-006 三处语言相关位之三）— v0 machine-readable core.
//!
//! Lives in the target repository (`nuwa.repo.json` at the checkout root by
//! convention). The instruction file self-teaches the runner: which check
//! proves the bug, which paths are off-limits, and any repo-specific notes
//! for the repair prompt. `notes` is free text the model reads; everything
//! else is enforced by Nuwa, not trusted from the model.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

use crate::issue::FailingCheck;

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
}

impl RepoInstructions {
    /// Load from `<local-checkout>/nuwa.repo.json`; the checkout root is the
    /// file's parent directory.
    pub fn load(path: &Path) -> anyhow::Result<(Self, PathBuf)> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("read instructions {}", path.display()))?;
        let instructions: RepoInstructions =
            serde_json::from_str(&raw).context("parse repo instructions JSON")?;
        let repo_dir = path
            .parent()
            .map(Path::to_path_buf)
            .context("instructions path has no parent")?;
        Ok((instructions, repo_dir))
    }
}
