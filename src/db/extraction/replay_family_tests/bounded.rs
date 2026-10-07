use super::*;

fn ordinary_parent() -> Result<(Connection, ExtractionTask, Vec<i64>)> {
    let mut conn = Connection::open_in_memory()?;
    crate::migrate::run_migrations(&conn)?;
    let mut events = Vec::new();
    for n in 0..4 {
        let event = db::record_captured_event(
            &conn,
            &CaptureEventInput {
                host: "codex-cli",
                session_id: "ordinary-bounded",
                project: "/tmp/remem-bounded-reentry",
                cwd: None,
                event_type: "session_stop",
                role: Some("user"),
                tool_name: None,
                content: &format!("bounded evidence {n}"),
                task_kind: Some(ExtractionTaskKind::SessionRollup),
            },
        )?;
        events.push(event.event_row_id);
    }
    let parent = claim_next_extraction_task(&mut conn, "producer", 60)?.unwrap();
    assert!(parent.replay_range_id.is_none());
    Ok((conn, parent, events))
}

fn enqueue(conn: &Connection, parent: &ExtractionTask, events: &[i64]) -> Result<i64> {
    enqueue_bounded_followup_extraction_task(
        conn,
        parent,
        ExtractionTaskKind::UserContextCandidate,
        events[0] - 1,
        events[3],
    )
}

fn fail_child(conn: &mut Connection, child: i64) -> Result<()> {
    let task = claim_extraction_task_by_id(conn, child, "child", 60)?.unwrap();
    mark_claimed_extraction_task_failed_or_retry(conn, &task, "child", "malformed source", 1)
}

#[test]
fn ordinary_bounded_reentry_resumes_an_existing_replay_members_own_progress() -> Result<()> {
    let (mut conn, parent, events) = ordinary_parent()?;
    let child = enqueue(&conn, &parent, &events)?;
    fail_child(&mut conn, child)?;
    let range = db::list_extraction_replay_ranges(&conn, None, 10)?[0].id;
    assert_eq!(enqueue(&conn, &parent, &events)?, child);
    let task = claim_extraction_task_by_id(&mut conn, child, "child", 60)?.unwrap();
    assert_eq!(task.replay_range_id, Some(range));
    mark_extraction_task_done(&conn, child, "child", Some(events[1]))?;
    fail_child(&mut conn, child)?;

    assert_eq!(enqueue(&conn, &parent, &events)?, child);
    assert_eq!(
        member_state(&conn, child)?,
        (
            "pending".into(),
            Some(events[1]),
            Some(events[1]),
            Some(events[0])
        )
    );
    let task = claim_extraction_task_by_id(&mut conn, child, "child", 60)?.unwrap();
    assert_eq!(task.cursor_event_id, Some(events[1]));
    assert_eq!(
        enqueue(&conn, &parent, &events)?,
        child,
        "repeated handoff must not disturb an existing owner"
    );
    mark_extraction_task_done(&conn, child, "child", Some(events[3]))?;
    assert_eq!(
        db::get_extraction_replay_range_evidence(&conn, range)?
            .range
            .status,
        "replayed"
    );
    assert_eq!(
        enqueue(&conn, &parent, &events)?,
        child,
        "a completed bounded handoff stays idempotent"
    );
    assert_eq!(member_state(&conn, child)?.0, "done");
    Ok(())
}

#[test]
fn ordinary_bounded_producer_cannot_revive_archived_replay_evidence() -> Result<()> {
    for linked in [false, true] {
        let (mut conn, parent, events) = ordinary_parent()?;
        let child = enqueue(&conn, &parent, &events)?;
        fail_child(&mut conn, child)?;
        let range = db::list_extraction_replay_ranges(&conn, None, 10)?[0].id;
        if linked {
            enqueue(&conn, &parent, &events)?;
            fail_child(&mut conn, child)?;
        }
        db::quarantine_extraction_replay_range(&conn, range)?;
        conn.execute(
            "UPDATE extraction_replay_ranges SET archived_at_epoch = 1 WHERE id = ?1",
            [range],
        )?;
        let before = member_state(&conn, child)?;
        let evidence = db::get_extraction_replay_range_evidence(&conn, range)?;
        let error = enqueue(&conn, &parent, &events).unwrap_err();
        assert!(error.to_string().contains("explicit exact recovery"));
        assert_eq!(member_state(&conn, child)?, before);
        assert_eq!(
            db::get_extraction_replay_range_evidence(&conn, range)?,
            evidence
        );
        let archived: i64 = conn.query_row(
            "SELECT archived_at_epoch FROM extraction_replay_ranges WHERE id = ?1",
            [range],
            |r| r.get(0),
        )?;
        assert_eq!(archived, 1);
    }
    Ok(())
}
