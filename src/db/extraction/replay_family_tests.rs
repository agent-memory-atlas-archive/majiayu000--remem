use anyhow::Result;
use rusqlite::{params, Connection};

use super::*;
use crate::db::{self, CaptureEventInput};

mod exact;

fn fixture() -> Result<(Connection, i64, Vec<i64>)> {
    let mut conn = Connection::open_in_memory()?;
    crate::migrate::run_migrations(&conn)?;
    let mut events = Vec::new();
    for n in 0..4 {
        let event = db::record_captured_event(
            &conn,
            &CaptureEventInput {
                host: "codex-cli",
                session_id: "replay-family",
                project: "/tmp/remem-family",
                cwd: None,
                event_type: "tool_result",
                role: None,
                tool_name: Some("Task"),
                content: &format!("family evidence {n}"),
                task_kind: Some(ExtractionTaskKind::ObservationExtract),
            },
        )?;
        events.push(event.event_row_id);
    }
    let source = claim_next_extraction_task(&mut conn, "source", 60)?.unwrap();
    mark_claimed_extraction_task_failed_or_retry(&conn, &source, "source", "malformed source", 1)?;
    let range_id = db::list_extraction_replay_ranges(&conn, None, 10)?[0].id;
    db::retry_extraction_replay_range(&conn, range_id, false)?;
    Ok((conn, range_id, events))
}

fn member_state(
    conn: &Connection,
    id: i64,
) -> Result<(String, Option<i64>, Option<i64>, Option<i64>)> {
    Ok(conn.query_row(
        "SELECT status, cursor_event_id, completed_event_id, replay_from_event_id
         FROM extraction_tasks WHERE id = ?1",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?)
}

#[test]
fn completed_replay_parent_retries_failed_child_from_its_own_checkpoint_and_converges() -> Result<()>
{
    let (mut conn, range, events) = fixture()?;
    let parent = claim_next_extraction_task(&mut conn, "parent", 60)?.unwrap();
    let child = enqueue_followup_extraction_task(
        &conn,
        &parent,
        ExtractionTaskKind::MemoryCandidate,
        events[3],
    )?;
    mark_extraction_task_done(&conn, parent.id, "parent", Some(events[3]))?;
    let task = claim_extraction_task_by_id(&mut conn, child, "child", 60)?.unwrap();
    mark_extraction_task_done(&conn, task.id, "child", Some(events[1]))?;
    let task = claim_extraction_task_by_id(&mut conn, child, "child", 60)?.unwrap();
    mark_claimed_extraction_task_failed_or_retry(&conn, &task, "child", "malformed source", 1)?;
    db::retry_extraction_replay_range(&conn, range, false)?;
    assert_eq!(member_state(&conn, parent.id)?.1, Some(events[3]));
    assert_eq!(
        member_state(&conn, child)?,
        (
            "pending".into(),
            Some(events[1]),
            Some(events[1]),
            Some(events[0])
        )
    );
    let parent = claim_extraction_task_by_id(&mut conn, parent.id, "retry", 60)?.unwrap();
    mark_extraction_task_done(&conn, parent.id, "retry", Some(events[3]))?;
    assert_eq!(
        db::get_extraction_replay_range_evidence(&conn, range)?
            .range
            .status,
        "requeued"
    );
    let child = claim_extraction_task_by_id(&mut conn, child, "retry", 60)?.unwrap();
    mark_extraction_task_done(&conn, child.id, "retry", Some(events[3]))?;
    assert_eq!(
        db::get_extraction_replay_range_evidence(&conn, range)?
            .range
            .status,
        "replayed"
    );
    Ok(())
}

#[test]
fn later_parent_chunk_does_not_skip_a_failed_followups_original_prefix() -> Result<()> {
    let (mut conn, range, events) = fixture()?;
    let parent = claim_next_extraction_task(&mut conn, "parent", 60)?.unwrap();
    let child_id = enqueue_followup_extraction_task(
        &conn,
        &parent,
        ExtractionTaskKind::MemoryCandidate,
        events[1],
    )?;
    mark_extraction_task_done(&conn, parent.id, "parent", Some(events[1]))?;
    let child = claim_extraction_task_by_id(&mut conn, child_id, "child", 60)?.unwrap();
    mark_claimed_extraction_task_failed_or_retry(&conn, &child, "child", "malformed source", 1)?;
    let next = claim_extraction_task_by_id(&mut conn, parent.id, "parent", 60)?.unwrap();
    assert_eq!(next.cursor_event_id, Some(events[1]));
    assert_eq!(
        enqueue_followup_extraction_task(
            &conn,
            &next,
            ExtractionTaskKind::MemoryCandidate,
            events[3]
        )?,
        child_id
    );
    let member = validated_replay_member(&conn, child_id, range)?;
    assert_eq!(
        (member.from, member.to, member.completed),
        (events[0], events[3], None)
    );
    assert_eq!(member_state(&conn, child_id)?.1, Some(events[0] - 1));
    Ok(())
}

#[test]
fn replay_retry_uses_verified_legacy_bounded_key_without_expanding_its_range() -> Result<()> {
    let (mut conn, range, events) = fixture()?;
    let parent = claim_next_extraction_task(&mut conn, "parent", 60)?.unwrap();
    let child_id = enqueue_bounded_followup_extraction_task(
        &conn,
        &parent,
        ExtractionTaskKind::UserContextCandidate,
        events[1] - 1,
        events[2],
    )?;
    mark_extraction_task_done(&conn, parent.id, "parent", Some(events[3]))?;
    let child = claim_extraction_task_by_id(&mut conn, child_id, "child", 60)?.unwrap();
    mark_claimed_extraction_task_failed_or_retry(&conn, &child, "child", "malformed source", 1)?;
    conn.execute(
        "UPDATE extraction_tasks SET replay_from_event_id = NULL, completed_event_id = NULL,
        cursor_event_id = high_watermark_event_id WHERE id = ?1",
        [child_id],
    )?;
    db::retry_extraction_replay_range(&conn, range, false)?;
    assert_eq!(
        member_state(&conn, child_id)?,
        ("pending".into(), Some(events[1] - 1), None, Some(events[1]))
    );
    Ok(())
}

#[test]
fn invalid_legacy_bounded_identity_or_scope_rolls_back_the_entire_replay_admission() -> Result<()> {
    for invalid in [
        "trailing",
        "numeric_alias",
        "wrong_target",
        "foreign_scope",
        "foreign_checkpoint",
    ] {
        let (mut conn, range, events) = fixture()?;
        let parent = claim_next_extraction_task(&mut conn, "parent", 60)?.unwrap();
        let child_id = enqueue_bounded_followup_extraction_task(
            &conn,
            &parent,
            ExtractionTaskKind::UserContextCandidate,
            events[1] - 1,
            events[2],
        )?;
        mark_extraction_task_done(&conn, parent.id, "parent", Some(events[3]))?;
        let child = claim_extraction_task_by_id(&mut conn, child_id, "child", 60)?.unwrap();
        mark_claimed_extraction_task_failed_or_retry(
            &conn,
            &child,
            "child",
            "malformed source",
            1,
        )?;
        conn.execute(
            "UPDATE extraction_tasks SET replay_from_event_id = NULL WHERE id = ?1",
            [child_id],
        )?;
        let key: String = conn.query_row(
            "SELECT idempotency_key FROM extraction_tasks WHERE id = ?1",
            [child_id],
            |r| r.get(0),
        )?;
        match invalid {
            "trailing" => {
                conn.execute(
                    "UPDATE extraction_tasks SET idempotency_key = ?1 WHERE id = ?2",
                    params![format!("{key}:extra"), child_id],
                )?;
            }
            "numeric_alias" => {
                conn.execute(
                    "UPDATE extraction_tasks SET idempotency_key = ?1 WHERE id = ?2",
                    params![key.replace(":bounded:1:", ":bounded:01:"), child_id],
                )?;
            }
            "wrong_target" => {
                conn.execute(
                    "UPDATE extraction_tasks SET high_watermark_event_id = ?1 WHERE id = ?2",
                    params![events[3], child_id],
                )?;
            }
            "foreign_scope" => {
                conn.execute(
                    "UPDATE extraction_tasks SET session_row_id = NULL WHERE id = ?1",
                    [child_id],
                )?;
            }
            _ => {
                conn.execute("UPDATE extraction_tasks SET completed_event_id = ?1, cursor_event_id = ?1 WHERE id = ?2", params![events[0], child_id])?;
            }
        }
        let before_parent = member_state(&conn, parent.id)?;
        let before_child = member_state(&conn, child_id)?;
        let before_range = db::get_extraction_replay_range_evidence(&conn, range)?;
        assert!(
            db::retry_extraction_replay_range(&conn, range, false).is_err(),
            "{invalid}"
        );
        assert_eq!(member_state(&conn, parent.id)?, before_parent, "{invalid}");
        assert_eq!(member_state(&conn, child_id)?, before_child, "{invalid}");
        assert_eq!(
            db::get_extraction_replay_range_evidence(&conn, range)?,
            before_range,
            "{invalid}"
        );
    }
    Ok(())
}
