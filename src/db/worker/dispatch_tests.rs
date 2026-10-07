use std::{
    path::PathBuf,
    sync::{Arc, Barrier},
    thread,
    time::Duration,
};

use anyhow::{Context, Result};
use rusqlite::{params, Connection};

use crate::db::{self, CaptureEventInput, ExtractionTask, ExtractionTaskKind, JobType};

use super::{preferred_worker_queue, WorkerQueue};

fn setup() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    crate::migrate::run_migrations(&conn)?;
    Ok(conn)
}

struct TempDatabase(PathBuf);

impl TempDatabase {
    fn new(label: &str) -> Self {
        Self(db::test_support::unique_temp_db_path(label))
    }

    fn open(&self) -> Result<Connection> {
        let conn = Connection::open(&self.0)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        Ok(conn)
    }
}

impl Drop for TempDatabase {
    fn drop(&mut self) {
        db::test_support::cleanup_temp_db_files(&self.0);
    }
}

fn capture(
    conn: &Connection,
    kind: ExtractionTaskKind,
    project: &str,
    session: &str,
    content: &str,
) -> Result<i64> {
    db::record_captured_event_with_precomputed_git_branch(
        conn,
        &CaptureEventInput {
            host: "codex-cli",
            session_id: session,
            project,
            cwd: None,
            event_type: "tool_result",
            role: None,
            tool_name: Some("Task"),
            content,
            task_kind: Some(kind),
        },
        None,
        None,
        &[],
        None,
    )?
    .extraction_task_id
    .context("capture should enqueue extraction")
}

fn finish(conn: &Connection, task: &ExtractionTask, owner: &str) -> Result<()> {
    db::mark_extraction_task_done(conn, task.id, owner, task.high_watermark_event_id)
}

type DispatchRow = (String, String, String, String, i64, Option<i64>);

fn history(conn: &Connection) -> Result<Vec<DispatchRow>> {
    let mut stmt = conn.prepare(
        "SELECT scope, stage, host, project, ready_sequence, last_claim_sequence
         FROM worker_dispatch_state ORDER BY scope, stage, host, project",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
        ))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn sequence(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COALESCE(MAX(last_claim_sequence), 0) FROM worker_dispatch_state",
        [],
        |row| row.get(0),
    )?)
}

#[test]
fn dispatch_hot_capture_and_new_arrivals_cannot_starve_waiting_groups() -> Result<()> {
    let mut conn = setup()?;
    let hot = capture(
        &conn,
        ExtractionTaskKind::ObservationExtract,
        "/hot",
        "hot",
        "first",
    )?;
    let downstream = capture(
        &conn,
        ExtractionTaskKind::MemoryCandidate,
        "/hot",
        "hot",
        "derived",
    )?;
    let other = capture(
        &conn,
        ExtractionTaskKind::ObservationExtract,
        "/other",
        "other",
        "other",
    )?;
    let mut claims = Vec::new();
    for round in 0..12 {
        if round > 0 {
            capture(
                &conn,
                ExtractionTaskKind::CapturedGitLink,
                &format!("/new-{round}"),
                &format!("new-{round}"),
                &format!("new high-priority evidence {round}"),
            )?;
        }
        let task = db::claim_next_extraction_task(&mut conn, "worker", 60)?
            .context("one ready extraction should remain")?;
        claims.push(task.id);
        finish(&conn, &task, "worker")?;
        let before = history(&conn)?;
        assert_eq!(
            capture(
                &conn,
                ExtractionTaskKind::ObservationExtract,
                "/hot",
                "hot",
                &format!("hot {round}")
            )?,
            hot
        );
        assert_eq!(
            history(&conn)?,
            before,
            "capture must not refresh service history"
        );
    }
    assert_eq!(&claims[..3], &[hot, other, downstream]);
    assert_eq!(sequence(&conn)?, 12);
    Ok(())
}

#[test]
fn dispatch_queue_turns_survive_connection_reopen_and_hints_do_not_consume_turns() -> Result<()> {
    let file = TempDatabase::new("dispatch-reopen");
    let conn = file.open()?;
    crate::migrate::run_migrations(&conn)?;
    for session in ["first", "second"] {
        capture(
            &conn,
            ExtractionTaskKind::ObservationExtract,
            "/extract",
            session,
            session,
        )?;
        db::enqueue_job(
            &conn,
            "codex-cli",
            JobType::Compress,
            "/jobs",
            Some(session),
            "{}",
            10,
        )?;
    }
    drop(conn);

    for (index, expected) in [
        WorkerQueue::Extraction,
        WorkerQueue::Job,
        WorkerQueue::Extraction,
        WorkerQueue::Job,
    ]
    .into_iter()
    .enumerate()
    {
        let mut conn = file.open()?;
        assert_eq!(preferred_worker_queue(&conn)?, Some(expected));
        let before = history(&conn)?;
        assert_eq!(preferred_worker_queue(&conn)?, Some(expected));
        assert_eq!(history(&conn)?, before);
        match expected {
            WorkerQueue::Extraction => {
                let task = db::claim_next_extraction_task(&mut conn, "reopened", 60)?.unwrap();
                finish(&conn, &task, "reopened")?;
            }
            WorkerQueue::Job => {
                let job = db::claim_next_job(&mut conn, "reopened", 60)?.unwrap();
                db::mark_job_done(&conn, job.id, "reopened")?;
            }
        }
        assert_eq!(sequence(&conn)?, index as i64 + 1);
    }
    let conn = file.open()?;
    let before = history(&conn)?;
    assert_eq!(preferred_worker_queue(&conn)?, None);
    assert_eq!(history(&conn)?, before);
    Ok(())
}

#[test]
fn dispatch_registered_group_retains_its_place_across_future_retry() -> Result<()> {
    let mut conn = setup()?;
    let hot = capture(
        &conn,
        ExtractionTaskKind::ObservationExtract,
        "/hot",
        "hot",
        "first",
    )?;
    let sleeper = capture(
        &conn,
        ExtractionTaskKind::MemoryCandidate,
        "/waiting",
        "waiting",
        "waiting",
    )?;
    let task = db::claim_next_extraction_task(&mut conn, "worker", 60)?.unwrap();
    assert_eq!(task.id, hot);
    finish(&conn, &task, "worker")?;
    let group_before: (i64, Option<i64>) = conn.query_row(
        "SELECT ready_sequence, last_claim_sequence FROM worker_dispatch_state
         WHERE scope='extraction' AND stage='memory_candidate'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(group_before, (0, None));
    conn.execute(
        "UPDATE extraction_tasks SET next_retry_epoch=?2 WHERE id=?1",
        params![sleeper, chrono::Utc::now().timestamp() + 3600],
    )?;
    let newcomer = capture(
        &conn,
        ExtractionTaskKind::CapturedGitLink,
        "/new",
        "new",
        "new",
    )?;
    let next = db::claim_next_extraction_task(&mut conn, "worker", 60)?.unwrap();
    assert_eq!(next.id, newcomer);
    finish(&conn, &next, "worker")?;
    assert!(db::claim_next_extraction_task(&mut conn, "worker", 60)?.is_none());
    assert_eq!(sequence(&conn)?, 2);

    capture(
        &conn,
        ExtractionTaskKind::ObservationExtract,
        "/hot",
        "hot",
        "later",
    )?;
    conn.execute(
        "UPDATE extraction_tasks SET next_retry_epoch=0 WHERE id=?1",
        [sleeper],
    )?;
    let resumed = db::claim_next_extraction_task(&mut conn, "worker", 60)?.unwrap();
    assert_eq!(
        resumed.id, sleeper,
        "temporary unready state must not reset first-ready order"
    );
    let group_after: (i64, Option<i64>) = conn.query_row(
        "SELECT ready_sequence, last_claim_sequence FROM worker_dispatch_state
         WHERE scope='extraction' AND stage='memory_candidate'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(group_after, (group_before.0, Some(3)));
    Ok(())
}

#[test]
fn dispatch_blocked_rules_future_jobs_and_cleanup_do_not_consume_ordinary_turns() -> Result<()> {
    let mut conn = setup()?;
    let predecessor = db::enqueue_job(
        &conn,
        "codex-cli",
        JobType::CompileRules,
        "/rules",
        None,
        "{}",
        1,
    )?;
    assert_eq!(
        db::claim_next_job(&mut conn, "rules-owner", 60)?
            .unwrap()
            .id,
        predecessor
    );
    let successor = db::enqueue_job(
        &conn,
        "codex-cli",
        JobType::CompileRules,
        "/rules",
        None,
        "{}",
        1,
    )?;
    assert_ne!(predecessor, successor);
    let future = db::enqueue_job(
        &conn,
        "codex-cli",
        JobType::Compress,
        "/future",
        None,
        "{}",
        0,
    )?;
    conn.execute(
        "UPDATE jobs SET next_retry_epoch=?2 WHERE id=?1",
        params![future, chrono::Utc::now().timestamp() + 3600],
    )?;
    db::maybe_enqueue_cleanup_job_at(&conn, chrono::Utc::now().timestamp())?;
    let before = history(&conn)?;
    assert_eq!(preferred_worker_queue(&conn)?, None);
    assert!(db::claim_next_job(&mut conn, "other", 60)?.is_none());
    assert!(db::claim_ready_cleanup_job(&mut conn, "cleanup", 60)?.is_some());
    assert_eq!(history(&conn)?, before);
    assert!(!before.iter().any(|row| row.3 == "/future"));

    db::mark_job_done(&conn, predecessor, "rules-owner")?;
    assert_eq!(preferred_worker_queue(&conn)?, Some(WorkerQueue::Job));
    let task = db::claim_next_job(&mut conn, "other", 60)?.unwrap();
    assert_eq!(task.id, successor);
    assert_eq!(sequence(&conn)?, 2);
    conn.execute("UPDATE jobs SET next_retry_epoch=0 WHERE id=?1", [future])?;
    assert_eq!(
        db::claim_next_job(&mut conn, "other", 60)?.unwrap().id,
        future
    );
    assert_eq!(sequence(&conn)?, 3);
    Ok(())
}

#[test]
fn dispatch_failure_rolls_back_both_task_leases_and_service_history() -> Result<()> {
    let mut conn = setup()?;
    let extraction = capture(
        &conn,
        ExtractionTaskKind::ObservationExtract,
        "/extract",
        "extract",
        "extract",
    )?;
    let job = db::enqueue_job(&conn, "codex-cli", JobType::Compress, "/job", None, "{}", 1)?;
    conn.execute_batch(
        "CREATE TRIGGER fail_dispatch BEFORE INSERT ON worker_dispatch_state
         WHEN NEW.last_claim_sequence IS NOT NULL
         BEGIN SELECT RAISE(ABORT, 'fixture dispatch failure'); END;",
    )?;
    for error in [
        db::claim_next_extraction_task(&mut conn, "owner", 60).unwrap_err(),
        db::claim_next_job(&mut conn, "owner", 60).unwrap_err(),
    ] {
        assert!(
            error.to_string().contains("fixture dispatch failure"),
            "{error:#}"
        );
    }
    assert!(
        history(&conn)?.is_empty(),
        "registration must roll back with a failed claim"
    );
    for (table, state, id) in [
        ("extraction_tasks", "status", extraction),
        ("jobs", "state", job),
    ] {
        let stored: (String, Option<String>, Option<i64>) = conn.query_row(
            &format!("SELECT {state}, lease_owner, lease_expires_epoch FROM {table} WHERE id=?1"),
            [id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(stored, ("pending".into(), None, None));
    }
    conn.execute_batch("DROP TRIGGER fail_dispatch")?;
    assert_eq!(
        db::claim_next_extraction_task(&mut conn, "owner", 60)?
            .unwrap()
            .id,
        extraction
    );
    assert_eq!(db::claim_next_job(&mut conn, "owner", 60)?.unwrap().id, job);
    assert_eq!(sequence(&conn)?, 2);
    Ok(())
}

#[test]
fn dispatch_concurrent_claims_serialize_and_expired_owner_cannot_publish_progress() -> Result<()> {
    let file = TempDatabase::new("dispatch-concurrent");
    let initial = file.open()?;
    initial.pragma_update(None, "journal_mode", "WAL")?;
    crate::migrate::run_migrations(&initial)?;
    let first = capture(
        &initial,
        ExtractionTaskKind::ObservationExtract,
        "/first",
        "first",
        "first",
    )?;
    let second = capture(
        &initial,
        ExtractionTaskKind::ObservationExtract,
        "/second",
        "second",
        "second",
    )?;
    drop(initial);
    let connections = [file.open()?, file.open()?];
    let barrier = Arc::new(Barrier::new(3));
    let handles = ["owner-a", "owner-b"]
        .into_iter()
        .zip(connections)
        .map(|(owner, mut conn)| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || -> Result<(&'static str, ExtractionTask)> {
                barrier.wait();
                let task =
                    db::claim_next_extraction_task(&mut conn, owner, 60)?.context("ready task")?;
                Ok((owner, task))
            })
        })
        .collect::<Vec<_>>();
    barrier.wait();
    let claims = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .map_err(|_| anyhow::anyhow!("claim thread panicked"))?
        })
        .collect::<Result<Vec<_>>>()?;
    let mut ids = claims.iter().map(|(_, task)| task.id).collect::<Vec<_>>();
    ids.sort_unstable();
    assert_eq!(ids, vec![first, second]);
    let mut conn = file.open()?;
    assert_eq!(sequence(&conn)?, 2);
    for (owner, task) in &claims {
        let stored: (String, i64) = conn.query_row(
            "SELECT lease_owner, (SELECT last_claim_sequence FROM worker_dispatch_state d
               WHERE d.scope='extraction' AND d.stage=t.task_kind
                 AND d.host=CAST(t.host_id AS TEXT) AND d.project=CAST(t.project_id AS TEXT))
             FROM extraction_tasks t WHERE t.id=?1",
            [task.id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(stored.0, *owner);
        assert_eq!(stored.1, if task.id == first { 1 } else { 2 });
    }
    let (expired_owner, expired_task) = claims.iter().find(|(_, task)| task.id == first).unwrap();
    conn.execute(
        "UPDATE extraction_tasks SET lease_expires_epoch=0 WHERE id=?1",
        [first],
    )?;
    let before = history(&conn)?;
    assert_eq!(db::release_expired_extraction_task_leases(&conn)?, 1);
    assert_eq!(history(&conn)?, before, "lease recovery is not a claim");
    let replacement = db::claim_next_extraction_task(&mut conn, "replacement", 60)?.unwrap();
    assert_eq!(replacement.id, first);
    assert_eq!(sequence(&conn)?, 3);
    assert!(finish(&conn, expired_task, expired_owner).is_err());
    finish(&conn, &replacement, "replacement")?;
    assert_eq!(sequence(&conn)?, 3);
    Ok(())
}

#[test]
fn dispatch_exact_replay_claim_preserves_ordinary_service_history() -> Result<()> {
    let mut conn = setup()?;
    capture(
        &conn,
        ExtractionTaskKind::ObservationExtract,
        "/replay",
        "replay",
        "replay",
    )?;
    let task = db::claim_next_extraction_task(&mut conn, "ordinary", 60)?.unwrap();
    db::mark_claimed_extraction_task_failed_or_retry(
        &conn,
        &task,
        "ordinary",
        "malformed source",
        1,
    )?;
    let range = db::list_extraction_replay_ranges(&conn, None, 10)?.remove(0);
    let before = history(&conn)?;
    let owner = db::exact_replay_worker_owner(1, 1);
    let exact =
        db::retry_and_claim_extraction_replay_range(&mut conn, range.id, false, false, &owner, 60)?;
    assert_eq!(exact.replay_range_id, Some(range.id));
    assert_eq!(history(&conn)?, before);
    Ok(())
}

#[test]
fn dispatch_schema_rejects_invalid_sequences_and_reports_missing_index() -> Result<()> {
    let conn = setup()?;
    for (scope, ready, claimed) in [
        ("queue", -1, None),
        ("queue", 3, Some(2)),
        ("invalid", 0, None),
    ] {
        assert!(conn.execute(
            "INSERT INTO worker_dispatch_state(scope,stage,host,project,ready_sequence,last_claim_sequence)
             VALUES(?1,'job','','',?2,?3)", params![scope, ready, claimed],
        ).is_err());
    }
    conn.execute_batch("DROP INDEX idx_worker_dispatch_last_claim")?;
    let findings = crate::migrate::validate_schema_invariants(&conn)?;
    assert!(
        findings
            .iter()
            .any(|finding| finding.contains("v095_worker_fair_dispatch")
                && finding.contains("idx_worker_dispatch_last_claim")),
        "{findings:?}"
    );
    Ok(())
}
