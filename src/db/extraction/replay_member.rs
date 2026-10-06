use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection};

/// Original evidence bounds and successful progress are independent facts. In
/// particular, a legacy exhausted cursor proves neither of them.
#[derive(Debug)]
pub(crate) struct ReplayMemberProgress {
    pub id: i64,
    pub from: i64,
    pub to: i64,
    pub completed: Option<i64>,
    pub status: String,
    pub lease_owner: Option<String>,
    pub lease_expires_epoch: Option<i64>,
    pub next_retry_epoch: Option<i64>,
}

impl ReplayMemberProgress {
    pub fn resume_cursor(&self) -> i64 {
        self.completed.unwrap_or(self.from - 1)
    }

    pub fn is_complete(&self) -> bool {
        self.completed == Some(self.to)
    }
}

pub(crate) fn validated_replay_members(
    conn: &Connection,
    range_id: i64,
) -> Result<Vec<ReplayMemberProgress>> {
    let ids = {
        let mut stmt = conn.prepare(
            "SELECT t.id FROM extraction_tasks t JOIN extraction_replay_ranges r ON r.id = ?1
             WHERE t.replay_range_id = r.id
             ORDER BY t.id IS NOT r.replay_task_id, t.priority, t.id",
        )?;
        let rows = stmt.query_map([range_id], |r| r.get::<_, i64>(0))?;
        crate::db::query::collect_rows(rows)?
    };
    ids.into_iter()
        .map(|id| validated_replay_member(conn, id, range_id))
        .collect()
}

pub(crate) fn validated_replay_member(
    conn: &Connection,
    task_id: i64,
    range_id: i64,
) -> Result<ReplayMemberProgress> {
    let (
        key,
        prefix,
        stored_from,
        cursor,
        mut member,
        range_from,
        canonical,
        source,
        scope_valid,
        kind,
    ) = conn
        .query_row(
            "SELECT t.idempotency_key,
                t.host_id || ':' || t.project_id || ':' || t.session_row_id || ':' || t.task_kind,
                t.replay_from_event_id, t.cursor_event_id, t.high_watermark_event_id,
                t.completed_event_id, t.status, t.lease_owner, t.lease_expires_epoch,
                t.next_retry_epoch, r.from_event_id, r.replay_task_id IS t.id,
                r.source_task_id IS t.id,
                r.from_event_id > 0 AND r.to_event_id >= r.from_event_id
                  AND t.host_id = r.host_id AND t.workspace_id = r.workspace_id
                  AND t.project_id = r.project_id AND t.session_row_id IS r.session_row_id
                  AND t.session_row_id IS NOT NULL
                  AND t.high_watermark_event_id BETWEEN r.from_event_id AND r.to_event_id
                  AND (r.replay_task_id IS NOT t.id OR
                       (r.task_kind = t.task_kind AND t.high_watermark_event_id = r.to_event_id)),
                t.task_kind
             FROM extraction_tasks t JOIN extraction_replay_ranges r ON r.id = ?2
             WHERE t.id = ?1 AND t.replay_range_id = r.id",
            params![task_id, range_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    ReplayMemberProgress {
                        id: task_id,
                        from: 0,
                        to: row.get(4)?,
                        completed: row.get(5)?,
                        status: row.get(6)?,
                        lease_owner: row.get(7)?,
                        lease_expires_epoch: row.get(8)?,
                        next_retry_epoch: row.get(9)?,
                    },
                    row.get::<_, i64>(10)?,
                    row.get::<_, bool>(11)?,
                    row.get::<_, bool>(12)?,
                    row.get::<_, bool>(13)?,
                    row.get::<_, String>(14)?,
                ))
            },
        )
        .with_context(|| format!("invalid replay member {task_id} for range {range_id}"))?;
    ensure!(
        scope_valid,
        "replay member {task_id} scope or target mismatch"
    );
    crate::db::ExtractionTaskKind::from_db(&kind)?;

    let bounded_from = bounded_key_start(&key, &prefix, range_id, member.to, source)?;
    let known_followup = key == format!("{prefix}:replay:{range_id}")
        || key == format!("{prefix}:replay-range:{range_id}");
    ensure!(
        canonical || bounded_from.is_some() || known_followup,
        "replay member {task_id} has unverifiable original identity"
    );
    let inferred_from = bounded_from.unwrap_or(range_from);
    member.from = stored_from.unwrap_or(if canonical { range_from } else { inferred_from });
    ensure!(
        member.from >= range_from
            && member.from <= member.to
            && (!canonical || member.from == range_from)
            && bounded_from.is_none_or(|from| from == member.from),
        "replay member {task_id} original lower bound mismatch"
    );
    ensure!(
        captured_endpoint(conn, task_id, member.to)?
            && (bounded_from.is_none() || captured_endpoint(conn, task_id, member.from)?),
        "replay member {task_id} has missing or foreign source endpoints"
    );
    if let Some(completed) = member.completed {
        ensure!(
            completed >= member.from
                && completed <= member.to
                && cursor.is_some_and(|cursor| cursor >= completed && cursor <= member.to)
                && captured_endpoint(conn, task_id, completed)?,
            "replay member {task_id} has an invalid successful checkpoint"
        );
    }
    Ok(member)
}

fn captured_endpoint(conn: &Connection, task_id: i64, event_id: i64) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM extraction_tasks t JOIN captured_events e
           ON e.id = ?2 AND e.host_id = t.host_id AND e.workspace_id = t.workspace_id
             AND e.project_id = t.project_id AND e.session_row_id IS t.session_row_id
         WHERE t.id = ?1)",
        params![task_id, event_id],
        |r| r.get(0),
    )?)
}

/// Full grammar and roundtrip reject partial parses, numeric aliases, mismatched
/// scopes/kinds/ranges, and stale HWM values. A bare bounded key is valid only
/// for the original bounded source later explicitly linked to its own range.
fn bounded_key_start(
    key: &str,
    prefix: &str,
    range_id: i64,
    high: i64,
    is_source: bool,
) -> Result<Option<i64>> {
    let Some(suffix) = key.strip_prefix(&format!("{prefix}:bounded:")) else {
        ensure!(
            !key.contains(":bounded:"),
            "replay bounded identity scope mismatch"
        );
        return Ok(None);
    };
    let parts: Vec<_> = suffix.split(':').collect();
    ensure!(
        parts.len() == 4 || (is_source && parts.len() == 2),
        "invalid replay bounded identity grammar"
    );
    let cursor = parts[0].parse::<i64>().context("invalid bounded start")?;
    let target = parts[1].parse::<i64>().context("invalid bounded target")?;
    let expected = if parts.len() == 4 {
        format!("{prefix}:bounded:{cursor}:{target}:replay:{range_id}")
    } else {
        format!("{prefix}:bounded:{cursor}:{target}")
    };
    ensure!(
        key == expected && cursor >= 0 && target == high && target > cursor,
        "replay bounded identity range mismatch"
    );
    Ok(Some(cursor + 1))
}

/// Called inside the replay admission transaction after every member validates.
/// A terminal status alone may come from quarantine cleanup, so only the explicit
/// successful checkpoint allows a member to remain done without processing.
pub(crate) fn restore_replay_family_members(conn: &Connection, range_id: i64) -> Result<()> {
    ensure!(
        !conn.is_autocommit(),
        "replay family recovery requires a transaction"
    );
    let members = super::retry_admission::validate_replay_family_admission(conn, range_id)?;
    let now = chrono::Utc::now().timestamp();
    for member in members {
        if member.status == "pending" {
            // Waiting does not spend attempts. Preserve its schedule when
            // restoring the failed prerequisite, and retain its own evidence.
            let updated = conn.execute(
                "UPDATE extraction_tasks SET cursor_event_id = ?1,
                     replay_from_event_id = COALESCE(replay_from_event_id, ?2), updated_at_epoch = ?3
                 WHERE id = ?4 AND replay_range_id = ?5 AND status = 'pending'
                   AND lease_owner IS NULL AND lease_expires_epoch IS NULL",
                params![member.resume_cursor(), member.from, now, member.id, range_id],
            )?;
            super::loaders::ensure_task_updated(updated, member.id)?;
            continue;
        }
        conn.execute(
            "UPDATE extraction_tasks SET status = ?1, cursor_event_id = ?2,
                 replay_from_event_id = COALESCE(replay_from_event_id, ?3),
                 attempts = 0, next_retry_epoch = NULL, last_error = NULL,
                 failure_class = NULL, failed_at_epoch = NULL, archived_at_epoch = NULL,
                 updated_at_epoch = ?4
             WHERE id = ?5 AND replay_range_id = ?6 AND status IN ('done', 'failed')
               AND lease_owner IS NULL AND lease_expires_epoch IS NULL",
            params![
                if member.is_complete() {
                    "done"
                } else {
                    "pending"
                },
                member.resume_cursor(),
                member.from,
                now,
                member.id,
                range_id
            ],
        )?;
    }
    Ok(())
}
