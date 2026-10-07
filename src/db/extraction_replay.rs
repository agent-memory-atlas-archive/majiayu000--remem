use anyhow::{ensure, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde::Serialize;

use crate::db::ExtractionTaskKind;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtractionReplayRange {
    pub id: i64,
    pub source_task_id: i64,
    pub replay_task_id: Option<i64>,
    pub task_kind: String,
    pub project: String,
    pub session_id: Option<String>,
    pub from_event_id: i64,
    pub to_event_id: i64,
    pub status: String,
    pub attempts: i64,
    pub updated_at_epoch: i64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtractionReplayTaskEvidence {
    pub id: i64,
    pub status: String,
    pub attempts: i64,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExtractionReplayRangeEvidence {
    pub range: ExtractionReplayRange,
    pub replay_task: Option<ExtractionReplayTaskEvidence>,
}

pub fn list_extraction_replay_ranges(
    conn: &Connection,
    project: Option<&str>,
    limit: i64,
) -> Result<Vec<ExtractionReplayRange>> {
    let mut stmt = conn.prepare(
        "SELECT r.id, r.source_task_id, r.replay_task_id, r.task_kind, p.project_path,
                s.session_id, r.from_event_id, r.to_event_id, r.status, r.attempts,
                r.updated_at_epoch, r.last_error
         FROM extraction_replay_ranges r
         JOIN projects p ON p.id = r.project_id
         LEFT JOIN sessions s ON s.id = r.session_row_id
         WHERE r.status IN ('pending', 'failed', 'requeued', 'quarantined')
           AND (?1 IS NULL OR p.project_path = ?1)
         ORDER BY r.updated_at_epoch DESC, r.id DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![project, limit.max(1)], |row| {
        Ok(ExtractionReplayRange {
            id: row.get(0)?,
            source_task_id: row.get(1)?,
            replay_task_id: row.get(2)?,
            task_kind: row.get(3)?,
            project: row.get(4)?,
            session_id: row.get(5)?,
            from_event_id: row.get(6)?,
            to_event_id: row.get(7)?,
            status: row.get(8)?,
            attempts: row.get(9)?,
            updated_at_epoch: row.get(10)?,
            last_error: row.get(11)?,
        })
    })?;
    crate::db::query::collect_rows(rows)
}

pub fn get_extraction_replay_range_evidence(
    conn: &Connection,
    range_id: i64,
) -> Result<ExtractionReplayRangeEvidence> {
    ensure!(range_id > 0, "extraction replay range id must be positive");
    conn.query_row(
        "SELECT r.id, r.source_task_id, r.replay_task_id, r.task_kind, p.project_path,
                s.session_id, r.from_event_id, r.to_event_id, r.status, r.attempts,
                r.updated_at_epoch, r.last_error,
                t.id, t.status, t.attempts, t.last_error
         FROM extraction_replay_ranges r
         JOIN projects p ON p.id = r.project_id
         LEFT JOIN sessions s ON s.id = r.session_row_id
         LEFT JOIN extraction_tasks t ON t.id = r.replay_task_id
         WHERE r.id = ?1",
        params![range_id],
        |row| {
            let replay_task_id = row.get::<_, Option<i64>>(12)?;
            Ok(ExtractionReplayRangeEvidence {
                range: ExtractionReplayRange {
                    id: row.get(0)?,
                    source_task_id: row.get(1)?,
                    replay_task_id: row.get(2)?,
                    task_kind: row.get(3)?,
                    project: row.get(4)?,
                    session_id: row.get(5)?,
                    from_event_id: row.get(6)?,
                    to_event_id: row.get(7)?,
                    status: row.get(8)?,
                    attempts: row.get(9)?,
                    updated_at_epoch: row.get(10)?,
                    last_error: row.get(11)?,
                },
                replay_task: if let Some(id) = replay_task_id {
                    Some(ExtractionReplayTaskEvidence {
                        id,
                        status: row.get(13)?,
                        attempts: row.get(14)?,
                        last_error: row.get(15)?,
                    })
                } else {
                    None
                },
            })
        },
    )
    .optional()?
    .ok_or_else(|| anyhow::anyhow!("extraction replay range {range_id} does not exist"))
}

pub fn count_retryable_extraction_replay_ranges(
    conn: &Connection,
    project: Option<&str>,
    limit: i64,
) -> Result<i64> {
    Ok(
        query_retryable_replay_range_ids(conn, project, None, false, false, limit, true)?.len()
            as i64,
    )
}

pub(crate) fn count_quarantinable_extraction_replay_ranges(
    conn: &Connection,
    project: Option<&str>,
    limit: i64,
) -> Result<i64> {
    Ok(
        query_retryable_replay_range_ids(conn, project, None, false, false, limit, false)?.len()
            as i64,
    )
}

fn query_retryable_replay_range_ids(
    conn: &Connection,
    project: Option<&str>,
    range_id: Option<i64>,
    acknowledge_quarantine: bool,
    include_archived: bool,
    limit: i64,
    include_blocked_graph: bool,
) -> Result<Vec<i64>> {
    let family_predicate = if include_blocked_graph {
        crate::db::replay_retry_family_predicate()
    } else {
        "NOT EXISTS (SELECT 1 FROM extraction_tasks t WHERE t.replay_range_id = r.id
          AND t.status IN ('pending', 'processing'))"
            .to_owned()
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT r.id
         FROM extraction_replay_ranges r
         JOIN projects p ON p.id = r.project_id
         WHERE (r.status IN ('pending', 'failed')
                OR (?3 = 1 AND r.status = 'quarantined'))
           AND (?4 = 1 OR r.archived_at_epoch IS NULL)
           AND ({family_predicate})
           AND (?1 IS NULL OR p.project_path = ?1)
           AND (?2 IS NULL OR r.id = ?2)
         ORDER BY r.updated_at_epoch ASC, r.id ASC
         LIMIT ?5"
    ))?;
    let rows = stmt.query_map(
        params![
            project,
            range_id,
            acknowledge_quarantine,
            include_archived,
            limit.max(1)
        ],
        |row| row.get::<_, i64>(0),
    )?;
    let ids = crate::db::query::collect_rows(rows)?;
    if include_blocked_graph {
        for id in &ids {
            crate::db::validate_replay_family_admission(conn, *id)?;
        }
    }
    Ok(ids)
}

pub fn ensure_extraction_replay_range_retryable(
    conn: &Connection,
    range_id: i64,
    acknowledge_quarantine: bool,
    include_archived: bool,
) -> Result<()> {
    ensure!(range_id > 0, "extraction replay range id must be positive");
    let range_ids = query_retryable_replay_range_ids(
        conn,
        None,
        Some(range_id),
        acknowledge_quarantine,
        include_archived,
        1,
        true,
    )?;
    ensure!(
        range_ids == [range_id],
        "extraction replay range {range_id} is not retryable"
    );
    Ok(())
}

pub(crate) fn ensure_extraction_replay_range_quarantinable(
    conn: &Connection,
    range_id: i64,
) -> Result<()> {
    ensure!(range_id > 0, "extraction replay range id must be positive");
    let ids = query_retryable_replay_range_ids(conn, None, Some(range_id), false, false, 1, false)?;
    ensure!(
        ids == [range_id],
        "extraction replay range {range_id} is not quiescent for quarantine"
    );
    Ok(())
}

pub fn retry_extraction_replay_range(
    conn: &Connection,
    range_id: i64,
    acknowledge_quarantine: bool,
) -> Result<()> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    ensure_extraction_replay_range_retryable(&tx, range_id, acknowledge_quarantine, false)?;
    enqueue_replay_extraction_task(&tx, range_id, acknowledge_quarantine)?;
    tx.commit()?;
    Ok(())
}

pub(crate) fn retry_and_claim_extraction_replay_range(
    conn: &mut Connection,
    range_id: i64,
    acknowledge_quarantine: bool,
    include_archived: bool,
    lease_owner: &str,
    lease_secs: i64,
) -> Result<crate::db::ExtractionTask> {
    ensure!(
        crate::db::is_exact_replay_worker_owner(lease_owner),
        "exact replay recovery requires an exact replay worker owner"
    );
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    ensure_extraction_replay_range_retryable(
        &tx,
        range_id,
        acknowledge_quarantine,
        include_archived,
    )?;
    let now = chrono::Utc::now().timestamp();
    for member in crate::db::validated_replay_members(&tx, range_id)? {
        ensure!(
            member.next_retry_epoch.is_none_or(|retry| retry <= now),
            "replay member {} is not retry-ready for exact claim",
            member.id
        );
    }
    let task_id = enqueue_replay_extraction_task(&tx, range_id, acknowledge_quarantine)?;
    let task = crate::db::claim_extraction_task_by_id_in_transaction(
        &tx,
        task_id,
        lease_owner,
        lease_secs,
    )?
    .ok_or_else(|| {
        anyhow::anyhow!(
            "extraction replay task {task_id} is not pending and retry-ready for exact claim"
        )
    })?;
    let lease_expires: i64 = tx.query_row(
        "SELECT lease_expires_epoch FROM extraction_tasks WHERE id = ?1",
        [task_id],
        |r| r.get(0),
    )?;
    tx.execute(
        "UPDATE extraction_tasks SET status = 'processing', lease_owner = ?1,
             lease_expires_epoch = ?2, updated_at_epoch = ?3
         WHERE replay_range_id = ?4 AND status = 'pending'
           AND lease_owner IS NULL AND lease_expires_epoch IS NULL
           AND (next_retry_epoch IS NULL OR next_retry_epoch <= ?3)",
        params![lease_owner, lease_expires, now, range_id],
    )?;
    crate::db::load_claimed_exact_replay_family(&tx, task_id, lease_owner)?;
    tx.commit()?;
    Ok(task)
}

pub fn quarantine_extraction_replay_range(conn: &Connection, range_id: i64) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    ensure_extraction_replay_range_quarantinable(&tx, range_id)?;
    let now = chrono::Utc::now().timestamp();
    tx.execute(
        "UPDATE extraction_replay_ranges
         SET status = 'quarantined', updated_at_epoch = ?1
         WHERE id = ?2",
        params![now, range_id],
    )?;
    clear_terminal_failures_for_quiesced_range(&tx, range_id, now)?;
    tx.commit()?;
    Ok(())
}

pub fn retry_extraction_replay_ranges(
    conn: &Connection,
    project: Option<&str>,
    limit: i64,
) -> Result<usize> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let range_ids =
        query_retryable_replay_range_ids(&tx, project, None, false, false, limit, true)?;
    for range_id in &range_ids {
        enqueue_replay_extraction_task(&tx, *range_id, false)?;
    }
    tx.commit()?;
    Ok(range_ids.len())
}

pub fn quarantine_extraction_replay_ranges(
    conn: &Connection,
    project: Option<&str>,
    limit: i64,
) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let range_ids =
        query_retryable_replay_range_ids(&tx, project, None, false, false, limit, false)?;
    let now = chrono::Utc::now().timestamp();
    for range_id in &range_ids {
        tx.execute(
            "UPDATE extraction_replay_ranges
             SET status = 'quarantined', updated_at_epoch = ?1
             WHERE id = ?2",
            params![now, range_id],
        )?;
        clear_terminal_failures_for_quiesced_range(&tx, *range_id, now)?;
    }
    tx.commit()?;
    Ok(range_ids.len())
}

pub(crate) fn enqueue_replay_extraction_task(
    conn: &Connection,
    range_id: i64,
    acknowledge_quarantine: bool,
) -> Result<i64> {
    if conn.is_autocommit() {
        let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
        ensure_extraction_replay_range_retryable(&tx, range_id, acknowledge_quarantine, false)?;
        let id = enqueue_replay_extraction_task(&tx, range_id, acknowledge_quarantine)?;
        tx.commit()?;
        return Ok(id);
    }
    let (task_kind, host_id, workspace_id, project_id, session_row_id, from_event_id, to_event_id) =
        conn.query_row(
            "SELECT task_kind, host_id, workspace_id, project_id, session_row_id,
                    from_event_id, to_event_id
             FROM extraction_replay_ranges
             WHERE id = ?1
               AND (status IN ('pending', 'failed')
                    OR (?2 = 1 AND status = 'quarantined'))",
            params![range_id, acknowledge_quarantine],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )?;
    let session_row_id = session_row_id.ok_or_else(|| {
        anyhow::anyhow!("extraction replay range {range_id} is missing session_row_id")
    })?;
    let completed_event_id = crate::db::replay_resume_event_id(conn, range_id)?;
    crate::db::restore_replay_family_members(conn, range_id)?;
    let task_kind_value = ExtractionTaskKind::from_db(&task_kind)?;
    let now = chrono::Utc::now().timestamp();
    let idempotency_key =
        format!("{host_id}:{project_id}:{session_row_id}:{task_kind}:replay-range:{range_id}");
    let updated = conn.execute(
        "INSERT INTO extraction_tasks
         (task_kind, host_id, workspace_id, project_id, session_row_id, priority, status,
          idempotency_key, cursor_event_id, high_watermark_event_id, attempts,
          next_retry_epoch, lease_owner, lease_expires_epoch, last_error, created_at_epoch,
          updated_at_epoch, replay_range_id, completed_event_id, replay_from_event_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?9, 0, NULL, NULL, NULL, NULL,
                 ?10, ?10, ?11, ?12, ?13)
         ON CONFLICT(idempotency_key) DO UPDATE SET
             status = CASE
                 WHEN extraction_tasks.status IN ('pending', 'done', 'failed') THEN 'pending'
                 ELSE extraction_tasks.status
             END,
             cursor_event_id = excluded.cursor_event_id,
             completed_event_id = excluded.completed_event_id,
             replay_from_event_id = COALESCE(extraction_tasks.replay_from_event_id, excluded.replay_from_event_id),
             high_watermark_event_id = excluded.high_watermark_event_id,
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
             replay_range_id = excluded.replay_range_id,
             updated_at_epoch = excluded.updated_at_epoch
         WHERE extraction_tasks.status IN ('pending', 'done', 'failed')
           AND extraction_tasks.lease_owner IS NULL AND extraction_tasks.lease_expires_epoch IS NULL
           AND extraction_tasks.task_kind = excluded.task_kind
           AND extraction_tasks.host_id = excluded.host_id
           AND extraction_tasks.workspace_id = excluded.workspace_id
           AND extraction_tasks.project_id = excluded.project_id
           AND extraction_tasks.session_row_id IS excluded.session_row_id
           AND extraction_tasks.replay_range_id = excluded.replay_range_id
           AND (extraction_tasks.replay_from_event_id IS NULL
                OR extraction_tasks.replay_from_event_id = excluded.replay_from_event_id)",
        params![
            task_kind,
            host_id,
            workspace_id,
            project_id,
            session_row_id,
            task_kind_value.priority(),
            idempotency_key,
            completed_event_id.unwrap_or(from_event_id - 1),
            to_event_id,
            now,
            range_id,
            completed_event_id,
            from_event_id
        ],
    )?;
    ensure!(
        updated == 1,
        "extraction replay range {range_id} task identity changed"
    );
    let replay_task_id: i64 = conn.query_row(
        "SELECT id FROM extraction_tasks WHERE idempotency_key = ?1",
        params![idempotency_key],
        |row| row.get(0),
    )?;
    conn.execute(
        "UPDATE extraction_replay_ranges
         SET status = 'requeued',
             replay_task_id = ?1,
             attempts = attempts + 1,
             failure_class = NULL,
             failed_at_epoch = NULL,
             archived_at_epoch = NULL,
             updated_at_epoch = ?2
         WHERE id = ?3",
        params![replay_task_id, now, range_id],
    )?;
    Ok(replay_task_id)
}

pub(crate) fn archive_exact_replay_range_after_task_failure(
    conn: &Connection,
    range_id: i64,
    replay_task_id: i64,
    error: &str,
    failure_class: crate::db::FailureClass,
    now: i64,
) -> Result<()> {
    let updated = conn.execute(
        "UPDATE extraction_replay_ranges
         SET status = 'quarantined',
             last_error = ?2,
             failure_class = ?3,
             failed_at_epoch = COALESCE(failed_at_epoch, ?4),
             archived_at_epoch = ?4,
             updated_at_epoch = ?4
         WHERE id = ?5
           AND EXISTS (
             SELECT 1
             FROM extraction_tasks t
             WHERE t.id = ?1 AND t.replay_range_id = extraction_replay_ranges.id
           )",
        params![
            replay_task_id,
            crate::db::truncate_str(error, 2000),
            failure_class.as_str(),
            now,
            range_id
        ],
    )?;
    ensure!(
        updated == 1,
        "exact replay range {range_id} is not linked to replay task {replay_task_id}"
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_exhausted_replay_range(
    conn: &Connection,
    source_task_id: i64,
    task_kind: &str,
    host_id: i64,
    workspace_id: i64,
    project_id: i64,
    session_row_id: Option<i64>,
    from_event_id: i64,
    to_event_id: i64,
    _attempts: i64,
    err: &str,
    failure_class: crate::db::FailureClass,
    now: i64,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO extraction_replay_ranges
         (source_task_id, task_kind, host_id, workspace_id, project_id, session_row_id,
          from_event_id, to_event_id, status, attempts, last_error, failure_class,
          failed_at_epoch, archived_at_epoch, created_at_epoch, updated_at_epoch)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', 0, ?9, ?10, ?11, NULL, ?11, ?11)
         ON CONFLICT(source_task_id, from_event_id, to_event_id) DO UPDATE SET
             status = CASE
                 WHEN extraction_replay_ranges.status = 'quarantined' THEN 'quarantined'
                 ELSE 'pending'
             END,
             last_error = excluded.last_error,
             failure_class = excluded.failure_class,
             failed_at_epoch = COALESCE(extraction_replay_ranges.failed_at_epoch, excluded.failed_at_epoch),
             archived_at_epoch = NULL,
             updated_at_epoch = excluded.updated_at_epoch",
        params![
            source_task_id,
            task_kind,
            host_id,
            workspace_id,
            project_id,
            session_row_id,
            from_event_id,
            to_event_id,
            crate::db::truncate_str(err, 2000),
            failure_class.as_str(),
            now
        ],
    )?;
    conn.query_row(
        "SELECT id
         FROM extraction_replay_ranges
         WHERE source_task_id = ?1 AND from_event_id = ?2 AND to_event_id = ?3",
        params![source_task_id, from_event_id, to_event_id],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

pub(crate) fn mark_replay_range_replayed_if_done(
    conn: &Connection,
    task_id: i64,
    now: i64,
) -> Result<()> {
    let updated = conn.execute(
        "UPDATE extraction_replay_ranges
         SET status = 'replayed',
             replay_task_id = COALESCE(replay_task_id, ?1),
             last_error = NULL,
             failure_class = NULL,
             failed_at_epoch = NULL,
             archived_at_epoch = NULL,
             updated_at_epoch = ?2
         WHERE id = (
             SELECT replay_range_id FROM extraction_tasks
             WHERE id = ?1 AND status = 'done' AND replay_range_id IS NOT NULL
         )
           AND status = 'requeued'
           AND NOT EXISTS (
             SELECT 1 FROM extraction_tasks t
             WHERE t.replay_range_id = extraction_replay_ranges.id
               AND t.status != 'done'
         )",
        params![task_id, now],
    )?;
    if updated > 0 {
        let range_id = conn.query_row(
            "SELECT replay_range_id
             FROM extraction_tasks
             WHERE id = ?1",
            params![task_id],
            |row| row.get::<_, i64>(0),
        )?;
        clear_terminal_failures_for_quiesced_range(conn, range_id, now)?;
    }
    Ok(())
}

fn clear_terminal_failures_for_quiesced_range(
    conn: &Connection,
    range_id: i64,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE extraction_tasks
         SET status = 'done',
             attempts = 0,
             lease_owner = NULL,
             lease_expires_epoch = NULL,
             next_retry_epoch = NULL,
             last_error = NULL,
             failure_class = NULL,
             failed_at_epoch = NULL,
             archived_at_epoch = NULL,
             updated_at_epoch = ?2
         WHERE replay_range_id = ?1
           AND status = 'failed'",
        params![range_id, now],
    )?;
    conn.execute(
        "UPDATE extraction_tasks
         SET status = 'done',
             attempts = 0,
             lease_owner = NULL,
             lease_expires_epoch = NULL,
             next_retry_epoch = NULL,
             last_error = NULL,
             failure_class = NULL,
             failed_at_epoch = NULL,
             archived_at_epoch = NULL,
             updated_at_epoch = ?2
         WHERE id = (
             SELECT source_task_id
             FROM extraction_replay_ranges
             WHERE id = ?1
         )
           AND status = 'failed'
           AND NOT EXISTS (
             SELECT 1
             FROM extraction_replay_ranges r
             WHERE r.source_task_id = extraction_tasks.id
               AND r.status NOT IN ('replayed', 'quarantined')
         )",
        params![range_id, now],
    )?;
    Ok(())
}

pub(crate) fn mark_replay_range_failed(
    conn: &Connection,
    task_id: i64,
    now: i64,
    err: &str,
) -> Result<()> {
    conn.execute(
        "UPDATE extraction_replay_ranges
         SET status = 'failed',
             replay_task_id = COALESCE(replay_task_id, ?1),
             attempts = COALESCE((SELECT attempts FROM extraction_tasks WHERE id = ?1), attempts),
             last_error = ?2,
             failure_class = ?3,
             failed_at_epoch = COALESCE(failed_at_epoch, ?4),
             archived_at_epoch = NULL,
             updated_at_epoch = ?4
         WHERE id = (
             SELECT replay_range_id FROM extraction_tasks
             WHERE id = ?1 AND replay_range_id IS NOT NULL
         )",
        params![
            task_id,
            crate::db::truncate_str(err, 2000),
            crate::db::classify_failure(err).as_str(),
            now
        ],
    )?;
    Ok(())
}
