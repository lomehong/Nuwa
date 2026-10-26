//! 设置模型 A（docs/09 §5、§13）：env 文件为唯一事实源（systemd
//! EnvironmentFile）。本模块保留规范化键表、设置 schema、env 读写与迁移
//! 辅助；HTTP 读写与重启编排由 `/api/v1/settings` 提供（M2c）。

use std::collections::{BTreeMap, BTreeSet};

/// Every key the console knows (docs/09 §13.1). Uppercase is canonical; the
/// startup migration rewrites historical lowercase spellings.
pub const ENV_KEYS: &[&str] = &[
    "NUWA_GITHUB_TOKEN",
    "ZAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "NUWA_INGEST_TOKEN",
    "NUWA_UI_TOKEN",
    "NUWA_WEBHOOK_URL",
    "NUWA_WEBHOOK_SECRET",
    "NUWA_WEBHOOK_EVENTS",
    "NUWA_PROVIDER",
    "NUWA_MODEL",
    "NUWA_BASE_URL",
    "NUWA_PUBLIC_BASE_URL",
    "NUWA_INTERVAL",
    "NUWA_CI_TIMEOUT",
    "NUWA_SYNC_INTERVAL",
    "NUWA_DAILY_PR_LIMIT",
    "NUWA_MAX_TURNS",
    "NUWA_MAX_TOKENS",
    "NUWA_DAILY_TOKEN_LIMIT",
    "NUWA_MONTHLY_TOKEN_LIMIT",
    "NUWA_RUNNER_ID",
    "NUWA_SERVICE_UNIT",
    "NUWA_SERVE_UNIT",
    "NUWA_RUNNER_UNITS",
    "NUWA_COOKIE_SECURE",
    "NUWA_SESSION_TTL_SECS",
    "NUWA_STUCK_AFTER_SECS",
    "NUWA_EVENT_RETENTION_DAYS",
    "NUWA_ENV_FILE",
];

/// Secrets are derived from [`SettingSpec::secret`] in production; this list
/// exists so the schema test (and the API schema test) can cross-check it.
#[cfg(test)]
pub(crate) const SECRET_KEYS: &[&str] = &[
    "NUWA_GITHUB_TOKEN",
    "ZAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "NUWA_INGEST_TOKEN",
    "NUWA_UI_TOKEN",
    "NUWA_WEBHOOK_SECRET",
];

/// One canonical setting (docs/09 §9.2 settings schema, §13.1 key table).
/// Static so the API can serve the schema without allocating or copying.
#[derive(Debug, Clone, Copy)]
pub struct SettingSpec {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    /// `secret` | `string` | `int` | `enum` | `url` | `csv` | `path`.
    pub kind: &'static str,
    /// `None` for keys without a default (the secrets).
    pub default: Option<&'static str>,
    pub options: &'static [&'static str],
    pub secret: bool,
    /// `both` | `work` | `serve`.
    pub scope: &'static str,
    /// The env file is read at process start, so every write needs a restart.
    pub requires_restart: bool,
    pub read_only: bool,
    pub help: &'static str,
    pub min: Option<u64>,
    pub max: Option<u64>,
}

// One call per schema row; grouping the arguments into a struct would make
// the table harder to scan than the lint is worth.
#[allow(clippy::too_many_arguments)]
const fn spec(
    key: &'static str,
    label: &'static str,
    group: &'static str,
    kind: &'static str,
    default: Option<&'static str>,
    options: &'static [&'static str],
    secret: bool,
    scope: &'static str,
    help: &'static str,
) -> SettingSpec {
    SettingSpec {
        key,
        label,
        group,
        kind,
        default,
        options,
        secret,
        scope,
        requires_restart: true,
        read_only: false,
        help,
        min: None,
        max: None,
    }
}

const fn bounded(mut setting: SettingSpec, min: u64, max: u64) -> SettingSpec {
    setting.min = Some(min);
    setting.max = Some(max);
    setting
}

const fn read_only(mut setting: SettingSpec) -> SettingSpec {
    setting.read_only = true;
    setting
}

/// Canonical key schema from docs/09 §13.1, in table order.
pub const SETTINGS_SCHEMA: &[SettingSpec] = &[
    spec(
        "NUWA_GITHUB_TOKEN",
        "GitHub token",
        "credentials",
        "secret",
        None,
        &[],
        true,
        "both",
        "GitHub 细粒度 PAT：Contents 读写 + Actions 只读",
    ),
    spec(
        "ZAI_API_KEY",
        "Z.AI API key",
        "credentials",
        "secret",
        None,
        &[],
        true,
        "work",
        "智谱 API Key（provider=zai 时使用）",
    ),
    spec(
        "ANTHROPIC_API_KEY",
        "Anthropic API key",
        "credentials",
        "secret",
        None,
        &[],
        true,
        "work",
        "Anthropic API Key（provider=anthropic 时使用）",
    ),
    spec(
        "NUWA_INGEST_TOKEN",
        "Ingest token",
        "credentials",
        "secret",
        None,
        &[],
        true,
        "serve",
        "上报令牌（X-NUWA-Token）；留空 = 本地模式",
    ),
    spec(
        "NUWA_UI_TOKEN",
        "Console token",
        "credentials",
        "secret",
        None,
        &[],
        true,
        "serve",
        "控制台登录令牌；留空则回退到上报令牌",
    ),
    spec(
        "NUWA_WEBHOOK_URL",
        "Webhook URL",
        "ops",
        "url",
        Some(""),
        &[],
        false,
        "serve",
        "状态变更 webhook（escalated/ignored/heartbeat_stale/ci_red/sync_error/pr_opened）",
    ),
    spec(
        "NUWA_WEBHOOK_SECRET",
        "Webhook secret",
        "ops",
        "secret",
        None,
        &[],
        true,
        "serve",
        "HMAC-SHA256 签名密钥（X-Nuwa-Signature: sha256=…）",
    ),
    spec(
        "NUWA_WEBHOOK_EVENTS",
        "Webhook events",
        "ops",
        "csv",
        Some("escalated,ignored,provider_stand_down,heartbeat_stale,ci_red,sync_error,pr_opened"),
        &[],
        false,
        "serve",
        "订阅的事件（逗号分隔）",
    ),
    spec(
        "NUWA_PROVIDER",
        "LLM Provider",
        "model",
        "enum",
        Some("anthropic"),
        &["zai", "anthropic", "faux"],
        false,
        "work",
        "修复 runner 使用的模型提供方",
    ),
    spec(
        "NUWA_MODEL",
        "Model",
        "model",
        "string",
        Some(""),
        &[],
        false,
        "work",
        "模型名称，由 provider 决定",
    ),
    spec(
        "NUWA_BASE_URL",
        "Base URL",
        "model",
        "url",
        Some(""),
        &[],
        false,
        "work",
        "OpenAI 兼容端点；必须是 http/https URL",
    ),
    spec(
        "NUWA_PUBLIC_BASE_URL",
        "Public console URL",
        "model",
        "url",
        Some(""),
        &[],
        false,
        "work",
        "控制台公网地址；用于 PR 正文里的 issue/转录链接（留空则省略）",
    ),
    bounded(
        spec(
            "NUWA_INTERVAL",
            "Poll interval",
            "runner",
            "int",
            Some("30"),
            &[],
            false,
            "work",
            "轮询间隔（秒）",
        ),
        1,
        86_400,
    ),
    bounded(
        spec(
            "NUWA_CI_TIMEOUT",
            "CI timeout",
            "runner",
            "int",
            Some("600"),
            &[],
            false,
            "work",
            "CI 等待上限（秒）",
        ),
        1,
        86_400,
    ),
    bounded(
        spec(
            "NUWA_SYNC_INTERVAL",
            "Sync interval",
            "ops",
            "int",
            Some("600"),
            &[],
            false,
            "serve",
            "自动对账 pr_open 的间隔（秒，60–86400）",
        ),
        60,
        86_400,
    ),
    bounded(
        spec(
            "NUWA_DAILY_PR_LIMIT",
            "Daily PR limit",
            "runner",
            "int",
            Some("5"),
            &[],
            false,
            "work",
            "每日 PR 上限",
        ),
        1,
        100,
    ),
    bounded(
        spec(
            "NUWA_MAX_TURNS",
            "Max turns",
            "runner",
            "int",
            Some("30"),
            &[],
            false,
            "work",
            "单问题单次修复 turn 上限",
        ),
        1,
        1_000,
    ),
    bounded(
        spec(
            "NUWA_MAX_TOKENS",
            "Max tokens",
            "runner",
            "int",
            Some("1500000"),
            &[],
            false,
            "work",
            "单问题 token 上限",
        ),
        1,
        100_000_000,
    ),
    bounded(
        spec(
            "NUWA_DAILY_TOKEN_LIMIT",
            "Daily token limit",
            "runner",
            "int",
            Some("5000000"),
            &[],
            false,
            "work",
            "每仓库每日 token 花费上限（失败尝试也计入）",
        ),
        1,
        1_000_000_000,
    ),
    bounded(
        spec(
            "NUWA_MONTHLY_TOKEN_LIMIT",
            "Monthly token limit",
            "runner",
            "int",
            Some("50000000"),
            &[],
            false,
            "work",
            "每仓库滚动 30 天 token 花费上限",
        ),
        1,
        10_000_000_000,
    ),
    spec(
        "NUWA_RUNNER_ID",
        "Runner ID",
        "runner",
        "string",
        Some("host:repo"),
        &[],
        false,
        "work",
        "runner 身份；[A-Za-z0-9._:-]，≤128 字符",
    ),
    spec(
        "NUWA_SERVICE_UNIT",
        "Service unit",
        "runner",
        "string",
        Some("nuwa-work"),
        &[],
        false,
        "work",
        "work 进程的 systemd unit",
    ),
    spec(
        "NUWA_SERVE_UNIT",
        "Serve unit",
        "service",
        "string",
        Some("nuwa-serve"),
        &[],
        false,
        "serve",
        "serve 进程的 systemd unit",
    ),
    spec(
        "NUWA_RUNNER_UNITS",
        "Runner units",
        "service",
        "csv",
        Some("nuwa-work"),
        &[],
        false,
        "serve",
        "重启时一并重启的 work units（逗号分隔）",
    ),
    spec(
        "NUWA_COOKIE_SECURE",
        "Cookie secure",
        "security",
        "enum",
        Some("auto"),
        &["auto", "always", "never"],
        false,
        "serve",
        "会话 cookie 的 Secure 策略",
    ),
    bounded(
        spec(
            "NUWA_SESSION_TTL_SECS",
            "Session TTL",
            "security",
            "int",
            Some("86400"),
            &[],
            false,
            "serve",
            "控制台会话有效期（秒）",
        ),
        300,
        2_592_000,
    ),
    bounded(
        spec(
            "NUWA_STUCK_AFTER_SECS",
            "Stuck floor",
            "ops",
            "int",
            Some("300"),
            &[],
            false,
            "serve",
            "卡单判定下限（秒）",
        ),
        60,
        86_400,
    ),
    bounded(
        spec(
            "NUWA_EVENT_RETENTION_DAYS",
            "Event retention",
            "ops",
            "int",
            Some("90"),
            &[],
            false,
            "serve",
            "事件保留天数",
        ),
        1,
        3_650,
    ),
    read_only(spec(
        "NUWA_ENV_FILE",
        "Env file",
        "advanced",
        "path",
        Some("/opt/nuwa/nuwa.env"),
        &[],
        false,
        "both",
        "env 文件路径（只读展示）",
    )),
];

pub fn setting_spec(key: &str) -> Option<&'static SettingSpec> {
    SETTINGS_SCHEMA.iter().find(|setting| setting.key == key)
}

pub fn env_file_path() -> std::path::PathBuf {
    crate::env_store::var("NUWA_ENV_FILE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/opt/nuwa/nuwa.env"))
}

pub fn parse_env(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            map.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    map
}

/// 把历史小写键并入规范大写键；大写优先，删除小写重复项。
pub fn normalize_known_keys(
    map: &BTreeMap<String, String>,
) -> (BTreeMap<String, String>, Vec<String>) {
    let mut out = map.clone();
    let mut changed = BTreeSet::new();
    for key in ENV_KEYS {
        let lower = key.to_ascii_lowercase();
        if let Some(lower_value) = map.get(&lower).cloned() {
            if !map.contains_key(*key) {
                out.insert((*key).to_string(), lower_value);
            }
            out.remove(&lower);
            changed.insert((*key).to_string());
        }
    }
    (out, changed.into_iter().collect())
}

pub fn migrate_env_file_at(path: &std::path::Path) -> Result<Vec<String>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let (new, changed) = normalize_known_keys(&parse_env(&text));
    if changed.is_empty() {
        return Ok(changed);
    }
    // 迁移会重排文件（注释/顺序不保留）：先备份，再原子替换。
    let backup = path.with_extension("env.bak");
    std::fs::copy(path, &backup)
        .map_err(|e| format!("backup {} -> {}: {e}", path.display(), backup.display()))?;
    write_env_atomic(path, &render_env(&new))?;
    Ok(changed)
}

/// 原子写：Unix 先写临时文件（chmod 600）再 rename 覆盖；失败时清理临时文件。
pub(crate) fn write_env_atomic(path: &std::path::Path, contents: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // A unique temp name: two concurrent writers (or a migrate racing a
        // PUT) must not share `nuwa.env.tmp`, where one could rename or delete
        // the other's half-written file. The write lock in api/settings.rs
        // serializes PUTs, but migrate-env is a separate entry point.
        let tmp = path.with_extension(format!("env.{}.tmp", uuid::Uuid::new_v4().simple()));
        let write = (|| -> std::io::Result<()> {
            std::fs::write(&tmp, contents)?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
            std::fs::rename(&tmp, path)
        })();
        if let Err(e) = write {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("write {}: {e}", path.display()));
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents).map_err(|e| format!("write {}: {e}", path.display()))
    }
}

pub fn migrate_env_file() -> Result<Vec<String>, String> {
    migrate_env_file_at(&env_file_path())
}

pub fn render_env(map: &BTreeMap<String, String>) -> String {
    let mut out = String::from("# Nuwa env — edited via web console (docs/09 §5)\n");
    for (k, v) in map {
        out.push_str(&format!("{k}={v}\n"));
    }
    out
}

/// Copy `path` to the sibling `.bak` slot before a rewrite. A missing file is
/// a no-op; any other failure aborts the caller, so the original is never
/// lost by a write that could not be backed up. On Unix the copy is chmodded
/// to 0600 (best effort) so the backup is as private as the env file itself.
pub(crate) fn backup_env_file(path: &std::path::Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let backup = path.with_extension("env.bak");
    std::fs::copy(path, &backup)
        .map_err(|e| format!("backup {} -> {}: {e}", path.display(), backup.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // A failed chmod must not fail the save: the copy already exists and
        // the write is what the operator asked for.
        let _ = std::fs::set_permissions(&backup, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Rewrite `text` in place without reordering it (docs/09 §13.2):
///
/// * historical lowercase known keys move to their canonical uppercase
///   spelling on the same line; a lowercase duplicate is dropped when the
///   uppercase spelling is also present (uppercase wins);
/// * every `KEY=VALUE` line matching a key in `updates` gets the new value,
///   keeping its position and line ending;
/// * keys in `updates` that do not appear in the file are appended;
/// * comments, blank lines, unknown keys, malformed lines and CRLF endings
///   are preserved byte for byte.
pub(crate) fn update_env_text(text: &str, updates: &BTreeMap<String, String>) -> String {
    let parsed = parse_env(text);
    let present: BTreeSet<String> = parsed.keys().cloned().collect();
    // Only used for lines we append; existing line endings are kept verbatim.
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut written: BTreeSet<String> = BTreeSet::new();
    let mut out = String::with_capacity(text.len() + updates.len() * 24);

    for line in text.split_inclusive('\n') {
        let (body, ending) = split_line_ending(line);
        let trimmed = body.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            out.push_str(body);
            out.push_str(ending);
            continue;
        }
        let Some((raw_key, _)) = body.split_once('=') else {
            // Malformed lines are preserved; a known key they mention is not
            // seen by parse_env either, so an update for it is appended.
            out.push_str(body);
            out.push_str(ending);
            continue;
        };
        let raw_key = raw_key.trim();
        let key = raw_key.to_ascii_uppercase();
        if !ENV_KEYS.contains(&key.as_str()) {
            out.push_str(body);
            out.push_str(ending);
            continue;
        }
        if raw_key != key && present.contains(&key) {
            // Uppercase spelling wins; drop the historical lowercase duplicate.
            continue;
        }
        if raw_key == key && !updates.contains_key(&key) {
            out.push_str(body);
        } else {
            let value = updates
                .get(&key)
                .or_else(|| parsed.get(&key))
                .or_else(|| parsed.get(raw_key))
                .map(String::as_str)
                .unwrap_or_default();
            out.push_str(&format!("{key}={value}"));
            written.insert(key);
        }
        out.push_str(ending);
    }

    for (key, value) in updates {
        if written.contains(key) || present.contains(key) {
            continue;
        }
        if !out.is_empty() && !out.ends_with('\n') {
            out.push_str(eol);
        }
        out.push_str(&format!("{key}={value}{eol}"));
    }
    out
}

/// Split one `split_inclusive('\n')` segment into its body and line ending.
fn split_line_ending(line: &str) -> (&str, &str) {
    if let Some(body) = line.strip_suffix("\r\n") {
        (body, "\r\n")
    } else if let Some(body) = line.strip_suffix('\n') {
        (body, "\n")
    } else {
        (line, "")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        backup_env_file, migrate_env_file_at, normalize_known_keys, render_env, update_env_text,
        SECRET_KEYS, SETTINGS_SCHEMA,
    };
    use std::collections::BTreeMap;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn migration_moves_lowercase_known_keys_to_uppercase() {
        let old = map(&[("nuwa_interval", "30"), ("nuwa_provider", "zai")]);
        let (new, changed) = normalize_known_keys(&old);
        assert_eq!(new.get("NUWA_INTERVAL").map(String::as_str), Some("30"));
        assert_eq!(new.get("NUWA_PROVIDER").map(String::as_str), Some("zai"));
        assert!(!new.contains_key("nuwa_interval"));
        assert!(changed.contains(&"NUWA_INTERVAL".to_string()));
        assert!(changed.contains(&"NUWA_PROVIDER".to_string()));
    }

    #[test]
    fn uppercase_wins_over_lowercase_duplicate() {
        let old = map(&[("NUWA_INTERVAL", "60"), ("nuwa_interval", "30")]);
        let (new, _) = normalize_known_keys(&old);
        assert_eq!(new.get("NUWA_INTERVAL").map(String::as_str), Some("60"));
        assert!(!new.contains_key("nuwa_interval"));
    }

    #[test]
    fn render_env_writes_uppercase_keys() {
        let out = render_env(&map(&[("NUWA_INTERVAL", "60")]));
        assert!(out.contains("NUWA_INTERVAL=60"));
        assert!(!out.contains("nuwa_interval"));
    }

    #[test]
    fn update_env_text_replaces_in_place_and_appends_new_keys() {
        let text = "# operator note\n\
                    NUWA_MODEL=custom-model\n\
                    \n\
                    NUWA_INTERVAL=30\n\
                    LEGACY_KEY=keep-me\n";
        let updates = map(&[("NUWA_INTERVAL", "60"), ("NUWA_MAX_TURNS", "50")]);
        let out = update_env_text(text, &updates);
        assert_eq!(
            out,
            "# operator note\n\
             NUWA_MODEL=custom-model\n\
             \n\
             NUWA_INTERVAL=60\n\
             LEGACY_KEY=keep-me\n\
             NUWA_MAX_TURNS=50\n",
            "comments, order and unknown keys stay; only the target line and the appended key change"
        );
    }

    #[test]
    fn update_env_text_keeps_crlf_and_missing_trailing_newline() {
        let text = "# note\r\nNUWA_INTERVAL=30";
        let updates = map(&[("NUWA_INTERVAL", "60"), ("NUWA_MODEL", "glm")]);
        let out = update_env_text(text, &updates);
        assert_eq!(
            out, "# note\r\nNUWA_INTERVAL=60\r\nNUWA_MODEL=glm\r\n",
            "CRLF is kept and appended keys start on a new line"
        );
    }

    #[test]
    fn update_env_text_migrates_lowercase_keys_in_place() {
        let text = "# note\n\
                    nuwa_interval=45\n\
                    nuwa_provider=zai\n";
        let out = update_env_text(text, &BTreeMap::new());
        assert_eq!(out, "# note\nNUWA_INTERVAL=45\nNUWA_PROVIDER=zai\n");
    }

    #[test]
    fn update_env_text_drops_lowercase_duplicate_when_uppercase_wins() {
        let text = "NUWA_INTERVAL=60\n\
                    nuwa_interval=30\n";
        let out = update_env_text(text, &BTreeMap::new());
        assert_eq!(out, "NUWA_INTERVAL=60\n");
    }

    #[test]
    fn schema_secret_flags_match_the_secret_key_list() {
        for spec in SETTINGS_SCHEMA {
            assert_eq!(
                spec.secret,
                SECRET_KEYS.contains(&spec.key),
                "{}: secret flag and SECRET_KEYS must agree",
                spec.key
            );
        }
    }

    #[test]
    fn backup_env_file_copies_next_to_the_original() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nuwa.env");
        std::fs::write(&path, "NUWA_INTERVAL=30\n").expect("write");
        backup_env_file(&path).expect("backup");
        assert_eq!(
            std::fs::read_to_string(path.with_extension("env.bak")).expect("read backup"),
            "NUWA_INTERVAL=30\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path.with_extension("env.bak"))
                .expect("backup metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the backup must not be world-readable");
        }

        // A missing file has nothing to back up.
        let absent = dir.path().join("absent.env");
        backup_env_file(&absent).expect("missing file is a no-op");
        assert!(!absent.with_extension("env.bak").exists());
    }

    #[test]
    fn migrate_env_file_missing_path_is_noop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("absent.env");
        let changed = migrate_env_file_at(&path).expect("noop");
        assert!(changed.is_empty());
        assert!(!path.exists());
        assert!(!path.with_extension("env.bak").exists());
    }

    #[test]
    fn migrate_env_file_is_idempotent_and_keeps_uppercase_value() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nuwa.env");
        std::fs::write(
            &path,
            "NUWA_INTERVAL=60\nnuwa_interval=30\nnuwa_provider=zai\n",
        )
        .expect("write");

        let changed = migrate_env_file_at(&path).expect("migrate");
        assert!(changed.contains(&"NUWA_INTERVAL".to_string()));
        assert!(changed.contains(&"NUWA_PROVIDER".to_string()));

        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("NUWA_INTERVAL=60"));
        assert!(text.contains("NUWA_PROVIDER=zai"));
        assert!(!text.contains("nuwa_interval"));
        assert!(!text.contains("nuwa_provider"));

        // 迁移前原文件已备份（含小写历史键）。
        let backup = std::fs::read_to_string(path.with_extension("env.bak")).expect("backup");
        assert!(backup.contains("nuwa_interval=30"));

        assert!(migrate_env_file_at(&path).expect("second").is_empty());
    }
}
