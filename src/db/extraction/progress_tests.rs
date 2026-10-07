use anyhow::Result;
use rusqlite::{params, Connection};

use super::*;
use crate::db::{self, CaptureEventInput};

fn captured_task(conn: &Connection, session: &str, count: usize) -> Result<Vec<i64>> {
    let mut ids = Vec::new();
    for index in 0..count {
        let result = db::record_captured_event(
            conn,
            &CaptureEventInput {
                host: "codex-cli",
                session_id: session,
                project: "/tmp/remem-bounded",
                cwd: None,
                event_type: "tool_result",
                role: None,
                tool_name: Some("Task"),
                content: &format!("{session}: evidence {index}"),
                task_kind: Some(ExtractionTaskKind::ObservationExtract),
            },
        )?;
        ids.push(result.event_row_id);
    }
    Ok(ids)
}

fn setup() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    crate::migrate::run_migrations(&conn)?;
    Ok(conn)
}

fn progress(conn: &Connection, id: i64) -> Result<(String, Option<i64>, Option<i64>)> {
    Ok(conn.query_row(
        "SELECT status, cursor_event_id, completed_event_id FROM extraction_tasks WHERE id = ?1",
        [id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?)
}

fn replay_fixture(conn: &mut Connection) -> Result<(i64, Vec<i64>)> {
    let events = captured_task(conn, "replay", 3)?;
    let task = claim_next_extraction_task(conn, "source", 60)?.unwrap();
    mark_claimed_extraction_task_failed_or_retry(conn, &task, "source", "malformed source", 1)?;
    let range = db::list_extraction_replay_ranges(conn, None, 10)?.remove(0);
    Ok((range.id, events))
}

#[test]
fn successful_chunk_checkpoint_requeues_only_uncovered_events() -> Result<()> {
    let mut conn = setup()?;
    let events = captured_task(&conn, "bounded", 3)?;
    let task = claim_next_extraction_task(&mut conn, "worker", 60)?.unwrap();
    mark_extraction_task_done(&conn, task.id, "worker", Some(events[0]))?;
    assert_eq!(
        progress(&conn, task.id)?,
        ("pending".into(), Some(events[0]), Some(events[0]))
    );
    let resumed = claim_next_extraction_task(&mut conn, "worker", 60)?.unwrap();
    assert_eq!(resumed.cursor_event_id, Some(events[0]));
    assert_eq!(resumed.high_watermark_event_id, Some(events[2]));
    Ok(())
}

#[test]
fn extraction_success_rejects_expired_reclaimed_and_wrong_scope_boundaries() -> Result<()> {
    let mut conn = setup()?;
    let events = captured_task(&conn, "target", 2)?;
    let other = captured_task(&conn, "other", 1)?;
    let task = claim_next_extraction_task(&mut conn, "worker", 60)?.unwrap();
    conn.execute(
        "UPDATE extraction_tasks SET high_watermark_event_id = ?1 WHERE id = ?2",
        params![other[0], task.id],
    )?;
    let before = progress(&conn, task.id)?;
    for boundary in [other[0], events[1] + 100] {
        assert!(mark_extraction_task_done(&conn, task.id, "worker", Some(boundary)).is_err());
        assert_eq!(progress(&conn, task.id)?, before);
    }
    conn.execute(
        "UPDATE extraction_tasks SET lease_expires_epoch = 0 WHERE id = ?1",
        [task.id],
    )?;
    assert!(mark_extraction_task_done(&conn, task.id, "worker", Some(events[0])).is_err());
    conn.execute(
        "UPDATE extraction_tasks SET lease_owner = 'replacement', lease_expires_epoch = ?2 WHERE id = ?1",
        params![task.id, chrono::Utc::now().timestamp() + 60],
    )?;
    assert!(mark_extraction_task_done(&conn, task.id, "worker", Some(events[0])).is_err());
    assert_eq!(progress(&conn, task.id)?, before);
    Ok(())
}

#[test]
fn exhausted_chunk_records_only_attempted_range_and_preserves_success() -> Result<()> {
    let mut conn = setup()?;
    let events = captured_task(&conn, "chunk-failure", 4)?;
    let first = claim_next_extraction_task(&mut conn, "worker", 60)?.unwrap();
    mark_extraction_task_done(&conn, first.id, "worker", Some(events[0]))?;
    let mut attempted = claim_next_extraction_task(&mut conn, "worker", 60)?.unwrap();
    attempted.high_watermark_event_id = Some(events[1]);
    mark_claimed_extraction_task_failed_or_retry(
        &conn,
        &attempted,
        "worker",
        "malformed source",
        1,
    )?;
    let ranges = db::list_extraction_replay_ranges(&conn, None, 10)?;
    assert_eq!(ranges.len(), 1);
    assert_eq!(
        (ranges[0].from_event_id, ranges[0].to_event_id),
        (events[1], events[1])
    );
    assert_eq!(
        progress(&conn, first.id)?,
        ("pending".into(), Some(events[1]), Some(events[0]))
    );
    let next = claim_next_extraction_task(&mut conn, "worker", 60)?.unwrap();
    assert_eq!(next.cursor_event_id, Some(events[1]));
    assert_eq!(next.high_watermark_event_id, Some(events[3]));
    Ok(())
}

#[test]
fn exact_replay_resumes_successful_chunk_and_keeps_original_range() -> Result<()> {
    let mut conn = setup()?;
    let (range, events) = replay_fixture(&mut conn)?;
    let owner = db::exact_replay_worker_owner(1, 1);
    let task =
        db::retry_and_claim_extraction_replay_range(&mut conn, range, false, false, &owner, 60)?;
    checkpoint_claimed_extraction_task_chunk(&conn, &task, &owner, events[0])?;
    assert_eq!(
        progress(&conn, task.id)?,
        ("processing".into(), Some(events[0]), Some(events[0]))
    );
    // A stale in-memory cursor cannot publish a second checkpoint.
    assert!(checkpoint_claimed_extraction_task_chunk(&conn, &task, &owner, events[1]).is_err());
    db::archive_claimed_exact_replay_task(&conn, task.id, &owner, "second chunk timeout")?;
    let resumed =
        db::retry_and_claim_extraction_replay_range(&mut conn, range, true, true, &owner, 60)?;
    assert_eq!(resumed.cursor_event_id, Some(events[0]));
    assert_eq!(resumed.high_watermark_event_id, Some(events[2]));
    let evidence = db::get_extraction_replay_range_evidence(&conn, range)?;
    assert_eq!(
        (evidence.range.from_event_id, evidence.range.to_event_id),
        (events[0], events[2])
    );
    mark_extraction_task_done(&conn, resumed.id, &owner, Some(events[2]))?;
    assert_eq!(
        db::get_extraction_replay_range_evidence(&conn, range)?
            .range
            .status,
        "replayed"
    );
    Ok(())
}

#[test]
fn failed_replay_chunk_cannot_skip_to_later_evidence() -> Result<()> {
    let mut conn = setup()?;
    let (range, events) = replay_fixture(&mut conn)?;
    db::retry_extraction_replay_range(&conn, range, false)?;
    let first = claim_next_extraction_task(&mut conn, "worker", 60)?.unwrap();
    mark_extraction_task_done(&conn, first.id, "worker", Some(events[0]))?;
    let mut next = claim_next_extraction_task(&mut conn, "worker", 60)?.unwrap();
    next.high_watermark_event_id = Some(events[1]);
    mark_claimed_extraction_task_failed_or_retry(&conn, &next, "worker", "malformed source", 1)?;
    assert_eq!(
        progress(&conn, first.id)?,
        ("failed".into(), Some(events[0]), Some(events[0]))
    );
    assert!(claim_next_extraction_task(&mut conn, "worker", 60)?.is_none());
    db::retry_extraction_replay_range(&conn, range, false)?;
    let retry = claim_next_extraction_task(&mut conn, "worker", 60)?.unwrap();
    assert_eq!(retry.cursor_event_id, Some(events[0]));
    Ok(())
}

#[test]
fn replay_never_infers_success_from_legacy_cursor_and_rejects_foreign_checkpoint() -> Result<()> {
    let mut conn = setup()?;
    let (range, events) = replay_fixture(&mut conn)?;
    db::retry_extraction_replay_range(&conn, range, false)?;
    let task = claim_next_extraction_task(&mut conn, "worker", 60)?.unwrap();
    mark_claimed_extraction_task_failed_or_retry(&conn, &task, "worker", "malformed source", 1)?;
    conn.execute(
        "UPDATE extraction_tasks SET cursor_event_id = ?1, completed_event_id = NULL WHERE id = ?2",
        params![events[2], task.id],
    )?;
    assert_eq!(replay_resume_event_id(&conn, range)?, None);
    db::retry_extraction_replay_range(&conn, range, false)?;
    assert_eq!(progress(&conn, task.id)?.1, Some(events[0] - 1));

    let foreign = captured_task(&conn, "foreign", 1)?[0];
    conn.execute(
        "UPDATE extraction_tasks SET completed_event_id = ?1, cursor_event_id = ?1 WHERE id = ?2",
        params![foreign, task.id],
    )?;
    assert!(replay_resume_event_id(&conn, range).is_err());
    Ok(())
}

#[test]
fn replay_bounded_followup_completes_without_claiming_canonical_progress() -> Result<()> {
    let mut conn = setup()?;
    let (range, events) = replay_fixture(&mut conn)?;
    db::retry_extraction_replay_range(&conn, range, false)?;
    let parent = claim_next_extraction_task(&mut conn, "parent", 60)?.unwrap();
    let child_id = enqueue_bounded_followup_extraction_task(
        &conn,
        &parent,
        ExtractionTaskKind::UserContextCandidate,
        events[0] - 1,
        events[1],
    )?;
    let child = db::claim_extraction_task_by_id(&mut conn, child_id, "child", 60)?.unwrap();
    mark_extraction_task_done(&conn, child.id, "child", Some(events[1]))?;
    assert_eq!(progress(&conn, child.id)?.0, "done");
    assert_eq!(replay_resume_event_id(&conn, range)?, None);
    mark_extraction_task_done(&conn, parent.id, "parent", Some(events[2]))?;
    assert_eq!(
        db::get_extraction_replay_range_evidence(&conn, range)?
            .range
            .status,
        "replayed"
    );
    Ok(())
}
