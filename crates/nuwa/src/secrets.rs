//! Secret handling for the repair runner.
//!
//! The work process loads the credentials it needs, removes them from
//! `std::env`, and passes them explicitly to the provider/PR/CI call sites.
//! Agent tools and verification subprocesses therefore inherit nothing
//! sensitive. Output that leaves the process (PR body, evidence, transcripts)
//! is redacted as a second layer.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{Context, Result};
use regex::Regex;

/// Keys the work process never passes to a child. `NUWA_UI_TOKEN` and
/// `NUWA_INGEST_TOKEN` are serve-only but may be present in the shared env
/// file; they are captured for redaction and removed from the work process.
pub const KNOWN_SECRET_KEYS: &[&str] = &[
    "NUWA_GITHUB_TOKEN",
    "GITHUB_TOKEN",
    "ZAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "NUWA_UI_TOKEN",
    "NUWA_INGEST_TOKEN",
    "NUWA_WEBHOOK_SECRET",
];

/// Environment keys that a build/test command legitimately needs. This is a
/// whitelist, not a blocklist: anything not listed (including future secrets)
/// is dropped.
const SAFE_ENV: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "TMPDIR",
    "TMP",
    "TEMP",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "CommonProgramFiles",
    "SystemRoot",
    "SystemDrive",
    "ComSpec",
    "PATHEXT",
    "WINDIR",
    "GOCACHE",
    "GOMODCACHE",
    "GOPATH",
    "GOROOT",
    "GOTOOLCHAIN",
    "GOPROXY",
    "GOSUMDB",
    "GONOSUMDB",
    "GOPRIVATE",
    "GONOPROXY",
    "GOFLAGS",
    "GOVERSION",
    "GOENV",
    "GOTMPDIR",
    "CGO_ENABLED",
    "CC",
    "CXX",
    "PKG_CONFIG_PATH",
    "LD_LIBRARY_PATH",
    "DYLD_LIBRARY_PATH",
    "LIBRARY_PATH",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTFLAGS",
    "JAVA_HOME",
    "MAVEN_HOME",
    "GRADLE_HOME",
    "NODE_PATH",
    "NPM_CONFIG_CACHE",
    "PNPM_HOME",
    "YARN_CACHE_FOLDER",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
];

#[derive(Debug, Clone, Default)]
pub struct Secrets {
    pub github_token: Option<String>,
    pub zai_key: Option<String>,
    pub anthropic_key: Option<String>,
    redact_values: Vec<String>,
}

impl Secrets {
    /// Read known credentials, remove them from the process environment, and
    /// keep their values for redaction. Call once at process startup, before
    /// any agent tool or provider is constructed.
    pub fn load_and_scrub_env() -> Self {
        let mut vars: BTreeMap<String, String> = std::env::vars().collect();
        let secrets = Self::from_vars(&mut vars);
        for key in KNOWN_SECRET_KEYS {
            std::env::remove_var(key);
        }
        secrets
    }

    fn from_vars(vars: &mut BTreeMap<String, String>) -> Self {
        let mut redact_values = Vec::new();
        let mut take = |key: &str, redact: &mut Vec<String>| -> Option<String> {
            let value = vars.remove(key).filter(|value| !value.trim().is_empty());
            if let Some(value) = &value {
                redact.push(value.clone());
            }
            value
        };
        let github_token = take("NUWA_GITHUB_TOKEN", &mut redact_values)
            .or_else(|| take("GITHUB_TOKEN", &mut redact_values));
        // Consume the alias too, so both spellings are redacted and neither is
        // inherited by a child.
        let _ = take("GITHUB_TOKEN", &mut redact_values);
        let zai_key = take("ZAI_API_KEY", &mut redact_values);
        let anthropic_key = take("ANTHROPIC_API_KEY", &mut redact_values);
        let _ = take("NUWA_UI_TOKEN", &mut redact_values);
        let _ = take("NUWA_INGEST_TOKEN", &mut redact_values);
        let _ = take("NUWA_WEBHOOK_SECRET", &mut redact_values);
        // Proxy URLs in the child-env allowlist may embed credentials
        // (`http://user:password@host`). The build/test child legitimately needs
        // the value, so it is not removed — but the password must never surface
        // in a transcript, PR body or evidence. Capture it for redaction.
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "GOPROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            if let Some(value) = vars.get(key) {
                redact_values.extend(url_passwords(value));
            }
        }
        redact_values.sort_by_key(|value| std::cmp::Reverse(value.len()));
        redact_values.dedup();
        Self {
            github_token,
            zai_key,
            anthropic_key,
            redact_values,
        }
    }

    #[cfg(test)]
    pub fn with_values(values: Vec<String>) -> Self {
        Self {
            redact_values: values,
            ..Self::default()
        }
    }

    pub fn redact(&self, text: &str) -> String {
        let mut out = text.to_string();
        for value in &self.redact_values {
            // Very short secrets would corrupt ordinary prose; the settings
            // UI already refuses to echo them.
            if value.len() >= 6 {
                out = out.replace(value, "[REDACTED]");
            }
        }
        for pattern in secret_patterns() {
            out = pattern.replace_all(&out, "[REDACTED]").into_owned();
        }
        out
    }

    /// Rewrite `.jsonl` transcripts in `dir` after a run. This is deliberately
    /// post-hoc: the harness writes the transcript, and a tool output may have
    /// contained a secret before redaction existed.
    pub fn redact_session_dir(&self, dir: &Path) -> Result<()> {
        if self.redact_values.is_empty() {
            return Ok(());
        }
        redact_dir(self, dir)
    }
}

fn redact_dir(secrets: &Secrets, dir: &Path) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("read session dir {}", dir.display()))
        }
    };
    for entry in entries {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if file_type.is_dir() {
            redact_dir(secrets, &path)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let redacted = secrets.redact(&text);
        if redacted != text {
            std::fs::write(&path, redacted)
                .with_context(|| format!("redact transcript {}", path.display()))?;
        }
    }
    Ok(())
}

/// Build the child-process environment from the whitelist plus explicit
/// `NUWA_AGENT_ENV_ALLOW` additions. Known secrets are never admitted.
pub fn agent_env_allowlist() -> BTreeMap<String, String> {
    let vars: BTreeMap<String, String> = std::env::vars().collect();
    build_allowlist(&vars)
}

fn build_allowlist(vars: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    // Windows environment names are case-insensitive, but `std::env::vars()`
    // preserves the parent's spelling (Git Bash/MSYS exports `SYSTEMROOT`).
    // An exact lookup for the canonical `SystemRoot` would silently drop it and
    // break Windows children that need it (Node's test-runner children fail in
    // crypto init). Match case-insensitively and emit the canonical spelling.
    let normalized: BTreeMap<String, &String> = vars
        .iter()
        .map(|(key, value)| (key.to_ascii_lowercase(), value))
        .collect();
    let lookup = |key: &str| {
        normalized
            .get(&key.to_ascii_lowercase())
            .map(|value| (*value).clone())
    };
    let mut allow = BTreeMap::new();
    for key in SAFE_ENV {
        if let Some(value) = lookup(key) {
            allow.insert((*key).to_string(), value);
        }
    }
    if let Some(extra) = vars.get("NUWA_AGENT_ENV_ALLOW") {
        for key in extra
            .split(',')
            .map(str::trim)
            .filter(|key| !key.is_empty())
        {
            if KNOWN_SECRET_KEYS.contains(&key) {
                continue;
            }
            if let Some(value) = lookup(key) {
                allow.insert(key.to_string(), value);
            }
        }
    }
    allow
}

fn secret_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            r"ghp_[A-Za-z0-9]{20,}",
            r"github_pat_[A-Za-z0-9_]{20,}",
            r"sk-ant-[A-Za-z0-9_-]{20,}",
            r"sk-[A-Za-z0-9]{20,}",
            r"xox[baprs]-[A-Za-z0-9-]{10,}",
            r"(?i)bearer\s+[A-Za-z0-9._\-+/=]{20,}",
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("valid secret pattern"))
        .collect()
    })
}

/// Extract the password from any `scheme://user:password@host` userinfo in a
/// proxy URL, so it can be redacted from persisted output. Values without
/// userinfo (the common `http://host:port`) yield nothing.
fn url_passwords(value: &str) -> Vec<String> {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"[a-zA-Z][a-zA-Z0-9+.\-]*://([^/@\s]+)@").expect("valid userinfo pattern")
    });
    re.captures_iter(value)
        .filter_map(|cap| {
            let userinfo = cap.get(1)?.as_str();
            userinfo
                .rsplit_once(':')
                .map(|(_, password)| password.to_string())
                .filter(|password| !password.is_empty())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn from_vars_reads_known_keys_and_consumes_them() {
        let mut vars = vars(&[
            ("NUWA_GITHUB_TOKEN", "ghp_abcdefghijklmnopqrstuvwx1234"),
            ("ZAI_API_KEY", "zai-secret-value-123456"),
            ("PATH", "/usr/bin"),
        ]);
        let secrets = Secrets::from_vars(&mut vars);
        assert_eq!(
            secrets.github_token.as_deref(),
            Some("ghp_abcdefghijklmnopqrstuvwx1234")
        );
        assert_eq!(secrets.zai_key.as_deref(), Some("zai-secret-value-123456"));
        assert!(!vars.contains_key("NUWA_GITHUB_TOKEN"));
        assert!(!vars.contains_key("ZAI_API_KEY"));
        assert!(secrets
            .redact("token ghp_abcdefghijklmnopqrstuvwx1234")
            .contains("[REDACTED]"));
    }

    #[test]
    fn proxy_url_password_is_captured_and_redacted() {
        // The child keeps a working proxy value, but an embedded password must
        // never reach a transcript / PR body / evidence.
        let mut vars = vars(&[("HTTPS_PROXY", "http://alice:supersecret@proxy.corp:8080")]);
        let secrets = Secrets::from_vars(&mut vars);
        // The proxy value itself is still allowed through to the child.
        assert_eq!(
            vars.get("HTTPS_PROXY").map(String::as_str),
            Some("http://alice:supersecret@proxy.corp:8080")
        );
        let redacted = secrets.redact("dialing http://alice:supersecret@proxy.corp:8080");
        assert!(!redacted.contains("supersecret"), "{redacted}");
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn url_passwords_extracts_only_userinfo_passwords() {
        assert_eq!(
            url_passwords("http://user:pw123456@host:8080"),
            vec!["pw123456".to_string()]
        );
        assert!(url_passwords("http://host:8080").is_empty());
        assert!(url_passwords("https://corp.proxy:3128").is_empty());
        // A comma-separated GOPROXY list yields each credential.
        assert_eq!(
            url_passwords("https://a:pw1234@x,http://b:pw5678@y"),
            vec!["pw1234".to_string(), "pw5678".to_string()]
        );
    }

    #[test]
    fn redact_replaces_exact_values_and_common_patterns() {
        let secrets = Secrets {
            redact_values: vec!["supersecret-value".to_string()],
            ..Secrets::default()
        };
        let text = "a supersecret-value b sk-ant-abcdefghijklmnopqrstuvwxyz c Bearer abcdefghijklmnopqrstuvwxyz123456";
        let out = secrets.redact(text);
        assert!(!out.contains("supersecret-value"));
        assert!(!out.contains("sk-ant-abcdefghijklmnopqrstuvwxyz"));
        assert!(!out.contains("abcdefghijklmnopqrstuvwxyz123456"));
        assert_eq!(out.matches("[REDACTED]").count(), 3);
    }

    #[test]
    fn allowlist_excludes_secrets_and_honours_explicit_additions() {
        let vars = vars(&[
            ("PATH", "/usr/bin"),
            ("ZAI_API_KEY", "secret"),
            ("NUWA_AGENT_ENV_ALLOW", "ZAI_API_KEY,MY_BUILD_FLAG"),
            ("MY_BUILD_FLAG", "1"),
            ("SSH_AUTH_SOCK", "/tmp/agent.sock"),
        ]);
        let allow = build_allowlist(&vars);
        assert_eq!(allow.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(allow.get("MY_BUILD_FLAG").map(String::as_str), Some("1"));
        assert!(!allow.contains_key("ZAI_API_KEY"));
        assert!(!allow.contains_key("SSH_AUTH_SOCK"));
    }

    #[test]
    fn allowlist_matches_windows_env_names_case_insensitively() {
        // Git Bash exports `SYSTEMROOT`; Windows children still need the value.
        let vars = vars(&[
            ("SYSTEMROOT", r"C:\Windows"),
            ("path", r"C:\bin"),
            ("NUWA_AGENT_ENV_ALLOW", "custom_flag"),
            ("CUSTOM_FLAG", "1"),
        ]);
        let allow = build_allowlist(&vars);
        assert_eq!(
            allow.get("SystemRoot").map(String::as_str),
            Some(r"C:\Windows")
        );
        assert_eq!(allow.get("PATH").map(String::as_str), Some(r"C:\bin"));
        assert_eq!(allow.get("custom_flag").map(String::as_str), Some("1"));
    }

    #[test]
    fn redact_session_dir_rewrites_jsonl() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("run.jsonl");
        std::fs::write(
            &file,
            "{\"text\":\"token supersecret-value\"}\n{\"text\":\"clean\"}\n",
        )
        .expect("write");
        let secrets = Secrets {
            redact_values: vec!["supersecret-value".to_string()],
            ..Secrets::default()
        };
        secrets.redact_session_dir(dir.path()).expect("redact");
        let text = std::fs::read_to_string(&file).expect("read");
        assert!(!text.contains("supersecret-value"));
        assert!(text.contains("clean"));
    }
}
