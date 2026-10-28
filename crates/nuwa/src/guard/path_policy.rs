//! Canonical checkout policy. Authorization precedes I/O, but path-based
//! rechecks cannot prevent a concurrent host process from swapping an ancestor.
//! Hardlinks also share file identity without a link/reparse marker; this
//! pathname policy does not provide inode-level isolation from host processes.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context};
use globset::{GlobBuilder, GlobMatcher};
use rpi_tools::error::{io_to_file_error, FileError, FileErrorCode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Access {
    Read,
    Write,
}

/// Separate write-only protection from confidential paths. All links/reparse
/// points are refused, including in-root links: no alias may bypass patterns.
#[derive(Debug, Clone)]
pub struct PathPolicy {
    root: PathBuf,
    forbidden: Vec<GlobMatcher>,
    denied: Vec<GlobMatcher>,
    private: Vec<PathBuf>,
}

const DENIED: &[&str] = &[
    ".env",
    ".env.*",
    "*.env",
    ".envrc",
    ".ssh",
    ".aws",
    ".azure",
    ".kube",
    "**/.config/gcloud",
    "**/.docker/config.json",
    ".git",
    ".gitconfig",
    ".git-credentials",
    ".netrc",
    "_netrc",
    ".npmrc",
    ".pypirc",
    "credentials",
    "credentials.*",
    "id_rsa*",
    "id_ed25519*",
    "id_ecdsa*",
    "id_dsa*",
    "authorized_keys",
    "private_key*",
    "*.pem",
    "*.key",
    "*.ppk",
    "*.p12",
    "*.pfx",
    "*.keystore",
    "secrets.json",
    "service-account*.json",
    "**/.nuwa/private",
    "**/.nuwa/oracle",
    "**/.nuwa/verification",
    "**/.nuwa/sessions",
    ".nuwa-private",
    "instructions-*.json",
    "sessions",
    "shanhaijing.db*",
];

impl PathPolicy {
    pub(super) fn new(
        root: &Path,
        forbidden: &[String],
        denied: &[String],
    ) -> anyhow::Result<Self> {
        let root = checkout_root(root)?;
        let defaults: Vec<String> = DENIED.iter().map(|s| (*s).to_string()).collect();
        let mut matchers = compile(&defaults)?;
        matchers.extend(compile(denied)?);
        Ok(Self {
            root,
            forbidden: compile(forbidden)?,
            denied: matchers,
            private: Vec::new(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn check_root(&self, root: &Path) -> anyhow::Result<()> {
        if !same_path(&self.root, &checkout_root(root)?) {
            bail!("guard checkout root does not match repair checkout");
        }
        Ok(())
    }

    pub(super) fn deny_private_path(&mut self, path: &Path) -> anyhow::Result<()> {
        if let Some(relative) = relative_to(path, &self.root) {
            self.private.push(relative);
        }
        // A runner location outside the checkout may itself be an alias into
        // it. Resolve the nearest existing ancestor even before session parent
        // directories exist, then protect the physical destination as well.
        let mut existing = std::path::absolute(path).context("resolve private runner path")?;
        let mut missing = Vec::new();
        let mut physical = loop {
            match std::fs::canonicalize(&existing) {
                Ok(path) => break ordinary_path(path)?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    missing.push(
                        existing
                            .file_name()
                            .context("invalid private runner path")?
                            .to_owned(),
                    );
                    if !existing.pop() {
                        bail!("private runner path has no existing ancestor");
                    }
                }
                Err(e) => return Err(e).context("resolve private runner ancestor"),
            }
        };
        for component in missing.into_iter().rev() {
            physical.push(component);
        }
        if let Some(relative) = relative_to(&physical, &self.root) {
            self.private.push(relative);
        }
        Ok(())
    }

    pub(super) fn authorize(&self, raw: &str, access: Access) -> Result<PathBuf, FileError> {
        let path = self.resolve(raw)?;
        let relative = relative_to(&path, &self.root).ok_or_else(denied)?;
        if relative.as_os_str().is_empty()
            && self
                .denied
                .iter()
                .any(|g| g.is_match("") || g.is_match("."))
        {
            return Err(denied());
        }
        let mut prefix = PathBuf::new();
        // Check every prefix: a denied directory cannot expose its children,
        // including when the configured pattern does not end with '/'.
        for component in relative.components() {
            prefix.push(component);
            let name = match_name(&prefix.to_string_lossy());
            if self.denied.iter().any(|g| g.is_match(&name))
                || (access == Access::Write && self.forbidden.iter().any(|g| g.is_match(&name)))
            {
                return Err(denied());
            }
        }
        if self
            .private
            .iter()
            .any(|p| relative_to(&relative, p).is_some())
            || (access == Access::Write && relative.as_os_str().is_empty())
        {
            return Err(denied());
        }
        // Include ancestors above the root, so replacing a checkout ancestor
        // with a junction/symlink after construction also fails closed.
        inspect_ancestors(&path)?;
        Ok(path)
    }

    fn resolve(&self, raw: &str) -> Result<PathBuf, FileError> {
        let slash = validate_spelling(raw)?;
        let supplied = Path::new(&slash);
        let path = if supplied.is_absolute() {
            supplied.to_path_buf()
        } else {
            self.root.join(supplied)
        };
        // Use components, not string-prefix matching (repo != repo-sibling).
        let relative = relative_to(&path, &self.root).ok_or_else(denied)?;
        Ok(self.root.join(relative))
    }
}

fn compile(patterns: &[String]) -> anyhow::Result<Vec<GlobMatcher>> {
    let mut result = Vec::new();
    for pattern in patterns {
        let pattern = match_name(pattern.trim());
        if pattern.is_empty() {
            continue;
        }
        let mut candidates = vec![pattern.clone()];
        if pattern.ends_with('/') {
            // Match the directory itself as well as final-diff descendants.
            candidates.push(pattern.trim_end_matches('/').to_string());
            candidates.push(format!("{pattern}**"));
        } else if !pattern.contains('/') {
            candidates.push(format!("**/{pattern}"));
        }
        for candidate in candidates {
            result.push(
                GlobBuilder::new(&candidate)
                    .literal_separator(false)
                    .case_insensitive(cfg!(windows))
                    .build()
                    .with_context(|| format!("invalid guard glob `{candidate}`"))?
                    .compile_matcher(),
            );
        }
    }
    Ok(result)
}

fn checkout_root(root: &Path) -> anyhow::Result<PathBuf> {
    let spelling = validate_spelling(root.to_str().context("non-Unicode guard root")?)?;
    let root = std::path::absolute(spelling).context("resolve guard root")?;
    inspect_ancestors(&root).context("unsafe guard root")?;
    let canonical =
        ordinary_path(std::fs::canonicalize(&root).context("canonicalize guard root")?)?;
    // canonicalize returns a verbatim disk prefix on Windows. Keep only the
    // ordinary disk spelling internally; device/UNC input is never accepted.
    if !canonical.is_dir() {
        bail!("guard root must be a directory");
    }
    let policy_root = canonical.to_str().context("non-Unicode guard root")?;
    if policy_root.replace('\\', "/").starts_with("//") {
        bail!("UNC guard roots are not supported");
    }
    Ok(canonical)
}

pub(super) fn ordinary_path(path: PathBuf) -> Result<PathBuf, FileError> {
    #[cfg(windows)]
    {
        let text = path.to_str().ok_or_else(denied)?;
        let ordinary = text.strip_prefix(r"\\?\").unwrap_or(text);
        // Only canonical disk prefixes may lose the verbatim marker. In
        // particular, never reinterpret a verbatim UNC/device result as a
        // relative checkout path.
        let bytes = ordinary.as_bytes();
        if bytes.len() < 3
            || !bytes[0].is_ascii_alphabetic()
            || bytes[1] != b':'
            || !matches!(bytes[2], b'\\' | b'/')
        {
            return Err(denied());
        }
        Ok(PathBuf::from(ordinary))
    }
    #[cfg(not(windows))]
    {
        Ok(path)
    }
}

fn relative_to(path: &Path, root: &Path) -> Option<PathBuf> {
    let mut path = path.components();
    for expected in root.components() {
        let actual = path.next()?;
        if !same_path(
            Path::new(actual.as_os_str()),
            Path::new(expected.as_os_str()),
        ) {
            return None;
        }
    }
    Some(path.collect())
}

fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) {
        a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase()
    } else {
        a == b
    }
}

fn match_name(name: &str) -> String {
    let name = name.replace('\\', "/");
    if cfg!(windows) {
        name.to_lowercase()
    } else {
        name
    }
}

fn validate_spelling(raw: &str) -> Result<String, FileError> {
    if raw.is_empty() || raw.starts_with('~') || raw.contains('\0') || raw.contains("://") {
        return Err(denied());
    }
    let slash = raw.replace('\\', "/");
    // UNC, Win32/NT device paths, and root-relative drive aliases.
    if slash.starts_with("//") || slash.starts_with("/??/") {
        return Err(denied());
    }
    let bytes = slash.as_bytes();
    let drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if drive && (bytes.get(2) != Some(&b'/') || !cfg!(windows)) {
        return Err(denied());
    }
    if cfg!(windows) && slash.starts_with('/') {
        return Err(denied());
    }
    let body = if drive {
        &slash[3..]
    } else {
        slash.trim_start_matches('/')
    };
    for part in body.split('/') {
        if part == ".." || part.contains(':') || part.chars().any(char::is_control) {
            return Err(denied());
        }
        if !part.is_empty() && part != "." {
            // Reject ambiguous Win32 names on every host. Windows short names
            // are rejected rather than expanding them after pattern checks.
            if part.ends_with(['.', ' ']) || part.contains(['<', '>', '"', '|', '?', '*']) {
                return Err(denied());
            }
            let stem = part
                .split('.')
                .next()
                .unwrap_or(part)
                .trim_end_matches(' ')
                .to_ascii_uppercase();
            let numbered_device = stem
                .strip_prefix("COM")
                .or_else(|| stem.strip_prefix("LPT"))
                .is_some_and(|n| {
                    matches!(
                        n,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                });
            if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str())
                || numbered_device
                || (cfg!(windows) && part.contains('~'))
            {
                return Err(denied());
            }
        }
    }
    Ok(slash)
}

fn inspect_ancestors(path: &Path) -> Result<(), FileError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(denied());
        }
        current.push(component);
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                #[cfg(windows)]
                let reparse = {
                    use std::os::windows::fs::MetadataExt;
                    metadata.file_attributes() & 0x400 != 0
                };
                #[cfg(not(windows))]
                let reparse = false;
                if reparse
                    || metadata.file_type().is_symlink()
                    || (!metadata.is_file() && !metadata.is_dir())
                {
                    return Err(denied());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(io_to_file_error(e, None)),
        }
    }
    Ok(())
}

pub(super) fn denied() -> FileError {
    // Do not echo denied descendant names into recursive tool errors/results.
    FileError::new(
        FileErrorCode::PermissionDenied,
        "guard: path access denied by checkout policy",
    )
}
