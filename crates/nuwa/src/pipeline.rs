//! 修复流水线：常驻 runner（`work`）与一次性 `nuwa fix` 共用的唯一实现。
//!
//! 两条入口曾各自拼装修复任务与交付参数，漂移是必然的：CLI 路径漏掉仓库
//! 指令 notes、也漏掉外部输入脱敏。这里收敛「怎么修、怎么交」——调用方只
//! 负责寻址、记账与失败分类。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;

use crate::instructions::RepoInstructions;
use crate::{deliver, guard, issue, repair, secrets, skills};

pub type ProgressSink = Arc<dyn Fn(repair::RepairProgress) + Send + Sync>;

/// 一次修复的完整输入。
pub struct RepairRequest {
    pub repo: PathBuf,
    pub issue: issue::Issue,
    pub sessions_dir: PathBuf,
    pub provider: repair::ProviderKind,
    pub max_turns: u32,
    /// 仓库指令（notes 与技能白名单）；一次性 CLI 允许缺失。
    pub repo_config: Option<RepoInstructions>,
    pub secrets: Arc<secrets::Secrets>,
    pub progress: Option<ProgressSink>,
    /// benchmark 场景注入的确定性 faux 脚本；普通 runner 为 None。
    pub faux_script: Option<rpi_ai::providers::faux::FauxScript>,
    /// 常驻 runner 的 run id；`None` 表示一次性 CLI（平铺会话目录）。
    pub run_id: Option<String>,
}

/// 已定址、尚未执行的修复。`session_dir` 在启动前即可确定，调用方可以先
/// 落库再开跑；失败路径的转录脱敏也由这里统一负责。
pub struct PreparedRepair {
    job: repair::RepairJob,
    session_dir: PathBuf,
    skills_used: Vec<String>,
}

impl PreparedRepair {
    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    pub fn issue(&self) -> &issue::Issue {
        &self.job.issue
    }

    /// 本次注入的技能（`name@version`），供决策 trace 记录。
    pub fn skills_used(&self) -> &[String] {
        &self.skills_used
    }

    pub async fn execute(&self) -> anyhow::Result<repair::RepairReport> {
        let result = repair::run(&self.job).await;
        // 成功与失败都要脱敏：模型可能在 run 死掉前把凭证写进转录，而
        // serve 会实时读取该文件展示 transcript。
        let dir = match &result {
            Ok(report) => Path::new(&report.session_dir),
            Err(_) => self.session_dir.as_path(),
        };
        let _ = self.job.secrets.redact_session_dir(dir);
        result
    }
}

pub fn prepare_repair(req: RepairRequest) -> anyhow::Result<PreparedRepair> {
    let session_dir = repair::session_dir_for(&req.sessions_dir, &req.issue, req.run_id.as_deref());
    let session_dir = std::path::absolute(&session_dir)
        .with_context(|| format!("resolve {}", session_dir.display()))?;
    // 技能选择在这里发生：两条入口共享同一套确定性规则与长度上限。
    let allowlist = req
        .repo_config
        .as_ref()
        .map(|config| config.skills.as_slice())
        .unwrap_or(&[]);
    let selection = skills::select(&req.repo, &req.issue, allowlist);
    for warning in &selection.warnings {
        eprintln!("skills    : {warning}");
    }
    let base_notes = req
        .repo_config
        .as_ref()
        .map(|config| config.notes.trim())
        .unwrap_or("");
    let notes = match (base_notes.is_empty(), selection.section.is_empty()) {
        (true, true) => String::new(),
        (false, true) => base_notes.to_string(),
        (true, false) => selection.section.trim_start().to_string(),
        (false, false) => format!("{base_notes}\n{}", selection.section),
    };
    let guards = Arc::new(guard::Guards::new(req.issue.forbidden_paths.clone()));
    let job = repair::RepairJob {
        repo: req.repo,
        issue: req.issue,
        sessions_dir: req.sessions_dir,
        provider: req.provider,
        max_turns: req.max_turns,
        guards,
        secrets: req.secrets,
        notes,
        progress: req.progress,
        faux_script: req.faux_script,
        run_id: req.run_id,
    };
    Ok(PreparedRepair {
        job,
        session_dir,
        skills_used: selection.used,
    })
}

/// 交付闸门的完整输入。策略参数（push/create_pr/草稿）由调用方决定，
/// 其余（护栏、脱敏、会话目录）在流水线内固定。
pub struct DeliveryRequest<'a> {
    pub repo: &'a Path,
    pub issue: &'a issue::Issue,
    pub report: &'a repair::RepairReport,
    pub sessions_dir: &'a Path,
    pub push: bool,
    pub create_pr: bool,
    pub public_base_url: Option<&'a str>,
    pub draft: bool,
    pub secrets: &'a secrets::Secrets,
}

pub async fn deliver(req: DeliveryRequest<'_>) -> anyhow::Result<deliver::DeliverReport> {
    deliver::deliver(
        req.repo,
        req.issue,
        req.report,
        req.push,
        req.create_pr,
        req.sessions_dir,
        req.secrets,
        req.public_base_url,
        req.draft,
    )
    .await
}

/// 外部信号 → 问题单。原始信号是外部输入：进入 LLM 上下文前先脱敏，
/// 两条入口不再各写一份字段映射。
pub fn issue_from_snapshot(
    snapshot: &shanhaijing::IssueSnapshot,
    repo_config: &RepoInstructions,
    prior_context: String,
    secrets: &secrets::Secrets,
) -> issue::Issue {
    redact_issue(
        issue::Issue {
            fingerprint: snapshot.fingerprint.clone(),
            title: snapshot.title.clone(),
            language: snapshot.language.clone(),
            occurrences: snapshot.occurrences,
            severity: snapshot.severity.clone(),
            first_seen: format_timestamp(snapshot.first_seen),
            last_seen: format_timestamp(snapshot.last_seen),
            stack_trace: snapshot.stack_sample.clone(),
            log_window: snapshot.log_window.clone(),
            prior_context,
            failing_check: repo_config.failing_check.clone(),
            reproduce_check: repo_config.reproduce_check.clone(),
            forbidden_paths: repo_config.forbidden_paths.clone(),
            verification_mode: Default::default(),
        },
        secrets,
    )
}

/// Unix milliseconds as `YYYY-MM-DD HH:MM:SS UTC` for the repair prompt.
/// Hand-rolled so the runner needs no date library.
fn format_timestamp(ms: i64) -> String {
    if ms <= 0 {
        return "(unknown)".to_string();
    }
    let seconds = ms.div_euclid(1_000);
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60
    )
}

/// Days since 1970-01-01 → `(year, month, day)`. Howard Hinnant's
/// `civil_from_days` (public-domain algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { year + 1 } else { year }, month as u32, day as u32)
}

/// 一次性 CLI 从文件加载的问题单同样是外部输入。
pub fn redact_issue(mut issue: issue::Issue, secrets: &secrets::Secrets) -> issue::Issue {
    issue.title = secrets.redact(&issue.title);
    issue.stack_trace = secrets.redact(&issue.stack_trace);
    issue.log_window = issue
        .log_window
        .iter()
        .map(|line| secrets.redact(line))
        .collect();
    issue.prior_context = secrets.redact(&issue.prior_context);
    issue
}

/// 人类可读的 provider 标识，打印与日志共用。
pub fn provider_label(provider: &repair::ProviderKind) -> String {
    match provider {
        repair::ProviderKind::Faux => "faux".to_string(),
        repair::ProviderKind::Anthropic { model } => match model {
            Some(m) => format!("anthropic:{m}"),
            None => "anthropic:<catalog default>".to_string(),
        },
        repair::ProviderKind::Zai { model, .. } => match model {
            Some(m) => format!("zai:{m}"),
            None => "zai:glm-5.3-flash".to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::FailingCheck;

    fn issue() -> issue::Issue {
        issue::Issue {
            fingerprint: "abc/def".into(),
            title: "token ghp_abcdefghijklmnopqrstuvwxyz0123456789".into(),
            language: "go".into(),
            occurrences: 1,
            severity: "high".into(),
            first_seen: String::new(),
            last_seen: String::new(),
            stack_trace: "panic".into(),
            log_window: vec!["auth header: Bearer ghp_abcdefghijklmnopqrstuvwxyz0123456789".into()],
            prior_context: String::new(),
            failing_check: FailingCheck {
                command: "go test ./...".into(),
                target: None,
                timeout_secs: 60,
            },
            reproduce_check: None,
            forbidden_paths: Vec::new(),
            verification_mode: Default::default(),
        }
    }

    #[test]
    fn redaction_covers_every_external_field() {
        let secrets = secrets::Secrets::with_values(vec![]);
        let redacted = redact_issue(issue(), &secrets);
        assert!(!redacted.title.contains("ghp_"));
        assert!(!redacted.log_window[0].contains("ghp_"));
    }

    fn snapshot() -> shanhaijing::IssueSnapshot {
        shanhaijing::IssueSnapshot {
            fingerprint: "abc/def".into(),
            title: "panic".into(),
            language: "go".into(),
            repo: "example.com/app".into(),
            severity: "high".into(),
            occurrences: 2,
            attempts: 0,
            stack_sample: "panic: boom".into(),
            log_window: vec!["line one".into(), "line two".into()],
            first_seen: 1_000,
            last_seen: 2_000,
        }
    }

    #[test]
    fn issue_from_snapshot_carries_the_log_window_and_timestamps() {
        let instructions = RepoInstructions {
            repo: "example.com/app".into(),
            failing_check: FailingCheck {
                command: "go test ./...".into(),
                target: None,
                timeout_secs: 60,
            },
            forbidden_paths: Vec::new(),
            notes: String::new(),
            skills: Vec::new(),
            reproduce_check: None,
            max_attempts: 3,
            draft_first_n: 0,
        };
        let built = issue_from_snapshot(
            &snapshot(),
            &instructions,
            String::new(),
            &secrets::Secrets::default(),
        );
        assert_eq!(built.log_window, vec!["line one".to_string(), "line two".to_string()]);
        assert_eq!(built.first_seen, "1970-01-01 00:00:01 UTC");
        assert_eq!(built.last_seen, "1970-01-01 00:00:02 UTC");
    }

    #[test]
    fn timestamps_format_as_utc_for_the_prompt() {
        assert_eq!(format_timestamp(0), "(unknown)");
        assert_eq!(format_timestamp(1_000), "1970-01-01 00:00:01 UTC");
        // Unix billennium — a well-known checkpoint.
        assert_eq!(format_timestamp(1_000_000_000_000), "2001-09-09 01:46:40 UTC");
        // Leap day in a leap century (2000 is divisible by 400).
        assert_eq!(format_timestamp(951_782_400_000), "2000-02-29 00:00:00 UTC");
    }

    #[test]
    fn session_dir_is_per_run_and_absolute() {
        let req = RepairRequest {
            repo: PathBuf::from("."),
            issue: issue(),
            sessions_dir: PathBuf::from("sessions"),
            provider: repair::ProviderKind::Faux,
            max_turns: 1,
            repo_config: None,
            secrets: Arc::new(secrets::Secrets::default()),
            progress: None,
            faux_script: None,
            run_id: Some("run-1".into()),
        };
        let prepared = prepare_repair(req).expect("prepare");
        assert!(prepared.session_dir().is_absolute());
        assert!(prepared.session_dir().ends_with("abc-def/run-1"));
    }

    #[test]
    fn cli_session_dir_stays_flat() {
        let dir = std::env::temp_dir().join("nuwa-pipeline-flat");
        let dir = repair::session_dir_for(&dir, &issue(), None);
        assert!(dir.ends_with("abc-def"));
    }
}
