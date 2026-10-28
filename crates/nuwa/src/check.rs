//! `nuwa check` — validate a repository's instruction file before the first
//! repair. Catches bad JSON, empty commands, invalid globs, and a
//! `failing_check` that is already green on the base (which cannot prove a
//! red→green fix).

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::Serialize;

use crate::git_ops;
use crate::instructions::{RepoInstructions, TrustedInstructions, VerificationPolicy};
use crate::secrets::Secrets;
use crate::verify;

#[derive(Debug, Serialize)]
struct Probe {
    command: String,
    status: String,
    output: String,
}

#[derive(Debug, Serialize)]
struct CheckReport {
    ok: bool,
    errors: Vec<String>,
    warnings: Vec<String>,
    repo: String,
    base: String,
    origin: Option<String>,
    failing_check: Option<Probe>,
    reproduce_check: Option<Probe>,
    verification: String,
}

pub async fn run(args: Vec<String>) -> anyhow::Result<()> {
    let mut instructions_path: Option<PathBuf> = None;
    let mut repo_path: Option<PathBuf> = None;
    let mut base = "main".to_string();
    let mut json = false;
    let mut it = args.into_iter();
    while let Some(flag) = it.next() {
        let mut value = |name: &str| {
            it.next()
                .with_context(|| format!("missing value for {name}"))
        };
        match flag.as_str() {
            "--instructions" => instructions_path = Some(PathBuf::from(value("--instructions")?)),
            "--repo" => repo_path = Some(PathBuf::from(value("--repo")?)),
            "--base" => base = value("--base")?,
            "--json" => json = true,
            other => bail!("unknown check flag {other}"),
        }
    }
    let instructions_path = instructions_path.context("--instructions <path> is required")?;
    let repo_dir = repo_path
        .or_else(|| instructions_path.parent().map(Path::to_path_buf))
        .context("instructions path has no parent")?;
    if !repo_dir.join(".git").exists() {
        bail!("{} is not a git repository", repo_dir.display());
    }
    let base = resolve_base(&repo_dir, &base)?;
    // Check exactly the authority repair will adopt, not a mutable JSON copy.
    let private_owner = tempfile::tempdir().context("check owner authority")?;
    let owner =
        TrustedInstructions::load(&repo_dir, &instructions_path, &base, private_owner.path())?;
    let config = &owner.instructions;

    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    validate_config(config, &mut errors, &mut warnings);
    let (skill_errors, skill_warnings) = crate::skills::validate(&repo_dir, &config.skills);
    errors.extend(skill_errors);
    warnings.extend(skill_warnings);
    let origin = git_ops::git(&repo_dir, &["remote", "get-url", "origin"])
        .ok()
        .map(|url| git_ops::sanitize_remote(&url));
    if origin.is_none() {
        warnings.push("no origin remote; push/PR checks are unavailable".to_string());
    }

    let secrets = Secrets::default();
    let pinned_base = git_ops::git(&repo_dir, &["rev-parse", "--verify", &base])?;
    let prepared = verify::prepare(&repo_dir, &pinned_base, Some(&owner), &secrets).await;
    let owner_ready = prepared.is_ok();
    let verification = match prepared {
        Ok(context) if context.is_trusted() => "owner_trusted: baseline red".to_string(),
        Ok(_) => "legacy_untrusted".to_string(),
        Err(error) => {
            errors.push(format!(
                "owner verification configuration/baseline invalid: {error:#}"
            ));
            "invalid".to_string()
        }
    };
    let failing = if !owner_ready {
        None
    } else {
        match verify::probe_check(&repo_dir, &base, &config.failing_check, &secrets).await {
            Ok(output) => {
                if output.timed_out || !matches!(output.exit_code, Some(0 | 1)) {
                    errors.push("failing_check baseline has an infrastructure/timeout failure, not a demonstrated bug".into());
                }
                if output.ok && config.trusted_verification.is_none() {
                    warnings.push(
                        "failing_check is green on the base commit; configure reproduce_check or add a reproducer"
                            .to_string(),
                    );
                }
                Some(Probe {
                    command: config.failing_check.command.clone(),
                    status: status(&output),
                    output: if config.trusted_verification.is_some() {
                        format!("exit={:?}; timeout={}", output.exit_code, output.timed_out)
                    } else {
                        output.output
                    },
                })
            }
            Err(error) => {
                errors.push(if config.trusted_verification.is_some() {
                    "trusted failing_check baseline could not execute".into()
                } else {
                    format!("failing_check probe failed: {error:#}")
                });
                None
            }
        }
    };
    // A legacy reproducer is never the oracle for a configured trusted run.
    let reproduce = match config
        .reproduce_check
        .as_ref()
        .filter(|_| owner_ready && config.trusted_verification.is_none())
    {
        Some(check) => match verify::probe_check(&repo_dir, &base, check, &secrets).await {
            Ok(output) => {
                if output.timed_out || !matches!(output.exit_code, Some(0 | 1)) {
                    errors.push("reproduce_check baseline has an infrastructure/timeout failure, not a demonstrated bug".into());
                }
                if output.ok {
                    errors.push(
                        "reproduce_check is green on the base commit — it does not reproduce the bug"
                            .to_string(),
                    );
                }
                Some(Probe {
                    command: check.command.clone(),
                    status: status(&output),
                    output: output.output,
                })
            }
            Err(error) => {
                errors.push(format!("reproduce_check probe failed: {error:#}"));
                None
            }
        },
        None => None,
    };

    let report = CheckReport {
        ok: errors.is_empty(),
        errors,
        warnings,
        repo: config.repo.clone(),
        base,
        origin,
        failing_check: failing,
        reproduce_check: reproduce,
        verification,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("repo       : {}", report.repo);
        println!("base       : {}", report.base);
        println!("verification: {}", report.verification);
        println!("origin     : {}", report.origin.as_deref().unwrap_or("-"));
        if let Some(probe) = &report.failing_check {
            println!(
                "failing    : {} ({})\n            {}",
                probe.command, probe.status, probe.output
            );
        }
        if let Some(probe) = &report.reproduce_check {
            println!(
                "reproduce  : {} ({})\n            {}",
                probe.command, probe.status, probe.output
            );
        }
        for warning in &report.warnings {
            println!("warning    : {warning}");
        }
        for error in &report.errors {
            println!("error      : {error}");
        }
        if report.ok {
            println!("result     : ok");
        }
    }
    if !report.ok {
        std::process::exit(1);
    }
    Ok(())
}

fn validate_config(
    config: &RepoInstructions,
    errors: &mut Vec<String>,
    warnings: &mut Vec<String>,
) {
    if config.repo.trim().is_empty() {
        errors.push("repo must not be empty".to_string());
    }
    if config.failing_check.command.trim().is_empty() {
        errors.push("failing_check.command must not be empty".to_string());
    }
    if config.max_attempts == 0 {
        errors.push("max_attempts must be >= 1".to_string());
    }
    if let Err(error) = verify::validate_forbidden_paths(&config.forbidden_paths) {
        errors.push(format!("{error:#}"));
    }
    if let Err(error) = verify::validate_forbidden_paths(&config.denied_read_paths) {
        errors.push(format!("denied_read_paths: {error:#}"));
    }
    if config.verification_policy == VerificationPolicy::Strict
        && config.trusted_verification.is_none()
    {
        errors.push("strict verification requires an owner oracle".to_string());
    }
    if let Some(spec) = &config.trusted_verification {
        if let Err(error) = crate::instructions::validate_owner_oracle(spec) {
            errors.push(format!("owner oracle: {error:#}"));
        }
    }
    if let Some(check) = &config.reproduce_check {
        if check.command.trim().is_empty() {
            errors.push("reproduce_check.command must not be empty".to_string());
        }
    } else if config.trusted_verification.is_none() {
        warnings.push(
            "reproduce_check is not configured; a base-green failing_check will require .nuwa/repro.sh"
                .to_string(),
        );
    }
}

fn resolve_base(repo_dir: &Path, requested: &str) -> anyhow::Result<String> {
    if git_ops::branch_exists(repo_dir, requested)? {
        return Ok(requested.to_string());
    }
    for candidate in ["main", "master"] {
        if git_ops::branch_exists(repo_dir, candidate)? {
            return Ok(candidate.to_string());
        }
    }
    bail!("no usable base branch (requested `{requested}`; tried main/master)")
}

fn status(output: &verify::CheckOutput) -> String {
    if output.timed_out {
        "timed out".to_string()
    } else if output.ok {
        "green".to_string()
    } else {
        "red".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::issue::FailingCheck;

    fn config() -> RepoInstructions {
        RepoInstructions {
            repo: "example.com/app".into(),
            failing_check: FailingCheck {
                command: "go test ./...".into(),
                target: None,
                timeout_secs: 600,
            },
            forbidden_paths: vec!["*.pem".into()],
            denied_read_paths: Vec::new(),
            verification_policy: Default::default(),
            trusted_verification: None,
            notes: String::new(),
            skills: Vec::new(),
            reproduce_check: None,
            max_attempts: 3,
            draft_first_n: 0,
        }
    }

    #[test]
    fn valid_config_has_no_errors() {
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        validate_config(&config(), &mut errors, &mut warnings);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(!warnings.is_empty(), "missing reproduce_check is a warning");
    }

    #[test]
    fn empty_fields_and_bad_globs_are_errors() {
        let mut bad = config();
        bad.repo.clear();
        bad.failing_check.command.clear();
        bad.max_attempts = 0;
        bad.forbidden_paths = vec!["[".into()];
        let mut errors = Vec::new();
        let mut warnings = Vec::new();
        validate_config(&bad, &mut errors, &mut warnings);
        assert!(errors.iter().any(|error| error.contains("repo")));
        assert!(errors.iter().any(|error| error.contains("failing_check")));
        assert!(errors.iter().any(|error| error.contains("max_attempts")));
        assert!(errors.iter().any(|error| error.contains("glob")));
    }

    #[test]
    fn strict_missing_oracle_is_configuration_error_not_a_reproducer_warning() {
        let mut config = config();
        config.verification_policy = VerificationPolicy::Strict;
        let mut errors = Vec::new();
        validate_config(&config, &mut errors, &mut Vec::new());
        assert!(errors.iter().any(|error| error.contains("owner oracle")));
    }
}
