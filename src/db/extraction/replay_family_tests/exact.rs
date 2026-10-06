use super::*;

struct Family {
    conn: Connection,
    range: i64,
    events: Vec<i64>,
    parent: i64,
    child: i64,
    grandchild: i64,
    bounded: i64,
}

fn archived_family() -> Result<Family> {
    let (mut conn, range, events) = fixture()?;
    let parent = claim_next_extraction_task(&mut conn, "parent", 60)?.unwrap();
    let child_id = enqueue_followup_extraction_task(
        &conn,
        &parent,
        ExtractionTaskKind::MemoryCandidate,
        events[3],
    )?;
    let bounded = enqueue_bounded_followup_extraction_task(
        &conn,
        &parent,
        ExtractionTaskKind::UserContextCandidate,
        events[1] - 1,
        events[2],
    )?;
    mark_extraction_task_done(&conn, parent.id, "parent", Some(events[3]))?;
    let child = claim_extraction_task_by_id(&mut conn, child_id, "child", 60)?.unwrap();
    let grandchild = enqueue_followup_extraction_task(
        &conn,
        &child,
        ExtractionTaskKind::GraphCandidate,
        events[2],
    )?;
    mark_claimed_extraction_task_failed_or_retry(&conn, &child, "child", "malformed source", 1)?;
    for id in [grandchild, bounded] {
        let task = claim_extraction_task_by_id(&mut conn, id, "child", 60)?.unwrap();
        mark_claimed_extraction_task_failed_or_retry(&conn, &task, "child", "malformed source", 1)?;
    }
    conn.execute(
        "UPDATE extraction_replay_ranges SET status = 'quarantined', archived_at_epoch = 1
        WHERE id = ?1",
        [range],
    )?;
    conn.execute(
        "UPDATE extraction_tasks SET archived_at_epoch = 1
        WHERE replay_range_id = ?1 AND status = 'failed'",
        [range],
    )?;
    Ok(Family {
        conn,
        range,
        events,
        parent: parent.id,
        child: child_id,
        grandchild,
        bounded,
    })
}

fn snapshot(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT json_array(id, task_kind, host_id, workspace_id, project_id, session_row_id,
           status, idempotency_key, cursor_event_id, high_watermark_event_id, replay_range_id,
           replay_from_event_id, completed_event_id, attempts, next_retry_epoch, lease_owner,
           lease_expires_epoch, last_error, failure_class, failed_at_epoch, archived_at_epoch,
           created_at_epoch, updated_at_epoch) FROM extraction_tasks ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut values = db::query::collect_rows(rows)?;
    let mut stmt = conn.prepare(
        "SELECT json_array(id, replay_task_id, status, attempts, from_event_id, to_event_id,
           last_error, failure_class, failed_at_epoch, archived_at_epoch, updated_at_epoch)
         FROM extraction_replay_ranges ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    values.extend(db::query::collect_rows(rows)?);
    Ok(values)
}

#[test]
fn exact_replay_restores_existing_family_and_preserves_each_member_on_later_failure() -> Result<()>
{
    let mut f = archived_family()?;
    let owner = db::exact_replay_worker_owner(11, 11);
    let parent =
        db::retry_and_claim_extraction_replay_range(&mut f.conn, f.range, true, true, &owner, 60)?;
    let family = db::load_claimed_exact_replay_family(&f.conn, parent.id, &owner)?;
    assert_eq!(family.len(), 4);
    assert_eq!(family[0].cursor_event_id, Some(f.events[3]));
    let pending: i64 = f.conn.query_row(
        "SELECT COUNT(*) FROM extraction_tasks WHERE status = 'pending'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(pending, 0, "admission cannot expose ordinary daemon work");
    let deadlines: i64 = f.conn.query_row(
        "SELECT COUNT(DISTINCT lease_expires_epoch) FROM extraction_tasks
        WHERE replay_range_id = ?1 AND status = 'processing'",
        [f.range],
        |r| r.get(0),
    )?;
    assert_eq!(deadlines, 1);
    let child = family.iter().find(|t| t.id == f.child).unwrap();
    checkpoint_claimed_extraction_task_chunk(&f.conn, child, &owner, f.events[3])?;
    let mut grandchild = family
        .iter()
        .find(|t| t.id == f.grandchild)
        .unwrap()
        .clone();
    grandchild.high_watermark_event_id = Some(f.events[1]);
    checkpoint_claimed_extraction_task_chunk(&f.conn, &grandchild, &owner, f.events[1])?;
    db::archive_claimed_exact_replay_task(&f.conn, f.grandchild, &owner, "later member timeout")?;
    assert_eq!(member_state(&f.conn, f.parent)?.0, "done");
    assert_eq!(member_state(&f.conn, f.child)?.0, "done");
    assert_eq!(
        member_state(&f.conn, f.grandchild)?,
        (
            "failed".into(),
            Some(f.events[1]),
            Some(f.events[1]),
            Some(f.events[0])
        )
    );
    assert_eq!(
        member_state(&f.conn, f.bounded)?,
        (
            "failed".into(),
            Some(f.events[1] - 1),
            None,
            Some(f.events[1])
        )
    );
    let evidence = db::get_extraction_replay_range_evidence(&f.conn, f.range)?;
    assert_eq!(evidence.range.status, "quarantined");
    assert_eq!(
        evidence.range.replay_task_id,
        Some(parent.id),
        "child failure must not replace canonical evidence"
    );
    assert!(claim_next_extraction_task(&mut f.conn, "ordinary", 60)?.is_none());

    let next_owner = db::exact_replay_worker_owner(12, 12);
    let parent = db::retry_and_claim_extraction_replay_range(
        &mut f.conn,
        f.range,
        true,
        true,
        &next_owner,
        60,
    )?;
    let family = db::load_claimed_exact_replay_family(&f.conn, parent.id, &next_owner)?;
    assert_eq!(
        family.len(),
        3,
        "fully successful child must not be processed again"
    );
    for member in &family {
        if member.cursor_event_id != member.high_watermark_event_id {
            checkpoint_claimed_extraction_task_chunk(
                &f.conn,
                member,
                &next_owner,
                member.high_watermark_event_id.unwrap(),
            )?;
        }
    }
    db::finish_claimed_exact_replay_family(&f.conn, parent.id, &next_owner)?;
    assert_eq!(
        db::get_extraction_replay_range_evidence(&f.conn, f.range)?
            .range
            .status,
        "replayed"
    );
    let total: i64 = f.conn.query_row(
        "SELECT COUNT(*) FROM extraction_tasks WHERE replay_range_id = ?1",
        [f.range],
        |r| r.get(0),
    )?;
    assert_eq!(
        total, 4,
        "existing-family recovery must not create successors"
    );
    let raw: i64 = f
        .conn
        .query_row("SELECT COUNT(*) FROM captured_events", [], |r| r.get(0))?;
    assert_eq!(raw, 4);
    Ok(())
}

#[test]
fn exact_family_admission_rejects_foreign_owner_scope_bound_and_future_retry_without_writes(
) -> Result<()> {
    for condition in ["owner", "scope", "bound", "retry"] {
        let mut f = archived_family()?;
        match condition {
            "owner" => {
                f.conn.execute("UPDATE extraction_tasks SET lease_owner = 'foreign', lease_expires_epoch = ?1 WHERE id = ?2",
                params![chrono::Utc::now().timestamp() + 120, f.child])?;
            }
            "scope" => {
                f.conn.execute(
                    "UPDATE extraction_tasks SET session_row_id = NULL WHERE id = ?1",
                    [f.child],
                )?;
            }
            "bound" => {
                f.conn.execute(
                    "UPDATE extraction_tasks SET replay_from_event_id = ?1 WHERE id = ?2",
                    params![f.events[0], f.bounded],
                )?;
            }
            _ => {
                f.conn.execute(
                    "UPDATE extraction_tasks SET next_retry_epoch = ?1 WHERE id = ?2",
                    params![chrono::Utc::now().timestamp() + 120, f.child],
                )?;
            }
        }
        let before = snapshot(&f.conn)?;
        let owner = db::exact_replay_worker_owner(21, 21);
        assert!(
            db::retry_and_claim_extraction_replay_range(
                &mut f.conn,
                f.range,
                true,
                true,
                &owner,
                60
            )
            .is_err(),
            "{condition}"
        );
        assert_eq!(snapshot(&f.conn)?, before, "{condition}");
    }
    Ok(())
}

#[test]
fn expired_exact_family_archives_once_and_keeps_successful_members() -> Result<()> {
    let mut f = archived_family()?;
    let owner = db::exact_replay_worker_owner(31, 31);
    let parent =
        db::retry_and_claim_extraction_replay_range(&mut f.conn, f.range, true, true, &owner, 60)?;
    let family = db::load_claimed_exact_replay_family(&f.conn, parent.id, &owner)?;
    let child = family.iter().find(|t| t.id == f.child).unwrap();
    checkpoint_claimed_extraction_task_chunk(&f.conn, child, &owner, f.events[3])?;
    f.conn.execute(
        "UPDATE extraction_tasks SET lease_expires_epoch = 0 WHERE replay_range_id = ?1
        AND status = 'processing'",
        [f.range],
    )?;
    assert_eq!(release_expired_extraction_task_leases(&f.conn)?, 4);
    assert_eq!(release_expired_extraction_task_leases(&f.conn)?, 0);
    assert_eq!(member_state(&f.conn, f.parent)?.0, "done");
    assert_eq!(member_state(&f.conn, f.child)?.0, "done");
    assert_eq!(member_state(&f.conn, f.bounded)?.0, "failed");
    assert_eq!(
        db::get_extraction_replay_range_evidence(&f.conn, f.range)?
            .range
            .status,
        "quarantined"
    );
    assert!(claim_next_extraction_task(&mut f.conn, "ordinary", 60)?.is_none());
    Ok(())
}

#[test]
fn exact_family_failure_never_clears_another_members_replacement_owner() -> Result<()> {
    let mut f = archived_family()?;
    let owner = db::exact_replay_worker_owner(41, 41);
    let replacement = db::exact_replay_worker_owner(42, 42);
    let parent =
        db::retry_and_claim_extraction_replay_range(&mut f.conn, f.range, true, true, &owner, 60)?;
    f.conn.execute(
        "UPDATE extraction_tasks SET lease_owner = ?1 WHERE id = ?2",
        params![replacement, f.child],
    )?;
    let before = snapshot(&f.conn)?;
    assert!(db::finish_claimed_exact_replay_family(&f.conn, parent.id, &owner).is_err());
    assert_eq!(snapshot(&f.conn)?, before);
    let lease = |conn: &Connection| -> Result<(String, Option<String>, Option<i64>, Option<i64>)> {
        Ok(conn.query_row(
            "SELECT status, lease_owner, lease_expires_epoch, completed_event_id
            FROM extraction_tasks WHERE id = ?1",
            [f.child],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?)
    };
    let before = lease(&f.conn)?;
    db::archive_claimed_exact_replay_task(&f.conn, parent.id, &owner, "family ownership changed")?;
    assert_eq!(lease(&f.conn)?, before);
    assert_eq!(member_state(&f.conn, f.bounded)?.0, "failed");
    assert_eq!(
        db::get_extraction_replay_range_evidence(&f.conn, f.range)?
            .range
            .status,
        "quarantined"
    );
    Ok(())
}
