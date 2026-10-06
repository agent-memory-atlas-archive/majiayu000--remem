use super::{process_with_graph_generator, GraphCandidateResult};
use anyhow::Result;
use rusqlite::{params, Connection};

use crate::db::{self, CaptureEventInput, ExtractionTask, ExtractionTaskKind};

struct WaitingFamily {
    conn: Connection,
    range: i64,
    parent: ExtractionTask,
    memory: i64,
    graph: i64,
    events: Vec<i64>,
}

async fn waiting_family() -> Result<WaitingFamily> {
    let mut conn = Connection::open_in_memory()?;
    crate::migrate::run_migrations(&conn)?;
    let mut events = Vec::new();
    for index in 0..4 {
        events.push(
            db::record_captured_event_with_precomputed_git_branch(
                &conn,
                &CaptureEventInput {
                    host: "codex-cli",
                    session_id: "replay-waiting",
                    project: "/synthetic/replay-waiting",
                    cwd: None,
                    event_type: "tool_result",
                    role: None,
                    tool_name: Some("Task"),
                    content: &format!("replay dependency evidence {index}"),
                    task_kind: Some(ExtractionTaskKind::ObservationExtract),
                },
                None,
                None,
                &[],
                None,
            )?
            .event_row_id,
        );
    }
    let source = db::claim_next_extraction_task(&mut conn, "source", 60)?.unwrap();
    db::mark_claimed_extraction_task_failed_or_retry(
        &conn,
        &source,
        "source",
        "malformed source",
        1,
    )?;
    let range = db::list_extraction_replay_ranges(&conn, None, 10)?[0].id;
    db::retry_extraction_replay_range(&conn, range, false)?;
    let parent = db::claim_next_extraction_task(&mut conn, "parent", 60)?.unwrap();
    let memory = db::enqueue_followup_extraction_task(
        &conn,
        &parent,
        ExtractionTaskKind::MemoryCandidate,
        events[3],
    )?;
    db::mark_extraction_task_done(&conn, parent.id, "parent", Some(events[3]))?;
    let child = db::claim_extraction_task_by_id(&mut conn, memory, "memory", 60)?.unwrap();
    let graph = db::enqueue_followup_extraction_task(
        &conn,
        &child,
        ExtractionTaskKind::GraphCandidate,
        events[3],
    )?;
    insert_observation(&conn, &child, &events)?;

    // Candidate persistence/enqueue precedes the worker success checkpoint.
    // A crash in that window leaves Graph queued while Memory retries.
    conn.execute(
        "UPDATE extraction_tasks SET lease_expires_epoch = 0 WHERE id = ?1",
        [memory],
    )?;
    assert_eq!(db::release_expired_extraction_task_leases(&conn)?, 1);
    let child = db::claim_extraction_task_by_id(&mut conn, memory, "memory-retry", 60)?.unwrap();
    db::mark_claimed_extraction_task_failed_or_retry(
        &conn,
        &child,
        "memory-retry",
        "malformed source",
        1,
    )?;

    let graph_task = db::claim_extraction_task_by_id(&mut conn, graph, "graph", 60)?.unwrap();
    let outcome = process_with_graph_generator(&mut conn, &graph_task, |_| async {
        anyhow::bail!("graph generator must not run before its failed dependency recovers")
    })
    .await?;
    let GraphCandidateResult::Waiting { reason } = outcome else {
        anyhow::bail!("expected a real Graph dependency wait, got {outcome:?}")
    };
    assert!(reason.contains(&format!("memory_candidate task {memory} is failed")));
    db::wait_extraction_task(&conn, graph, "graph", &reason, 300)?;
    conn.execute(
        "UPDATE extraction_tasks SET attempts = 2 WHERE id = ?1",
        [graph],
    )?;
    assert_eq!(
        db::get_extraction_replay_range_evidence(&conn, range)?
            .range
            .status,
        "failed"
    );
    Ok(WaitingFamily {
        conn,
        range,
        parent,
        memory,
        graph,
        events,
    })
}

fn insert_observation(conn: &Connection, task: &ExtractionTask, events: &[i64]) -> Result<()> {
    let text = "The worker persists extraction effects before the completion checkpoint.";
    let id = db::insert_observation_with_branch(
        conn,
        "replay-waiting",
        &task.project,
        "decision",
        Some("Worker recovery"),
        None,
        Some(text),
        None,
        None,
        None,
        None,
        None,
        12,
        None,
        None,
    )?;
    conn.execute(
        "UPDATE observations SET host_id = ?1, project_id = ?2, session_row_id = ?3,
             observation_type = 'decision', text = ?4, evidence_event_ids = ?5,
             confidence = 0.91 WHERE id = ?6",
        params![
            task.host_id,
            task.project_id,
            task.session_row_id,
            text,
            serde_json::to_string(events)?,
            id
        ],
    )?;
    Ok(())
}

fn graph_wait_state(conn: &Connection, id: i64) -> Result<(String, i64, Option<i64>, Option<i64>)> {
    Ok(conn.query_row(
        "SELECT status, attempts, next_retry_epoch, cursor_event_id
         FROM extraction_tasks WHERE id = ?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?)
}

fn snapshot(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT json_array(id, task_kind, host_id, workspace_id, project_id, session_row_id,
           status, idempotency_key, cursor_event_id, high_watermark_event_id, replay_range_id,
           replay_from_event_id, completed_event_id, attempts, next_retry_epoch, lease_owner,
           lease_expires_epoch, last_error, failure_class, failed_at_epoch, archived_at_epoch,
           created_at_epoch, updated_at_epoch) FROM extraction_tasks ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut values = db::query::collect_rows(rows)?;
    let mut stmt = conn.prepare(
        "SELECT json_array(id, replay_task_id, status, attempts, from_event_id, to_event_id,
           last_error, failure_class, failed_at_epoch, archived_at_epoch, updated_at_epoch)
         FROM extraction_replay_ranges ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    values.extend(db::query::collect_rows(rows)?);
    Ok(values)
}

async fn assert_graph_unblocked(conn: &mut Connection, task: &ExtractionTask) -> Result<()> {
    let outcome = process_with_graph_generator(conn, task, |_| async {
        Ok("<no_graph_candidates reason=\"No durable graph relation\"/>".to_owned())
    })
    .await?;
    assert_eq!(outcome, GraphCandidateResult::NoCandidates);
    Ok(())
}

#[tokio::test]
async fn pending_graph_dependency_recovers_through_explicit_and_batch_retry_without_resetting_wait(
) -> Result<()> {
    for batch in [false, true] {
        let mut f = waiting_family().await?;
        let before_graph = graph_wait_state(&f.conn, f.graph)?;
        assert_eq!(
            db::count_retryable_extraction_replay_ranges(&f.conn, None, 10)?,
            1
        );
        let before = snapshot(&f.conn)?;
        assert!(db::quarantine_extraction_replay_range(&f.conn, f.range).is_err());
        assert_eq!(snapshot(&f.conn)?, before);
        if batch {
            assert_eq!(db::retry_extraction_replay_ranges(&f.conn, None, 10)?, 1);
        } else {
            db::retry_extraction_replay_range(&f.conn, f.range, false)?;
        }
        assert_eq!(graph_wait_state(&f.conn, f.graph)?, before_graph);
        assert!(db::claim_extraction_task_by_id(&mut f.conn, f.graph, "too-early", 60)?.is_none());
        for id in [f.parent.id, f.memory] {
            let task = db::claim_extraction_task_by_id(&mut f.conn, id, "retry", 60)?.unwrap();
            db::mark_extraction_task_done(&f.conn, task.id, "retry", task.high_watermark_event_id)?;
        }
        f.conn.execute(
            "UPDATE extraction_tasks SET next_retry_epoch = 0 WHERE id = ?1",
            [f.graph],
        )?;
        let graph = db::claim_extraction_task_by_id(&mut f.conn, f.graph, "retry", 60)?.unwrap();
        assert_graph_unblocked(&mut f.conn, &graph).await?;
        db::mark_extraction_task_done(&f.conn, graph.id, "retry", Some(f.events[3]))?;
        assert_eq!(
            db::get_extraction_replay_range_evidence(&f.conn, f.range)?
                .range
                .status,
            "replayed"
        );
    }
    Ok(())
}

#[tokio::test]
async fn retry_ready_pending_graph_joins_exact_family_and_uses_completed_dependency_checkpoint(
) -> Result<()> {
    let mut f = waiting_family().await?;
    f.conn.execute(
        "UPDATE extraction_tasks SET next_retry_epoch = 0 WHERE id = ?1",
        [f.graph],
    )?;
    let owner = db::exact_replay_worker_owner(71, 71);
    let parent = db::retry_and_claim_extraction_replay_range(
        &mut f.conn,
        f.range,
        false,
        false,
        &owner,
        60,
    )?;
    let family = db::load_claimed_exact_replay_family(&f.conn, parent.id, &owner)?;
    assert_eq!(
        family.iter().map(|task| task.id).collect::<Vec<_>>(),
        vec![f.parent.id, f.memory, f.graph]
    );
    let (pending, deadlines): (i64, i64) = f.conn.query_row(
        "SELECT SUM(status = 'pending'), COUNT(DISTINCT lease_expires_epoch)
         FROM extraction_tasks WHERE replay_range_id = ?1",
        [f.range],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!((pending, deadlines), (0, 1));
    let memory = family.iter().find(|task| task.id == f.memory).unwrap();
    db::checkpoint_claimed_extraction_task_chunk(&f.conn, memory, &owner, f.events[3])?;
    let graph = family.iter().find(|task| task.id == f.graph).unwrap();
    // Memory is still processing under the retained family lease. Its verified
    // completed cursor is sufficient for the real dependency gate.
    assert_graph_unblocked(&mut f.conn, graph).await?;
    db::checkpoint_claimed_extraction_task_chunk(&f.conn, graph, &owner, f.events[3])?;
    db::finish_claimed_exact_replay_family(&f.conn, parent.id, &owner)?;
    assert_eq!(
        db::get_extraction_replay_range_evidence(&f.conn, f.range)?
            .range
            .status,
        "replayed"
    );
    Ok(())
}

#[tokio::test]
async fn exact_pending_graph_future_retry_rejects_the_whole_attempt_without_writes() -> Result<()> {
    let mut f = waiting_family().await?;
    let before = snapshot(&f.conn)?;
    let owner = db::exact_replay_worker_owner(72, 72);
    assert!(db::retry_and_claim_extraction_replay_range(
        &mut f.conn,
        f.range,
        false,
        false,
        &owner,
        60,
    )
    .is_err());
    assert_eq!(snapshot(&f.conn)?, before);
    Ok(())
}

#[tokio::test]
async fn pending_dependency_exception_rejects_processing_owned_foreign_and_unrelated_members(
) -> Result<()> {
    for condition in [
        "processing",
        "pending_owner",
        "foreign_scope",
        "unrelated",
        "canonical",
    ] {
        let mut f = waiting_family().await?;
        f.conn.execute(
            "UPDATE extraction_tasks SET next_retry_epoch = 0 WHERE id = ?1",
            [f.graph],
        )?;
        match condition {
            "processing" => {
                db::claim_extraction_task_by_id(&mut f.conn, f.graph, "foreign", 60)?.unwrap();
            }
            "pending_owner" => {
                f.conn.execute("UPDATE extraction_tasks SET lease_owner = 'foreign', lease_expires_epoch = ?1 WHERE id = ?2",
                    params![chrono::Utc::now().timestamp() + 60, f.graph])?;
            }
            "foreign_scope" => {
                f.conn.execute(
                    "UPDATE extraction_tasks SET session_row_id = NULL WHERE id = ?1",
                    [f.graph],
                )?;
            }
            "unrelated" => {
                db::enqueue_bounded_followup_extraction_task(
                    &f.conn,
                    &f.parent,
                    ExtractionTaskKind::UserContextCandidate,
                    f.events[0] - 1,
                    f.events[3],
                )?;
            }
            _ => {
                f.conn.execute(
                    "UPDATE extraction_tasks SET status = 'pending' WHERE id = ?1",
                    [f.parent.id],
                )?;
            }
        }
        let before = snapshot(&f.conn)?;
        assert!(
            db::retry_extraction_replay_range(&f.conn, f.range, false).is_err(),
            "{condition}"
        );
        assert_eq!(snapshot(&f.conn)?, before, "{condition}");
        let owner = db::exact_replay_worker_owner(73, 73);
        assert!(
            db::retry_and_claim_extraction_replay_range(
                &mut f.conn,
                f.range,
                false,
                false,
                &owner,
                60,
            )
            .is_err(),
            "{condition}"
        );
        assert_eq!(snapshot(&f.conn)?, before, "{condition}");
    }
    Ok(())
}

#[tokio::test]
async fn due_maintenance_recovers_failed_memory_without_resetting_its_pending_graph_wait(
) -> Result<()> {
    let mut f = waiting_family().await?;
    let before_graph = graph_wait_state(&f.conn, f.graph)?;
    // Make the failed range a due transient maintenance candidate. Graph keeps
    // the actual production Waiting state established by waiting_family.
    f.conn.execute(
        "UPDATE extraction_replay_ranges SET failure_class = 'transient', attempts = 1,
             failed_at_epoch = ?1 WHERE id = ?2",
        params![chrono::Utc::now().timestamp() - 1200, f.range],
    )?;
    let maintained = db::maintain_failure_lifecycle(&f.conn)?;
    assert_eq!(maintained.retried_extraction_replay_ranges, 1);
    assert_eq!(graph_wait_state(&f.conn, f.graph)?, before_graph);
    assert!(db::claim_extraction_task_by_id(&mut f.conn, f.graph, "too-early", 60)?.is_none());
    for id in [f.parent.id, f.memory] {
        let task = db::claim_extraction_task_by_id(&mut f.conn, id, "maintenance", 60)?.unwrap();
        db::mark_extraction_task_done(&f.conn, id, "maintenance", task.high_watermark_event_id)?;
    }
    assert_eq!(graph_wait_state(&f.conn, f.graph)?, before_graph);
    f.conn.execute(
        "UPDATE extraction_tasks SET next_retry_epoch = 0 WHERE id = ?1",
        [f.graph],
    )?;
    let graph = db::claim_extraction_task_by_id(&mut f.conn, f.graph, "maintenance", 60)?.unwrap();
    assert_graph_unblocked(&mut f.conn, &graph).await?;
    db::mark_extraction_task_done(&f.conn, graph.id, "maintenance", Some(f.events[3]))?;
    assert_eq!(
        db::get_extraction_replay_range_evidence(&f.conn, f.range)?
            .range
            .status,
        "replayed"
    );
    assert_eq!(
        db::maintain_failure_lifecycle(&f.conn)?.retried_extraction_replay_ranges,
        0
    );
    Ok(())
}
