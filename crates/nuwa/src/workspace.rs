//! The checkout one repair edits.
//!
//! The single-run loop borrows the primary checkout (`Direct`). A future
//! in-process pool gives each run its own detached git worktree so concurrent
//! fetch/reset/repair cannot clobber another run. This module owns only the
//! checkout lifecycle; splitting config/skills roots from the editable path is
//! the next step before the pool can use `Worktree` in production.

use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::{git_ops, issue};

pub struct RunWorkspace {
    repo: PathBuf,
    path: PathBuf,
    owned: bool,
}

impl RunWorkspace {
    /// Edit the primary checkout (current single-run behavior).
    pub fn direct(repo: &Path) -> Self {
        Self {
            repo: repo.to_path_buf(),
            path: repo.to_path_buf(),
            owned: false,
        }
    }

    /// Create a detached worktree at `base`. Drop removes only an unchanged,
    /// detached checkout; repair evidence is retained for owner review.
    ///
    /// Not used by the single-run loop yet: the pool first needs the pipeline
    /// to read skills/config from the primary checkout while editing this one.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn worktree(repo: &Path, base: &str, fingerprint: &str) -> anyhow::Result<Self> {
        let reference =
            if git_ops::git(repo, &["rev-parse", "--verify", &format!("origin/{base}")]).is_ok() {
                format!("origin/{base}")
            } else {
                base.to_string()
            };
        let path = std::env::temp_dir().join(format!(
            "nuwa-run-{}-{}",
            issue::short_fingerprint(fingerprint),
            uuid::Uuid::new_v4().simple()
        ));
        let path_str = path.to_str().context("run worktree path is not UTF-8")?;
        git_ops::git(repo, &["worktree", "add", "--detach", path_str, &reference])
            .with_context(|| format!("create run worktree at {}", path.display()))?;
        Ok(Self {
            repo: repo.to_path_buf(),
            path,
            owned: true,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for RunWorkspace {
    fn drop(&mut self) {
        if !self.owned {
            return;
        }
        // Never let pool teardown bypass delivery's evidence-preserving guard.
        // A clean agent branch can still identify an unfinished remote effect.
        if !git_ops::current_branch(&self.path).is_ok_and(|branch| branch == "HEAD") {
            return;
        }
        if let Some(path) = self.path.to_str() {
            // Git refuses dirty/untracked worktrees without --force. A refusal
            // is evidence to retain, not permission for recursive deletion.
            let _ = git_ops::git(&self.repo, &["worktree", "remove", path]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
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

    #[test]
    fn direct_borrows_without_cleanup() {
        let dir = tempfile::tempdir().expect("tempdir");
        init_repo(dir.path());
        let workspace = RunWorkspace::direct(dir.path());
        assert_eq!(workspace.path(), dir.path());
        drop(workspace);
        assert!(dir.path().join("base.txt").exists());
    }

    #[test]
    fn worktree_is_detached_and_removed_on_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        let workspace = RunWorkspace::worktree(&repo, "main", "fp-abcdef").expect("worktree");
        let path = workspace.path().to_path_buf();
        assert!(path.exists());
        assert!(path.join("base.txt").exists());
        assert_ne!(path, repo);

        let name = path
            .file_name()
            .expect("worktree name")
            .to_string_lossy()
            .to_string();
        drop(workspace);
        assert!(!path.exists());
        let list = git_ops::git(&repo, &["worktree", "list", "--porcelain"]).expect("list");
        assert!(
            !list.contains(&name),
            "removed worktree still registered: {list}"
        );
    }

    #[test]
    fn drop_preserves_dirty_or_attached_worktree_evidence() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path().join("repo");
        init_repo(&repo);
        for attached in [false, true] {
            let workspace = RunWorkspace::worktree(&repo, "main", "evidence").expect("worktree");
            let path = workspace.path().to_path_buf();
            if attached {
                git(&path, &["checkout", "-b", "agent/fix/evidence"]);
            } else {
                std::fs::write(path.join("only-worktree.txt"), "repair evidence").expect("write");
                assert!(!repo.join("only-worktree.txt").exists());
            }
            drop(workspace);
            assert!(path.join("base.txt").is_file());
            if !attached {
                assert!(path.join("only-worktree.txt").is_file());
            }
            // These are synthetic fixtures owned by this test, not recovery.
            git(
                &repo,
                &[
                    "worktree",
                    "remove",
                    "--force",
                    path.to_str().expect("path"),
                ],
            );
        }
    }
}
