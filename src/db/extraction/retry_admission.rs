use anyhow::{ensure, Result};
use rusqlite::{params, Connection};

use super::replay_member::{validated_replay_members, ReplayMemberProgress};

// Only the existing Graph -> failed Memory dependency may remain pending during
// replay admission. A review wait, an unrelated pending task, or a processing
// owner is not evidence that this range's failed prerequisite needs recovery.
const BLOCKED_GRAPH_SQL: &str = "t.id IS NOT r.replay_task_id
    AND t.task_kind = 'graph_candidate' AND t.archived_at_epoch IS NULL
    AND EXISTS (SELECT 1 FROM extraction_tasks p
      WHERE p.replay_range_id = r.id AND p.task_kind = 'memory_candidate'
        AND p.status = 'failed' AND p.lease_owner IS NULL AND p.lease_expires_epoch IS NULL
        AND p.host_id = t.host_id AND p.workspace_id = t.workspace_id
        AND p.project_id = t.project_id AND p.session_row_id IS t.session_row_id
        AND p.high_watermark_event_id >= t.high_watermark_event_id
        AND COALESCE(p.cursor_event_id, 0) < t.high_watermark_event_id)";

/// Shared by explicit/batch/automatic selection and its read-only count. Full
/// identity, original bounds and checkpoint validation still runs before writes.
pub(crate) fn replay_retry_family_predicate() -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM extraction_tasks t
      WHERE t.replay_range_id = r.id AND (
        t.lease_owner IS NOT NULL OR t.lease_expires_epoch IS NOT NULL
        OR t.status = 'processing'
        OR (t.status = 'pending' AND NOT ({BLOCKED_GRAPH_SQL}))))"
    )
}

pub(crate) fn validate_replay_family_admission(
    conn: &Connection,
    range_id: i64,
) -> Result<Vec<ReplayMemberProgress>> {
    let members = validated_replay_members(conn, range_id)?;
    for member in &members {
        ensure!(
            member.lease_owner.is_none() && member.lease_expires_epoch.is_none(),
            "replay member {} is owned by another attempt",
            member.id
        );
        let allowed = if member.status == "pending" {
            conn.query_row(
                &format!(
                    "SELECT {BLOCKED_GRAPH_SQL} FROM extraction_tasks t
                  JOIN extraction_replay_ranges r ON r.id = t.replay_range_id
                  WHERE t.id = ?1 AND r.id = ?2"
                ),
                params![member.id, range_id],
                |r| r.get::<_, bool>(0),
            )?
        } else {
            matches!(member.status.as_str(), "done" | "failed")
        };
        ensure!(
            allowed,
            "replay member {} is active and not blocked by this family's failed prerequisite",
            member.id
        );
    }
    Ok(members)
}
