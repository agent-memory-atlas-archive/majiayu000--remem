use anyhow::Result;
use rusqlite::{params, Connection};

use crate::db::{self, ExtractionTaskKind};

pub(super) fn claim_rollup_task(conn: &mut Connection) -> Result<db::ExtractionTask> {
    // Each rollup fixture has one ready rollup. Other stages can remain queued
    // and receive earlier service from the production fair dispatcher.
    let candidates = {
        let mut statement = conn.prepare(
            "SELECT id, host_id, project_id, session_row_id, cursor_event_id,
                    high_watermark_event_id
             FROM extraction_tasks
             WHERE task_kind = 'session_rollup' AND status = 'pending'
               AND (next_retry_epoch IS NULL OR next_retry_epoch <= ?1)
             ORDER BY id LIMIT 2",
        )?;
        let rows = statement.query_map(params![chrono::Utc::now().timestamp()], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    anyhow::ensure!(
        candidates.len() == 1,
        "rollup fixture requires exactly one ready session_rollup task, found {}",
        candidates.len()
    );
    let expected = candidates[0];
    let task = db::claim_extraction_task_by_id(conn, expected.0, "worker-a", 60)?
        .ok_or_else(|| anyhow::anyhow!("expected identified rollup task"))?;
    assert_eq!(task.task_kind, ExtractionTaskKind::SessionRollup);
    assert_eq!(
        (
            task.id,
            task.host_id,
            task.project_id,
            task.session_row_id,
            task.cursor_event_id,
            task.high_watermark_event_id,
        ),
        expected,
        "fixture claim must preserve the selected task identity and captured range"
    );
    Ok(task)
}
