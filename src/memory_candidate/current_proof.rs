//! Read-only, batch-scoped proof for summary-path confidence in current context.
use std::collections::BTreeSet;

use anyhow::Result;
use rusqlite::Connection;

use crate::memory::activation::{
    request_fingerprint_matches, ActivationActorKind, ExpectedActiveMemory,
};
use crate::memory::poisoning::{scan_instruction_pattern, SourceTrustClass};

use super::auto_promote::{summary_auto_promote_verdict, SummaryAutoPromoteVerdict};
use super::{CandidateRoute, ParsedMemoryCandidate, SummaryEvidenceResolver};

const MAX_SUMMARY_PROOF_EVENTS: usize = 256;

struct SummaryProof {
    memory_id: i64,
    candidate_id: i64,
    candidate: ParsedMemoryCandidate,
    evidence_json: String,
    source_project: String,
    route: CandidateRoute,
    memory_project: String,
    memory: ExpectedActiveMemory,
    trust: String,
    activation_id: String,
    request_sha256: String,
    result_sha256: String,
    result_trust: String,
    receipt_superseded_ids: String,
    operation_superseded_ids: String,
}

/// The caller supplies only rows that failed the ordinary confidence check.
/// Receipt/candidate reads and captured-event reads each use one statement per
/// caller batch. Evidence is cached only here, never across context requests.
pub(crate) fn summary_current_confidence_proofs(
    conn: &Connection,
    memory_ids: &[i64],
) -> Result<BTreeSet<i64>> {
    if memory_ids.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut stmt = conn.prepare(
        "WITH latest_receipts AS (
             SELECT result_memory_id, MAX(rowid) AS receipt_rowid
             FROM memory_activation_requests
             WHERE result_memory_id IN (SELECT value FROM json_each(?1))
             GROUP BY result_memory_id
         ), latest_operations AS (
             SELECT source_candidate_id, result_memory_id, MAX(id) AS operation_id
             FROM memory_operation_log
             WHERE source = 'memory_candidate'
               AND result_memory_id IN (SELECT value FROM json_each(?1))
             GROUP BY source_candidate_id, result_memory_id
         )
         SELECT m.id, c.id, c.scope, c.memory_type, c.topic_key, c.text,
                c.confidence, c.risk_class, c.evidence_event_ids,
                c.source_project, c.target_project, c.owner_scope, c.owner_key,
                c.topic_domain, c.routing_confidence, c.routing_reason, c.context_class,
                m.project, m.title, m.content, m.memory_type, m.topic_key, m.files,
                m.evidence_event_ids, m.source_candidate_id, m.source_trust_class,
                r.activation_id, r.request_sha256, r.result_sha256,
                r.result_source_trust_class, r.superseded_ids_json, operation.superseded_ids
         FROM memories m
         JOIN memory_candidates c ON c.id = m.source_candidate_id
         JOIN latest_receipts lr ON lr.result_memory_id = m.id
         JOIN memory_activation_requests r ON r.rowid = lr.receipt_rowid
         JOIN latest_operations lo ON lo.result_memory_id = m.id
                                  AND lo.source_candidate_id = c.id
         JOIN memory_operation_log operation ON operation.id = lo.operation_id
         WHERE m.id IN (SELECT value FROM json_each(?1))
           AND c.source_kind = 'summary' AND c.review_status = 'auto_promoted'
           AND c.scope = 'project' AND c.owner_scope = 'repo'
           AND c.source_project IS NOT NULL AND c.target_project IS NOT NULL
           AND c.owner_key IS NOT NULL AND c.routing_confidence IS NOT NULL
           AND c.routing_reason IS NOT NULL AND c.context_class IS NOT NULL
           AND c.quarantine_pattern_id IS NULL AND c.quarantine_pattern_version IS NULL
           AND c.acknowledged_pattern_id IS NULL AND c.acknowledged_pattern_version IS NULL
           AND c.acknowledged_at_epoch IS NULL AND c.review_actor IS NULL
           AND c.reviewed_at_epoch IS NULL AND c.review_action_source IS NULL
           AND c.review_batch_id IS NULL AND c.review_reason IS NULL
           AND m.status = 'active' AND m.scope = 'project' AND m.branch IS NULL
           AND m.owner_scope = c.owner_scope AND m.owner_key = c.owner_key
           AND m.target_project IS c.target_project AND m.source_project IS c.source_project
           AND m.confidence = c.confidence AND m.source_trust_class = c.source_trust_class
           AND m.source_trust_class IN ('local_tool_output', 'repo_file')
           AND r.route_kind = 'candidate_promotion' AND r.actor_kind = 'automatic_worker'
           AND r.source_operation = 'candidate_promotion' AND r.provenance_kind = 'candidate'
           AND r.poisoning_verdict = 'upstream_validated'
         ORDER BY m.id",
    )?;
    let rows = stmt.query_map([serde_json::to_string(memory_ids)?], |row| {
        Ok(SummaryProof {
            memory_id: row.get(0)?,
            candidate_id: row.get(1)?,
            candidate: ParsedMemoryCandidate {
                scope: row.get(2)?,
                memory_type: row.get(3)?,
                topic_key: row.get(4)?,
                text: row.get(5)?,
                confidence: row.get(6)?,
                risk_class: row.get(7)?,
                title_override: None,
                outcome: None,
                facts: Vec::new(),
            },
            evidence_json: row.get(8)?,
            source_project: row.get(9)?,
            route: CandidateRoute {
                target_project: row.get(10)?,
                owner_scope: row.get(11)?,
                owner_key: row.get(12)?,
                topic_domain: row.get(13)?,
                routing_confidence: row.get(14)?,
                routing_reason: row.get(15)?,
                context_class: row.get(16)?,
            },
            memory_project: row.get(17)?,
            memory: ExpectedActiveMemory {
                title: row.get(18)?,
                content: row.get(19)?,
                memory_type: row.get(20)?,
                topic_key: row.get(21)?,
                files: row.get(22)?,
                evidence_event_ids: row.get(23)?,
                source_candidate_id: row.get(24)?,
            },
            trust: row.get(25)?,
            activation_id: row.get(26)?,
            request_sha256: row.get(27)?,
            result_sha256: row.get(28)?,
            result_trust: row.get(29)?,
            receipt_superseded_ids: row.get(30)?,
            operation_superseded_ids: row.get(31)?,
        })
    })?;
    let rows = crate::db::query::collect_rows(rows)?;
    let proofs = rows
        .into_iter()
        .filter_map(|row| {
            let ids = positive_ids(&row.evidence_json, false)?;
            (ids.len() <= MAX_SUMMARY_PROOF_EVENTS).then_some((row, ids))
        })
        .collect::<Vec<_>>();
    let source_ids = proofs
        .iter()
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let sources = SummaryEvidenceResolver::load(conn, &source_ids, super::SOURCE_KIND_SUMMARY)?;
    let mut verified = BTreeSet::new();
    for (row, ids) in proofs {
        let Some((source_texts, source_trust)) = sources.proof_sources(&row.candidate, &ids) else {
            continue;
        };
        if SourceTrustClass::parse(&row.trust) != Some(source_trust)
            || scan_instruction_pattern(&row.candidate.text).is_some()
            || summary_auto_promote_verdict(
                &row.candidate,
                &row.route,
                &row.evidence_json,
                &source_texts,
                source_trust,
            ) != SummaryAutoPromoteVerdict::WouldPromote
        {
            continue;
        }
        let Some(superseded_ids) = positive_ids(&row.operation_superseded_ids, true) else {
            continue;
        };
        let Some(receipt_ids) = positive_ids(&row.receipt_superseded_ids, true) else {
            continue;
        };
        if superseded_ids.iter().collect::<BTreeSet<_>>()
            != receipt_ids.iter().collect::<BTreeSet<_>>()
        {
            continue;
        }
        let request = super::apply::activation_request::build(
            &row.source_project,
            &row.memory_project,
            "project",
            row.candidate_id,
            &row.candidate,
            &row.evidence_json,
            &row.route,
            source_trust,
            ActivationActorKind::AutomaticWorker,
            &superseded_ids,
            None,
            None,
        )?;
        if request.activation_id == row.activation_id
            && request.expected_memory == row.memory
            && row.memory.sha256() == row.result_sha256
            && request_fingerprint_matches(&request, &row.request_sha256, &row.result_trust)?
        {
            verified.insert(row.memory_id);
        }
    }
    Ok(verified)
}

fn positive_ids(raw: &str, allow_empty: bool) -> Option<Vec<i64>> {
    let ids: Vec<i64> = serde_json::from_str(raw).ok()?;
    (ids.iter().all(|id| *id > 0)
        && (allow_empty || !ids.is_empty())
        && ids.iter().collect::<BTreeSet<_>>().len() == ids.len())
    .then_some(ids)
}
