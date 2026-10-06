use anyhow::Result;
use rusqlite::Connection;

pub(super) fn log_failure_transition(conn: &Connection, task_id: i64, cause: &str) -> Result<()> {
    let (status, retry): (String, Option<i64>) = conn.query_row(
        "SELECT status, next_retry_epoch FROM extraction_tasks WHERE id = ?1",
        [task_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let detail = format!(
        "extraction id={task_id} failed: {}",
        crate::db::truncate_str(cause, 300)
    );
    match (status.as_str(), retry) {
        ("pending", Some(epoch)) => crate::log::warn(
            "worker",
            &format!("{detail}; retry scheduled at epoch={epoch}"),
        ),
        ("pending", None) => crate::log::error(
            "worker",
            &format!("{detail}; failed range retained for replay; later events pending"),
        ),
        ("failed" | "done", _) => crate::log::error(
            "worker",
            &format!("{detail}; status={status}; no retry scheduled"),
        ),
        _ => crate::log::error(
            "worker",
            &format!("{detail}; status changed to {status}; retry scheduling is not confirmed"),
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{self, test_support::ScopedTestDataDir, CaptureEventInput, ExtractionTaskKind};

    #[test]
    fn extraction_diagnostics_distinguish_retry_terminal_and_later_evidence() -> Result<()> {
        for (attempts, cause, later_events, expected_level, expected_detail) in [
            (
                0,
                "synthetic transient failure",
                false,
                "WARN",
                "retry scheduled at epoch=",
            ),
            (
                db::EXTRACTION_TASK_MAX_ATTEMPTS - 1,
                "synthetic exhausted failure",
                false,
                "ERROR",
                "no retry scheduled",
            ),
            (0, "malformed source", false, "ERROR", "no retry scheduled"),
            (
                0,
                "malformed source",
                true,
                "ERROR",
                "failed range retained for replay; later events pending",
            ),
        ] {
            let data = ScopedTestDataDir::new_offline("extraction-diagnostics");
            let mut conn = db::open_db()?;
            let event = db::record_captured_event(
                &conn,
                &CaptureEventInput {
                    host: "codex-cli",
                    session_id: "diagnostic",
                    project: "/synthetic",
                    cwd: None,
                    event_type: "tool_result",
                    role: None,
                    tool_name: Some("Task"),
                    content: "first",
                    task_kind: Some(ExtractionTaskKind::ObservationExtract),
                },
            )?;
            let id = event.extraction_task_id.expect("captured task");
            conn.execute(
                "UPDATE extraction_tasks SET attempts = ?1 WHERE id = ?2",
                rusqlite::params![attempts, id],
            )?;
            let task = db::claim_next_extraction_task(&mut conn, "worker-a", 60)?.unwrap();
            if later_events {
                db::record_captured_event(
                    &conn,
                    &CaptureEventInput {
                        host: "codex-cli",
                        session_id: "diagnostic",
                        project: "/synthetic",
                        cwd: None,
                        event_type: "tool_result",
                        role: None,
                        tool_name: Some("Task"),
                        content: "later",
                        task_kind: Some(ExtractionTaskKind::ObservationExtract),
                    },
                )?;
            }
            db::mark_claimed_extraction_task_failed_or_retry(&conn, &task, "worker-a", cause, 30)?;
            log_failure_transition(&conn, id, cause)?;
            let date = chrono::Local::now().format("%Y-%m-%d");
            let log =
                std::fs::read_to_string(data.path.join("logs").join(format!("remem-{date}.log")))?;
            let line = log
                .lines()
                .find(|line| line.contains(&format!("extraction id={id} failed:")))
                .expect("failure diagnostic");
            assert!(line.contains(expected_level), "{line}");
            assert!(line.contains(expected_detail), "{line}");
            assert!(!line.contains("retry in"), "{line}");
        }
        Ok(())
    }
}
