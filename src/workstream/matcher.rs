use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use super::{
    identity::{
        has_continuity_alias, normalize_title, title_has_continuity, title_is_specific,
        MATCH_REASON_ALIAS_EXACT, MATCH_REASON_SESSION_LINK, MATCH_REASON_TITLE_CONTAINS,
        MATCH_REASON_TITLE_EXACT,
    },
    query::{map_workstream_row, SELECT_FIELDS, SELECT_FIELDS_ALIASED},
    WorkStream,
};

pub(super) struct WorkStreamMatch {
    pub workstream: WorkStream,
    pub reason: &'static str,
}

pub fn find_matching_workstream(
    conn: &Connection,
    project: &str,
    title: &str,
) -> Result<Option<WorkStream>> {
    Ok(find_title_workstream(conn, project, title)?.map(|matched| matched.workstream))
}

pub(super) fn find_workstream_for_upsert(
    conn: &Connection,
    project: &str,
    memory_session_id: &str,
    title: &str,
) -> Result<Option<WorkStreamMatch>> {
    if !memory_session_id_maps_to_unique_content_session(conn, project, memory_session_id)? {
        crate::log::warn(
            "workstream",
            &format!("session_link_collision project={project} session={memory_session_id}"),
        );
        return Ok(None);
    }
    if let Some(matched) = find_linked_workstream(conn, project, memory_session_id, title)? {
        return Ok(Some(matched));
    }
    if !title_is_specific(title) {
        return Ok(None);
    }
    match find_alias_workstream(conn, project, title)? {
        AliasMatch::Unique(matched) => Ok(Some(*matched)),
        AliasMatch::Ambiguous => Ok(None),
        AliasMatch::Missing => find_conservative_title_workstream(conn, project, title),
    }
}

/// Automatic mutation must never inherit the public lookup's loose substring
/// matching or its recency-based first result.
fn find_conservative_title_workstream(
    conn: &Connection,
    project: &str,
    title: &str,
) -> Result<Option<WorkStreamMatch>> {
    let mut stmt = conn.prepare(&format!(
        "{SELECT_FIELDS}
         WHERE status IN ('active', 'paused')
           AND merged_into_workstream_id IS NULL
           AND ((owner_scope = 'repo' AND owner_key = ?1)
                OR (owner_scope = 'repo' AND target_project = ?1)
                OR (owner_scope = 'workstream' AND target_project = ?1)
                OR (owner_scope IS NULL AND project = ?1))"
    ))?;
    let rows = stmt.query_map(params![project], map_workstream_row)?;
    let candidates = crate::db::query::collect_rows(rows)?;
    let normalized = normalize_title(title);
    let exact: Vec<_> = candidates
        .iter()
        .filter(|candidate| normalize_title(&candidate.title) == normalized)
        .collect();
    let (matches, reason) = if exact.is_empty() {
        (
            candidates
                .iter()
                .filter(|candidate| title_has_continuity(&candidate.title, title))
                .collect::<Vec<_>>(),
            MATCH_REASON_TITLE_CONTAINS,
        )
    } else {
        (exact, MATCH_REASON_TITLE_EXACT)
    };
    let [candidate] = matches.as_slice() else {
        if matches.len() > 1 {
            crate::log::warn(
                "workstream",
                &format!(
                    "title_match_ambiguous project={project} candidates={}",
                    matches.len()
                ),
            );
        }
        return Ok(None);
    };
    Ok(Some(WorkStreamMatch {
        workstream: (*candidate).clone(),
        reason,
    }))
}

fn find_title_workstream(
    conn: &Connection,
    project: &str,
    title: &str,
) -> Result<Option<WorkStreamMatch>> {
    let exact = conn
        .query_row(
            &format!(
                "{SELECT_FIELDS}
             WHERE title = ?2 AND status IN ('active', 'paused')
               AND merged_into_workstream_id IS NULL
               AND ((owner_scope = 'repo' AND owner_key = ?1)
                    OR (owner_scope = 'repo' AND target_project = ?1)
                    OR (owner_scope = 'workstream' AND target_project = ?1)
                    OR (owner_scope IS NULL AND project = ?1))"
            ),
            params![project, title],
            map_workstream_row,
        )
        .optional()?;
    if let Some(workstream) = exact {
        return Ok(Some(WorkStreamMatch {
            workstream,
            reason: MATCH_REASON_TITLE_EXACT,
        }));
    }

    let title_lower = title.to_lowercase();
    let mut stmt = conn.prepare(&format!(
        "{SELECT_FIELDS}
         WHERE status IN ('active', 'paused')
           AND merged_into_workstream_id IS NULL
           AND ((owner_scope = 'repo' AND owner_key = ?1)
                OR (owner_scope = 'repo' AND target_project = ?1)
                OR (owner_scope = 'workstream' AND target_project = ?1)
                OR (owner_scope IS NULL AND project = ?1))
         ORDER BY updated_at_epoch DESC"
    ))?;
    let rows = stmt.query_map(params![project], map_workstream_row)?;
    for row in rows {
        let workstream = row?;
        let candidate_title = workstream.title.to_lowercase();
        if candidate_title.contains(&title_lower) || title_lower.contains(&candidate_title) {
            return Ok(Some(WorkStreamMatch {
                workstream,
                reason: MATCH_REASON_TITLE_CONTAINS,
            }));
        }
    }

    Ok(None)
}

fn find_linked_workstream(
    conn: &Connection,
    project: &str,
    memory_session_id: &str,
    title: &str,
) -> Result<Option<WorkStreamMatch>> {
    let mut stmt = conn.prepare(&format!(
        "{}
         JOIN workstream_sessions wss ON wss.workstream_id = ws.id
         WHERE wss.memory_session_id = ?2
           AND ws.status IN ('active', 'paused')
           AND ws.merged_into_workstream_id IS NULL
           AND ((ws.owner_scope = 'repo' AND ws.owner_key = ?1)
                OR (ws.owner_scope = 'repo' AND ws.target_project = ?1)
                OR (ws.owner_scope = 'workstream' AND ws.target_project = ?1)
                OR (ws.owner_scope IS NULL AND ws.project = ?1))
         ORDER BY ws.updated_at_epoch DESC",
        SELECT_FIELDS_ALIASED.replacen("SELECT ", "SELECT DISTINCT ", 1)
    ))?;
    let rows = stmt.query_map(params![project, memory_session_id], map_workstream_row)?;
    let candidates = crate::db::query::collect_rows(rows)?;

    let mut continuity_candidates = Vec::new();
    for candidate in &candidates {
        if title_has_continuity(&candidate.title, title)
            || has_continuity_alias(conn, candidate.id, title)?
        {
            continuity_candidates.push(candidate);
        }
    }

    let [candidate] = continuity_candidates.as_slice() else {
        if continuity_candidates.len() > 1 {
            crate::log::warn(
                "workstream",
                &format!(
                    "session_link_ambiguous project={project} session={memory_session_id} candidates={}",
                    continuity_candidates.len()
                ),
            );
        } else if !candidates.is_empty() {
            crate::log::warn(
                "workstream",
                &format!(
                    "session_link_without_continuity project={project} session={memory_session_id} candidates={}",
                    candidates.len()
                ),
            );
        }
        return Ok(None);
    };

    Ok(Some(WorkStreamMatch {
        workstream: (*candidate).clone(),
        reason: MATCH_REASON_SESSION_LINK,
    }))
}

fn memory_session_id_maps_to_unique_content_session(
    conn: &Connection,
    project: &str,
    memory_session_id: &str,
) -> Result<bool> {
    if !crate::retrieval::temporal::sqlite_table_exists(conn, "sdk_sessions")? {
        return Ok(true);
    }

    let content_session_count: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT content_session_id)
         FROM sdk_sessions
         WHERE project = ?1
           AND memory_session_id = ?2",
        params![project, memory_session_id],
        |row| row.get(0),
    )?;
    Ok(content_session_count <= 1)
}

enum AliasMatch {
    Missing,
    Unique(Box<WorkStreamMatch>),
    Ambiguous,
}

fn find_alias_workstream(conn: &Connection, project: &str, title: &str) -> Result<AliasMatch> {
    let normalized_title = normalize_title(title);
    if normalized_title.is_empty() {
        return Ok(AliasMatch::Missing);
    }

    let mut stmt = conn.prepare(&format!(
        "{SELECT_FIELDS_ALIASED}
         JOIN workstream_aliases wa ON wa.workstream_id = ws.id
         WHERE wa.normalized_title = ?2
           AND ws.status IN ('active', 'paused')
           AND ws.merged_into_workstream_id IS NULL
           AND ((ws.owner_scope = 'repo' AND ws.owner_key = ?1)
                OR (ws.owner_scope = 'repo' AND ws.target_project = ?1)
                OR (ws.owner_scope = 'workstream' AND ws.target_project = ?1)
                OR (ws.owner_scope IS NULL AND ws.project = ?1))
         ORDER BY ws.updated_at_epoch DESC"
    ))?;
    let rows = stmt.query_map(params![project, normalized_title], map_workstream_row)?;
    let candidates = crate::db::query::collect_rows(rows)?;

    let [candidate] = candidates.as_slice() else {
        if candidates.len() > 1 {
            crate::log::warn(
                "workstream",
                &format!(
                    "alias_exact_ambiguous project={project} normalized_title={normalized_title} candidates={}",
                    candidates.len()
                ),
            );
        }
        return Ok(if candidates.is_empty() {
            AliasMatch::Missing
        } else {
            AliasMatch::Ambiguous
        });
    };

    Ok(AliasMatch::Unique(Box::new(WorkStreamMatch {
        workstream: candidate.clone(),
        reason: MATCH_REASON_ALIAS_EXACT,
    })))
}
