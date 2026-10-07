use anyhow::{ensure, Result};
use rusqlite::{params, Connection};

use super::ExtractionTask;

pub(crate) const CAPTURED_EVENT_BATCH_LIMIT: usize = 64;
pub(crate) const EXTRACTION_INPUT_MAX_BYTES: usize = 256 * 1024;
pub(crate) const EXTRACTION_WRAPPER_RESERVE_BYTES: usize = 4096;

pub(crate) fn extraction_input_bytes(system: &str, prompt: &str) -> usize {
    system
        .len()
        .saturating_add(prompt.len())
        .saturating_add(EXTRACTION_WRAPPER_RESERVE_BYTES)
}

pub(crate) fn extraction_prompt_fits(system: &str, prompt: &str) -> bool {
    extraction_input_bytes(system, prompt) <= EXTRACTION_INPUT_MAX_BYTES
}

/// Freeze the actual attempted prefix before payload decoding can fail. Keep
/// the durable coalesced target untouched so later capture is still pending.
pub(crate) fn bound_captured_event_attempt(
    conn: &Connection,
    task: &mut ExtractionTask,
) -> Result<()> {
    let Some(high) = task.high_watermark_event_id else {
        return Ok(());
    };
    let cursor = task.cursor_event_id.unwrap_or(0);
    if high <= cursor {
        return Ok(());
    }
    let session = task
        .session_row_id
        .ok_or_else(|| anyhow::anyhow!("missing extraction session evidence"))?;
    let ids = conn
        .prepare(
            "SELECT id FROM captured_events
         WHERE host_id = ?1 AND project_id = ?2 AND session_row_id = ?3
           AND id > ?4 AND id <= ?5 ORDER BY id ASC LIMIT ?6",
        )?
        .query_map(
            params![
                task.host_id,
                task.project_id,
                session,
                cursor,
                high,
                CAPTURED_EVENT_BATCH_LIMIT as i64
            ],
            |row| row.get::<_, i64>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        !ids.is_empty(),
        "missing evidence rows for extraction task {} range {}..{}",
        task.id,
        cursor + 1,
        high
    );
    task.high_watermark_event_id = ids.last().copied();
    Ok(())
}

pub(crate) fn log_extraction_input(
    task: &ExtractionTask,
    system: &str,
    prompt: &str,
    truncated_event_ids: &[i64],
) {
    crate::log::info("extraction", &format!(
        "input task={} kind={} range={}..{} input_bytes={} max_input_bytes={} content_truncated_event_ids={:?}; raw evidence retained",
        task.id, task.task_kind.as_str(), task.cursor_event_id.unwrap_or(0) + 1,
        task.high_watermark_event_id.unwrap_or(0), extraction_input_bytes(system, prompt),
        EXTRACTION_INPUT_MAX_BYTES, truncated_event_ids
    ));
}
