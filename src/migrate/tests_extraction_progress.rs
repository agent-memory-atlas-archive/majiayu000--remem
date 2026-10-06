use anyhow::Result;
use rusqlite::Connection;

#[test]
fn v094_preserves_legacy_exhausted_cursor_without_inventing_success() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    super::state::ensure_migration_table(&conn)?;
    for migration in super::MIGRATIONS.iter().filter(|m| m.version <= 93) {
        conn.execute_batch(migration.sql)?;
        super::state::mark_applied(&conn, migration.version, migration.name)?;
    }
    let outcome = crate::db::record_captured_event(
        &conn,
        &crate::db::CaptureEventInput {
            host: "codex-cli",
            session_id: "legacy",
            project: "/tmp/remem-progress-migration",
            cwd: None,
            event_type: "tool_result",
            role: None,
            tool_name: Some("Task"),
            content: "legacy exhausted evidence",
            task_kind: Some(crate::db::ExtractionTaskKind::ObservationExtract),
        },
    )?;
    let task = outcome.extraction_task_id.unwrap();
    conn.execute(
        "UPDATE extraction_tasks SET status = 'failed', cursor_event_id = high_watermark_event_id,
         attempts = 5, last_error = 'legacy exhausted evidence' WHERE id = ?1",
        [task],
    )?;
    super::run_migrations(&conn)?;
    super::run_migrations(&conn)?;
    let row: (String, i64, Option<i64>, i64, String) = conn.query_row(
        "SELECT status, cursor_event_id, completed_event_id, attempts, last_error
         FROM extraction_tasks WHERE id = ?1",
        [task],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    assert_eq!(
        row,
        (
            "failed".into(),
            outcome.event_row_id,
            None,
            5,
            "legacy exhausted evidence".into()
        )
    );
    conn.execute_batch("ALTER TABLE extraction_tasks DROP COLUMN completed_event_id")?;
    let error = super::run_migrations(&conn)
        .expect_err("missing successful-progress column must fail closed");
    assert!(format!("{error:#}").contains("completed_event_id"));
    Ok(())
}
