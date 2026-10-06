use anyhow::{bail, ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

use crate::db::ExtractionTaskKind;

use super::ExtractionTask;

pub fn enqueue_followup_extraction_task(
    conn: &Connection,
    source: &ExtractionTask,
    task_kind: ExtractionTaskKind,
    high_watermark_event_id: i64,
) -> Result<i64> {
    if conn.is_autocommit() {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let id = enqueue_followup_extraction_task(&tx, source, task_kind, high_watermark_event_id)?;
        tx.commit()?;
        return Ok(id);
    }
    let session_row_id = source
        .session_row_id
        .ok_or_else(|| anyhow::anyhow!("follow-up extraction task requires session_row_id"))?;
    let now = chrono::Utc::now().timestamp();
    let idempotency_key = if let Some(replay_range_id) = source.replay_range_id {
        format!(
            "{}:{}:{}:{}:replay:{}",
            source.host_id,
            source.project_id,
            session_row_id,
            task_kind.as_str(),
            replay_range_id
        )
    } else {
        format!(
            "{}:{}:{}:{}",
            source.host_id,
            source.project_id,
            session_row_id,
            task_kind.as_str()
        )
    };
    let cursor_event_id = source.replay_range_id.and(source.cursor_event_id);
    let (replay_from, resume_cursor) = replay_followup_bounds(
        conn,
        source,
        &idempotency_key,
        cursor_event_id,
        high_watermark_event_id,
    )?;
    conn.execute(
        "INSERT INTO extraction_tasks
         (task_kind, host_id, workspace_id, project_id, session_row_id, priority, status,
          idempotency_key, cursor_event_id, high_watermark_event_id, attempts,
          next_retry_epoch, lease_owner, lease_expires_epoch, last_error, created_at_epoch,
          updated_at_epoch, replay_range_id, replay_from_event_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?9, 0, NULL, NULL, NULL, NULL,
                 ?10, ?10, ?11, ?12)
         ON CONFLICT(idempotency_key) DO UPDATE SET
             high_watermark_event_id = MAX(COALESCE(extraction_tasks.high_watermark_event_id, 0), excluded.high_watermark_event_id),
             cursor_event_id = CASE
                 WHEN excluded.replay_range_id IS NOT NULL
                  AND extraction_tasks.status IN ('done', 'failed') THEN ?13
                 ELSE extraction_tasks.cursor_event_id
             END,
             status = CASE
                 WHEN extraction_tasks.status IN ('done', 'failed') THEN 'pending'
                 ELSE extraction_tasks.status
             END,
             -- Reviving a terminal task resets its retry budget: the old
             -- attempts counted a range the exhaust path already skipped, so
             -- the new range must start with fresh attempts or it would fail
             -- terminally on its first defer.
             attempts = CASE
                 WHEN extraction_tasks.status IN ('done', 'failed') THEN 0
                 ELSE extraction_tasks.attempts
             END,
             next_retry_epoch = CASE
                 WHEN extraction_tasks.status IN ('done', 'failed') THEN NULL
                 ELSE extraction_tasks.next_retry_epoch
             END,
             last_error = CASE
                 WHEN extraction_tasks.status IN ('done', 'failed') THEN NULL
                 ELSE extraction_tasks.last_error
             END,
             failure_class = CASE
                 WHEN extraction_tasks.status IN ('done', 'failed') THEN NULL
                 ELSE extraction_tasks.failure_class
             END,
             failed_at_epoch = CASE
                 WHEN extraction_tasks.status IN ('done', 'failed') THEN NULL
                 ELSE extraction_tasks.failed_at_epoch
             END,
             archived_at_epoch = CASE
                 WHEN extraction_tasks.status IN ('done', 'failed') THEN NULL
                 ELSE extraction_tasks.archived_at_epoch
             END,
             replay_range_id = COALESCE(extraction_tasks.replay_range_id, excluded.replay_range_id),
             replay_from_event_id = COALESCE(extraction_tasks.replay_from_event_id, excluded.replay_from_event_id),
             updated_at_epoch = excluded.updated_at_epoch",
        params![
            task_kind.as_str(),
            source.host_id,
            source.workspace_id,
            source.project_id,
            session_row_id,
            task_kind.priority(),
            idempotency_key,
            cursor_event_id,
            high_watermark_event_id,
            now,
            source.replay_range_id,
            replay_from,
            resume_cursor
        ],
    )?;
    let id = conn.query_row(
        "SELECT id FROM extraction_tasks WHERE idempotency_key = ?1",
        params![idempotency_key],
        |row| row.get(0),
    )?;
    if let Some(range_id) = source.replay_range_id {
        super::replay_member::validated_replay_member(conn, id, range_id)?;
    }
    Ok(id)
}

pub fn enqueue_bounded_followup_extraction_task(
    conn: &Connection,
    source: &ExtractionTask,
    task_kind: ExtractionTaskKind,
    cursor_event_id: i64,
    high_watermark_event_id: i64,
) -> Result<i64> {
    if conn.is_autocommit() {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        let id = enqueue_bounded_followup_extraction_task(
            &tx,
            source,
            task_kind,
            cursor_event_id,
            high_watermark_event_id,
        )?;
        tx.commit()?;
        return Ok(id);
    }
    if high_watermark_event_id <= cursor_event_id {
        bail!(
            "bounded follow-up extraction task requires high_watermark_event_id > cursor_event_id"
        );
    }
    let session_row_id = source
        .session_row_id
        .ok_or_else(|| anyhow::anyhow!("follow-up extraction task requires session_row_id"))?;
    let now = chrono::Utc::now().timestamp();
    let idempotency_key = if let Some(replay_range_id) = source.replay_range_id {
        format!(
            "{}:{}:{}:{}:bounded:{}:{}:replay:{}",
            source.host_id,
            source.project_id,
            session_row_id,
            task_kind.as_str(),
            cursor_event_id,
            high_watermark_event_id,
            replay_range_id
        )
    } else {
        format!(
            "{}:{}:{}:{}:bounded:{}:{}",
            source.host_id,
            source.project_id,
            session_row_id,
            task_kind.as_str(),
            cursor_event_id,
            high_watermark_event_id
        )
    };
    let (replay_from, resume_cursor) = replay_followup_bounds(
        conn,
        source,
        &idempotency_key,
        Some(cursor_event_id),
        high_watermark_event_id,
    )?;
    conn.execute(
        "INSERT INTO extraction_tasks
         (task_kind, host_id, workspace_id, project_id, session_row_id, priority, status,
          idempotency_key, cursor_event_id, high_watermark_event_id, attempts,
          next_retry_epoch, lease_owner, lease_expires_epoch, last_error, created_at_epoch,
          updated_at_epoch, replay_range_id, replay_from_event_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?9, 0, NULL, NULL, NULL, NULL,
                 ?10, ?10, ?11, ?12)
         ON CONFLICT(idempotency_key) DO UPDATE SET
             status = CASE
                 WHEN extraction_tasks.status = 'failed' THEN 'pending'
                 ELSE extraction_tasks.status
             END,
             attempts = CASE
                 WHEN extraction_tasks.status = 'failed' THEN 0
                 ELSE extraction_tasks.attempts
             END,
             cursor_event_id = CASE
                 WHEN extraction_tasks.status = 'failed' THEN ?13
                 ELSE extraction_tasks.cursor_event_id
             END,
             high_watermark_event_id = CASE
                 WHEN extraction_tasks.status = 'failed' THEN excluded.high_watermark_event_id
                 ELSE extraction_tasks.high_watermark_event_id
             END,
             next_retry_epoch = CASE
                 WHEN extraction_tasks.status = 'failed' THEN NULL
                 ELSE extraction_tasks.next_retry_epoch
             END,
             lease_owner = CASE
                 WHEN extraction_tasks.status = 'failed' THEN NULL
                 ELSE extraction_tasks.lease_owner
             END,
             lease_expires_epoch = CASE
                 WHEN extraction_tasks.status = 'failed' THEN NULL
                 ELSE extraction_tasks.lease_expires_epoch
             END,
             last_error = CASE
                 WHEN extraction_tasks.status = 'failed' THEN NULL
                 ELSE extraction_tasks.last_error
             END,
             failure_class = CASE
                 WHEN extraction_tasks.status = 'failed' THEN NULL
                 ELSE extraction_tasks.failure_class
             END,
             failed_at_epoch = CASE
                 WHEN extraction_tasks.status = 'failed' THEN NULL
                 ELSE extraction_tasks.failed_at_epoch
             END,
             archived_at_epoch = CASE
                 WHEN extraction_tasks.status = 'failed' THEN NULL
                 ELSE extraction_tasks.archived_at_epoch
             END,
             replay_from_event_id = COALESCE(extraction_tasks.replay_from_event_id, excluded.replay_from_event_id),
             updated_at_epoch = excluded.updated_at_epoch",
        params![
            task_kind.as_str(),
            source.host_id,
            source.workspace_id,
            source.project_id,
            session_row_id,
            task_kind.priority(),
            idempotency_key,
            cursor_event_id,
            high_watermark_event_id,
            now,
            source.replay_range_id,
            replay_from,
            resume_cursor
        ],
    )?;
    let task_id = conn.query_row(
        "SELECT id FROM extraction_tasks WHERE idempotency_key = ?1",
        params![idempotency_key],
        |row| row.get(0),
    )?;
    link_matching_replay_range_for_bounded_retry(
        conn,
        task_id,
        task_kind,
        cursor_event_id,
        high_watermark_event_id,
        now,
    )?;
    if let Some(range_id) = source.replay_range_id {
        super::replay_member::validated_replay_member(conn, task_id, range_id)?;
    }
    Ok(task_id)
}

fn replay_followup_bounds(
    conn: &Connection,
    source: &ExtractionTask,
    key: &str,
    cursor: Option<i64>,
    high: i64,
) -> Result<(Option<i64>, Option<i64>)> {
    let Some(range_id) = source.replay_range_id else {
        return Ok((None, cursor));
    };
    let parent = super::replay_member::validated_replay_member(conn, source.id, range_id)?;
    let from = cursor
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("replay follow-up start overflow"))?;
    ensure!(
        from >= parent.from && from <= high && high <= parent.to,
        "replay follow-up exceeds its parent evidence range"
    );
    let existing: Option<i64> = conn
        .query_row(
            "SELECT id FROM extraction_tasks WHERE idempotency_key = ?1",
            [key],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        let member = super::replay_member::validated_replay_member(conn, id, range_id)?;
        ensure!(
            !matches!(member.status.as_str(), "done" | "failed")
                || (member.lease_owner.is_none() && member.lease_expires_epoch.is_none()),
            "terminal replay follow-up {id} still has another owner"
        );
        Ok((Some(member.from), Some(member.resume_cursor())))
    } else {
        Ok((Some(from), cursor))
    }
}

fn link_matching_replay_range_for_bounded_retry(
    conn: &Connection,
    task_id: i64,
    task_kind: ExtractionTaskKind,
    cursor_event_id: i64,
    high_watermark_event_id: i64,
    now: i64,
) -> Result<()> {
    let range_id = conn
        .query_row(
            "SELECT id
             FROM extraction_replay_ranges
             WHERE source_task_id = ?1
               AND task_kind = ?2
               AND from_event_id = ?3
               AND to_event_id = ?4
               AND status IN ('pending', 'failed', 'requeued')
             ORDER BY id DESC
             LIMIT 1",
            params![
                task_id,
                task_kind.as_str(),
                cursor_event_id + 1,
                high_watermark_event_id
            ],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(range_id) = range_id else {
        return Ok(());
    };
    let linked = conn.execute(
        "UPDATE extraction_tasks
         SET replay_range_id = ?1,
             replay_from_event_id = COALESCE(replay_from_event_id, ?4 + 1),
             updated_at_epoch = ?2
         WHERE id = ?3
           AND status = 'pending'
           AND cursor_event_id = ?4
           AND high_watermark_event_id = ?5",
        params![
            range_id,
            now,
            task_id,
            cursor_event_id,
            high_watermark_event_id
        ],
    )?;
    if linked != 1 {
        bail!("failed to link bounded extraction task {task_id} to replay range {range_id}");
    }
    conn.execute(
        "UPDATE extraction_replay_ranges
         SET status = 'requeued',
             replay_task_id = ?1,
             attempts = attempts + 1,
             updated_at_epoch = ?2
         WHERE id = ?3
           AND status IN ('pending', 'failed')",
        params![task_id, now, range_id],
    )?;
    Ok(())
}
