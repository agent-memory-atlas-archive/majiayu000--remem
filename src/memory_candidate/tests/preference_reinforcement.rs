use anyhow::{Context, Result};

use crate::memory_candidate::review::approve_candidate;

use super::{
    insert_source_observation, process_with_generator, setup_conn, setup_task,
    MemoryCandidateResult,
};

const PREFERENCE: &str = "Use bun, not npm, for package installation commands in this project.";

#[tokio::test]
async fn approved_duplicate_preferences_increment_canonical_reinforcement() -> Result<()> {
    let mut conn = setup_conn();
    let mut evidence_ids = std::collections::BTreeSet::new();
    let mut canonical_memory_id = None;

    for round in 1..=3 {
        let task = setup_task(&mut conn, &format!("sess-preference-reinforce-{round}"))?;
        let event_id = task
            .high_watermark_event_id
            .context("preference task watermark")?;
        assert!(
            evidence_ids.insert(event_id),
            "each round needs new evidence"
        );
        insert_source_observation(&conn, &task, PREFERENCE)?;
        let candidate_xml = format!(
            "<memory_candidate>\
                    <scope>project</scope>\
                    <type>preference</type>\
                    <topic_key>package-manager-choice</topic_key>\
                    <risk_class>low</risk_class>\
                    <confidence>0.95</confidence>\
                    <text>{PREFERENCE}</text>\
                 </memory_candidate>"
        );
        let result = process_with_generator(&mut conn, &task, |_prompt| async {
            Ok(candidate_xml.clone())
        })
        .await?;

        assert_eq!(
            result,
            MemoryCandidateResult::Written {
                candidates: 1,
                promoted: 0,
                pending_review: 1,
                to_event_id: task
                    .high_watermark_event_id
                    .context("preference task watermark")?,
            }
        );
        let (candidate_id, evidence, review_status): (i64, String, String) = conn.query_row(
            "SELECT id, evidence_event_ids, review_status
             FROM memory_candidates ORDER BY id DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(serde_json::from_str::<Vec<i64>>(&evidence)?, vec![event_id]);
        assert_eq!(review_status, "pending_review");
        let count_before_approval: i64 = conn.query_row(
            "SELECT COALESCE(SUM(reinforcement_count), 0)
             FROM memory_preference_reinforcements",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count_before_approval, round - 1);
        let memory_id = approve_candidate(&mut conn, candidate_id)?
            .context("approved preference should resolve to a memory")?;
        assert_eq!(*canonical_memory_id.get_or_insert(memory_id), memory_id);
        let before_replay: (i64, String) = conn.query_row(
            "SELECT reinforcement_count, source_evidence
             FROM memory_preference_reinforcements WHERE memory_id = ?1",
            [memory_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(before_replay.0, round);
        assert_eq!(
            serde_json::from_str::<Vec<i64>>(&before_replay.1)?,
            evidence_ids.iter().copied().collect::<Vec<_>>()
        );
        let replay =
            process_with_generator(&mut conn, &task, |_prompt| async { Ok(candidate_xml) }).await?;
        assert_eq!(
            replay,
            MemoryCandidateResult::Written {
                candidates: 0,
                promoted: 0,
                pending_review: 0,
                to_event_id: event_id,
            }
        );
        let after_replay: (i64, String) = conn.query_row(
            "SELECT reinforcement_count, source_evidence
             FROM memory_preference_reinforcements WHERE memory_id = ?1",
            [memory_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(after_replay, before_replay);
        let candidate_count: i64 =
            conn.query_row("SELECT COUNT(*) FROM memory_candidates", [], |row| {
                row.get(0)
            })?;
        assert_eq!(candidate_count, round);
    }

    let memory_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM memories WHERE memory_type = 'preference'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(memory_count, 1);
    let (count, machine_checkable, risk_class): (i64, i64, String) = conn.query_row(
        "SELECT reinforcement_count, machine_checkable, risk_class
         FROM memory_preference_reinforcements",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(count, 3);
    assert_eq!(machine_checkable, 1);
    assert_eq!(risk_class, "low");
    Ok(())
}

#[tokio::test]
async fn same_evidence_candidates_count_as_one_reinforcement() -> Result<()> {
    let mut conn = setup_conn();
    let task = setup_task(&mut conn, "sess-preference-same-evidence")?;
    insert_source_observation(&conn, &task, PREFERENCE)?;

    let result = process_with_generator(&mut conn, &task, |_prompt| async {
        Ok((1..=3)
            .map(|index| {
                format!(
                    "<memory_candidate>\
                        <scope>project</scope>\
                        <type>preference</type>\
                        <topic_key>package-manager-same-evidence-{index}</topic_key>\
                        <risk_class>low</risk_class>\
                        <confidence>0.95</confidence>\
                        <text>{PREFERENCE}</text>\
                     </memory_candidate>"
                )
            })
            .collect::<String>())
    })
    .await?;
    assert_eq!(
        result,
        MemoryCandidateResult::Written {
            candidates: 3,
            promoted: 0,
            pending_review: 3,
            to_event_id: task
                .high_watermark_event_id
                .context("preference task watermark")?,
        }
    );

    let candidate_ids = conn
        .prepare("SELECT id FROM memory_candidates ORDER BY id")?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for candidate_id in candidate_ids {
        approve_candidate(&mut conn, candidate_id)?
            .context("approved preference should resolve to a memory")?;
    }

    let (count, source_evidence): (i64, Option<String>) = conn.query_row(
        "SELECT reinforcement_count, source_evidence
         FROM memory_preference_reinforcements",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(count, 1, "one evidence set must count only once");
    assert_eq!(
        source_evidence,
        Some(serde_json::to_string(&vec![task
            .high_watermark_event_id
            .context("preference task watermark")?])?)
    );
    Ok(())
}
