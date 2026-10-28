//! Saved configuration and immutable, secret-free startup evidence.
//!
//! A file marker is not startup evidence. Only a matching startup environment,
//! file and committed revision can establish which revision a process loaded.
//! This proves the managed startup inputs, not equality with final effective
//! values: explicit CLI overrides remain visible alongside that input proof.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use shanhaijing::{RunnerRow, Store};

use crate::settings::{self, SettingSpec};

pub const REVISION_KEY: &str = "NUWA_CONFIG_REVISION";
pub const FORMAT_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct EffectiveValue {
    pub value: Option<String>,
    pub source: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct StartupConfig {
    pub format_version: u32,
    pub process_instance_id: String,
    pub role: String,
    pub runner_id: Option<String>,
    pub runner_instance_id: Option<String>,
    pub env_id: String,
    pub started_at_ms: i64,
    /// Proven managed input revision at startup, independent of CLI overrides.
    pub loaded_revision: Option<String>,
    pub revision_reason: Option<String>,
    pub values: BTreeMap<String, EffectiveValue>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct SavedConfigDto {
    pub env_id: String,
    pub revision: Option<String>,
    pub state: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "ts-export", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts-export", ts(export))]
pub struct ProcessConfigDto {
    pub role: String,
    pub runner_id: Option<String>,
    pub snapshot: Option<StartupConfig>,
    pub activation: String,
    pub freshness: String,
    pub last_seen_at_ms: Option<i64>,
    pub reason: Option<String>,
}

/// Resolve symlinks, including the nearest existing ancestor of a missing file.
/// The same identity is used before and after the file's first save.
pub fn env_identity(path: &Path) -> Result<String, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| "env_identity_unavailable".to_string())?
            .join(path)
    };
    let mut ancestor = absolute.as_path();
    let mut suffix = Vec::new();
    let mut resolved = loop {
        match std::fs::canonicalize(ancestor) {
            Ok(resolved) => break resolved,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = ancestor
                    .file_name()
                    .ok_or_else(|| "env_identity_unavailable".to_string())?;
                suffix.push(name.to_os_string());
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| "env_identity_unavailable".to_string())?;
            }
            Err(_) => return Err("env_identity_unavailable".to_string()),
        }
    };
    for part in suffix.into_iter().rev() {
        resolved.push(part);
    }
    let mut normalized = PathBuf::new();
    for part in resolved.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    let identity = normalized
        .to_str()
        .ok_or_else(|| "env_identity_unavailable".to_string())?
        .to_string();
    #[cfg(windows)]
    let identity = identity.to_lowercase();
    Ok(identity)
}

fn applicable(spec: &SettingSpec, role: &str) -> bool {
    spec.scope == "both" || spec.scope == role
}

/// Proof uses only the simple env syntax our writer emits. Ambiguous systemd
/// quoting, duplicate keys and legacy spellings must never create proof.
pub(crate) fn proven_file(text: &str) -> Option<BTreeMap<String, String>> {
    let mut seen = BTreeSet::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once('=')?;
        let key = key.trim();
        let upper = key.to_ascii_uppercase();
        if settings::setting_spec(&upper).is_none() && upper != REVISION_KEY {
            continue;
        }
        if key != upper
            || !seen.insert(key)
            || value.contains(['\'', '"', '\\'])
            || value.chars().any(char::is_control)
        {
            return None;
        }
    }
    let values = settings::parse_env(text);
    uuid::Uuid::parse_str(values.get(REVISION_KEY)?).ok()?;
    Some(values)
}

/// DB agreement is required even if a file happens to carry a valid marker.
pub fn saved_config(store: &Store, path: &Path) -> SavedConfigDto {
    match read_saved_file(store, path) {
        Ok((_, saved)) => saved,
        Err(error) => error.saved,
    }
}

#[derive(Debug)]
pub(crate) struct SavedFileError {
    pub reason: &'static str,
    saved: SavedConfigDto,
}

/// Read file values and revision evidence in one save transaction. Callers must
/// use this owned text, not reread the file when presenting the saved revision.
pub(crate) fn read_saved_file(
    store: &Store,
    path: &Path,
) -> Result<(Option<String>, SavedConfigDto), SavedFileError> {
    let mut saved = SavedConfigDto {
        env_id: String::new(),
        revision: None,
        state: "unknown".to_string(),
        reason: Some("env_identity_unavailable".to_string()),
    };
    let Ok(env_id) = env_identity(path) else {
        return Err(SavedFileError {
            reason: "env_identity_unavailable",
            saved,
        });
    };
    saved.env_id = env_id;
    // Serialize the file read with saves as well; otherwise a reader could see
    // the new file and old DB row in the middle of a legitimate save.
    let evidence: Result<_, shanhaijing::ShanError> = store.with_config_save(&saved.env_id, |tx| {
        Ok((tx.current()?, std::fs::read_to_string(path)))
    });
    let text = match evidence {
        Ok((committed, Err(error))) if error.kind() == std::io::ErrorKind::NotFound => {
            if committed.is_none() {
                saved.state = "unversioned".to_string();
                saved.reason = Some("not_saved".to_string());
            } else {
                saved.reason = Some("env_file_unavailable".to_string());
            }
            None
        }
        Ok((committed, Ok(text))) => {
            let file = proven_file(&text);
            let marker = file.as_ref().and_then(|file| file.get(REVISION_KEY));
            match (committed, marker) {
                (Some(row), Some(marker)) if row.revision == *marker => {
                    saved.revision = Some(row.revision);
                    saved.state = "saved".to_string();
                    saved.reason = None;
                }
                (None, None) if !settings::parse_env(&text).contains_key(REVISION_KEY) => {
                    saved.state = "unversioned".to_string();
                    saved.reason = Some("not_saved".to_string());
                }
                _ => saved.reason = Some("file_db_mismatch".to_string()),
            }
            Some(text)
        }
        Ok(_) => {
            saved.reason = Some("env_file_unavailable".to_string());
            return Err(SavedFileError {
                reason: "env_file_unavailable",
                saved,
            });
        }
        Err(_) => {
            saved.reason = Some("revision_unavailable".to_string());
            return Err(SavedFileError {
                reason: "revision_unavailable",
                saved,
            });
        }
    };
    Ok((text, saved))
}

impl StartupConfig {
    /// Compare the supplied startup inputs exactly, including secrets, in memory
    /// only. Persist only sanitized non-secret values, never raw inputs/errors.
    /// Production supplies the process snapshot, not the scrubbed OS environment.
    pub fn capture(
        store: &Store,
        path: &Path,
        role: &str,
        process_instance_id: String,
        started_at_ms: i64,
        environment: &BTreeMap<String, String>,
    ) -> Self {
        let mut snapshot = Self {
            format_version: FORMAT_VERSION,
            process_instance_id,
            role: role.to_string(),
            runner_id: None,
            runner_instance_id: None,
            env_id: env_identity(path).unwrap_or_default(),
            started_at_ms,
            loaded_revision: None,
            revision_reason: Some("startup_revision_unproven".to_string()),
            values: BTreeMap::new(),
        };
        let proof: Result<Option<String>, shanhaijing::ShanError> =
            store.with_config_save(&snapshot.env_id, |tx| {
                let committed = tx.current()?;
                let file = std::fs::read_to_string(path)
                    .ok()
                    .and_then(|text| proven_file(&text));
                let Some(file) = file else { return Ok(None) };
                let marker = environment.get(REVISION_KEY);
                let matches = !snapshot.env_id.is_empty()
                    && matches!(role, "serve" | "work")
                    && marker.is_some()
                    && marker == file.get(REVISION_KEY)
                    && committed.as_ref().map(|row| &row.revision) == marker
                    && settings::SETTINGS_SCHEMA.iter().filter(|spec| {
                        applicable(spec, role) && !spec.read_only
                    }).all(|spec| environment.get(spec.key) == file.get(spec.key))
                    // A GitHub fallback not present in the managed file is an
                    // external credential override, even though it is secret.
                    && !(environment.contains_key("GITHUB_TOKEN")
                        && environment.get("NUWA_GITHUB_TOKEN")
                            .is_none_or(|value| value.trim().is_empty()));
                Ok(if matches { marker.cloned() } else { None })
            });
        if let Ok(Some(revision)) = proof {
            snapshot.loaded_revision = Some(revision);
            snapshot.revision_reason = None;
        }
        for spec in settings::SETTINGS_SCHEMA
            .iter()
            .filter(|spec| applicable(spec, role) && !spec.secret)
        {
            let (value, source) = match environment.get(spec.key) {
                Some(value) => (
                    Some(value.clone()),
                    if snapshot.loaded_revision.is_some() && !spec.read_only {
                        "file"
                    } else {
                        "environment"
                    },
                ),
                None => (spec.default.map(str::to_string), "default"),
            };
            let mut effective = sanitize(spec.key, value, source);
            if effective
                .value
                .as_ref()
                .is_some_and(|value| contains_secret(value, environment))
            {
                effective.value = None;
                effective.reason = Some("sensitive_value_omitted".to_string());
            }
            snapshot.values.insert(spec.key.to_string(), effective);
        }
        snapshot
    }

    /// Record actual resolved values, never credentials or arbitrary fields.
    /// CLI overrides (matching or divergent), defaults and known derived values
    /// preserve the managed startup input proof without changing their source.
    /// Unknown sources and unexplained input changes invalidate that proof;
    /// callers must label known normalization/clamping as `derived`.
    pub fn set_effective(&mut self, key: &str, value: Option<String>, source: &str) {
        let Some(spec) = settings::setting_spec(key).filter(|spec| !spec.secret) else {
            return;
        };
        if !applicable(spec, &self.role) {
            return;
        }
        // Compare before sanitization: distinct raw environment inputs must not
        // look unchanged just because redaction/URL normalization hides them.
        let unexpected_input = !matches!(source, "cli" | "default" | "derived")
            && self.values.get(key).is_none_or(|old| old.value != value);
        let mut effective = sanitize(key, value, source);
        // Production env_store retains the startup secret copies: settings saves
        // never set it, and Secrets::load consumes a local copy, not the store.
        // Read that process snapshot, not the scrubbed OS environment. The map
        // supplied to capture is used there only; synthetic maps do not install
        // secrets in env_store, and no raw secrets are retained in this DTO.
        if effective.value.as_ref().is_some_and(|value| {
            settings::SETTINGS_SCHEMA
                .iter()
                .filter(|spec| spec.secret)
                .any(|spec| {
                    crate::env_store::var(spec.key)
                        .is_some_and(|secret| !secret.is_empty() && value.contains(secret.as_str()))
                })
                || crate::env_store::var("GITHUB_TOKEN")
                    .is_some_and(|secret| !secret.is_empty() && value.contains(secret.as_str()))
        }) {
            effective.value = None;
            effective.reason = Some("sensitive_value_omitted".to_string());
        }
        if self.loaded_revision.is_some()
            && (effective.source == "unknown" || source == "override" || unexpected_input)
        {
            self.loaded_revision = None;
            self.revision_reason = Some("effective_override".to_string());
            for old in self.values.values_mut().filter(|old| old.source == "file") {
                old.source = "environment".to_string();
            }
        }
        if self.loaded_revision.is_none() && effective.source == "file" {
            effective.source = "environment".to_string();
        }
        self.values.insert(key.to_string(), effective);
    }
}

fn contains_secret(value: &str, environment: &BTreeMap<String, String>) -> bool {
    settings::SETTINGS_SCHEMA
        .iter()
        .filter(|spec| spec.secret)
        .map(|spec| spec.key)
        .chain(std::iter::once("GITHUB_TOKEN"))
        .filter_map(|key| environment.get(key))
        .any(|secret| !secret.is_empty() && value.contains(secret.as_str()))
}

fn sanitize(key: &str, value: Option<String>, source: &str) -> EffectiveValue {
    let mut effective = EffectiveValue {
        value,
        source: match source {
            "default" | "environment" | "file" | "cli" | "derived" | "runtime" | "override" => {
                source
            }
            _ => "unknown",
        }
        .to_string(),
        reason: None,
    };
    let Some(value) = effective.value.as_ref() else {
        return effective;
    };
    if value.chars().any(char::is_control)
        || ["Bearer ", "ghp_", "github_pat_", "sk-"]
            .iter()
            .any(|prefix| value.contains(*prefix))
    {
        effective.value = None;
        effective.reason = Some("sensitive_value_omitted".to_string());
    } else if !value.is_empty()
        && (settings::setting_spec(key).is_some_and(|spec| spec.kind == "url")
            || value.contains("://"))
    {
        match reqwest::Url::parse(value) {
            Ok(mut url) if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() => {
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.set_query(None);
                url.set_fragment(None);
                effective.value = Some(url.to_string());
            }
            _ => {
                effective.value = None;
                effective.reason = Some("invalid_url_omitted".to_string());
            }
        }
    }
    effective
}

fn valid_snapshot(snapshot: &StartupConfig) -> bool {
    snapshot.format_version == FORMAT_VERSION
        && matches!(snapshot.role.as_str(), "serve" | "work")
        && !snapshot.process_instance_id.is_empty()
        && settings::SETTINGS_SCHEMA
            .iter()
            .filter(|spec| applicable(spec, &snapshot.role) && !spec.secret)
            .all(|spec| snapshot.values.contains_key(spec.key))
        && snapshot.values.iter().all(|(key, value)| {
            settings::setting_spec(key)
                .is_some_and(|spec| !spec.secret && applicable(spec, &snapshot.role))
                && sanitize(key, value.value.clone(), &value.source).value == value.value
                && matches!(
                    value.source.as_str(),
                    "default"
                        | "environment"
                        | "file"
                        | "cli"
                        | "derived"
                        | "runtime"
                        | "override"
                        | "unknown"
                )
                // Known omissions (including invalid URLs with value=None) are
                // safe evidence; they do not invalidate the entire snapshot.
                && value.reason.as_deref().is_none_or(|reason| {
                    matches!(reason, "sensitive_value_omitted" | "invalid_url_omitted")
                })
        })
        && snapshot
            .loaded_revision
            .as_ref()
            .is_none_or(|revision| uuid::Uuid::parse_str(revision).is_ok())
        && snapshot.revision_reason.as_deref().is_none_or(|reason| {
            matches!(
                reason,
                "startup_revision_unproven" | "effective_override" | "revision_persistence_failed"
            )
        })
}

/// Project activation from managed input revisions, not final CLI value equality.
pub fn project_process(
    snapshot: Option<StartupConfig>,
    saved: &SavedConfigDto,
    freshness: &str,
    last_seen_at_ms: Option<i64>,
) -> ProcessConfigDto {
    let snapshot = snapshot.filter(valid_snapshot);
    let mut process = ProcessConfigDto {
        role: snapshot
            .as_ref()
            .map(|snapshot| snapshot.role.clone())
            .unwrap_or_else(|| "unknown".to_string()),
        runner_id: snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.runner_id.clone()),
        snapshot: None,
        activation: "unknown".to_string(),
        freshness: freshness.to_string(),
        last_seen_at_ms,
        reason: Some("startup_snapshot_unavailable".to_string()),
    };
    if let Some(snapshot) = snapshot {
        process.reason = Some("startup_revision_unproven".to_string());
        if saved.state == "saved" && snapshot.env_id == saved.env_id && !saved.env_id.is_empty() {
            if let (Some(loaded), Some(current)) = (&snapshot.loaded_revision, &saved.revision) {
                if snapshot.revision_reason.is_none() {
                    process.activation =
                        if loaded == current { "active" } else { "stale" }.to_string();
                    process.reason = None;
                }
            }
        } else {
            process.reason = Some("saved_revision_unproven".to_string());
        }
        process.snapshot = Some(snapshot);
    }
    process
}

pub fn runner_configuration(
    runner: &RunnerRow,
    saved: &SavedConfigDto,
    now: i64,
) -> ProcessConfigDto {
    let snapshot = runner
        .startup_config_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<StartupConfig>(json).ok())
        .filter(|snapshot| {
            snapshot.role == "work"
                && snapshot.runner_id.as_deref() == Some(runner.id.as_str())
                && snapshot
                    .runner_instance_id
                    .as_ref()
                    .is_some_and(|id| !id.is_empty())
                && snapshot.started_at_ms == runner.started_at
        });
    // Store.set_runner_startup_config fences the lease instance, and a new
    // acquisition clears the snapshot. RunnerRow intentionally exposes no lease
    // instance; do not invent one from mutable legacy config_json telemetry.
    let freshness =
        if now.saturating_sub(runner.heartbeat_at) > crate::api::common::RUNNER_OFFLINE_MS {
            "offline"
        } else {
            "online"
        };
    let mut process = project_process(snapshot, saved, freshness, Some(runner.heartbeat_at));
    process.role = "work".to_string();
    process.runner_id = Some(runner.id.clone());
    process
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved_fixture() -> (tempfile::TempDir, Store, PathBuf, BTreeMap<String, String>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(dir.path().join("config.db")).expect("store");
        let path = dir.path().join("nuwa.env");
        let mut environment = BTreeMap::from([
            ("NUWA_INTERVAL".to_string(), "30".to_string()),
            (
                "NUWA_GITHUB_TOKEN".to_string(),
                "synthetic-github-credential".to_string(),
            ),
            (
                "ANTHROPIC_API_KEY".to_string(),
                "synthetic-model-credential".to_string(),
            ),
            (
                "NUWA_UI_TOKEN".to_string(),
                "synthetic-console-credential".to_string(),
            ),
        ]);
        let revision = uuid::Uuid::new_v4().to_string();
        environment.insert(REVISION_KEY.to_string(), revision.clone());
        settings::write_env_atomic(&path, &settings::render_env(&environment)).expect("file");
        let identity = env_identity(&path).expect("identity");
        store
            .with_config_save(&identity, |tx| tx.record_saved(&revision, 1, &[]))
            .expect("revision");
        (dir, store, path, environment)
    }

    #[test]
    fn saved_file_read_pairs_exact_text_with_its_revision_evidence() {
        let (_dir, store, path, environment) = saved_fixture();
        let expected = settings::render_env(&environment);
        let (text, saved) = read_saved_file(&store, &path).expect("saved file");
        assert_eq!(text.as_deref(), Some(expected.as_str()));
        assert_eq!(saved.state, "saved");
        assert_eq!(saved.revision.as_ref(), environment.get(REVISION_KEY));
        assert!(saved.reason.is_none());

        let mut inconsistent = environment.clone();
        inconsistent.insert(REVISION_KEY.to_string(), uuid::Uuid::new_v4().to_string());
        inconsistent.insert("NUWA_INTERVAL".to_string(), "60".to_string());
        let expected = settings::render_env(&inconsistent);
        settings::write_env_atomic(&path, &expected).expect("external edit");
        let (text, saved) = read_saved_file(&store, &path).expect("inconsistent file");
        assert_eq!(text.as_deref(), Some(expected.as_str()));
        assert_eq!(saved.state, "unknown");
        assert!(saved.revision.is_none());
        assert_eq!(saved.reason.as_deref(), Some("file_db_mismatch"));
        assert_eq!(saved_config(&store, &path).reason, saved.reason);
    }

    #[test]
    fn saved_file_read_preserves_missing_unversioned_and_unavailable_behavior() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = Store::open(":memory:").expect("store");
        let path = dir.path().join("nuwa.env");
        let (text, saved) = read_saved_file(&store, &path).expect("missing file");
        assert!(text.is_none());
        assert_eq!(saved.state, "unversioned");
        assert_eq!(saved.reason.as_deref(), Some("not_saved"));

        settings::write_env_atomic(&path, "NUWA_INTERVAL=30\n").expect("unversioned file");
        let (text, saved) = read_saved_file(&store, &path).expect("unversioned read");
        assert_eq!(text.as_deref(), Some("NUWA_INTERVAL=30\n"));
        assert_eq!(saved.state, "unversioned");
        assert!(saved.revision.is_none());

        let missing = dir.path().join("previously-saved.env");
        let identity = env_identity(&missing).expect("identity");
        store
            .with_config_save(&identity, |tx| {
                tx.record_saved(&uuid::Uuid::new_v4().to_string(), 1, &[])
            })
            .expect("saved revision without file");
        let (text, saved) = read_saved_file(&store, &missing).expect("missing saved file");
        assert!(text.is_none());
        assert_eq!(saved.state, "unknown");
        assert_eq!(saved.reason.as_deref(), Some("env_file_unavailable"));

        let error = read_saved_file(&store, dir.path()).expect_err("directory is not an env file");
        assert_eq!(error.reason, "env_file_unavailable");
        let saved = saved_config(&store, dir.path());
        assert_eq!(saved.state, "unknown");
        assert!(saved.revision.is_none());
        assert_eq!(saved.reason.as_deref(), Some(error.reason));
    }

    fn capture(
        store: &Store,
        path: &Path,
        environment: &BTreeMap<String, String>,
    ) -> StartupConfig {
        StartupConfig::capture(
            store,
            path,
            "work",
            "process-a".to_string(),
            10,
            environment,
        )
    }

    #[test]
    fn startup_requires_marker_and_every_applicable_input_including_absence_and_secrets() {
        let (_dir, store, path, environment) = saved_fixture();
        let good = capture(&store, &path, &environment);
        assert_eq!(good.loaded_revision.as_ref(), environment.get(REVISION_KEY));
        assert_eq!(good.values["NUWA_INTERVAL"].source, "file");
        assert_eq!(good.values["NUWA_MAX_TURNS"].source, "default");
        let serialized = serde_json::to_string(&good).expect("snapshot");
        assert!(!serialized.contains("synthetic-github-credential"));
        assert!(!serialized.contains("synthetic-model-credential"));
        assert!(!good.values.contains_key("ANTHROPIC_API_KEY"));
        for (key, replacement) in [
            (REVISION_KEY, None),
            ("NUWA_INTERVAL", Some("60")),
            ("ANTHROPIC_API_KEY", Some("different-synthetic-credential")),
            ("NUWA_GITHUB_TOKEN", None),
            ("NUWA_MAX_TURNS", Some("30")),
        ] {
            let mut overridden = environment.clone();
            match replacement {
                Some(value) => {
                    overridden.insert(key.to_string(), value.to_string());
                }
                None => {
                    overridden.remove(key);
                }
            }
            let snapshot = capture(&store, &path, &overridden);
            assert!(snapshot.loaded_revision.is_none(), "override key: {key}");
            assert_ne!(snapshot.values["NUWA_INTERVAL"].source, "file");
        }
        let mut alias = environment.clone();
        alias.remove("NUWA_GITHUB_TOKEN");
        settings::write_env_atomic(&path, &settings::render_env(&alias)).expect("no canonical key");
        alias.insert("GITHUB_TOKEN".to_string(), "synthetic-alias".to_string());
        assert!(capture(&store, &path, &alias).loaded_revision.is_none());
        let mut empty_canonical = environment.clone();
        empty_canonical.insert("NUWA_GITHUB_TOKEN".to_string(), String::new());
        settings::write_env_atomic(&path, &settings::render_env(&empty_canonical))
            .expect("empty canonical key");
        empty_canonical.insert("GITHUB_TOKEN".to_string(), "synthetic-alias".to_string());
        assert!(capture(&store, &path, &empty_canonical)
            .loaded_revision
            .is_none());
        settings::write_env_atomic(&path, &settings::render_env(&environment))
            .expect("restore fixture");
        // Serve proof includes auth secrets, but not work-only model inputs.
        let mut serve_env = environment.clone();
        serve_env.insert(
            "NUWA_UI_TOKEN".to_string(),
            "different-console-credential".to_string(),
        );
        assert!(
            StartupConfig::capture(&store, &path, "serve", "serve-a".into(), 10, &serve_env)
                .loaded_revision
                .is_none()
        );
    }

    #[test]
    fn marker_without_committed_revision_and_missing_file_are_not_proof() {
        let (dir, _store, path, environment) = saved_fixture();
        let independent = Store::open(dir.path().join("empty.db")).expect("empty store");
        assert_eq!(saved_config(&independent, &path).state, "unknown");
        assert!(capture(&independent, &path, &environment)
            .loaded_revision
            .is_none());
        let missing = dir.path().join("missing").join("deep").join("nuwa.env");
        let before = env_identity(&missing).expect("nearest existing parent");
        std::fs::create_dir_all(missing.parent().expect("parent")).expect("mkdir");
        settings::write_env_atomic(&missing, "").expect("create");
        assert_eq!(env_identity(&missing).expect("existing"), before);
    }

    #[test]
    fn cli_overrides_retain_input_revision_and_expose_values_across_saves() {
        for value in ["30", "60"] {
            let (_dir, store, path, mut environment) = saved_fixture();
            let loaded_revision = environment[REVISION_KEY].clone();
            let mut snapshot = capture(&store, &path, &environment);
            snapshot.set_effective("NUWA_INTERVAL", Some(value.to_string()), "cli");
            assert_eq!(snapshot.loaded_revision.as_ref(), Some(&loaded_revision));
            assert!(snapshot.revision_reason.is_none());

            let saved = saved_config(&store, &path);
            let active = project_process(Some(snapshot.clone()), &saved, "online", Some(20));
            assert_eq!(active.activation, "active");
            let active = active.snapshot.expect("CLI snapshot stays visible");
            assert_eq!(active.values["NUWA_INTERVAL"].value.as_deref(), Some(value));
            assert_eq!(active.values["NUWA_INTERVAL"].source, "cli");

            let next_revision = uuid::Uuid::new_v4().to_string();
            environment.insert(REVISION_KEY.to_string(), next_revision.clone());
            environment.insert("NUWA_INTERVAL".to_string(), "90".to_string());
            store
                .with_config_save(&saved.env_id, |tx| {
                    settings::write_env_atomic(&path, &settings::render_env(&environment))
                        .expect("next file");
                    tx.record_saved(&next_revision, 30, &[])
                })
                .expect("next revision");
            let saved = saved_config(&store, &path);
            assert_eq!(saved.revision.as_ref(), Some(&next_revision));
            let stale = project_process(Some(snapshot), &saved, "online", Some(40));
            assert_eq!(stale.activation, "stale");
            let stale = stale.snapshot.expect("stale CLI snapshot stays visible");
            assert_eq!(stale.loaded_revision.as_ref(), Some(&loaded_revision));
            assert!(stale.revision_reason.is_none());
            assert_eq!(stale.values["NUWA_INTERVAL"].value.as_deref(), Some(value));
            assert_eq!(stale.values["NUWA_INTERVAL"].source, "cli");
        }
    }

    #[test]
    fn known_defaults_and_derived_values_preserve_input_proof_and_sources() {
        let (_dir, store, path, environment) = saved_fixture();
        let mut snapshot = capture(&store, &path, &environment);
        snapshot.set_effective("NUWA_INTERVAL", Some("30".to_string()), "environment");
        assert_eq!(snapshot.values["NUWA_INTERVAL"].source, "environment");
        snapshot.set_effective("NUWA_INTERVAL", Some("60".to_string()), "derived");
        snapshot.set_effective("NUWA_MAX_TURNS", Some("30".to_string()), "default");
        assert_eq!(
            snapshot.loaded_revision.as_ref(),
            environment.get(REVISION_KEY)
        );
        assert_eq!(snapshot.values["NUWA_INTERVAL"].source, "derived");
        assert_eq!(snapshot.values["NUWA_MAX_TURNS"].source, "default");
        let saved = saved_config(&store, &path);
        assert_eq!(
            project_process(Some(snapshot), &saved, "online", Some(20)).activation,
            "active"
        );
    }

    #[test]
    fn unexpected_inputs_and_unknown_sources_invalidate_proof() {
        let (_dir, store, path, environment) = saved_fixture();
        let saved = saved_config(&store, &path);
        for (source, value) in [
            ("environment", "60"),
            ("file", "60"),
            ("runtime", "60"),
            ("override", "30"),
            ("unknown", "30"),
            ("", "30"),
            ("CLI", "30"),
        ] {
            let mut snapshot = capture(&store, &path, &environment);
            snapshot.set_effective("NUWA_INTERVAL", Some(value.to_string()), source);
            assert!(snapshot.loaded_revision.is_none(), "source: {source}");
            assert_eq!(
                snapshot.revision_reason.as_deref(),
                Some("effective_override")
            );
            assert!(snapshot.values.values().all(|value| value.source != "file"));
            assert_eq!(
                project_process(Some(snapshot), &saved, "online", Some(20)).activation,
                "unknown"
            );
        }
    }

    #[test]
    fn runtime_overrides_and_sensitive_values_keep_distinct_evidence() {
        let (_dir, store, path, environment) = saved_fixture();
        let mut snapshot = capture(&store, &path, &environment);
        snapshot.set_effective("NUWA_INTERVAL", Some("60".to_string()), "cli");
        assert_eq!(
            snapshot.loaded_revision.as_ref(),
            environment.get(REVISION_KEY)
        );
        snapshot.set_effective(
            "NUWA_GITHUB_TOKEN",
            Some("synthetic-hidden".to_string()),
            "cli",
        );
        snapshot.set_effective("UNRECOGNIZED", Some("synthetic-hidden".to_string()), "cli");
        assert!(!snapshot.values.contains_key("NUWA_GITHUB_TOKEN"));
        assert!(!snapshot.values.contains_key("UNRECOGNIZED"));
        assert_eq!(
            snapshot.loaded_revision.as_ref(),
            environment.get(REVISION_KEY)
        );
        snapshot.set_effective("NUWA_BASE_URL", Some(
            "https://user:synthetic-password@example.test/v1?token=synthetic-query#synthetic-fragment".to_string()
        ), "environment");
        assert_eq!(
            snapshot.values["NUWA_BASE_URL"].value.as_deref(),
            Some("https://example.test/v1")
        );
        assert!(snapshot.loaded_revision.is_none());
        assert_eq!(snapshot.values["NUWA_BASE_URL"].source, "environment");
        assert_eq!(snapshot.values["NUWA_INTERVAL"].source, "cli");
        snapshot.set_effective(
            "NUWA_MODEL",
            Some("Bearer synthetic-key".to_string()),
            "environment",
        );
        assert!(snapshot.values["NUWA_MODEL"].value.is_none());
        assert_eq!(
            snapshot.values["NUWA_MODEL"].reason.as_deref(),
            Some("sensitive_value_omitted")
        );
        let saved = saved_config(&store, &path);
        let projected = project_process(Some(snapshot), &saved, "online", Some(20));
        assert_eq!(projected.activation, "unknown");
        assert!(projected.snapshot.is_some());
    }

    #[test]
    fn raw_environment_changes_cannot_hide_behind_url_sanitization() {
        let (_dir, store, path, mut environment) = saved_fixture();
        environment.insert(
            "NUWA_BASE_URL".to_string(),
            "https://example.test/v1?mode=startup".to_string(),
        );
        settings::write_env_atomic(&path, &settings::render_env(&environment)).expect("file");
        let mut snapshot = capture(&store, &path, &environment);
        assert!(snapshot.loaded_revision.is_some());
        snapshot.set_effective(
            "NUWA_BASE_URL",
            Some("https://example.test/v1?mode=unexpected".to_string()),
            "environment",
        );
        assert_eq!(
            snapshot.values["NUWA_BASE_URL"].value.as_deref(),
            Some("https://example.test/v1")
        );
        assert!(snapshot.loaded_revision.is_none());
        assert_eq!(snapshot.values["NUWA_BASE_URL"].source, "environment");
    }

    #[test]
    fn capture_omits_secrets_from_synthetic_frozen_inputs_without_installing_them() {
        let (_dir, store, path, mut environment) = saved_fixture();
        // This synthetic map is local to capture, not an env_store override or
        // an OS environment mutation. It models the process snapshot even when
        // OS copies have already been scrubbed; it makes no startup-order claim.
        environment.insert("GITHUB_TOKEN".to_string(), "synthetic-alias".to_string());
        for key in [
            "NUWA_GITHUB_TOKEN",
            "ANTHROPIC_API_KEY",
            "NUWA_UI_TOKEN",
            "GITHUB_TOKEN",
        ] {
            let secret = environment[key].clone();
            environment.insert("NUWA_MODEL".to_string(), format!("model-{secret}"));
            settings::write_env_atomic(&path, &settings::render_env(&environment)).expect("file");
            let snapshot = capture(&store, &path, &environment);
            assert_eq!(
                snapshot.loaded_revision.as_ref(),
                environment.get(REVISION_KEY)
            );
            assert!(snapshot.values["NUWA_MODEL"].value.is_none());
            assert_eq!(
                snapshot.values["NUWA_MODEL"].reason.as_deref(),
                Some("sensitive_value_omitted")
            );
            let json = serde_json::to_string(&snapshot).expect("secret-free snapshot");
            assert!(!json.contains(&secret));
            let snapshot = serde_json::from_str(&json).expect("round trip");
            let saved = saved_config(&store, &path);
            assert_eq!(
                project_process(Some(snapshot), &saved, "online", Some(20)).activation,
                "active"
            );
        }
    }

    #[test]
    fn known_url_omissions_preserve_serialized_snapshot_and_input_proof() {
        for (raw, reason) in [
            ("not a URL", "invalid_url_omitted"),
            ("Bearer synthetic-key", "sensitive_value_omitted"),
        ] {
            let (_dir, store, path, mut environment) = saved_fixture();
            environment.insert("NUWA_BASE_URL".to_string(), raw.to_string());
            settings::write_env_atomic(&path, &settings::render_env(&environment)).expect("file");
            let mut snapshot = capture(&store, &path, &environment);
            for source in ["file", "cli"] {
                if source == "cli" {
                    snapshot.set_effective("NUWA_BASE_URL", Some(raw.to_string()), source);
                }
                assert_eq!(
                    snapshot.loaded_revision.as_ref(),
                    environment.get(REVISION_KEY)
                );
                let url = &snapshot.values["NUWA_BASE_URL"];
                assert!(url.value.is_none());
                assert_eq!(url.reason.as_deref(), Some(reason));
                assert_eq!(url.source, source);
                let json = serde_json::to_string(&snapshot).expect("snapshot");
                let restored = serde_json::from_str(&json).expect("round trip");
                let saved = saved_config(&store, &path);
                let projected = project_process(Some(restored), &saved, "online", Some(20));
                assert_eq!(projected.activation, "active");
                assert!(projected.snapshot.is_some());
            }
            snapshot
                .values
                .get_mut("NUWA_BASE_URL")
                .expect("URL")
                .reason = Some("unknown_omission".to_string());
            let saved = saved_config(&store, &path);
            assert!(project_process(Some(snapshot), &saved, "online", Some(20))
                .snapshot
                .is_none());
        }
    }

    fn register(store: &Store, id: &str, instance: &str, snapshot: Option<&StartupConfig>) {
        let row = shanhaijing::RunnerUpsert {
            id: id.to_string(),
            repo: format!("example.test/{id}"),
            host: "synthetic-host".to_string(),
            pid: Some(1),
            started_at: 10,
            heartbeat_at: 20,
            status: "idle".to_string(),
            provider: None,
            model: None,
            daily_pr_limit: None,
            repo_path: Some(format!("/synthetic/{id}")),
            service_unit: Some(format!("nuwa-{id}")),
            config_json: "{}".to_string(),
        };
        assert_eq!(
            store
                .try_acquire_runner_lease(&row, instance, 20, 1000)
                .expect("register"),
            shanhaijing::RunnerLeaseOutcome::Acquired
        );
        if let Some(snapshot) = snapshot {
            assert!(store
                .set_runner_startup_config(
                    id,
                    instance,
                    &serde_json::to_string(snapshot).expect("json"),
                    21
                )
                .expect("persist"));
        }
    }

    #[test]
    fn mixed_fleet_keeps_activation_separate_from_liveness_and_rejects_legacy_snapshots() {
        let (_dir, store, path, environment) = saved_fixture();
        let saved = saved_config(&store, &path);
        let mut active = capture(&store, &path, &environment);
        active.runner_id = Some("active".into());
        active.runner_instance_id = Some("instance-a".into());
        register(&store, "active", "instance-a", Some(&active));
        let mut old = active.clone();
        old.runner_id = Some("old".into());
        old.runner_instance_id = Some("instance-b".into());
        old.loaded_revision = Some(uuid::Uuid::new_v4().to_string());
        register(&store, "old", "instance-b", Some(&old));
        register(&store, "legacy", "instance-c", None);
        let rows = store.list_runners().expect("rows");
        for row in &rows {
            let projection = runner_configuration(row, &saved, 100_000);
            assert_eq!(projection.freshness, "offline");
            assert_eq!(
                projection.activation,
                match row.id.as_str() {
                    "active" => "active",
                    "old" => "stale",
                    _ => "unknown",
                }
            );
        }
        let mut malformed = rows
            .iter()
            .find(|row| row.id == "active")
            .expect("active")
            .clone();
        for json in ["not json", "{}"] {
            malformed.startup_config_json = Some(json.to_string());
            assert_eq!(
                runner_configuration(&malformed, &saved, 21).activation,
                "unknown"
            );
        }
        let mut invalid = active;
        for case in ["id", "time", "version", "secret", "missing", "instance"] {
            let mut snapshot = invalid.clone();
            match case {
                "id" => snapshot.runner_id = Some("another-runner".into()),
                "time" => snapshot.started_at_ms += 1,
                "version" => snapshot.format_version += 1,
                "secret" => {
                    snapshot.values.insert(
                        "ANTHROPIC_API_KEY".into(),
                        EffectiveValue {
                            value: Some("synthetic-key".into()),
                            source: "file".into(),
                            reason: None,
                        },
                    );
                }
                "missing" => snapshot.values.clear(),
                _ => snapshot.runner_instance_id = None,
            }
            malformed.startup_config_json = Some(serde_json::to_string(&snapshot).expect("json"));
            let result = runner_configuration(&malformed, &saved, 21);
            assert_eq!(result.activation, "unknown", "case: {case}");
            assert!(result.snapshot.is_none());
        }
        invalid.env_id = "different-environment".to_string();
        assert_eq!(
            project_process(Some(invalid), &saved, "online", Some(21)).activation,
            "unknown"
        );
    }
}
