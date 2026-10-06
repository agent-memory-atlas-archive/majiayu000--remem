use anyhow::Result;

use super::*;

#[tokio::test]
async fn captured_summary_reaches_current_context_only_with_trusted_local_evidence() -> Result<()> {
    let data_dir =
        crate::db::test_support::ScopedTestDataDir::new_offline("rollup-summary-current");
    std::fs::create_dir_all(&data_dir.path)?;
    // All host paths and side effects remain under an isolated fixture root.
    let project_dir = data_dir.path.join("repo");
    std::fs::create_dir_all(&project_dir)?;
    let project = project_dir.to_str().expect("utf8 fixture path");
    for (tool, expected_trust, promoted) in [
        ("Bash", "local_tool_output", true),
        ("Grep", "repo_file", true),
        ("WebFetch", "external_content", false),
    ] {
        let mut conn = setup_conn();
        let session_id = format!("rollup-summary-current-{tool}");
        let request = "Capture compiled reader evidence";
        let decision =
            "The compiled reader stores validated schema fragments in a repository cache.";
        let candidate_text = format!("[Context: {request}]\n\n{decision}");
        let event = record_captured_event(
            &conn,
            &CaptureEventInput {
                host: "claude-code",
                session_id: &session_id,
                project,
                cwd: Some(project),
                event_type: "tool_result",
                role: None,
                tool_name: Some(tool),
                content: &candidate_text,
                task_kind: Some(ExtractionTaskKind::SessionRollup),
            },
        )?;
        let stop = record_captured_event(
            &conn,
            &CaptureEventInput {
                host: "claude-code",
                session_id: &session_id,
                project,
                cwd: Some(project),
                event_type: "session_stop",
                role: None,
                tool_name: None,
                content: &serde_json::json!({"session_id": session_id, "cwd": project}).to_string(),
                task_kind: Some(ExtractionTaskKind::SessionRollup),
            },
        )?;
        let task = claim_rollup_task(&mut conn)?;
        let result = process_with_summarizer(&mut conn, &task, |_prompt| async move {
            Ok(xml_response_with_structured_fields(
                "Recorded the compiled reader cache design.",
                request,
                decision,
                "",
                "",
                "",
                "",
            ))
        })
        .await?;
        assert_eq!(result, SessionRollupResult::Written);
        let (candidate_id, review, evidence, trust): (i64, String, String, String) = conn
            .query_row(
                "SELECT id, review_status, evidence_event_ids, source_trust_class
             FROM memory_candidates WHERE text = ?1",
                [&candidate_text],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
        assert_eq!(trust, expected_trust);
        assert_eq!(
            serde_json::from_str::<Vec<i64>>(&evidence)?,
            vec![event.event_row_id]
        );
        assert!(!serde_json::from_str::<Vec<i64>>(&evidence)?.contains(&stop.event_row_id));
        let memory_ids = conn
            .prepare("SELECT id FROM memories WHERE source_candidate_id = ?1")?
            .query_map([candidate_id], |row| row.get::<_, i64>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        if !promoted {
            assert_eq!(review, "pending_review");
            assert!(memory_ids.is_empty());
            let receipt_count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM memory_activation_requests",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(receipt_count, 0);
            continue;
        }
        assert_eq!(review, "auto_promoted");
        assert_eq!(memory_ids.len(), 1);
        let memory_id = memory_ids[0];
        let (score, receipt_count): (f64, i64) = conn.query_row(
            "SELECT confidence, (SELECT COUNT(*) FROM memory_activation_requests
               WHERE result_memory_id = memories.id AND actor_kind = 'automatic_worker')
             FROM memories WHERE id = ?1",
            [memory_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(score, 0.74);
        assert_eq!(receipt_count, 1);
        assert!(
            crate::truth::classify_memory(&conn, memory_id, chrono::Utc::now().timestamp())?
                .current_context_eligible
        );
        let loaded = crate::context::load_session_start_candidates_with_limits(
            &conn,
            project,
            project,
            None,
            &crate::context::ContextLimits::default(),
        )?;
        let canonical_ref = format!("memory:{memory_id}");
        assert!(
            loaded
                .candidates
                .iter()
                .any(|item| item.stable_key == canonical_ref
                    && item.trust != crate::context_bundle::TrustClass::Quarantined),
            "{loaded:?}"
        );
        let projection = loaded
            .current_truth_projection
            .expect("current truth projection");
        assert!(
            projection.truths.iter().any(|truth| {
                truth.validity == crate::truth::ValidityState::Current
                    && truth
                        .claim
                        .as_ref()
                        .is_some_and(|claim| claim.canonical_ref == canonical_ref)
            }),
            "{projection:?}"
        );
    }
    Ok(())
}
