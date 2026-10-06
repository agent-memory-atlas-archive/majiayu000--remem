use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};

// Shared eligibility keeps queue hints, group registration and claim selection
// aligned. These queries contain only identifiers and scheduling metadata.
pub(crate) const READY_EXTRACTION_DISPATCH_SQL: &str =
    "SELECT id, task_kind, host_id, project_id, priority, created_at_epoch
     FROM extraction_tasks WHERE status = 'pending'
       AND (next_retry_epoch IS NULL OR next_retry_epoch <= ?1)";

pub(crate) const READY_JOB_DISPATCH_SQL: &str =
    "SELECT candidate.id, candidate.job_type, candidate.host, candidate.project,
            candidate.priority, candidate.created_at_epoch
     FROM jobs AS candidate
     WHERE candidate.state = 'pending' AND candidate.job_type <> 'cleanup'
       AND candidate.next_retry_epoch <= ?1
       AND NOT (candidate.job_type = 'compile_rules' AND EXISTS (
           SELECT 1 FROM jobs AS predecessor
           WHERE predecessor.job_type = 'compile_rules'
             AND predecessor.project = candidate.project AND predecessor.state = 'processing'))";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerQueue {
    Extraction,
    Job,
}

impl WorkerQueue {
    fn as_str(self) -> &'static str {
        match self {
            Self::Extraction => "extraction",
            Self::Job => "job",
        }
    }
}

fn current_sequence(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(last_claim_sequence), 0) FROM worker_dispatch_state",
        [],
        |row| row.get(0),
    )?)
}

/// Registration freezes a new group's place. A moving default would either
/// starve first-time groups or let an endless stream of newcomers jump the queue.
pub(crate) fn register_ready_dispatch_groups(
    conn: &Connection,
    queue: WorkerQueue,
    now: i64,
) -> Result<()> {
    ensure!(
        !conn.is_autocommit(),
        "worker dispatch registration requires a claim transaction"
    );
    let (ready, stage, host, project) = match queue {
        WorkerQueue::Extraction => (
            READY_EXTRACTION_DISPATCH_SQL,
            "task_kind",
            "CAST(host_id AS TEXT)",
            "CAST(project_id AS TEXT)",
        ),
        WorkerQueue::Job => (READY_JOB_DISPATCH_SQL, "job_type", "host", "project"),
    };
    conn.execute(&format!(
        "INSERT INTO worker_dispatch_state(scope, stage, host, project, ready_sequence, last_claim_sequence)
         SELECT DISTINCT ?3, {stage}, {host}, {project}, ?2, NULL FROM ({ready}) WHERE 1
         ON CONFLICT(scope, stage, host, project) DO NOTHING"
    ), params![now, current_sequence(conn)?, queue.as_str()])?;
    Ok(())
}

/// This is an eligibility hint, not a claim. A selected lane may become empty;
/// only the later successful claim advances persisted service history.
pub(crate) fn preferred_worker_queue(conn: &Connection) -> Result<Option<WorkerQueue>> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let now = chrono::Utc::now().timestamp();
    tx.execute(&format!(
        "INSERT INTO worker_dispatch_state(scope, stage, host, project, ready_sequence, last_claim_sequence)
         SELECT 'queue', 'extraction', '', '', ?2, NULL WHERE EXISTS ({READY_EXTRACTION_DISPATCH_SQL})
         UNION ALL SELECT 'queue', 'job', '', '', ?2, NULL WHERE EXISTS ({READY_JOB_DISPATCH_SQL})
         ON CONFLICT(scope, stage, host, project) DO NOTHING"
    ), params![now, current_sequence(&tx)?])?;
    let selected: Option<String> = tx
        .query_row(
            &format!(
                "SELECT stage FROM worker_dispatch_state WHERE scope = 'queue' AND (
             (stage = 'extraction' AND EXISTS ({READY_EXTRACTION_DISPATCH_SQL})) OR
             (stage = 'job' AND EXISTS ({READY_JOB_DISPATCH_SQL})))
         ORDER BY COALESCE(last_claim_sequence, ready_sequence), last_claim_sequence IS NOT NULL,
             CASE stage WHEN 'extraction' THEN 0 ELSE 1 END LIMIT 1"
            ),
            [now],
            |row| row.get(0),
        )
        .optional()?;
    tx.commit()?;
    Ok(selected.map(|queue| {
        if queue == "extraction" {
            WorkerQueue::Extraction
        } else {
            WorkerQueue::Job
        }
    }))
}

pub(crate) fn record_extraction_dispatch(
    conn: &Connection,
    task: &crate::db::ExtractionTask,
    owner: &str,
) -> Result<()> {
    if super::is_exact_replay_worker_owner(owner) {
        return Ok(());
    }
    record_dispatch(
        conn,
        WorkerQueue::Extraction,
        task.task_kind.as_str(),
        &task.host_id.to_string(),
        &task.project_id.to_string(),
    )
}

pub(crate) fn record_job_dispatch(conn: &Connection, job: &crate::db::Job) -> Result<()> {
    record_dispatch(
        conn,
        WorkerQueue::Job,
        job.job_type.as_str(),
        &job.host,
        &job.project,
    )
}

fn record_dispatch(
    conn: &Connection,
    queue: WorkerQueue,
    stage: &str,
    host: &str,
    project: &str,
) -> Result<()> {
    ensure!(
        !conn.is_autocommit(),
        "worker dispatch update requires a claim transaction"
    );
    let previous = current_sequence(conn)?;
    let next = previous
        .checked_add(1)
        .context("worker dispatch sequence exhausted")?;
    conn.execute(
        "INSERT INTO worker_dispatch_state(scope, stage, host, project, ready_sequence, last_claim_sequence)
         VALUES ('queue', ?1, '', '', ?5, ?6), (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(scope, stage, host, project) DO UPDATE SET last_claim_sequence = excluded.last_claim_sequence",
        params![queue.as_str(), stage, host, project, previous, next],
    )?;
    Ok(())
}
