use anyhow::{ensure, Context};

#[derive(Debug, Clone, Copy)]
pub(crate) struct DeliveryLimits {
    pub daily_pr_limit: u64,
    pub max_tokens: u64,
    pub daily_token_limit: u64,
    pub monthly_token_limit: u64,
}

impl Default for DeliveryLimits {
    fn default() -> Self {
        Self {
            daily_pr_limit: 5,
            max_tokens: 1_500_000,
            daily_token_limit: 5_000_000,
            monthly_token_limit: 50_000_000,
        }
    }
}

impl DeliveryLimits {
    pub(crate) fn from_environment() -> Self {
        let defaults = Self::default();
        let number = |key, default| {
            crate::env_opt(key)
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(default)
        };
        Self {
            daily_pr_limit: number("NUWA_DAILY_PR_LIMIT", defaults.daily_pr_limit),
            max_tokens: number("NUWA_MAX_TOKENS", defaults.max_tokens),
            daily_token_limit: number("NUWA_DAILY_TOKEN_LIMIT", defaults.daily_token_limit),
            monthly_token_limit: number("NUWA_MONTHLY_TOKEN_LIMIT", defaults.monthly_token_limit),
        }
    }

    /// Refuse known exhausted repository quotas before starting a model run.
    /// Final gates still recheck durable spend and atomically reserve PR slots.
    pub(crate) fn check_admission(
        &self,
        store: &shanhaijing::Store,
        repo: &str,
        create_pr: bool,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        let day_start = now_ms.saturating_sub(now_ms.rem_euclid(86_400_000));
        let month_start = now_ms.saturating_sub(30 * 86_400_000);
        for (since, limit, scope) in [
            (day_start, self.daily_token_limit, "daily"),
            (
                month_start,
                self.monthly_token_limit,
                "monthly (rolling 30-day)",
            ),
        ] {
            let spent = store.tokens_used_since(Some(repo), since)?;
            ensure!(
                spent < limit,
                "{scope} token cap reached: {spent}/{limit} for {repo}"
            );
        }
        if create_pr && store.pr_slots_used(repo, day_start)? >= self.daily_pr_limit {
            return Err(crate::deliver::PrQuotaExceeded.into());
        }
        Ok(())
    }

    pub(crate) fn check_tokens(
        &self,
        store: &shanhaijing::Store,
        repo: &str,
        run_id: Option<&str>,
        report: &crate::repair::RepairReport,
        now_ms: i64,
    ) -> anyhow::Result<()> {
        // Do not silently accept a missing id or one belonging to another repo.
        let run = match run_id {
            Some(id) => {
                let run = store
                    .run_by_id(id)?
                    .with_context(|| format!("token gate run not found: {id}"))?;
                ensure!(
                    run.repo == repo,
                    "token gate run belongs to a different repo"
                );
                Some(run)
            }
            None => None,
        };
        let recorded_uncached = run
            .as_ref()
            .map_or(0, |run| run.tokens_uncached.max(0) as u64);
        let recorded_cached = run
            .as_ref()
            .map_or(0, |run| run.tokens_cached.max(0) as u64);
        let recorded = recorded_uncached.saturating_add(recorded_cached);
        // Match update_run's component-wise monotone usage accounting.
        let spent = report
            .uncached_tokens
            .max(recorded_uncached)
            .saturating_add(report.cached_tokens.max(recorded_cached));
        if spent > self.max_tokens {
            return Err(TokenGateExceeded::Individual {
                spent,
                limit: self.max_tokens,
            }
            .into());
        }
        let day_start = now_ms.saturating_sub(now_ms.rem_euclid(86_400_000));
        let month_start = now_ms.saturating_sub(30 * 86_400_000);
        for (since, limit, daily) in [
            (day_start, self.daily_token_limit, true),
            (month_start, self.monthly_token_limit, false),
        ] {
            let already_counted = run
                .as_ref()
                .filter(|run| run.started_at >= since)
                .map_or(0, |_| recorded);
            // Include this repair even if it straddles a window boundary, but
            // subtract its recorded contribution before adding effective usage.
            let total = store
                .tokens_used_since(Some(repo), since)?
                .saturating_sub(already_counted)
                .saturating_add(spent);
            if total > limit {
                let exceeded = if daily {
                    TokenGateExceeded::Daily {
                        spent: total,
                        limit,
                    }
                } else {
                    TokenGateExceeded::Monthly {
                        spent: total,
                        limit,
                    }
                };
                return Err(exceeded.into());
            }
        }
        Ok(())
    }
}

/// Only actual threshold denials carry this type; storage/identity faults do not.
#[derive(Debug, Clone, Copy)]
pub(crate) enum TokenGateExceeded {
    Individual { spent: u64, limit: u64 },
    Daily { spent: u64, limit: u64 },
    Monthly { spent: u64, limit: u64 },
}

impl std::fmt::Display for TokenGateExceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (scope, spent, limit) = match self {
            Self::Individual { spent, limit } => ("per-issue", spent, limit),
            Self::Daily { spent, limit } => ("daily", spent, limit),
            Self::Monthly { spent, limit } => ("monthly (rolling 30-day)", spent, limit),
        };
        write!(f, "{scope} token cap exceeded: {spent} > {limit}")
    }
}

impl std::error::Error for TokenGateExceeded {}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;
    const NOW: i64 = 100 * DAY + 12_345;
    const REPO: &str = "example.com/quota";

    fn fixture() -> (shanhaijing::Store, String) {
        let store = shanhaijing::Store::open(":memory:").expect("store");
        store.report_signal(&shanhaijing::Signal {
            repo: REPO.into(), language: "go".into(), title: "panic".into(),
            stack_trace: "panic: boom\ngoroutine 1 [running]:\nexample.com/quota.F()\n\t/app/a.go:1 +0x1".into(),
            ..Default::default()
        }, NOW).expect("signal");
        let fp = store.list_issues().expect("issues")[0].fingerprint.clone();
        (store, fp)
    }

    fn seed(
        store: &shanhaijing::Store,
        fp: &str,
        id: &str,
        repo: &str,
        at: i64,
        tokens: (i64, i64),
    ) {
        store
            .start_run(&shanhaijing::RunStart {
                id: id.into(),
                fingerprint: fp.into(),
                runner_id: "test".into(),
                repo: repo.into(),
                started_at: at,
                provider: None,
                model: None,
            })
            .expect("run");
        store
            .update_run(
                id,
                &shanhaijing::RunUpdate {
                    tokens_uncached: Some(tokens.0),
                    tokens_cached: Some(tokens.1),
                    ..Default::default()
                },
            )
            .expect("usage");
    }

    fn report(uncached: u64, cached: u64) -> crate::repair::RepairReport {
        crate::repair::RepairReport {
            run_id: "harness".into(),
            root_cause_text: String::new(),
            turns: 1,
            uncached_tokens: uncached,
            cached_tokens: cached,
            session_dir: String::new(),
            context: Default::default(),
        }
    }

    #[test]
    fn final_token_gate_uses_monotone_usage_without_double_counting() {
        let (store, fp) = fixture();
        seed(&store, &fp, "current", REPO, NOW, (30, 20));
        seed(&store, &fp, "prior-failure", REPO, NOW - 1, (30, 0));
        store
            .finish_run(
                "prior-failure",
                &shanhaijing::RunFinish {
                    finished_at: NOW,
                    outcome: "repair_failed".into(),
                    ci_verdict: None,
                    error: None,
                },
            )
            .expect("failed attempt");
        let limits = DeliveryLimits {
            max_tokens: 70,
            daily_token_limit: 100,
            monthly_token_limit: 100,
            ..Default::default()
        };
        // Effective current usage is max(50, 30) + max(10, 20) = 70.
        limits
            .check_tokens(&store, REPO, Some("current"), &report(50, 10), NOW)
            .expect("exact individual/daily/monthly thresholds allowed");
        let error = limits
            .check_tokens(&store, REPO, Some("current"), &report(51, 10), NOW)
            .expect_err("report crosses individual limit");
        assert!(matches!(
            error.downcast_ref::<TokenGateExceeded>(),
            Some(TokenGateExceeded::Individual {
                spent: 71,
                limit: 70
            })
        ));
        let error = DeliveryLimits {
            max_tokens: 49,
            ..limits
        }
        .check_tokens(&store, REPO, Some("current"), &report(1, 1), NOW)
        .expect_err("persisted usage cannot be hidden by a smaller report");
        assert!(matches!(
            error.downcast_ref::<TokenGateExceeded>(),
            Some(TokenGateExceeded::Individual {
                spent: 50,
                limit: 49
            })
        ));
        let error = DeliveryLimits {
            max_tokens: 100,
            daily_token_limit: 99,
            ..limits
        }
        .check_tokens(&store, REPO, Some("current"), &report(50, 10), NOW)
        .expect_err("aggregate threshold");
        assert!(matches!(
            error.downcast_ref::<TokenGateExceeded>(),
            Some(TokenGateExceeded::Daily {
                spent: 100,
                limit: 99
            })
        ));
    }

    #[test]
    fn token_gate_windows_are_utc_day_and_rolling_thirty_days_scoped_to_repo() {
        let (store, fp) = fixture();
        seed(&store, &fp, "old", REPO, NOW - 30 * DAY - 1, (1_000, 0));
        seed(&store, &fp, "month-edge", REPO, NOW - 30 * DAY, (20, 0));
        seed(&store, &fp, "yesterday", REPO, 100 * DAY - 1, (10, 0));
        seed(&store, &fp, "day-edge", REPO, 100 * DAY, (5, 0));
        seed(&store, &fp, "other", "example.com/other", NOW, (1_000, 0));
        let limits = DeliveryLimits {
            max_tokens: 10,
            daily_token_limit: 15,
            monthly_token_limit: 45,
            ..Default::default()
        };
        limits
            .check_tokens(&store, REPO, None, &report(8, 2), NOW)
            .expect("unrecorded benchmark report added once at exact bounds");
        let error = DeliveryLimits {
            monthly_token_limit: 44,
            ..limits
        }
        .check_tokens(&store, REPO, None, &report(8, 2), NOW)
        .expect_err("inclusive rolling window");
        assert!(matches!(
            error.downcast_ref::<TokenGateExceeded>(),
            Some(TokenGateExceeded::Monthly {
                spent: 45,
                limit: 44
            })
        ));
        let error = DeliveryLimits {
            daily_token_limit: 14,
            ..limits
        }
        .check_tokens(&store, REPO, None, &report(8, 2), NOW)
        .expect_err("inclusive UTC day");
        assert!(matches!(
            error.downcast_ref::<TokenGateExceeded>(),
            Some(TokenGateExceeded::Daily {
                spent: 15,
                limit: 14
            })
        ));
        // A current repair crossing midnight still contributes once to today.
        let error = DeliveryLimits {
            max_tokens: 100,
            daily_token_limit: 14,
            ..limits
        }
        .check_tokens(&store, REPO, Some("yesterday"), &report(8, 2), NOW)
        .expect_err("current repair crossing the day boundary");
        assert!(matches!(
            error.downcast_ref::<TokenGateExceeded>(),
            Some(TokenGateExceeded::Daily {
                spent: 17,
                limit: 14
            })
        ));
    }

    #[test]
    fn invalid_run_identity_is_not_a_typed_quota_denial() {
        let (store, fp) = fixture();
        seed(&store, &fp, "other", "example.com/other", NOW, (1, 0));
        for id in ["missing", "other"] {
            let error = DeliveryLimits {
                max_tokens: 0,
                ..Default::default()
            }
            .check_tokens(&store, REPO, Some(id), &report(100, 0), NOW)
            .expect_err("invalid run");
            assert!(error.downcast_ref::<TokenGateExceeded>().is_none());
        }
    }
}
