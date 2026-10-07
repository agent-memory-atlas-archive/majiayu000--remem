//! New evidence may reconsider untouched pending content, never a human decision.
use std::collections::BTreeSet;

use anyhow::{ensure, Context, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};

use crate::memory::activation::{activation_id_from_key, payload_sha256};
use crate::memory::poisoning::SourceTrustClass;

use super::{candidate_title, CandidateRoute, ParsedMemoryCandidate};

const SYSTEM_REASSESSMENT: &str = "candidate_evidence_superseded";

struct PriorCandidate {
    id: i64,
    version: i64,
    evidence_json: String,
    confidence: f64,
    trust: String,
    source_project: Option<String>,
    source_kind: String,
    status: String,
    untouched: bool,
    system_replaced: bool,
    review_reason: Option<String>,
    has_memory: bool,
    expires_at_epoch: Option<i64>,
}

#[derive(Default)]
pub(super) struct ReassessmentPlan {
    pending: Vec<PriorCandidate>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EvidenceReplacement {
    version: u8,
    prior_candidate_id: i64,
    replacement_candidate_id: i64,
    prior_evidence_sha256: String,
    replacement_evidence_sha256: String,
}

pub(super) fn plan(
    conn: &Connection,
    project_id: i64,
    candidate: &ParsedMemoryCandidate,
    event_ids: &[i64],
    route: &CandidateRoute,
    incoming_trust: SourceTrustClass,
    candidate_has_ttl: bool,
    now: i64,
) -> Result<Option<ReassessmentPlan>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.version, c.evidence_event_ids, c.confidence, c.source_trust_class,
                c.source_project, c.review_status,
                c.review_actor IS NULL AND c.reviewed_at_epoch IS NULL
                    AND c.review_action_source IS NULL AND c.review_batch_id IS NULL
                    AND c.review_reason IS NULL AND c.quarantine_pattern_id IS NULL
                    AND c.quarantine_pattern_version IS NULL
                    AND c.acknowledged_pattern_id IS NULL
                    AND c.acknowledged_pattern_version IS NULL AND c.acknowledged_at_epoch IS NULL,
                c.review_status = 'discarded'
                    AND c.review_action_source = 'candidate_evidence_superseded'
                    AND c.review_actor = 'automatic_worker',
                c.review_reason,
                EXISTS(SELECT 1 FROM memories m WHERE m.source_candidate_id = c.id),
                c.expires_at_epoch, c.source_kind
         FROM memory_candidates c
         WHERE c.project_id = ?1 AND c.scope = ?2 AND c.memory_type = ?3
           AND c.topic_key = ?4 AND c.text = ?5
         ORDER BY c.id",
    )?;
    let rows = stmt.query_map(
        params![
            project_id,
            candidate.scope,
            candidate.memory_type,
            candidate.topic_key,
            candidate.text
        ],
        |row| {
            Ok(PriorCandidate {
                id: row.get(0)?,
                version: row.get(1)?,
                evidence_json: row.get(2)?,
                confidence: row.get(3)?,
                trust: row.get(4)?,
                source_project: row.get(5)?,
                status: row.get(6)?,
                untouched: row.get::<_, Option<bool>>(7)?.unwrap_or(false),
                system_replaced: row.get::<_, Option<bool>>(8)?.unwrap_or(false),
                review_reason: row.get(9)?,
                has_memory: row.get(10)?,
                expires_at_epoch: row.get(11)?,
                source_kind: row.get(12)?,
            })
        },
    )?;
    let rows = crate::db::query::collect_rows(rows)?;
    if rows.is_empty() {
        return Ok(Some(ReassessmentPlan::default()));
    }
    // Preserve the existing operational-state renewal path. It requires new
    // trusted evidence below, and never treats a human terminal row as expired.
    let ttl_renewal = |row: &PriorCandidate| {
        candidate_has_ttl
            && row.status == "auto_promoted"
            && row.untouched
            && (matches!(row.source_kind.as_str(), "observation" | "summary")
                || (row.source_kind == "unattributed" && row.expires_at_epoch.is_none()))
            && row.expires_at_epoch.is_none_or(|expires| expires <= now)
    };
    // A human-touched identity continues to veto this content even if an
    // earlier system snapshot has been replaced or a TTL has elapsed.
    if rows.iter().any(|row| {
        !ttl_renewal(row)
            && (row.has_memory
                || !matches!(row.source_kind.as_str(), "observation" | "summary")
                || !(row.status == "pending_review" && row.untouched || row.system_replaced))
    }) || !incoming_trust.allows_auto_promote()
    {
        return Ok(None);
    }
    let incoming_ids = positive_event_set(event_ids)?;
    let mut seen_ids = BTreeSet::new();
    let mut activation_ids = Vec::new();
    for row in &rows {
        let Some(source_project) = row.source_project.as_deref() else {
            return Ok(None);
        };
        if !ttl_renewal(row) {
            activation_ids.push(activation_id_from_key(
                "candidate",
                &format!("{source_project}:{}", row.id),
            ));
        }
        let prior_ids: Vec<i64> = serde_json::from_str(&row.evidence_json)
            .with_context(|| format!("candidate {} has malformed reassessment evidence", row.id))?;
        // Pre-provenance operational rows may have no event identity. Their
        // fresh replacement must still provide a nonempty trusted event set.
        if !(ttl_renewal(row) && prior_ids.is_empty()) {
            seen_ids.extend(positive_event_set(&prior_ids)?);
        }
        if row.system_replaced {
            let Some(link) = row
                .review_reason
                .as_deref()
                .and_then(|raw| serde_json::from_str::<EvidenceReplacement>(raw).ok())
            else {
                return Ok(None);
            };
            let replacement = rows
                .iter()
                .find(|next| next.id == link.replacement_candidate_id);
            if link.version != 1
                || link.prior_candidate_id != row.id
                || link.replacement_candidate_id <= row.id
                || link.prior_evidence_sha256 != evidence_digest(&row.evidence_json)?
                || replacement.is_none_or(|next| {
                    evidence_digest(&next.evidence_json).ok().as_deref()
                        != Some(link.replacement_evidence_sha256.as_str())
                })
            {
                return Ok(None);
            }
        } else if SourceTrustClass::parse(&row.trust).is_none_or(|trust| incoming_trust < trust)
            || !row.confidence.is_finite()
            || candidate.confidence < row.confidence
        {
            return Ok(None);
        }
    }
    // Equality, reordered sets, subsets, and an A -> B -> A replay add no proof.
    if incoming_ids.is_subset(&seen_ids) {
        return Ok(None);
    }
    let activated: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM memory_activation_requests
                       WHERE activation_id IN (SELECT value FROM json_each(?1)))",
        [serde_json::to_string(&activation_ids)?],
        |row| row.get(0),
    )?;
    if activated || candidate_is_suppressed(conn, candidate, route)? {
        return Ok(None);
    }
    let pending = rows
        .into_iter()
        .filter(|row| row.status == "pending_review")
        .collect::<Vec<_>>();
    Ok(Some(ReassessmentPlan { pending }))
}

impl ReassessmentPlan {
    pub(super) fn finish(
        self,
        conn: &Connection,
        replacement_id: i64,
        candidate: &ParsedMemoryCandidate,
        route: &CandidateRoute,
        evidence_json: &str,
        now: i64,
    ) -> Result<()> {
        for prior in self.pending {
            let link = EvidenceReplacement {
                version: 1,
                prior_candidate_id: prior.id,
                replacement_candidate_id: replacement_id,
                prior_evidence_sha256: evidence_digest(&prior.evidence_json)?,
                replacement_evidence_sha256: evidence_digest(evidence_json)?,
            };
            let reason = serde_json::to_string(&link)?;
            let changed = conn.execute(
                "UPDATE memory_candidates SET review_status = 'discarded',
                     review_actor = 'automatic_worker', review_action_source = ?1,
                     review_reason = ?2, reviewed_at_epoch = ?3, updated_at_epoch = ?3
                 WHERE id = ?4 AND version = ?5 AND review_status = 'pending_review'
                   AND review_actor IS NULL AND review_action_source IS NULL
                   AND reviewed_at_epoch IS NULL",
                params![SYSTEM_REASSESSMENT, reason, now, prior.id, prior.version],
            )?;
            ensure!(
                changed == 1,
                "candidate evidence reassessment lost version/review ownership"
            );
            conn.execute(
                "INSERT INTO memory_operation_log
                 (operation, planner_version, actor, source, owner_scope, owner_key,
                  memory_type, input_topic_key, source_candidate_id, reason, created_at_epoch)
                 VALUES ('candidate_evidence_reassessment', 'gh1105-v1', 'automatic_worker',
                         'memory_candidate', ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    route.owner_scope,
                    route.owner_key,
                    candidate.memory_type,
                    candidate.topic_key,
                    replacement_id,
                    reason,
                    now
                ],
            )?;
        }
        Ok(())
    }
}

fn positive_event_set(ids: &[i64]) -> Result<BTreeSet<i64>> {
    ensure!(
        !ids.is_empty() && ids.iter().all(|id| *id > 0),
        "candidate reassessment requires nonempty positive event identities"
    );
    Ok(ids.iter().copied().collect())
}

fn evidence_digest(raw: &str) -> Result<String> {
    let ids: Vec<i64> = serde_json::from_str(raw)?;
    Ok(payload_sha256(&[&serde_json::to_string(
        &positive_event_set(&ids)?,
    )?]))
}

fn candidate_is_suppressed(
    conn: &Connection,
    candidate: &ParsedMemoryCandidate,
    route: &CandidateRoute,
) -> Result<bool> {
    let title = candidate_title(candidate);
    let entities = crate::retrieval::entity::extract_entities(&title, &candidate.text);
    let memory_policy = crate::memory::suppression::memory_policy_filter_sql("m");
    let sql = format!(
        "SELECT EXISTS(
             SELECT 1 FROM memory_suppressions ms WHERE ms.status = 'active' AND (
                 (ms.target_kind = 'topic_key' AND ms.target_value = ?1)
                 OR (ms.target_kind = 'pattern' AND ms.target_value IS NOT NULL
                     AND (instr(lower(?2), lower(ms.target_value)) > 0
                          OR instr(lower(?3), lower(ms.target_value)) > 0))
                 OR (ms.target_kind = 'entity' AND ms.target_value IS NOT NULL
                     AND EXISTS(SELECT 1 FROM json_each(?4)
                                WHERE lower(value) = lower(ms.target_value)))
             )
         ) OR EXISTS(
             SELECT 1 FROM memories m
             WHERE COALESCE(m.owner_scope, 'repo') = ?5
               AND COALESCE(m.owner_key, m.project) = ?6 AND m.memory_type = ?7
               AND (m.topic_key = ?1 OR m.content = ?3) AND NOT ({memory_policy})
         )"
    );
    conn.query_row(
        &sql,
        params![
            candidate.topic_key,
            title,
            candidate.text,
            serde_json::to_string(&entities)?,
            route.owner_scope,
            route.owner_key,
            candidate.memory_type
        ],
        |row| row.get(0),
    )
    .map_err(Into::into)
}
