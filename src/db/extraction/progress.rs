use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use super::ExtractionTask;

/// Commit only after every required processor effect succeeded. Artifact writes
/// remain idempotent and at-least-once; this transaction prevents skipping their
/// unfinished effects or allowing a stale lease owner to publish progress.
pub fn mark_extraction_task_done(
    conn: &Connection,
    task_id: i64,
    lease_owner: &str,
    completed_high_watermark_event_id: Option<i64>,
) -> Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let now = chrono::Utc::now().timestamp();
    validate_completion(
        &tx,
        task_id,
        lease_owner,
        completed_high_watermark_event_id,
        now,
        crate::db::is_exact_replay_worker_owner(lease_owner),
    )?;
    let updated = tx.execute(
        "UPDATE extraction_tasks
         SET status = CASE WHEN high_watermark_event_id > ?4 THEN 'pending' ELSE 'done' END,
             attempts = CASE WHEN high_watermark_event_id > ?4 THEN 0 ELSE attempts END,
             cursor_event_id = ?4,
             completed_event_id = COALESCE(?4, completed_event_id),
             lease_owner = NULL, lease_expires_epoch = NULL, next_retry_epoch = NULL,
             last_error = NULL, failure_class = NULL, failed_at_epoch = NULL,
             archived_at_epoch = NULL, updated_at_epoch = ?1
         WHERE id = ?2 AND lease_owner = ?3 AND status = 'processing'
           AND lease_expires_epoch > ?1",
        params![now, task_id, lease_owner, completed_high_watermark_event_id],
    )?;
    super::loaders::ensure_task_updated(updated, task_id)?;
    crate::db::extraction_replay::mark_replay_range_replayed_if_done(&tx, task_id, now)?;
    tx.commit()?;
    Ok(())
}

/// Exact replay retains its one lease across successful bounded chunks.
pub(crate) fn checkpoint_claimed_extraction_task_chunk(
    conn: &Connection,
    task: &ExtractionTask,
    lease_owner: &str,
    completed_event_id: i64,
) -> Result<()> {
    ensure!(
        crate::db::is_exact_replay_worker_owner(lease_owner) && task.replay_range_id.is_some(),
        "retained extraction lease requires an exact replay task"
    );
    ensure!(
        completed_event_id > task.cursor_event_id.unwrap_or(0)
            && task
                .high_watermark_event_id
                .is_some_and(|high| completed_event_id <= high),
        "exact replay checkpoint exceeds the attempted chunk"
    );
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let now = chrono::Utc::now().timestamp();
    validate_completion(
        &tx,
        task.id,
        lease_owner,
        Some(completed_event_id),
        now,
        true,
    )?;
    let updated = tx.execute(
        "UPDATE extraction_tasks
         SET cursor_event_id = ?1, completed_event_id = ?1, attempts = 0,
             last_error = NULL, failure_class = NULL, failed_at_epoch = NULL,
             updated_at_epoch = ?2
         WHERE id = ?3 AND lease_owner = ?4 AND status = 'processing'
           AND lease_expires_epoch > ?2 AND cursor_event_id IS ?5
           AND replay_range_id = ?6 AND host_id = ?7 AND workspace_id = ?8
           AND project_id = ?9 AND session_row_id IS ?10 AND task_kind = ?11",
        params![
            completed_event_id,
            now,
            task.id,
            lease_owner,
            task.cursor_event_id,
            task.replay_range_id,
            task.host_id,
            task.workspace_id,
            task.project_id,
            task.session_row_id,
            task.task_kind.as_str()
        ],
    )?;
    super::loaders::ensure_task_updated(updated, task.id)?;
    tx.commit()?;
    Ok(())
}

fn validate_completion(
    conn: &Connection,
    task_id: i64,
    lease_owner: &str,
    completed: Option<i64>,
    now: i64,
    exact_canonical: bool,
) -> Result<()> {
    let valid = conn
        .query_row(
            "SELECT CASE WHEN ?3 IS NULL THEN t.high_watermark_event_id IS NULL
              ELSE ?3 >= COALESCE(t.cursor_event_id, 0)
               AND ?3 >= COALESCE(t.completed_event_id, 0)
               AND ?3 <= t.high_watermark_event_id
               AND EXISTS (SELECT 1 FROM captured_events e
                   WHERE e.id = ?3 AND e.host_id = t.host_id
                     AND e.project_id = t.project_id AND e.session_row_id IS t.session_row_id)
               AND (t.replay_range_id IS NULL OR EXISTS (
                   SELECT 1 FROM extraction_replay_ranges r
                   WHERE r.id = t.replay_range_id AND r.host_id = t.host_id
                     AND r.workspace_id = t.workspace_id AND r.project_id = t.project_id
                     AND r.session_row_id IS t.session_row_id
                     AND COALESCE(t.cursor_event_id, 0) >= r.from_event_id - 1
                     AND t.high_watermark_event_id BETWEEN r.from_event_id AND r.to_event_id
                     AND (r.replay_task_id IS NOT t.id OR
                         (r.task_kind = t.task_kind AND r.to_event_id = t.high_watermark_event_id))
                     AND (?5 = 0 OR (r.replay_task_id = t.id AND r.status = 'requeued'))
                     AND ?3 BETWEEN r.from_event_id AND r.to_event_id)) END
         FROM extraction_tasks t
         WHERE t.id = ?1 AND t.lease_owner = ?2 AND t.status = 'processing'
           AND t.lease_expires_epoch > ?4",
            params![task_id, lease_owner, completed, now, exact_canonical],
            |row| row.get::<_, bool>(0),
        )
        .optional()?;
    ensure!(
        valid == Some(true),
        "extraction task {task_id} completion rejected: stale lease or invalid evidence boundary"
    );
    Ok(())
}

/// Only a checkpoint on this range's linked replay task proves a successful
/// prefix. A historical cursor (including one advanced by exhaustion) cannot.
pub(crate) fn replay_resume_event_id(conn: &Connection, range_id: i64) -> Result<Option<i64>> {
    let (completed, valid): (Option<i64>, bool) = conn.query_row(
        "SELECT t.completed_event_id, CASE WHEN t.completed_event_id IS NULL THEN 1 ELSE
             t.status IN ('done', 'failed')
             AND t.replay_range_id = r.id AND t.task_kind = r.task_kind
             AND t.host_id = r.host_id AND t.workspace_id = r.workspace_id
             AND t.project_id = r.project_id AND t.session_row_id IS r.session_row_id
             AND t.high_watermark_event_id = r.to_event_id
             AND t.cursor_event_id >= t.completed_event_id
             AND t.completed_event_id BETWEEN r.from_event_id AND r.to_event_id
             AND EXISTS (SELECT 1 FROM captured_events e
                 WHERE e.id = t.completed_event_id AND e.host_id = r.host_id
                   AND e.project_id = r.project_id AND e.session_row_id IS r.session_row_id)
             END
         FROM extraction_replay_ranges r
         LEFT JOIN extraction_tasks t ON t.id = r.replay_task_id
         WHERE r.id = ?1",
        [range_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    ensure!(
        valid,
        "invalid successful checkpoint for extraction replay range {range_id}"
    );
    Ok(completed)
}
