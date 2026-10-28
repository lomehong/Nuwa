//! Nuwa's production local-tool environment. Filesystem access is rooted and
//! authorized before delegation, never filtered after reading file contents.
//! Shell deliberately delegates host-user privileges; this is NOT a sandbox.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use rpi_tools::env::check_cancel_file;
use rpi_tools::error::{io_to_file_error, ExecutionError, FileError, FileErrorCode};
use rpi_tools::file_mutation_queue::MutationQueueRegistry;
use rpi_tools::{
    ExecutionEnv, FileContent, FileInfo, FileSystem, MutatingEnv, OsExecutionEnv, Shell,
    ShellExecOptions, ShellOutput,
};
use tokio_util::sync::CancellationToken;

use super::path_policy::{denied, ordinary_path, Access, PathPolicy};

pub(crate) struct LocalExecutionEnv {
    inner: Arc<OsExecutionEnv>,
    policy: Arc<PathPolicy>,
    // Instrument the actual delegation boundary, not returned tool text.
    #[cfg(test)]
    delegated_reads: std::sync::Mutex<Vec<PathBuf>>,
    #[cfg(test)]
    delegated_read_count: std::sync::atomic::AtomicUsize,
}

impl LocalExecutionEnv {
    pub(crate) fn new(inner: Arc<OsExecutionEnv>, policy: Arc<PathPolicy>) -> Self {
        Self {
            inner,
            policy,
            #[cfg(test)]
            delegated_reads: std::sync::Mutex::new(Vec::new()),
            #[cfg(test)]
            delegated_read_count: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn authorize(
        &self,
        path: &str,
        access: Access,
        cancel: Option<&CancellationToken>,
    ) -> Result<PathBuf, FileError> {
        check_cancel_file(cancel, None)?;
        self.policy.authorize(path, access)
    }

    fn observe_read(&self, _path: &Path) {
        #[cfg(test)]
        {
            self.delegated_reads
                .lock()
                .expect("delegation log")
                .push(_path.to_path_buf());
            self.delegated_read_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Create one parent at a time, reauthorizing existing ancestors and newly
    /// created directories. The final open is rechecked after the last await.
    async fn prepare_parent(
        &self,
        path: &Path,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), FileError> {
        let parent = path.parent().ok_or_else(denied)?;
        let relative = parent
            .strip_prefix(self.policy.root())
            .map_err(|_| denied())?;
        let mut current = self.policy.root().to_path_buf();
        for component in relative.components() {
            current.push(component);
            self.authorize(&current.to_string_lossy(), Access::Write, cancel)?;
            match std::fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.is_dir() => {}
                Ok(_) => {
                    return Err(FileError::new(
                        FileErrorCode::NotDirectory,
                        "write parent is not a directory",
                    ))
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    self.authorize(&current.to_string_lossy(), Access::Write, cancel)?;
                    self.inner
                        .create_dir(&current.to_string_lossy(), false, cancel)
                        .await?;
                    self.authorize(&current.to_string_lossy(), Access::Write, cancel)?;
                }
                Err(e) => return Err(io_to_file_error(e, None)),
            }
        }
        Ok(())
    }

    /// Directory rename/removal must not mutate a protected descendant through
    /// its otherwise writable parent. Check the mapped destination tree too.
    fn authorize_tree(
        &self,
        source: &Path,
        destination: Option<&Path>,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), FileError> {
        let mut pending = vec![(source.to_path_buf(), destination.map(Path::to_path_buf))];
        while let Some((path, dest)) = pending.pop() {
            self.authorize(&path.to_string_lossy(), Access::Write, cancel)?;
            if let Some(dest) = &dest {
                self.authorize(&dest.to_string_lossy(), Access::Write, cancel)?;
            }
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() => {
                    for entry in std::fs::read_dir(&path).map_err(|e| io_to_file_error(e, None))? {
                        let entry = entry.map_err(|e| io_to_file_error(e, None))?;
                        let mapped = dest.as_ref().map(|d| d.join(entry.file_name()));
                        pending.push((entry.path(), mapped));
                    }
                }
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_to_file_error(e, None)),
            }
        }
        Ok(())
    }
}

#[async_trait]
impl FileSystem for LocalExecutionEnv {
    fn cwd(&self) -> &Path {
        self.policy.root()
    }

    async fn absolute_path(
        &self,
        path: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<PathBuf, FileError> {
        self.authorize(path, Access::Read, cancel)
    }

    async fn join_path(
        &self,
        parts: &[&str],
        cancel: Option<&CancellationToken>,
    ) -> Result<PathBuf, FileError> {
        let mut path = self.policy.root().to_path_buf();
        for part in parts {
            // Validate each part too: PathBuf::push may replace the entire path.
            self.authorize(part, Access::Read, cancel)?;
            path.push(part.replace('\\', "/"));
        }
        self.authorize(&path.to_string_lossy(), Access::Read, cancel)
    }

    async fn read_text_file(
        &self,
        path: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<String, FileError> {
        let path = self.authorize(path, Access::Read, cancel)?;
        self.observe_read(&path);
        self.inner
            .read_text_file(&path.to_string_lossy(), cancel)
            .await
    }

    async fn read_text_lines(
        &self,
        path: &str,
        max_lines: Option<usize>,
        cancel: Option<&CancellationToken>,
    ) -> Result<Vec<String>, FileError> {
        let path = self.authorize(path, Access::Read, cancel)?;
        self.observe_read(&path);
        self.inner
            .read_text_lines(&path.to_string_lossy(), max_lines, cancel)
            .await
    }

    async fn read_binary_file(
        &self,
        path: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<Vec<u8>, FileError> {
        let path = self.authorize(path, Access::Read, cancel)?;
        self.observe_read(&path);
        self.inner
            .read_binary_file(&path.to_string_lossy(), cancel)
            .await
    }

    async fn read_binary_file_range(
        &self,
        path: &str,
        offset: u64,
        len: usize,
        cancel: Option<&CancellationToken>,
    ) -> Result<Vec<u8>, FileError> {
        let path = self.authorize(path, Access::Read, cancel)?;
        self.observe_read(&path);
        self.inner
            .read_binary_file_range(&path.to_string_lossy(), offset, len, cancel)
            .await
    }

    async fn write_file(
        &self,
        path: &str,
        content: FileContent,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), FileError> {
        let path = self.authorize(path, Access::Write, cancel)?;
        self.prepare_parent(&path, cancel).await?;
        let path = self.authorize(&path.to_string_lossy(), Access::Write, cancel)?;
        self.inner
            .write_file(&path.to_string_lossy(), content, cancel)
            .await
    }

    async fn append_file(
        &self,
        path: &str,
        content: FileContent,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), FileError> {
        let path = self.authorize(path, Access::Write, cancel)?;
        self.prepare_parent(&path, cancel).await?;
        let path = self.authorize(&path.to_string_lossy(), Access::Write, cancel)?;
        self.inner
            .append_file(&path.to_string_lossy(), content, cancel)
            .await
    }

    async fn rename_file(
        &self,
        source: &str,
        dest: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), FileError> {
        let source = self.authorize(source, Access::Write, cancel)?;
        let dest = self.authorize(dest, Access::Write, cancel)?;
        self.authorize_tree(&source, Some(&dest), cancel)?;
        self.authorize_tree(&dest, None, cancel)?;
        // No await between the final endpoint checks and OS delegation.
        self.authorize(&source.to_string_lossy(), Access::Write, cancel)?;
        self.authorize(&dest.to_string_lossy(), Access::Write, cancel)?;
        self.inner
            .rename_file(&source.to_string_lossy(), &dest.to_string_lossy(), cancel)
            .await
    }

    async fn file_info(
        &self,
        path: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<FileInfo, FileError> {
        let path = self.authorize(path, Access::Read, cancel)?;
        self.inner.file_info(&path.to_string_lossy(), cancel).await
    }

    async fn list_dir(
        &self,
        path: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<Vec<FileInfo>, FileError> {
        let path = self.authorize(path, Access::Read, cancel)?;
        let mut entries = tokio::fs::read_dir(&path)
            .await
            .map_err(|e| io_to_file_error(e, None))?;
        let mut visible = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| io_to_file_error(e, None))?
        {
            check_cancel_file(cancel, None)?;
            let child = entry.path();
            let Some(child_str) = child.to_str() else {
                continue;
            };
            // Before even fetching metadata, exclude denied/ambiguous names.
            // Tools never see those names and cannot recurse into those dirs.
            match self.authorize(child_str, Access::Read, cancel) {
                Ok(_) => visible.push(self.file_info(child_str, cancel).await?),
                Err(e) if e.code == FileErrorCode::PermissionDenied => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(visible)
    }

    async fn canonical_path(
        &self,
        path: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<PathBuf, FileError> {
        let path = self.authorize(path, Access::Read, cancel)?;
        let canonical = ordinary_path(
            self.inner
                .canonical_path(&path.to_string_lossy(), cancel)
                .await?,
        )?;
        self.authorize(&canonical.to_string_lossy(), Access::Read, cancel)
    }

    async fn exists(
        &self,
        path: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<bool, FileError> {
        let path = self.authorize(path, Access::Read, cancel)?;
        self.inner.exists(&path.to_string_lossy(), cancel).await
    }

    async fn create_dir(
        &self,
        path: &str,
        recursive: bool,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), FileError> {
        let path = self.authorize(path, Access::Write, cancel)?;
        if recursive {
            self.prepare_parent(&path, cancel).await?;
        }
        let path = self.authorize(&path.to_string_lossy(), Access::Write, cancel)?;
        self.inner
            .create_dir(&path.to_string_lossy(), recursive, cancel)
            .await
    }

    async fn remove(
        &self,
        path: &str,
        recursive: bool,
        force: bool,
        cancel: Option<&CancellationToken>,
    ) -> Result<(), FileError> {
        let path = self.authorize(path, Access::Write, cancel)?;
        if recursive {
            self.authorize_tree(&path, None, cancel)?;
        }
        let path = self.authorize(&path.to_string_lossy(), Access::Write, cancel)?;
        self.inner
            .remove(&path.to_string_lossy(), recursive, force, cancel)
            .await
    }

    // OS temp creation allocates outside the checkout. No local file tool needs
    // it, so fail closed rather than creating an unauthorized temporary object.
    async fn create_temp_dir(
        &self,
        _prefix: Option<&str>,
        cancel: Option<&CancellationToken>,
    ) -> Result<PathBuf, FileError> {
        check_cancel_file(cancel, None)?;
        Err(denied())
    }

    async fn create_temp_file(
        &self,
        _prefix: &str,
        _suffix: &str,
        cancel: Option<&CancellationToken>,
    ) -> Result<PathBuf, FileError> {
        check_cancel_file(cancel, None)?;
        Err(denied())
    }

    async fn cleanup(&self) {
        FileSystem::cleanup(self.inner.as_ref()).await;
    }
}

#[async_trait]
impl Shell for LocalExecutionEnv {
    async fn exec<'a>(
        &'a self,
        command: &str,
        options: ShellExecOptions<'a>,
    ) -> Result<ShellOutput, ExecutionError> {
        // Deliberately NOT a sandbox: before_tool_hook remains a heuristic veto
        // for reserved Git/PR commands, not command or process confinement.
        self.inner.exec(command, options).await
    }

    async fn cleanup(&self) {
        Shell::cleanup(self.inner.as_ref()).await;
    }
}

#[async_trait]
impl ExecutionEnv for LocalExecutionEnv {}

impl MutatingEnv for LocalExecutionEnv {
    fn as_env(&self) -> &dyn ExecutionEnv {
        self
    }

    fn mutation_registry(&self) -> &MutationQueueRegistry {
        self.inner.mutation_registry()
    }
}

#[cfg(test)]
mod tests;
