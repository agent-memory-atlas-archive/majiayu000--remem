use anyhow::Result;
use rusqlite::{params, Connection};

use crate::db::{record_captured_event, CaptureEventInput, ExtractionTaskKind};
use crate::runtime_config::SummaryGateMode;
use crate::truth::{classify_memory, MemoryVisibilityClass, TruthQuery, ValidityState};

use super::super::summary::promote_summary_to_memory_candidates_with_gate_mode;
use super::promote::setup_conn;

const PROJECT: &str = "/tmp/remem-summary-current-fixture";
const CLAIM: &str = "The compiled schema cache stores validated reader fragments for reuse.";

/// Capture and promote through production APIs. In particular, the activation
/// receipt is never fabricated by a test-only INSERT or proof-seeding helper.
fn promoted_fixture(tool: &str, discovery: bool) -> Result<(Connection, i64, i64)> {
    let mut conn = setup_conn()?;
    let event = record_captured_event(
        &conn,
        &CaptureEventInput {
            host: "claude-code",
            session_id: "summary-current",
            project: PROJECT,
            cwd: Some(PROJECT),
            event_type: "tool_result",
            role: None,
            tool_name: Some(tool),
            content: CLAIM,
            task_kind: Some(ExtractionTaskKind::SessionRollup),
        },
    )?;
    let count = promote_summary_to_memory_candidates_with_gate_mode(
        &mut conn,
        "summary-current",
        PROJECT,
        None,
        (!discovery).then_some(CLAIM),
        discovery.then_some(CLAIM),
        None,
        SummaryGateMode::Enforce,
    )?;
    assert_eq!(count, 1);
    let memory_id = conn.query_row("SELECT id FROM memories", [], |row| row.get(0))?;
    let receipt_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memory_activation_requests
         WHERE result_memory_id = ?1 AND route_kind = 'candidate_promotion'
           AND actor_kind = 'automatic_worker' AND provenance_kind = 'candidate'
           AND poisoning_verdict = 'upstream_validated'",
        [memory_id],
        |row| row.get(0),
    )?;
    assert_eq!(receipt_count, 1);
    Ok((conn, memory_id, event.event_row_id))
}

fn assert_current(conn: &Connection, memory_id: i64) -> Result<()> {
    assert_eq!(
        classify_memory(conn, memory_id, chrono::Utc::now().timestamp())?.classification,
        MemoryVisibilityClass::Current
    );
    Ok(())
}

#[test]
fn governed_summary_confidence_reaches_current_truth_and_session_start() -> Result<()> {
    let data_dir =
        crate::db::test_support::ScopedTestDataDir::new_offline("summary-current-context");
    std::fs::create_dir_all(&data_dir.path)?;
    for (tool, trust, discovery) in [
        ("Bash", "local_tool_output", false),
        ("Grep", "repo_file", false),
        ("Grep", "repo_file", true),
    ] {
        let (conn, memory_id, _) = promoted_fixture(tool, discovery)?;
        let (confidence, source_trust, review): (f64, String, String) = conn.query_row(
            "SELECT m.confidence, m.source_trust_class, c.review_status
             FROM memories m JOIN memory_candidates c ON c.id = m.source_candidate_id
             WHERE m.id = ?1",
            [memory_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(
            confidence, 0.74,
            "summary calibration must remain unchanged"
        );
        assert_eq!(source_trust, trust);
        assert_eq!(review, "auto_promoted");
        assert_current(&conn, memory_id)?;

        let projection = crate::truth::project_current_truth_for_context(
            &conn,
            &TruthQuery {
                project: PROJECT.to_string(),
                ..Default::default()
            },
            &[memory_id],
        )?;
        let canonical_ref = format!("memory:{memory_id}");
        assert!(
            projection.truths.iter().any(|truth| {
                truth.validity == ValidityState::Current
                    && truth
                        .claim
                        .as_ref()
                        .is_some_and(|claim| claim.canonical_ref == canonical_ref)
            }),
            "{projection:?}"
        );
        let candidates = crate::context::load_session_start_candidates_with_limits(
            &conn,
            PROJECT,
            data_dir.path.to_str().expect("utf8 temp path"),
            None,
            &crate::context::ContextLimits::default(),
        )?;
        assert!(
            candidates.candidates.iter().any(|item| {
                item.stable_key == canonical_ref
                    && item.trust != crate::context_bundle::TrustClass::Quarantined
            }),
            "{candidates:?}"
        );
    }
    Ok(())
}

#[test]
fn summary_proof_is_bound_to_current_payload_candidate_and_captured_source() -> Result<()> {
    let _data_dir =
        crate::db::test_support::ScopedTestDataDir::new_offline("summary-proof-binding");
    for mutation in [
        "UPDATE memories SET title = 'Changed title' WHERE id = ?1",
        "UPDATE memories SET content = 'A different compiled reader claim.' WHERE id = ?1",
        "UPDATE memories SET files = '[\"changed.rs\"]' WHERE id = ?1",
        "UPDATE memory_candidates SET confidence = 0.73 WHERE id = (SELECT source_candidate_id FROM memories WHERE id = ?1)",
        "UPDATE memory_candidates SET risk_class = 'high' WHERE id = (SELECT source_candidate_id FROM memories WHERE id = ?1)",
        "UPDATE memory_candidates SET source_kind = 'observation' WHERE id = (SELECT source_candidate_id FROM memories WHERE id = ?1)",
        "UPDATE memory_candidates SET review_status = 'approved' WHERE id = (SELECT source_candidate_id FROM memories WHERE id = ?1)",
        "UPDATE memory_candidates SET review_actor = 'reviewer' WHERE id = (SELECT source_candidate_id FROM memories WHERE id = ?1)",
        "UPDATE captured_events SET tool_name = 'WebFetch' WHERE id IN (SELECT value FROM memories, json_each(memories.evidence_event_ids) WHERE memories.id = ?1)",
        "UPDATE captured_events SET content_blob_id = NULL, content_text = 'Unrelated local output.' WHERE id IN (SELECT value FROM memories, json_each(memories.evidence_event_ids) WHERE memories.id = ?1)",
    ] {
        let (conn, memory_id, _) = promoted_fixture("Bash", false)?;
        assert_current(&conn, memory_id)?;
        conn.execute(mutation, [memory_id])?;
        assert!(!classify_memory(&conn, memory_id, chrono::Utc::now().timestamp())?
            .current_context_eligible, "stale proof survived {mutation}");
    }
    Ok(())
}

#[test]
fn summary_receipt_binds_original_score_without_recalibrating_it() -> Result<()> {
    let _data_dir =
        crate::db::test_support::ScopedTestDataDir::new_offline("summary-score-binding");
    let (conn, memory_id, _) = promoted_fixture("Grep", false)?;
    assert_current(&conn, memory_id)?;
    // The memory result digest excludes confidence. The request fingerprint
    // must also match, even when both mutable rows agree on a changed score.
    conn.execute(
        "UPDATE memories SET confidence = 0.73 WHERE id = ?1",
        [memory_id],
    )?;
    conn.execute(
        "UPDATE memory_candidates SET confidence = 0.73
         WHERE id = (SELECT source_candidate_id FROM memories WHERE id = ?1)",
        [memory_id],
    )?;
    assert!(
        !classify_memory(&conn, memory_id, chrono::Utc::now().timestamp())?
            .current_context_eligible
    );
    Ok(())
}

#[test]
fn copied_candidate_provenance_without_its_activation_receipt_is_not_current() -> Result<()> {
    let _data_dir =
        crate::db::test_support::ScopedTestDataDir::new_offline("summary-missing-receipt");
    let (conn, original_id, _) = promoted_fixture("Bash", false)?;
    conn.execute(
        "INSERT INTO memories
         (session_id, project, title, content, memory_type, topic_key, files,
          scope, status, owner_scope, owner_key, source_project, target_project,
          source_candidate_id, evidence_event_ids, source_trust_class, confidence,
          valid_from_epoch, state_key_id, created_at_epoch, updated_at_epoch)
         SELECT session_id, project, title, content, memory_type, topic_key, files,
                scope, status, owner_scope, owner_key, source_project, target_project,
                source_candidate_id, evidence_event_ids, source_trust_class, confidence,
                valid_from_epoch, state_key_id, created_at_epoch, updated_at_epoch
         FROM memories WHERE id = ?1",
        [original_id],
    )?;
    let copied_id = conn.last_insert_rowid();
    let visibility = crate::truth::classify_memories(
        &conn,
        &[original_id, copied_id, original_id],
        chrono::Utc::now().timestamp(),
    )?;
    assert!(visibility[&original_id].current_context_eligible);
    assert!(!visibility[&copied_id].current_context_eligible);
    Ok(())
}

#[test]
fn summary_confidence_proof_preserves_lifecycle_and_state_identity_gates() -> Result<()> {
    let _data_dir =
        crate::db::test_support::ScopedTestDataDir::new_offline("summary-lifecycle-gates");
    for mutation in [
        "UPDATE memories SET status = 'quarantined' WHERE id = ?1",
        "UPDATE memories SET status = 'stale' WHERE id = ?1",
        "UPDATE memories SET expires_at_epoch = 1 WHERE id = ?1",
        "UPDATE memories SET valid_to_epoch = 1 WHERE id = ?1",
        "UPDATE memories SET valid_from_epoch = 4102444800 WHERE id = ?1",
        "UPDATE memories SET state_key_id = NULL WHERE id = ?1",
    ] {
        let (conn, memory_id, _) = promoted_fixture("Bash", false)?;
        assert_current(&conn, memory_id)?;
        conn.execute(mutation, [memory_id])?;
        assert!(
            !classify_memory(&conn, memory_id, chrono::Utc::now().timestamp())?
                .current_context_eligible,
            "lifecycle exclusion bypassed: {mutation}"
        );
    }
    Ok(())
}

#[test]
fn suppressed_summary_stays_out_of_current_truth_and_session_start() -> Result<()> {
    use crate::memory::suppression::{create_suppression, SuppressRequest, SuppressionTarget};
    let data_dir = crate::db::test_support::ScopedTestDataDir::new_offline("suppressed-summary");
    std::fs::create_dir_all(&data_dir.path)?;
    let (conn, memory_id, _) = promoted_fixture("Bash", false)?;
    create_suppression(
        &conn,
        &SuppressRequest {
            target: SuppressionTarget {
                kind: "memory".to_string(),
                id: Some(memory_id),
                value: None,
            },
            reason: Some("fixture user suppression"),
            actor: Some("test-reviewer"),
        },
    )?;
    let candidates = crate::context::load_session_start_candidates_with_limits(
        &conn,
        PROJECT,
        data_dir.path.to_str().expect("utf8 temp path"),
        None,
        &crate::context::ContextLimits::default(),
    )?;
    assert!(!candidates
        .candidates
        .iter()
        .any(|item| item.stable_key == format!("memory:{memory_id}")));
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memory_activation_requests WHERE result_memory_id = ?1",
        params![memory_id],
        |row| row.get(0),
    )?;
    assert_eq!(
        count, 1,
        "read admission must not erase the activation receipt"
    );
    Ok(())
}
