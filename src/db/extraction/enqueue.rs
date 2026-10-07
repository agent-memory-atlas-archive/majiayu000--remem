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
    let (replay_from, resume_cursor, replay_range_id) = replay_followup_bounds(
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
            replay_range_id,
            replay_from,
            resume_cursor
        ],
    )?;
    let id = conn.query_row(
        "SELECT id FROM extraction_tasks WHERE idempotency_key = ?1",
        params![idempotency_key],
        |row| row.get(0),
    )?;
    if let Some(range_id) = replay_range_id {
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
    let (replay_from, resume_cursor, replay_range_id) = replay_followup_bounds(
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
            replay_range_id,
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
        resume_cursor.unwrap_or(cursor_event_id),
        now,
    )?;
    if let Some(range_id) = replay_range_id {
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
) -> Result<(Option<i64>, Option<i64>, Option<i64>)> {
    let existing: Option<(i64, Option<i64>)> = conn
        .query_row(
            "SELECT id, replay_range_id FROM extraction_tasks WHERE idempotency_key = ?1",
            [key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some(range_id) = source
        .replay_range_id
        .or(existing.and_then(|(_, range)| range))
    else {
        if let Some((id, None)) = existing {
            let archived_range: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM extraction_replay_ranges r
                   JOIN extraction_tasks t ON t.id = r.source_task_id
                 WHERE r.source_task_id = ?1 AND r.task_kind = t.task_kind
                   AND r.from_event_id = ?2 AND r.to_event_id = ?3
                   AND (r.archived_at_epoch IS NOT NULL OR r.status = 'quarantined'
                        OR t.archived_at_epoch IS NOT NULL))",
                params![id, cursor.unwrap_or(0) + 1, high],
                |r| r.get(0),
            )?;
            ensure!(!archived_range, "bounded follow-up task {id} has an archived or quarantined replay range; use explicit exact recovery");
        }
        return Ok((None, cursor, None));
    };
    let mutable: bool = conn.query_row(
        "SELECT r.archived_at_epoch IS NULL
           AND (r.status IN ('pending', 'failed', 'requeued')
                OR (?3 AND r.status = 'replayed' AND EXISTS (
                    SELECT 1 FROM extraction_tasks t WHERE t.id = ?2 AND t.status = 'done')))
           AND NOT EXISTS (SELECT 1 FROM extraction_tasks t WHERE t.id = ?2 AND t.archived_at_epoch IS NOT NULL)
         FROM extraction_replay_ranges r WHERE r.id = ?1",
        params![range_id, existing.map(|(id, _)| id), source.replay_range_id.is_none()],
        |r| r.get(0),
    )?;
    ensure!(mutable, "replay follow-up range {range_id} is archived, quarantined or closed; use explicit exact recovery");
    let from = cursor
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| anyhow::anyhow!("replay follow-up start overflow"))?;
    if source.replay_range_id.is_some() {
        let parent = super::replay_member::validated_replay_member(conn, source.id, range_id)?;
        ensure!(
            from >= parent.from && from <= high && high <= parent.to,
            "replay follow-up exceeds its parent evidence range"
        );
    }
    if let Some((id, existing_range)) = existing {
        ensure!(
            existing_range == Some(range_id),
            "replay follow-up {id} range identity changed"
        );
        let member = super::replay_member::validated_replay_member(conn, id, range_id)?;
        let scoped: bool = conn.query_row(
            "SELECT host_id = ?2 AND workspace_id = ?3 AND project_id = ?4 AND session_row_id IS ?5
             FROM extraction_tasks WHERE id = ?1",
            params![
                id,
                source.host_id,
                source.workspace_id,
                source.project_id,
                source.session_row_id
            ],
            |r| r.get(0),
        )?;
        ensure!(scoped, "replay follow-up {id} producer scope changed");
        if source.replay_range_id.is_none() {
            ensure!(
                member.from == from && member.to == high,
                "ordinary producer cannot widen replay follow-up task {id}"
            );
        }
        ensure!(
            !matches!(member.status.as_str(), "done" | "failed")
                || (member.lease_owner.is_none() && member.lease_expires_epoch.is_none()),
            "terminal replay follow-up {id} still has another owner"
        );
        Ok((
            Some(member.from),
            Some(member.resume_cursor()),
            Some(range_id),
        ))
    } else {
        Ok((Some(from), cursor, Some(range_id)))
    }
}

fn link_matching_replay_range_for_bounded_retry(
    conn: &Connection,
    task_id: i64,
    task_kind: ExtractionTaskKind,
    cursor_event_id: i64,
    high_watermark_event_id: i64,
    resume_cursor: i64,
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
               AND archived_at_epoch IS NULL
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
    let already_handed_off: bool = conn.query_row(
        "SELECT replay_range_id IS ?2 AND status IN ('processing', 'done')
         FROM extraction_tasks WHERE id = ?1",
        params![task_id, range_id],
        |r| r.get(0),
    )?;
    if already_handed_off {
        super::replay_member::validated_replay_member(conn, task_id, range_id)?;
        return Ok(());
    }
    let linked = conn.execute(
        "UPDATE extraction_tasks
         SET replay_range_id = ?1,
             replay_from_event_id = COALESCE(replay_from_event_id, ?4 + 1),
             updated_at_epoch = ?2
         WHERE id = ?3
           AND status = 'pending'
           AND cursor_event_id = ?6
           AND high_watermark_event_id = ?5",
        params![
            range_id,
            now,
            task_id,
            cursor_event_id,
            high_watermark_event_id,
            resume_cursor
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
    super::replay_member::validated_replay_member(conn, task_id, range_id)?;
    Ok(())
}
