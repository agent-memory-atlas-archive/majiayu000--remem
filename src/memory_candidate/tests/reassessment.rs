use anyhow::Result;
use rusqlite::{params, Connection};

use super::{insert_source_observation, process_with_generator, setup_conn, setup_task};
use crate::db::{self, record_captured_event, CaptureEventInput, ExtractionTaskKind};
use crate::memory::suppression::{create_suppression, SuppressRequest, SuppressionTarget};
use crate::memory_candidate::review::{self, ReviewMeta};
use crate::memory_candidate::MemoryCandidateResult;

const TEXT: &str = "The remem worker loop in src/worker.rs processes extraction tasks after observation extraction.";

struct Fixture {
    conn: Connection,
    task: db::ExtractionTask,
    text: String,
    first_candidate_id: i64,
    _data_dir: crate::db::test_support::ScopedTestDataDir,
}

impl Fixture {
    async fn new(text: &str, confidence: f64) -> Result<Self> {
        let data_dir =
            crate::db::test_support::ScopedTestDataDir::new_offline("candidate-reassessment");
        let mut conn = setup_conn();
        let task = setup_task(&mut conn, "candidate-reassessment")?;
        insert_source_observation(&conn, &task, text)?;
        generate(&mut conn, &task, text, confidence).await?;
        let first_candidate_id =
            conn.query_row("SELECT id FROM memory_candidates", [], |row| row.get(0))?;
        Ok(Self {
            conn,
            task,
            text: text.to_string(),
            first_candidate_id,
            _data_dir: data_dir,
        })
    }

    fn fresh_evidence(&mut self, tool: &str) -> Result<i64> {
        let event = record_captured_event(
            &self.conn,
            &CaptureEventInput {
                host: &self.task.host,
                session_id: self.task.session_id.as_deref().expect("fixture session"),
                project: &self.task.project,
                cwd: None,
                event_type: "tool_result",
                role: None,
                tool_name: Some(tool),
                content: &self.text,
                task_kind: Some(ExtractionTaskKind::MemoryCandidate),
            },
        )?;
        self.task.cursor_event_id = self.task.high_watermark_event_id;
        self.task.high_watermark_event_id = Some(event.event_row_id);
        insert_source_observation(&self.conn, &self.task, &self.text)?;
        Ok(event.event_row_id)
    }

    async fn generate(&mut self, confidence: f64) -> Result<MemoryCandidateResult> {
        generate(&mut self.conn, &self.task, &self.text, confidence).await
    }

    fn candidate_count(&self) -> Result<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM memory_candidates", [], |row| {
                row.get(0)
            })?)
    }

    fn snapshot(&self) -> Result<(String, f64, String, i64, i64)> {
        Ok(self.conn.query_row(
            "SELECT evidence_event_ids, confidence, review_status, created_at_epoch, version
             FROM memory_candidates WHERE id = ?1",
            [self.first_candidate_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?)
    }
}

async fn generate(
    conn: &mut Connection,
    task: &db::ExtractionTask,
    text: &str,
    confidence: f64,
) -> Result<MemoryCandidateResult> {
    let xml = format!(
        "<memory_candidate><scope>project</scope><type>decision</type>\
        <topic_key>decision-worker-loop</topic_key><risk_class>low</risk_class>\
        <confidence>{confidence}</confidence><text>{text}</text></memory_candidate>"
    );
    process_with_generator(conn, task, |_prompt| async move { Ok(xml) }).await
}

fn assert_no_candidate(result: MemoryCandidateResult) {
    assert!(
        matches!(
            result,
            MemoryCandidateResult::Written {
                candidates: 0,
                promoted: 0,
                pending_review: 0,
                ..
            }
        ),
        "{result:?}"
    );
}

#[tokio::test]
async fn new_trusted_evidence_reassesses_pending_without_rewriting_original_snapshot() -> Result<()>
{
    let mut fixture = Fixture::new(TEXT, 0.65).await?;
    let before = fixture.snapshot()?;
    let next_event = fixture.fresh_evidence("Grep")?;
    let result = fixture.generate(0.95).await?;
    assert!(
        matches!(
            result,
            MemoryCandidateResult::Written {
                candidates: 1,
                promoted: 1,
                pending_review: 0,
                ..
            }
        ),
        "{result:?}"
    );
    assert_eq!(fixture.candidate_count()?, 2);
    let after = fixture.snapshot()?;
    assert_eq!(after.0, before.0);
    assert_eq!(after.1, before.1);
    assert_eq!(after.2, "discarded");
    assert_eq!(after.3, before.3);
    assert!(after.4 > before.4);
    let (replacement_id, memory_id, evidence): (i64, i64, String) = fixture.conn.query_row(
        "SELECT c.id, m.id, c.evidence_event_ids FROM memory_candidates c
         JOIN memories m ON m.source_candidate_id = c.id
         WHERE c.review_status = 'auto_promoted'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(
        serde_json::from_str::<Vec<i64>>(&evidence)?,
        vec![next_event]
    );
    let (actor, source, reason): (String, String, String) = fixture.conn.query_row(
        "SELECT review_actor, review_action_source, review_reason
         FROM memory_candidates WHERE id = ?1",
        [fixture.first_candidate_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(actor, "automatic_worker");
    assert_eq!(source, "candidate_evidence_superseded");
    let link: serde_json::Value = serde_json::from_str(&reason)?;
    assert_eq!(link["prior_candidate_id"], fixture.first_candidate_id);
    assert_eq!(link["replacement_candidate_id"], replacement_id);
    assert_ne!(
        link["prior_evidence_sha256"],
        link["replacement_evidence_sha256"]
    );
    let receipt_count: i64 = fixture.conn.query_row(
        "SELECT COUNT(*) FROM memory_activation_requests WHERE result_memory_id = ?1
           AND actor_kind = 'automatic_worker'",
        [memory_id],
        |row| row.get(0),
    )?;
    assert_eq!(receipt_count, 1);
    assert!(
        crate::truth::classify_memory(&fixture.conn, memory_id, chrono::Utc::now().timestamp())?
            .current_context_eligible
    );
    Ok(())
}

#[tokio::test]
async fn exact_evidence_does_not_replace_a_pending_snapshot_or_upgrade_its_score() -> Result<()> {
    let mut fixture = Fixture::new(TEXT, 0.65).await?;
    let before = fixture.snapshot()?;
    assert_no_candidate(fixture.generate(0.95).await?);
    assert_eq!(fixture.snapshot()?, before);
    assert_eq!(fixture.candidate_count()?, 1);
    Ok(())
}

#[tokio::test]
async fn reordered_and_previously_seen_batches_are_idempotent() -> Result<()> {
    let mut fixture = Fixture::new(TEXT, 0.65).await?;
    let original_task = fixture.task.clone();
    let first_event = original_task.high_watermark_event_id.expect("first event");
    let next_event = fixture.fresh_evidence("Bash")?;
    let result = fixture.generate(0.65).await?;
    assert!(matches!(
        result,
        MemoryCandidateResult::Written {
            candidates: 1,
            pending_review: 1,
            ..
        }
    ));
    assert_eq!(fixture.candidate_count()?, 2);
    assert_no_candidate(generate(&mut fixture.conn, &original_task, TEXT, 0.95).await?);
    fixture.task.cursor_event_id = None;
    fixture.conn.execute(
        "UPDATE observations SET evidence_event_ids = ?1",
        [serde_json::to_string(&vec![next_event, first_event])?],
    )?;
    assert_no_candidate(fixture.generate(0.95).await?);
    fixture.conn.execute(
        "UPDATE observations SET evidence_event_ids = ?1",
        [serde_json::to_string(&vec![first_event, next_event])?],
    )?;
    assert_no_candidate(fixture.generate(0.95).await?);
    assert_eq!(fixture.candidate_count()?, 2);
    Ok(())
}

#[tokio::test]
async fn weaker_confidence_or_external_evidence_cannot_replace_pending() -> Result<()> {
    for (tool, confidence) in [("WebFetch", 0.95), ("Bash", 0.60)] {
        let mut fixture = Fixture::new(TEXT, 0.65).await?;
        let before = fixture.snapshot()?;
        fixture.fresh_evidence(tool)?;
        assert_no_candidate(fixture.generate(confidence).await?);
        assert_eq!(fixture.snapshot()?, before);
        assert_eq!(fixture.candidate_count()?, 1);
    }
    Ok(())
}

#[tokio::test]
async fn human_rejection_and_approval_remain_content_identity_boundaries() -> Result<()> {
    for approve in [false, true] {
        let mut fixture = Fixture::new(TEXT, 0.65).await?;
        let meta = ReviewMeta::single("fixture-human");
        if approve {
            assert!(review::approve_candidate_with_meta(
                &mut fixture.conn,
                fixture.first_candidate_id,
                &meta,
            )?
            .is_some());
        } else {
            assert!(review::discard_candidate_with_meta(
                &fixture.conn,
                fixture.first_candidate_id,
                &meta,
            )?);
        }
        let before = fixture.snapshot()?;
        fixture.fresh_evidence("Grep")?;
        assert_no_candidate(fixture.generate(0.95).await?);
        assert_eq!(fixture.snapshot()?, before);
        assert_eq!(fixture.candidate_count()?, 1);
    }
    Ok(())
}

#[tokio::test]
async fn quarantine_review_metadata_and_external_identity_veto_reassessment() -> Result<()> {
    for mutation in [
        "UPDATE memory_candidates SET review_status = 'quarantined', quarantine_pattern_id = 'fixture', quarantine_pattern_version = 1",
        "UPDATE memory_candidates SET review_actor = 'fixture-human', review_action_source = 'single'",
        "UPDATE memory_candidates SET acknowledged_pattern_id = 'fixture', acknowledged_pattern_version = 1, acknowledged_at_epoch = 1",
        "UPDATE memory_candidates SET source_kind = 'native_import'",
    ] {
        let mut fixture = Fixture::new(TEXT, 0.65).await?;
        fixture.conn.execute(mutation, [])?;
        let before = fixture.snapshot()?;
        fixture.fresh_evidence("Grep")?;
        assert_no_candidate(fixture.generate(0.95).await?);
        assert_eq!(fixture.snapshot()?, before);
        assert_eq!(fixture.candidate_count()?, 1);
    }
    Ok(())
}

#[tokio::test]
async fn a_later_human_rejection_is_not_bypassed_by_a_system_replacement_link() -> Result<()> {
    let mut fixture = Fixture::new(TEXT, 0.65).await?;
    fixture.fresh_evidence("Bash")?;
    fixture.generate(0.65).await?;
    let replacement_id: i64 = fixture.conn.query_row(
        "SELECT id FROM memory_candidates WHERE review_status = 'pending_review'",
        [],
        |row| row.get(0),
    )?;
    assert!(review::discard_candidate_with_meta(
        &fixture.conn,
        replacement_id,
        &ReviewMeta::single("fixture-human"),
    )?);
    fixture.fresh_evidence("Grep")?;
    assert_no_candidate(fixture.generate(0.95).await?);
    assert_eq!(fixture.candidate_count()?, 2);
    Ok(())
}

#[tokio::test]
async fn content_suppression_vetoes_new_evidence_reassessment() -> Result<()> {
    for (kind, value) in [
        ("topic_key", "decision-worker-loop"),
        ("pattern", "worker loop"),
        ("entity", "remem"),
    ] {
        let mut fixture = Fixture::new(TEXT, 0.65).await?;
        let before = fixture.snapshot()?;
        create_suppression(
            &fixture.conn,
            &SuppressRequest {
                target: SuppressionTarget {
                    kind: kind.to_string(),
                    id: None,
                    value: Some(value.to_string()),
                },
                reason: Some("fixture suppression"),
                actor: Some("fixture-human"),
            },
        )?;
        fixture.fresh_evidence("Grep")?;
        assert_no_candidate(fixture.generate(0.95).await?);
        assert_eq!(fixture.snapshot()?, before);
    }
    Ok(())
}

#[tokio::test]
async fn reassessment_and_activation_failures_roll_back_the_original_candidate() -> Result<()> {
    for fail_at in ["memory_activation_requests", "memory_operation_log"] {
        let mut fixture = Fixture::new(TEXT, 0.65).await?;
        let before = fixture.snapshot()?;
        fixture.fresh_evidence("Grep")?;
        fixture.conn.execute_batch(&format!(
            "CREATE TRIGGER fail_reassessment_fixture BEFORE INSERT ON {fail_at}
             BEGIN SELECT RAISE(ABORT, 'fixture reassessment failure'); END;"
        ))?;
        assert!(fixture.generate(0.95).await.is_err());
        assert_eq!(fixture.snapshot()?, before);
        assert_eq!(fixture.candidate_count()?, 1);
        let memory_count: i64 =
            fixture
                .conn
                .query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
        assert_eq!(memory_count, 0);
    }
    Ok(())
}

#[tokio::test]
async fn operational_ttl_renewal_requires_new_evidence_and_respects_suppression() -> Result<()> {
    for suppress in [false, true] {
        let text = "Local dev server is currently running at localhost:3000 for remem.";
        let mut fixture = Fixture::new(text, 0.95).await?;
        assert_eq!(fixture.snapshot()?.2, "auto_promoted");
        let memory_id: i64 = fixture
            .conn
            .query_row("SELECT id FROM memories", [], |row| row.get(0))?;
        fixture
            .conn
            .execute("UPDATE memory_candidates SET expires_at_epoch = 1", [])?;
        fixture
            .conn
            .execute("UPDATE memories SET expires_at_epoch = 1", [])?;
        assert_no_candidate(fixture.generate(0.95).await?);
        if suppress {
            create_suppression(
                &fixture.conn,
                &SuppressRequest {
                    target: SuppressionTarget {
                        kind: "memory".to_string(),
                        id: Some(memory_id),
                        value: None,
                    },
                    reason: Some("fixture suppressed expired state"),
                    actor: Some("fixture-human"),
                },
            )?;
        }
        fixture.fresh_evidence("Grep")?;
        let result = fixture.generate(0.95).await?;
        if suppress {
            assert_no_candidate(result);
            assert_eq!(fixture.candidate_count()?, 1);
        } else {
            assert!(
                matches!(
                    result,
                    MemoryCandidateResult::Written {
                        candidates: 1,
                        promoted: 1,
                        ..
                    }
                ),
                "{result:?}"
            );
            let latest_expiry: i64 = fixture.conn.query_row(
                "SELECT expires_at_epoch FROM memories WHERE id <> ?1 AND status = 'active'",
                params![memory_id],
                |row| row.get(0),
            )?;
            assert!(latest_expiry > chrono::Utc::now().timestamp());
        }
    }
    Ok(())
}
