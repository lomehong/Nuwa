//! Secret handling for the repair runner.
//!
//! The work process loads the credentials it needs, removes them from
//! `std::env`, and passes them explicitly to the provider/PR/CI call sites.
//! Agent tools and verification subprocesses therefore inherit nothing
//! sensitive. Output that leaves the process (PR body, evidence, transcripts)
//! is redacted as a second layer.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::sync::OnceLock;

use anyhow::{bail, Context, Result};
use regex::Regex;
use serde_json::Value;

/// The only transcript tree serve may read: `<run session dir>/redacted/`.
/// Relative paths below it mirror the private JSONL tree; raw files stay private.
pub(crate) const REDACTED_SESSION_SUBDIR: &str = "redacted";
/// Shared publication/lookup bounds, measured below the raw/snapshot root.
pub(crate) const MAX_SESSION_DEPTH: usize = 4;
pub(crate) const MAX_SESSION_ENTRIES: usize = 1_000;

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

    /// Publish sanitized copies only after repair has stopped writing. Serve
    /// reads this separate tree, never private/in-flight/abandoned raw files.
    /// Each JSONL is atomically published in full or not at all; failures are
    /// reported even if other files have already been safely published.
    pub fn redact_session_dir(&self, dir: &Path) -> Result<()> {
        let absolute = std::path::absolute(dir).context("resolve session directory")?;
        let dir = absolute.as_path();
        let metadata = match std::fs::symlink_metadata(dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error).context("inspect session dir"),
        };
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        let mut budget = MAX_SESSION_ENTRIES;
        redact_dir(
            self,
            dir,
            &dir.join(REDACTED_SESSION_SUBDIR),
            0,
            &mut budget,
        )
    }
}

fn redact_dir(
    secrets: &Secrets,
    dir: &Path,
    destination: &Path,
    depth: usize,
    budget: &mut usize,
) -> Result<()> {
    if depth > MAX_SESSION_DEPTH {
        bail!("session directory depth limit exceeded");
    }
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("read session dir {}", dir.display()))?;
    for entry in entries {
        let entry = entry?;
        // Never walk our own published tree, including case aliases on Windows.
        if entry
            .file_name()
            .eq_ignore_ascii_case(REDACTED_SESSION_SUBDIR)
        {
            continue;
        }
        if *budget == 0 {
            bail!("session directory entry limit exceeded");
        }
        *budget -= 1;
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            redact_dir(secrets, &path, &target, depth + 1, budget)?;
        } else if file_type.is_file()
            && path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jsonl"))
        {
            publish_jsonl(secrets, &path, &target)
                .with_context(|| format!("publish transcript {}", path.display()))?;
        }
    }
    Ok(())
}

/// Do not follow pre-existing links in the snapshot path. Creating each level
/// separately also avoids create_dir_all traversing a linked ancestor.
fn snapshot_directory(dir: &Path) -> Result<()> {
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            bail!(
                "snapshot path is not a regular directory: {}",
                dir.display()
            );
        }
        Ok(_) => {
            if let Some(parent) = dir.parent() {
                snapshot_directory(parent)?;
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = dir.parent() {
                snapshot_directory(parent)?;
            }
            std::fs::create_dir(dir).context("create snapshot directory")?;
        }
        Err(error) => return Err(error).context("inspect snapshot directory"),
    }
    Ok(())
}

fn publish_jsonl(secrets: &Secrets, source: &Path, target: &Path) -> Result<()> {
    let input = std::fs::File::open(source).context("read private transcript")?;
    let modified = input.metadata()?.modified()?;
    let parent = target.parent().context("snapshot parent missing")?;
    snapshot_directory(parent)?;
    if let Ok(meta) = std::fs::symlink_metadata(target) {
        if meta.file_type().is_symlink() || !meta.is_file() {
            bail!("snapshot target is not a regular file");
        }
    }
    // A temporary file has no .jsonl suffix, so API lookup ignores it. persist
    // is a same-filesystem atomic rename, not an in-place rewrite visible to readers.
    let mut temp = tempfile::NamedTempFile::new_in(parent).context("create snapshot temp file")?;
    {
        let mut output = BufWriter::new(temp.as_file_mut());
        for (index, line) in BufReader::new(input).lines().enumerate() {
            let line = line.context("read transcript record")?;
            if line.trim().is_empty() {
                continue;
            }
            // Never include untrusted record text (or parser excerpts) in errors.
            let mut record: Value = serde_json::from_str(&line).map_err(|_| {
                anyhow::anyhow!("invalid JSON transcript record at line {}", index + 1)
            })?;
            redact_json(secrets, &mut record);
            serde_json::to_writer(&mut output, &record).context("serialize redacted record")?;
            output.write_all(b"\n")?;
        }
        output.flush()?;
    }
    // Preserve session selection order rather than publication traversal order.
    temp.as_file()
        .set_times(std::fs::FileTimes::new().set_modified(modified))?;
    temp.as_file().sync_all().context("sync snapshot")?;
    temp.persist(target)
        .context("atomically publish snapshot")?;
    Ok(())
}

fn redact_json(secrets: &Secrets, value: &mut Value) {
    match value {
        Value::String(text) => *text = secrets.redact(text),
        Value::Number(number) => {
            let text = number.to_string();
            let redacted = secrets.redact(&text);
            if redacted != text {
                *value = Value::String(redacted);
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_json(secrets, item);
            }
        }
        Value::Object(object) => {
            let original = std::mem::take(object);
            for (key, mut value) in original {
                redact_json(secrets, &mut value);
                object.insert(secrets.redact(&key), value);
            }
        }
        _ => {}
    }
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
    fn snapshot_preserves_raw_and_redacts_decoded_nested_strings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let secret = "private\"key\\with\nnewline";
        let raw = serde_json::json!({
            "kind": "entry", "type": "message",
            "message": {"content": [{"type": "toolCall", "arguments": {
                "nested": [{"value": secret}],
                "token": "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
                (secret): "also redact object keys"
            }}]}
        })
        .to_string();
        std::fs::write(dir.path().join("run.jsonl"), &raw).expect("write raw");
        Secrets::with_values(vec![secret.into()])
            .redact_session_dir(dir.path())
            .expect("publish");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("run.jsonl")).expect("test fixture"),
            raw
        );
        let snapshot = std::fs::read_to_string(dir.path().join("redacted/run.jsonl"))
            .expect("published snapshot");
        let record: serde_json::Value = serde_json::from_str(&snapshot).expect("valid JSON");
        let args = &record["message"]["content"][0]["arguments"];
        assert_eq!(args["nested"][0]["value"], "[REDACTED]");
        assert_eq!(args["token"], "[REDACTED]");
        assert_eq!(args["[REDACTED]"], "also redact object keys");
    }

    #[test]
    fn review_snapshot_redacts_numeric_credentials_without_changing_other_numbers() {
        let dir = tempfile::tempdir().expect("test fixture");
        let mut vars = vars(&[("NUWA_UI_TOKEN", "123456789")]);
        let secrets = Secrets::from_vars(&mut vars);
        let raw = r#"{"secret":123456789,"nested":[-123456789,99123456789,123456789.5],"count":42,"ratio":1.5,"nil":null,"ok":true}"#;
        std::fs::write(dir.path().join("numeric.jsonl"), raw).expect("test fixture");
        secrets
            .redact_session_dir(dir.path())
            .expect("test fixture");
        let snapshot = std::fs::read_to_string(dir.path().join("redacted/numeric.jsonl"))
            .expect("test fixture");
        let record: Value = serde_json::from_str(&snapshot).expect("test fixture");
        assert_eq!(record["secret"], "[REDACTED]");
        assert_eq!(
            record["nested"],
            serde_json::json!(["-[REDACTED]", "99[REDACTED]", "[REDACTED].5"])
        );
        assert_eq!(record["count"], serde_json::json!(42));
        assert_eq!(record["ratio"], serde_json::json!(1.5));
        assert!(record["nil"].is_null());
        assert_eq!(record["ok"], true);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("numeric.jsonl")).expect("test fixture"),
            raw
        );
    }

    #[test]
    fn snapshot_runs_patterns_without_known_credentials_and_skips_its_own_tree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let raw = "{\"text\":\"ghp_abcdefghijklmnopqrstuvwxyz0123456789\"}\n";
        std::fs::write(dir.path().join("run.jsonl"), raw).expect("test fixture");
        let secrets = Secrets::default();
        secrets.redact_session_dir(dir.path()).expect("publish");
        secrets.redact_session_dir(dir.path()).expect("republish");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("run.jsonl")).expect("test fixture"),
            raw
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("redacted/run.jsonl")).expect("test fixture"),
            "{\"text\":\"[REDACTED]\"}\n"
        );
        assert!(!dir.path().join("redacted/redacted").exists());
    }

    #[test]
    fn snapshot_invalid_record_fails_without_publishing_a_partial_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let raw = "{\"text\":\"first valid record\"}\n{broken secret-value\n";
        std::fs::write(dir.path().join("run.jsonl"), raw).expect("test fixture");
        let error = Secrets::default()
            .redact_session_dir(dir.path())
            .expect_err("invalid JSON");
        assert!(!format!("{error:#}").contains("secret-value"));
        assert!(!dir.path().join("redacted/run.jsonl").exists());
        assert_eq!(
            std::fs::read_to_string(dir.path().join("run.jsonl")).expect("test fixture"),
            raw
        );
        if dir.path().join("redacted").exists() {
            assert_eq!(
                std::fs::read_dir(dir.path().join("redacted"))
                    .expect("test fixture")
                    .count(),
                0
            );
        }
    }

    #[test]
    fn snapshot_replacement_is_atomic_and_failed_replacement_keeps_previous_snapshot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let raw_path = dir.path().join("run.jsonl");
        let snapshot = dir.path().join("redacted/run.jsonl");
        std::fs::write(&raw_path, "{\"text\":\"old\"}\n").expect("test fixture");
        let secrets = Secrets::default();
        secrets
            .redact_session_dir(dir.path())
            .expect("test fixture");
        let old_handle = std::fs::File::open(&snapshot).expect("published");
        std::fs::write(&raw_path, "{\"text\":\"new\"}\n").expect("test fixture");
        let replacement = secrets.redact_session_dir(dir.path());
        // Windows MoveFileEx may refuse to replace an open destination. This
        // must stay an explicit failure with the old snapshot intact, never a
        // delete/truncate fallback. Once the reader closes, publication works.
        #[cfg(windows)]
        if let Err(error) = &replacement {
            assert_eq!(
                error
                    .root_cause()
                    .downcast_ref::<std::io::Error>()
                    .expect("test fixture")
                    .kind(),
                std::io::ErrorKind::PermissionDenied
            );
            assert_eq!(
                std::fs::read_to_string(&snapshot).expect("test fixture"),
                "{\"text\":\"old\"}\n"
            );
        }
        #[cfg(not(windows))]
        assert!(replacement.is_ok(), "{replacement:?}");
        let old: serde_json::Value = serde_json::from_reader(old_handle).expect("test fixture");
        assert_eq!(
            old["text"], "old",
            "an open reader must not see an in-place rewrite"
        );
        drop(replacement);
        secrets
            .redact_session_dir(dir.path())
            .expect("test fixture");
        assert_eq!(
            std::fs::read_to_string(&snapshot).expect("test fixture"),
            "{\"text\":\"new\"}\n"
        );
        std::fs::write(&raw_path, "{\"text\":\"partial\"}\ninvalid\n").expect("test fixture");
        assert!(secrets.redact_session_dir(dir.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(&snapshot).expect("test fixture"),
            "{\"text\":\"new\"}\n"
        );
        assert_eq!(
            std::fs::read_dir(snapshot.parent().expect("test fixture"))
                .expect("test fixture")
                .count(),
            1
        );
    }

    #[test]
    fn snapshot_rejects_invalid_utf8_and_obstructed_destination() {
        let dir = tempfile::tempdir().expect("test fixture");
        std::fs::write(dir.path().join("run.jsonl"), [0xff, b'\n']).expect("test fixture");
        assert!(Secrets::default().redact_session_dir(dir.path()).is_err());
        assert!(!dir.path().join("redacted/run.jsonl").exists());
        let other = tempfile::tempdir().expect("test fixture");
        std::fs::write(other.path().join("run.jsonl"), "{}\n").expect("test fixture");
        std::fs::write(other.path().join("redacted"), "not a directory").expect("test fixture");
        assert!(Secrets::default().redact_session_dir(other.path()).is_err());
    }

    #[test]
    fn snapshot_directory_walk_has_depth_and_entry_budgets() {
        let dir = tempfile::tempdir().expect("test fixture");
        let deep = dir.path().join("one/two/three/four/five");
        std::fs::create_dir_all(&deep).expect("test fixture");
        std::fs::write(deep.join("run.jsonl"), "{}\n").expect("test fixture");
        assert!(Secrets::default().redact_session_dir(dir.path()).is_err());
        let wide = tempfile::tempdir().expect("test fixture");
        for index in 0..1_001 {
            std::fs::write(wide.path().join(format!("{index}.txt")), "").expect("test fixture");
        }
        assert!(Secrets::default().redact_session_dir(wide.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_skips_source_symlinks_and_rejects_destination_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().expect("test fixture");
        let outside = tempfile::tempdir().expect("test fixture");
        std::fs::write(outside.path().join("private.jsonl"), "{}\n").expect("test fixture");
        symlink(outside.path(), dir.path().join("linked-dir")).expect("test fixture");
        symlink(
            outside.path().join("private.jsonl"),
            dir.path().join("linked.jsonl"),
        )
        .expect("test fixture");
        Secrets::default()
            .redact_session_dir(dir.path())
            .expect("test fixture");
        assert!(!dir.path().join("redacted/linked.jsonl").exists());
        assert!(!dir.path().join("redacted/linked-dir").exists());
        std::fs::write(dir.path().join("run.jsonl"), "{}\n").expect("test fixture");
        symlink(outside.path(), dir.path().join("redacted")).expect("test fixture");
        assert!(Secrets::default().redact_session_dir(dir.path()).is_err());
        assert!(!outside.path().join("run.jsonl").exists());
    }
}
