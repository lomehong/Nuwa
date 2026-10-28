//! Durable PR admission accounting. Slot authority is always the delivery claim;
//! lease expiry never refunds quota. A confirmed PR has one slot per repository,
//! even when several delivery identities recover it.

use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::{
    owned_delivery_on, DeliveryClaim, DeliveryState, ShanError, ShanResult, Store, DAY_MS,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrSlotRow {
    pub id: i64,
    pub repo: String,
    pub state: String,
    pub admitted_at_ms: i64,
    pub admission_day_start_ms: i64,
    pub pr_url: Option<String>,
    /// Recorded PR creation evidence, never the local recovery/confirmation time.
    pub created_at_ms: Option<i64>,
    /// Local confirmation time; not a GitHub creation timestamp.
    pub confirmed_at_ms: Option<i64>,
}

const COLUMNS: &str = "s.id,s.repo,s.state,s.admitted_at_ms,s.admission_day_start_ms,
    s.pr_url,s.created_at_ms,s.confirmed_at_ms";

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PrSlotRow> {
    Ok(PrSlotRow {
        id: row.get(0)?,
        repo: row.get(1)?,
        state: row.get(2)?,
        admitted_at_ms: row.get(3)?,
        admission_day_start_ms: row.get(4)?,
        pr_url: row.get(5)?,
        created_at_ms: row.get(6)?,
        confirmed_at_ms: row.get(7)?,
    })
}

fn day_start(now_ms: i64) -> ShanResult<i64> {
    now_ms
        .div_euclid(DAY_MS)
        .checked_mul(DAY_MS)
        .ok_or(ShanError::InvalidPrReservation)
}

pub(super) fn for_delivery_on(
    conn: &Connection,
    delivery_id: &str,
) -> ShanResult<Option<PrSlotRow>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM pr_slots s JOIN pr_slot_deliveries m ON m.slot_id=s.id
                 WHERE m.delivery_id=?1"
            ),
            [delivery_id],
            from_row,
        )
        .optional()?)
}

fn used_on(conn: &Connection, repo: &str, admission_day_start_ms: i64) -> ShanResult<u64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM pr_slots WHERE repo=?1 AND admission_day_start_ms=?2
         AND state != 'released'",
        params![repo, admission_day_start_ms],
        |row| row.get(0),
    )?)
}

impl Store {
    /// Admit a new intent/pushed delivery under an IMMEDIATE write lock. An
    /// existing live reservation is idempotent, including requested/ambiguous
    /// retries after takeover or midnight. `false` means quota only; invalid or
    /// expired authority is an error. There is no independent reservation token.
    pub fn reserve_pr_slot_owned(
        &self,
        claim: &DeliveryClaim,
        daily_limit: u64,
        now_ms: i64,
    ) -> ShanResult<bool> {
        let mut conn = self.lock();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let delivery = owned_delivery_on(&tx, claim, now_ms)?;
        if !matches!(
            delivery.state,
            DeliveryState::Intent
                | DeliveryState::Pushed
                | DeliveryState::PrRequested
                | DeliveryState::Ambiguous
        ) {
            return Err(ShanError::InvalidPrReservation);
        }
        if let Some(slot) = for_delivery_on(&tx, &delivery.id)? {
            if !matches!(slot.state.as_str(), "reserved" | "requested" | "ambiguous") {
                return Err(ShanError::InvalidPrReservation);
            }
            tx.commit()?;
            return Ok(true);
        }
        if !matches!(
            delivery.state,
            DeliveryState::Intent | DeliveryState::Pushed
        ) {
            return Err(ShanError::InvalidPrReservation);
        }
        let day = day_start(now_ms)?;
        if used_on(&tx, &delivery.repo, day)? >= daily_limit {
            return Ok(false);
        }
        tx.execute(
            "INSERT INTO pr_slots(repo,evidence_identity,state,admitted_at_ms,admission_day_start_ms)
             VALUES (?1,?2,'reserved',?3,?4)",
            params![delivery.repo, format!("delivery:{}", delivery.id), now_ms, day],
        )?;
        tx.execute(
            "INSERT INTO pr_slot_deliveries(delivery_id,slot_id) VALUES (?1,?2)",
            params![delivery.id, tx.last_insert_rowid()],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Admissions on one exact UTC day, not the historical PR creation metric.
    /// Reserved/requested/ambiguous/consumed slots count; released slots do not.
    pub fn pr_slots_used(&self, repo: &str, admission_day_start_ms: i64) -> ShanResult<u64> {
        used_on(&self.lock(), repo, admission_day_start_ms)
    }

    /// Confirmed duplicate deliveries share the same row. A superseded,
    /// unrequested delivery's mapping is transferred to its new identity.
    pub fn pr_slot_for_delivery(&self, delivery_id: &str) -> ShanResult<Option<PrSlotRow>> {
        for_delivery_on(&self.lock(), delivery_id)
    }
}

/// Called before the delivery's terminal write clears its claim, inside the
/// caller's transaction. A failed issue/run/watch write rolls this back too.
pub(super) fn sync_owned_on(
    conn: &Connection,
    claim: &DeliveryClaim,
    state: DeliveryState,
    pr_url: Option<&str>,
    now_ms: i64,
) -> ShanResult<()> {
    let delivery = owned_delivery_on(conn, claim, now_ms)?;
    if state == DeliveryState::PrCreated {
        let url = pr_url
            .or(delivery.pr_url.as_deref())
            .filter(|url| !url.trim().is_empty())
            .ok_or(ShanError::InvalidPrReservation)?;
        return consume_owned_on(conn, claim, url, now_ms);
    }
    let slot = for_delivery_on(conn, &delivery.id)?;
    let target = match state {
        DeliveryState::PrRequested => {
            if slot.is_none() {
                return Err(ShanError::InvalidPrReservation);
            }
            "requested"
        }
        DeliveryState::Ambiguous => "ambiguous",
        DeliveryState::Failed | DeliveryState::Superseded => "released",
        DeliveryState::BranchOnly if slot.is_some() => {
            return Err(ShanError::InvalidPrReservation);
        }
        _ => return Ok(()),
    };
    if let Some(slot) = slot {
        if !matches!(slot.state.as_str(), "reserved" | "requested" | "ambiguous")
            || (state == DeliveryState::Superseded && slot.state != "reserved")
        {
            return Err(ShanError::InvalidPrReservation);
        }
        conn.execute(
            "UPDATE pr_slots SET state=?2 WHERE id=?1",
            params![slot.id, target],
        )?;
    }
    Ok(())
}

fn consume_owned_on(
    conn: &Connection,
    claim: &DeliveryClaim,
    pr_url: &str,
    now_ms: i64,
) -> ShanResult<()> {
    let delivery = owned_delivery_on(conn, claim, now_ms)?;
    let current = for_delivery_on(conn, &delivery.id)?;
    if current.as_ref().is_some_and(|slot| {
        slot.state == "released" || slot.pr_url.as_deref().is_some_and(|url| url != pr_url)
    }) {
        return Err(ShanError::InvalidPrReservation);
    }
    // Legacy pr_opened_at and delivery timestamps are local observations, not
    // GitHub creation evidence. Completion records explicit API evidence only.
    let canonical: Option<i64> = conn
        .query_row(
            "SELECT id FROM pr_slots WHERE repo=?1 AND pr_url=?2",
            params![delivery.repo, pr_url],
            |row| row.get(0),
        )
        .optional()?;
    let slot_id = if let Some(id) = canonical {
        if let Some(slot) = current.as_ref().filter(|slot| slot.id != id) {
            conn.execute(
                "UPDATE pr_slots SET state='released' WHERE id=?1",
                [slot.id],
            )?;
            // Superseded aliases, if any, must not retain a second charge.
            conn.execute(
                "UPDATE pr_slot_deliveries SET slot_id=?2 WHERE slot_id=?1",
                params![slot.id, id],
            )?;
        }
        id
    } else if let Some(slot) = &current {
        slot.id
    } else {
        // A recovered PR is adopted without fresh quota admission. Charge its
        // original local intent day, independently of API creation evidence.
        let admitted = delivery.created_at;
        conn.execute(
            "INSERT INTO pr_slots(repo,evidence_identity,state,admitted_at_ms,admission_day_start_ms)
             VALUES (?1,?2,'consumed',?3,?4)",
            params![
                delivery.repo,
                format!("delivery:{}", delivery.id),
                admitted,
                day_start(admitted)?
            ],
        )?;
        conn.last_insert_rowid()
    };
    conn.execute(
        "UPDATE pr_slots SET state='consumed',pr_url=?2,
            confirmed_at_ms=COALESCE(confirmed_at_ms,?3) WHERE id=?1",
        params![slot_id, pr_url, now_ms],
    )?;
    conn.execute(
        "INSERT INTO pr_slot_deliveries(delivery_id,slot_id) VALUES (?1,?2)
         ON CONFLICT(delivery_id) DO UPDATE SET slot_id=excluded.slot_id",
        params![delivery.id, slot_id],
    )?;
    Ok(())
}

pub(super) fn transfer_owned_on(
    conn: &Connection,
    old: &DeliveryClaim,
    next: &DeliveryClaim,
    now_ms: i64,
) -> ShanResult<()> {
    let old_delivery = owned_delivery_on(conn, old, now_ms)?;
    let next_delivery = owned_delivery_on(conn, next, now_ms)?;
    if !matches!(
        old_delivery.state,
        DeliveryState::Intent | DeliveryState::Pushed
    ) || next_delivery.state != DeliveryState::Intent
        || next_delivery.repo != old_delivery.repo
    {
        return Err(ShanError::InvalidPrReservation);
    }
    let old_slot = for_delivery_on(conn, &old_delivery.id)?;
    if for_delivery_on(conn, &next_delivery.id)?.is_some() {
        return Err(ShanError::InvalidPrReservation);
    }
    if let Some(slot) = old_slot {
        if slot.state != "reserved" {
            return Err(ShanError::InvalidPrReservation);
        }
        conn.execute(
            "UPDATE pr_slot_deliveries SET delivery_id=?2 WHERE delivery_id=?1",
            params![old_delivery.id, next_delivery.id],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DeliveryStart;

    const REPO: &str = "example.com/r";
    const URL: &str = "https://github.com/o/r/pull/1";

    fn start(id: &str, now: i64) -> DeliveryStart {
        DeliveryStart {
            id: id.into(),
            run_id: None,
            fingerprint: format!("fp-{id}"),
            repo: REPO.into(),
            branch: format!("agent/fix/{id}"),
            base: "main".into(),
            head_sha: format!("sha-{id}"),
            now,
        }
    }

    fn claim(store: &Store, start: &DeliveryStart, ttl_ms: i64) -> DeliveryClaim {
        store
            .claim_delivery(start, "owner", ttl_ms)
            .expect("claim")
            .expect("won")
    }

    fn slot(store: &Store, claim: &DeliveryClaim) -> PrSlotRow {
        store
            .pr_slot_for_delivery(&claim.delivery.id)
            .expect("slot")
            .expect("reserved")
    }

    fn mark(store: &Store, claim: &DeliveryClaim, state: DeliveryState, now: i64) {
        store
            .mark_delivery_owned(claim, state, None, None, None, now)
            .expect("transition");
    }

    #[test]
    fn last_slot_is_atomic_across_independent_connections() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("competition.db");
        let first = Store::open(&path).expect("first");
        let second = Store::open(&path).expect("second");
        let seed = claim(&first, &start("seed", 10), 100);
        assert!(first.reserve_pr_slot_owned(&seed, 2, 10).expect("seed"));
        let a = claim(&first, &start("a", 10), 100);
        let b = claim(&second, &start("b", 10), 100);
        mark(&first, &a, DeliveryState::Pushed, 11);
        mark(&second, &b, DeliveryState::Pushed, 11);
        let barrier = std::sync::Barrier::new(2);
        let (a_won, b_won) = std::thread::scope(|scope| {
            let left = scope.spawn(|| {
                barrier.wait();
                first.reserve_pr_slot_owned(&a, 2, 12).expect("left")
            });
            let right = scope.spawn(|| {
                barrier.wait();
                second.reserve_pr_slot_owned(&b, 2, 12).expect("right")
            });
            (
                left.join().expect("left thread"),
                right.join().expect("right thread"),
            )
        });
        assert_ne!(a_won, b_won, "exactly one admission wins the last slot");
        assert_eq!(first.pr_slots_used(REPO, 0).expect("used"), 2);
        assert_eq!(second.pr_slots_used(REPO, 0).expect("used"), 2);
        let winner = if a_won { &a } else { &b };
        assert!(second
            .reserve_pr_slot_owned(winner, 0, 13)
            .expect("idempotent"));
        assert_eq!(first.pr_slots_used(REPO, 0).expect("used"), 2);
        assert_eq!(slot(&first, winner).state, "reserved");
    }

    #[test]
    fn reservation_keeps_its_day_and_admission_across_takeover() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("midnight.db");
        let first = Store::open(&path).expect("first");
        let second = Store::open(&path).expect("second");
        let old = claim(&first, &start("old", DAY_MS - 2), 3);
        mark(&first, &old, DeliveryState::Pushed, DAY_MS - 2);
        assert!(first
            .reserve_pr_slot_owned(&old, 1, DAY_MS - 1)
            .expect("reserve"));
        let original = slot(&first, &old);
        assert!(matches!(
            first.reserve_pr_slot_owned(&old, 1, DAY_MS + 1),
            Err(ShanError::OwnershipLost)
        ));
        let next = second
            .claim_pending_deliveries(REPO, "next", DAY_MS + 1, 100, 1)
            .expect("takeover")
            .pop()
            .expect("claim");
        assert!(next.generation > old.generation);
        assert!(second
            .reserve_pr_slot_owned(&next, 0, DAY_MS + 2)
            .expect("reuse"));
        assert_eq!(slot(&second, &next), original);
        assert_eq!(original.admitted_at_ms, DAY_MS - 1);
        assert_eq!(original.admission_day_start_ms, 0);
        assert!(matches!(
            first.reserve_pr_slot_owned(&old, 100, DAY_MS + 2),
            Err(ShanError::OwnershipLost)
        ));
        assert!(matches!(
            first.mark_delivery_owned(&old, DeliveryState::Failed, None, None, None, DAY_MS + 2),
            Err(ShanError::OwnershipLost)
        ));
        assert!(matches!(
            first.complete_delivery_owned(&old, URL, 1, None, DAY_MS + 2),
            Err(ShanError::OwnershipLost)
        ));
        assert!(matches!(
            first.supersede_delivery_owned(&old, "stale-head", DAY_MS + 2, 100),
            Err(ShanError::OwnershipLost)
        ));
        let mut wrong_generation = next.clone();
        wrong_generation.generation = old.generation;
        assert!(matches!(
            first.reserve_pr_slot_owned(&wrong_generation, 100, DAY_MS + 2),
            Err(ShanError::OwnershipLost)
        ));
        let mut wrong_token = next.clone();
        wrong_token.token.clone_from(&old.token);
        assert!(matches!(
            first.reserve_pr_slot_owned(&wrong_token, 100, DAY_MS + 2),
            Err(ShanError::OwnershipLost)
        ));
        let mut forged_snapshot = next.clone();
        forged_snapshot.delivery.repo = "forged/repo".into();
        assert!(first
            .reserve_pr_slot_owned(&forged_snapshot, 0, DAY_MS + 2)
            .expect("db identity"));
        let today = claim(&second, &start("today", DAY_MS + 2), 100);
        assert!(second
            .reserve_pr_slot_owned(&today, 1, DAY_MS + 2)
            .expect("new day"));
        assert_eq!(second.pr_slots_used(REPO, 0).expect("yesterday"), 1);
        assert_eq!(second.pr_slots_used(REPO, DAY_MS).expect("today"), 1);
    }

    #[test]
    fn missing_reservation_rejects_request_without_changing_delivery() {
        let store = Store::open(":memory:").expect("store");
        let start = start("missing", 10);
        let delivery = claim(&store, &start, 100);
        mark(&store, &delivery, DeliveryState::Pushed, 11);
        assert!(matches!(
            store.mark_delivery_owned(&delivery, DeliveryState::PrRequested, None, None, None, 12),
            Err(ShanError::InvalidPrReservation)
        ));
        assert_eq!(
            store.begin_delivery(&start).expect("unchanged").state,
            DeliveryState::Pushed
        );
        assert!(!store
            .reserve_pr_slot_owned(&delivery, 0, 12)
            .expect("quota zero"));
        assert!(store
            .pr_slot_for_delivery(&start.id)
            .expect("no slot")
            .is_none());
        assert!(store
            .reserve_pr_slot_owned(&delivery, u64::MAX, 12)
            .expect("reserve"));
        mark(&store, &delivery, DeliveryState::PrRequested, 13);
        assert_eq!(slot(&store, &delivery).state, "requested");
    }

    #[test]
    fn ambiguous_survives_expiry_and_reopen_until_definitive_failure() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("ambiguous.db");
        let old = {
            let store = Store::open(&path).expect("store");
            let old = claim(&store, &start("uncertain", 10), 10);
            assert!(store.reserve_pr_slot_owned(&old, 1, 11).expect("reserve"));
            mark(&store, &old, DeliveryState::Pushed, 11);
            mark(&store, &old, DeliveryState::PrRequested, 12);
            mark(&store, &old, DeliveryState::Ambiguous, 13);
            old
        };
        let store = Store::open(&path).expect("reopen");
        let another = claim(&store, &start("another", 30), DAY_MS * 4);
        assert!(!store
            .reserve_pr_slot_owned(&another, 1, 30)
            .expect("still charged"));
        let next = store
            .claim_pending_deliveries(REPO, "recovery", 30, DAY_MS * 4, 1)
            .expect("recover")
            .pop()
            .expect("old delivery");
        assert_eq!(next.delivery.id, old.delivery.id);
        assert_eq!(slot(&store, &next).state, "ambiguous");
        store.prune(DAY_MS * 2, 0, 0, 0).expect("telemetry pruning");
        assert!(store
            .reserve_pr_slot_owned(&next, 0, DAY_MS * 2)
            .expect("not pruned"));
        assert_eq!(store.pr_slots_used(REPO, 0).expect("original day"), 1);
        assert!(matches!(
            store.supersede_delivery_owned(&next, "unsafe", DAY_MS * 2, 100),
            Err(ShanError::InvalidTransition { .. })
        ));
        mark(&store, &next, DeliveryState::PrRequested, DAY_MS * 2);
        mark(&store, &next, DeliveryState::Failed, DAY_MS * 2 + 1);
        assert_eq!(slot(&store, &next).state, "released");
        assert_eq!(store.pr_slots_used(REPO, 0).expect("refunded"), 0);
        assert!(matches!(
            store.reserve_pr_slot_owned(&next, 100, DAY_MS * 2 + 2),
            Err(ShanError::OwnershipLost)
        ));
    }

    #[test]
    fn definite_pre_request_failure_frees_capacity_but_not_the_terminal_identity() {
        let store = Store::open(":memory:").expect("store");
        let failed_start = start("failed", 10);
        let failed = claim(&store, &failed_start, 100);
        assert!(store.reserve_pr_slot_owned(&failed, 1, 11).expect("admit"));
        mark(&store, &failed, DeliveryState::Failed, 12);
        assert_eq!(slot(&store, &failed).state, "released");
        assert!(matches!(
            store.reserve_pr_slot_owned(&failed, 1, 13),
            Err(ShanError::OwnershipLost)
        ));
        assert!(store
            .claim_delivery(&failed_start, "next", 100)
            .expect("terminal")
            .is_none());
        let next = claim(&store, &start("next", 13), 100);
        assert!(store
            .reserve_pr_slot_owned(&next, 1, 14)
            .expect("freed capacity"));
        assert_eq!(store.pr_slots_used(REPO, 0).expect("one charge"), 1);
    }

    #[test]
    fn no_pending_slot_state_is_ttl_pruned() {
        let store = Store::open(":memory:").expect("store");
        for (id, state) in [
            ("reserved", None),
            ("requested", Some(DeliveryState::PrRequested)),
            ("ambiguous", Some(DeliveryState::Ambiguous)),
        ] {
            let delivery = claim(&store, &start(id, 10), 100);
            assert!(store
                .reserve_pr_slot_owned(&delivery, 3, 11)
                .expect("reserve"));
            mark(&store, &delivery, DeliveryState::Pushed, 12);
            if let Some(state) = state {
                mark(&store, &delivery, state, 13);
            }
            assert!(store
                .release_delivery_claim(&delivery, 14)
                .expect("release claim, not slot"));
        }
        store.prune(DAY_MS * 50, 0, 0, 0).expect("prune");
        for id in ["reserved", "requested", "ambiguous"] {
            let retained = store
                .pr_slot_for_delivery(id)
                .expect("lookup")
                .expect("retained");
            assert_eq!(retained.state, id);
        }
        assert_eq!(store.pr_slots_used(REPO, 0).expect("retained capacity"), 3);
    }

    #[test]
    fn supersession_transfers_only_unrequested_reservations_atomically() {
        let store = Store::open(":memory:").expect("store");
        let old = claim(&store, &start("old", DAY_MS - 3), DAY_MS * 2);
        assert!(store
            .reserve_pr_slot_owned(&old, 1, DAY_MS - 2)
            .expect("reserve"));
        let original = slot(&store, &old);
        mark(&store, &old, DeliveryState::Pushed, DAY_MS - 1);
        let next = store
            .supersede_delivery_owned(&old, "rebased", DAY_MS + 1, 100)
            .expect("transfer");
        assert!(store
            .pr_slot_for_delivery(&old.delivery.id)
            .expect("old unmapped")
            .is_none());
        assert_eq!(slot(&store, &next), original);
        assert!(store
            .reserve_pr_slot_owned(&next, 0, DAY_MS + 2)
            .expect("same admission"));
        mark(&store, &next, DeliveryState::Pushed, DAY_MS + 2);
        mark(&store, &next, DeliveryState::PrRequested, DAY_MS + 3);
        assert!(matches!(
            store.supersede_delivery_owned(&next, "unsafe", DAY_MS + 4, 100),
            Err(ShanError::InvalidTransition { .. })
        ));
        assert_eq!(store.pr_slots_used(REPO, 0).expect("one slot"), 1);
        assert_eq!(
            store
                .pending_deliveries(REPO)
                .expect("no unsafe target")
                .len(),
            1
        );
    }

    #[test]
    fn unsafe_target_reservation_rolls_back_supersession_and_claims() {
        let store = Store::open(":memory:").expect("store");
        let original = start("old", 10);
        let old = claim(&store, &original, 100);
        let target_start = DeliveryStart {
            id: "target".into(),
            head_sha: "rebased".into(),
            ..original.clone()
        };
        let target = claim(&store, &target_start, 100);
        assert!(store.reserve_pr_slot_owned(&old, 2, 11).expect("old slot"));
        assert!(store
            .reserve_pr_slot_owned(&target, 2, 11)
            .expect("target slot"));
        assert!(store
            .release_delivery_claim(&target, 11)
            .expect("release owner only"));
        assert!(matches!(
            store.supersede_delivery_owned(&old, "rebased", 12, 100),
            Err(ShanError::InvalidPrReservation)
        ));
        assert!(store.delivery_claim_valid(&old, 12).expect("old preserved"));
        assert_eq!(
            store.begin_delivery(&original).expect("old").state,
            DeliveryState::Intent
        );
        assert_eq!(slot(&store, &old).state, "reserved");
        assert_eq!(slot(&store, &target).state, "reserved");
        let reclaimed = store
            .claim_delivery(&target_start, "reclaim", 100)
            .expect("claim")
            .expect("free");
        assert_eq!(
            reclaimed.generation,
            target.generation + 1,
            "failed transfer rolled back its claim"
        );
        assert_eq!(store.pr_slots_used(REPO, 0).expect("unchanged"), 2);
    }

    #[test]
    fn completion_rolls_back_consumption_with_or_without_a_reservation() {
        for reserved in [false, true] {
            let store = Store::open(":memory:").expect("store");
            let delivery = claim(&store, &start("recover", 10), 100);
            if reserved {
                assert!(store
                    .reserve_pr_slot_owned(&delivery, 1, 11)
                    .expect("reserve"));
            }
            let before = store
                .pr_slot_for_delivery(&delivery.delivery.id)
                .expect("before");
            store
                .lock()
                .execute_batch(
                    "CREATE TRIGGER fail_complete BEFORE UPDATE ON deliveries
                     WHEN NEW.state='pr_created' BEGIN SELECT RAISE(ABORT,'injected'); END;",
                )
                .expect("inject");
            assert!(matches!(
                store.complete_delivery_owned_at(&delivery, URL, 1, None, Some(5), 12),
                Err(ShanError::Db(_))
            ));
            assert_eq!(
                store
                    .pr_slot_for_delivery(&delivery.delivery.id)
                    .expect("rollback"),
                before
            );
            assert!(store
                .delivery_claim_valid(&delivery, 12)
                .expect("claim rollback"));
            store
                .lock()
                .execute_batch("DROP TRIGGER fail_complete")
                .expect("unblock");
            store
                .complete_delivery_owned(&delivery, URL, 1, None, 13)
                .expect("recover without admission");
            let consumed = slot(&store, &delivery);
            assert_eq!(consumed.state, "consumed");
            assert_eq!(consumed.created_at_ms, None);
            assert_eq!(consumed.confirmed_at_ms, Some(13));
            assert_eq!(consumed.admitted_at_ms, if reserved { 11 } else { 10 });
            assert_eq!(store.pr_slots_used(REPO, 0).expect("charged"), 1);
            assert_eq!(
                store
                    .prs_opened_since(REPO, 0)
                    .expect("no invented creation"),
                0
            );
        }
    }

    #[test]
    fn confirmed_pr_deduplicates_deliveries_and_days_but_not_repositories() {
        let store = Store::open(":memory:").expect("store");
        let first = claim(&store, &start("first", DAY_MS - 2), 100);
        assert!(store
            .reserve_pr_slot_owned(&first, 1, DAY_MS - 1)
            .expect("first"));
        store
            .complete_delivery_owned_at(&first, URL, 1, None, Some(DAY_MS - 1), DAY_MS)
            .expect("confirmed");
        let canonical = slot(&store, &first);
        assert_eq!(canonical.created_at_ms, Some(DAY_MS - 1));
        let duplicate = claim(&store, &start("duplicate", DAY_MS + 1), 100);
        assert!(store
            .reserve_pr_slot_owned(&duplicate, 1, DAY_MS + 1)
            .expect("duplicate admission"));
        let reserved = slot(&store, &duplicate);
        assert!(matches!(
            store.complete_delivery_owned_at(&duplicate, URL, 1, None, Some(DAY_MS), DAY_MS + 2),
            Err(ShanError::InvalidPrReservation)
        ));
        assert_eq!(
            slot(&store, &duplicate),
            reserved,
            "conflict rolls back remapping"
        );
        assert_eq!(
            slot(&store, &first),
            canonical,
            "creation evidence immutable"
        );
        assert!(store
            .delivery_claim_valid(&duplicate, DAY_MS + 2)
            .expect("conflict retains ownership"));
        store
            .lock()
            .execute_batch(
                "CREATE TRIGGER fail_duplicate BEFORE UPDATE ON deliveries
                 WHEN NEW.id='duplicate' BEGIN SELECT RAISE(ABORT,'injected'); END;",
            )
            .expect("inject duplicate failure");
        assert!(matches!(
            store.complete_delivery_owned(&duplicate, URL, 1, None, DAY_MS + 2),
            Err(ShanError::Db(_))
        ));
        assert_eq!(
            slot(&store, &duplicate),
            reserved,
            "refund and remapping rolled back"
        );
        assert_eq!(slot(&store, &first), canonical, "canonical PR untouched");
        assert_eq!(
            store.pr_slots_used(REPO, DAY_MS).expect("not refunded yet"),
            1
        );
        store
            .lock()
            .execute_batch("DROP TRIGGER fail_duplicate")
            .expect("unblock");
        store
            .complete_delivery_owned_at(&duplicate, URL, 1, None, Some(DAY_MS - 1), DAY_MS + 2)
            .expect("same PR");
        assert_eq!(slot(&store, &duplicate), canonical);
        assert_eq!(store.pr_slots_used(REPO, 0).expect("original"), 1);
        assert_eq!(
            store
                .pr_slots_used(REPO, DAY_MS)
                .expect("duplicate refunded"),
            0
        );
        let recovered = claim(&store, &start("missing", DAY_MS + 3), 100);
        store
            .complete_delivery_owned(&recovered, URL, 1, None, DAY_MS + 4)
            .expect("missing reservation");
        assert_eq!(slot(&store, &recovered), canonical);
        let other_repo = claim(
            &store,
            &DeliveryStart {
                repo: "other/repo".into(),
                ..start("other", DAY_MS + 3)
            },
            100,
        );
        store
            .complete_delivery_owned(&other_repo, URL, 1, None, DAY_MS + 4)
            .expect("repo-scoped identity");
        assert_ne!(slot(&store, &other_repo).id, canonical.id);
        assert_eq!(
            store
                .pr_slots_used("other/repo", DAY_MS)
                .expect("other repo"),
            1
        );
    }

    #[test]
    fn consumption_ignores_legacy_observation_until_api_creation_is_supplied() {
        let store = Store::open(":memory:").expect("store");
        store
            .lock()
            .execute(
                "INSERT INTO runs(id,fingerprint,runner_id,repo,started_at,phase,pr_url,pr_opened_at)
                 VALUES ('history','history','runner',?1,1,'deliver',?2,?3)",
                params![REPO, URL, DAY_MS - 1],
            )
            .expect("legacy local observation");
        let delivery = claim(&store, &start("new", DAY_MS + 1), 100);
        assert!(store
            .reserve_pr_slot_owned(&delivery, 1, DAY_MS + 1)
            .expect("admission"));
        store
            .complete_delivery_owned(&delivery, URL, 1, None, DAY_MS + 2)
            .expect("consume");
        let consumed = slot(&store, &delivery);
        assert_eq!(consumed.created_at_ms, None);
        assert_eq!(consumed.confirmed_at_ms, Some(DAY_MS + 2));
        assert_eq!(consumed.admission_day_start_ms, DAY_MS);
        let recovery = claim(&store, &start("recovery", DAY_MS + 3), 100);
        for invalid in [0, -1] {
            assert!(matches!(
                store.complete_delivery_owned_at(
                    &recovery,
                    URL,
                    1,
                    None,
                    Some(invalid),
                    DAY_MS + 4,
                ),
                Err(ShanError::InvalidPrReservation)
            ));
            assert_eq!(slot(&store, &delivery), consumed);
            assert!(store
                .pr_slot_for_delivery(&recovery.delivery.id)
                .expect("no invalid adoption")
                .is_none());
        }
        store
            .complete_delivery_owned_at(&recovery, URL, 1, None, Some(DAY_MS - 2), DAY_MS + 4)
            .expect("API creation evidence");
        let confirmed = slot(&store, &recovery);
        assert_eq!(confirmed.id, consumed.id);
        assert_eq!(confirmed.created_at_ms, Some(DAY_MS - 2));
        assert_eq!(confirmed.confirmed_at_ms, consumed.confirmed_at_ms);
        assert_eq!(confirmed.admitted_at_ms, consumed.admitted_at_ms);
        assert_eq!(slot(&store, &delivery), confirmed);
        assert_eq!(
            store
                .prs_opened_since(REPO, DAY_MS)
                .expect("actual creation"),
            0
        );
        assert_eq!(store.pr_slots_used(REPO, DAY_MS).expect("admission"), 1);
    }
}
