use anyhow::{ensure, Result};
use rusqlite::{params, Connection, Transaction, TransactionBehavior};

use super::replay_member::validated_replay_members;
use super::ExtractionTask;

fn exact_range(conn: &Connection, canonical_id: i64, owner: &str, now: i64) -> Result<i64> {
    ensure!(
        crate::db::is_exact_replay_worker_owner(owner),
        "exact replay requires an exact owner"
    );
    Ok(conn.query_row(
        "SELECT r.id FROM extraction_replay_ranges r JOIN extraction_tasks t
           ON t.id = r.replay_task_id AND t.replay_range_id = r.id
         WHERE t.id = ?1 AND t.status = 'processing' AND t.lease_owner = ?2
           AND t.lease_expires_epoch > ?3 AND r.status = 'requeued'",
        params![canonical_id, owner, now],
        |r| r.get(0),
    )?)
}

pub(crate) fn load_claimed_exact_replay_family(
    conn: &Connection,
    canonical_id: i64,
    owner: &str,
) -> Result<Vec<ExtractionTask>> {
    let now = chrono::Utc::now().timestamp();
    let range_id = exact_range(conn, canonical_id, owner, now)?;
    let mut tasks = Vec::new();
    for member in validated_replay_members(conn, range_id)? {
        if member.status == "done"
            && member.is_complete()
            && member.lease_owner.is_none()
            && member.lease_expires_epoch.is_none()
        {
            continue;
        }
        ensure!(
            member.status == "processing"
                && member.lease_owner.as_deref() == Some(owner)
                && member
                    .lease_expires_epoch
                    .is_some_and(|expiry| expiry > now),
            "exact replay member {} is not owned by this admitted family",
            member.id
        );
        tasks.push(super::loaders::load_claimed_extraction_task(
            conn, member.id,
        )?);
    }
    ensure!(
        tasks.first().is_some_and(|t| t.id == canonical_id),
        "exact replay canonical task is missing"
    );
    Ok(tasks)
}

/// Every successful member keeps its processing lease until this transaction.
/// Thus a later failure can archive all unfinished owned members together, while
/// no intermediate pending row can escape to an ordinary worker.
pub(crate) fn finish_claimed_exact_replay_family(
    conn: &Connection,
    canonical_id: i64,
    owner: &str,
) -> Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let now = chrono::Utc::now().timestamp();
    let range_id = exact_range(&tx, canonical_id, owner, now)?;
    for member in validated_replay_members(&tx, range_id)? {
        ensure!(
            member.is_complete(),
            "exact replay member {} has incomplete evidence",
            member.id
        );
        ensure!(
            (member.status == "done"
                && member.lease_owner.is_none()
                && member.lease_expires_epoch.is_none())
                || (member.status == "processing"
                    && member.lease_owner.as_deref() == Some(owner)
                    && member
                        .lease_expires_epoch
                        .is_some_and(|expiry| expiry > now)),
            "exact replay member {} has changed ownership",
            member.id
        );
    }
    tx.execute(
        "UPDATE extraction_tasks SET status = 'done', lease_owner = NULL, lease_expires_epoch = NULL,
             next_retry_epoch = NULL, last_error = NULL, failure_class = NULL,
             failed_at_epoch = NULL, archived_at_epoch = NULL, updated_at_epoch = ?1
         WHERE replay_range_id = ?2 AND status = 'processing' AND lease_owner = ?3
           AND lease_expires_epoch > ?1 AND completed_event_id = high_watermark_event_id",
        params![now, range_id, owner],
    )?;
    crate::db::extraction_replay::mark_replay_range_replayed_if_done(&tx, canonical_id, now)?;
    let replayed: bool = tx.query_row(
        "SELECT status = 'replayed' FROM extraction_replay_ranges WHERE id = ?1",
        [range_id],
        |r| r.get(0),
    )?;
    ensure!(replayed, "exact replay family did not finish atomically");
    tx.commit()?;
    Ok(())
}

pub(super) fn archive_owned_exact_family(
    conn: &Connection,
    failed_task_id: i64,
    owner: &str,
    error: &str,
    failure_class: crate::db::FailureClass,
    now: i64,
) -> Result<()> {
    ensure!(
        !conn.is_autocommit() && crate::db::is_exact_replay_worker_owner(owner),
        "exact replay archive requires a transaction and exact owner"
    );
    let range_id: i64 = conn.query_row(
        "SELECT replay_range_id FROM extraction_tasks
         WHERE id = ?1 AND status = 'processing' AND lease_owner = ?2",
        params![failed_task_id, owner],
        |r| r.get(0),
    )?;
    let ids = {
        let mut stmt = conn.prepare(
            "SELECT id FROM extraction_tasks WHERE replay_range_id = ?1
             AND status = 'processing' AND lease_owner = ?2 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![range_id, owner], |r| r.get::<_, i64>(0))?;
        crate::db::query::collect_rows(rows)?
    };
    for id in ids {
        let member = super::replay_member::validated_replay_member(conn, id, range_id)?;
        let complete = member.is_complete();
        let detail = if id == failed_task_id {
            error.to_owned()
        } else {
            format!("exact replay family interrupted by task {failed_task_id}: {error}")
        };
        let updated = conn.execute(
            "UPDATE extraction_tasks SET status = ?1,
                 attempts = attempts + ?2, lease_owner = NULL, lease_expires_epoch = NULL,
                 next_retry_epoch = NULL, last_error = ?3, failure_class = ?4,
                 failed_at_epoch = CASE WHEN ?5 THEN NULL ELSE COALESCE(failed_at_epoch, ?6) END,
                 archived_at_epoch = CASE WHEN ?5 THEN NULL ELSE ?6 END, updated_at_epoch = ?6
             WHERE id = ?7 AND status = 'processing' AND lease_owner = ?8",
            params![
                if complete { "done" } else { "failed" },
                i64::from(!complete && id == failed_task_id),
                (!complete).then(|| crate::db::truncate_str(&detail, 2000)),
                (!complete).then_some(failure_class.as_str()),
                complete,
                now,
                id,
                owner
            ],
        )?;
        super::loaders::ensure_task_updated(updated, id)?;
    }
    crate::db::extraction_replay::archive_exact_replay_range_after_task_failure(
        conn,
        range_id,
        failed_task_id,
        error,
        failure_class,
        now,
    )
}
