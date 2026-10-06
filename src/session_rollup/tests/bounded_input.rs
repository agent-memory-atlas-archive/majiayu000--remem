use super::*;

#[tokio::test]
async fn rollup_input_budget_preserves_actual_chunk_watermarks() -> Result<()> {
    let _data_dir = crate::db::test_support::ScopedTestDataDir::new("rollup-bounded-input");
    let mut conn = setup_conn();
    let mut task_id = 0;
    for index in 0..30 {
        task_id = capture(
            &conn,
            "bounded-rollup",
            "tool_result",
            &format!("event-{index:03} {}", "x".repeat(28 * 1024)),
        )?;
    }
    let mut previous = 0;
    let mut chunks = 0;
    while let Some(mut task) = db::claim_extraction_task_by_id(&mut conn, task_id, "worker-a", 60)?
    {
        let result = process_with_summarizer_in_range(&mut conn, &mut task, |prompt| async move {
            assert!(
                db::extraction_input_bytes(SESSION_ROLLUP_SYSTEM, &prompt)
                    <= db::EXTRACTION_INPUT_MAX_BYTES
            );
            assert!(prompt.contains("content_truncated_event_ids=\""));
            assert!(prompt.contains("raw_evidence_retained=\"true\""));
            Ok(xml_response("Inspected the bounded captured evidence.", ""))
        })
        .await?;
        assert_eq!(result, SessionRollupResult::Written);
        let end = task.high_watermark_event_id.unwrap();
        assert!(end > previous);
        let range: (i64, i64) = conn.query_row(
            "SELECT covered_from_event_id, covered_to_event_id FROM session_summaries
             WHERE session_row_id = ?1 ORDER BY covered_to_event_id DESC LIMIT 1",
            params![task.session_row_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(range, (previous + 1, end));
        db::mark_extraction_task_done(&conn, task.id, "worker-a", Some(end))?;
        previous = end;
        chunks += 1;
    }
    assert_eq!(previous, 30);
    assert!(chunks > 1);
    assert_eq!(summary_count(&conn), chunks);
    let raw_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM captured_events", [], |row| row.get(0))?;
    assert_eq!(raw_count, 30);
    Ok(())
}

#[tokio::test]
async fn rollup_retry_recovers_persisted_bounded_prefix_before_generating_again() -> Result<()> {
    let _data_dir = crate::db::test_support::ScopedTestDataDir::new("rollup-bounded-checkpoint");
    let mut conn = setup_conn();
    for index in 0..20 {
        capture(
            &conn,
            "prefix-retry",
            "tool_result",
            &format!("event-{index:03} {}", "x".repeat(28 * 1024)),
        )?;
    }
    let claimed = claim_rollup_task(&mut conn)?;
    let mut attempt = claimed.clone();
    conn.execute_batch(
        "CREATE TRIGGER fail_bounded_followup BEFORE INSERT ON extraction_tasks
         WHEN NEW.task_kind = 'user_context_candidate'
         BEGIN SELECT RAISE(FAIL, 'forced bounded followup failure'); END;",
    )?;
    let error = process_with_summarizer_in_range(&mut conn, &mut attempt, |_| async {
        Ok(xml_response("A durable bounded rollup checkpoint.", ""))
    })
    .await
    .expect_err("required followup failure keeps success checkpoint pending");
    assert!(
        error
            .to_string()
            .contains("forced bounded followup failure"),
        "{error:#}"
    );
    let attempted_end = attempt.high_watermark_event_id;
    assert!(attempted_end < claimed.high_watermark_event_id);
    assert_eq!(summary_count(&conn), 1);
    let completed: Option<i64> = conn.query_row(
        "SELECT completed_event_id FROM extraction_tasks WHERE id = ?1",
        [claimed.id],
        |row| row.get(0),
    )?;
    assert_eq!(completed, None);
    conn.execute_batch("DROP TRIGGER fail_bounded_followup")?;
    let mut retried = claimed;
    let result = process_with_summarizer_in_range(&mut conn, &mut retried, |_| async {
        anyhow::bail!("persisted bounded prefix must not call the model again")
    })
    .await?;
    assert_eq!(result, SessionRollupResult::AlreadyExists);
    assert_eq!(retried.high_watermark_event_id, attempted_end);
    assert_eq!(summary_count(&conn), 1);
    db::mark_extraction_task_done(&conn, retried.id, "worker-a", attempted_end)?;
    Ok(())
}

#[tokio::test]
async fn rollup_captured_transcript_budget_never_omits_claimed_prefix_events() -> Result<()> {
    let _data_dir = crate::db::test_support::ScopedTestDataDir::new("rollup-captured-budget");
    let mut conn = setup_conn();
    for index in 0..20 {
        db::record_captured_event(
            &conn,
            &CaptureEventInput {
                host: "codex-cli",
                session_id: "captured-transcript-budget",
                project: "/tmp/remem",
                cwd: None,
                event_type: "message",
                role: Some("assistant"),
                tool_name: Some(crate::memory::raw_transcript::CODEX_TRANSCRIPT_MESSAGE_TOOL),
                content: &format!("message-{index:03} {}", "x".repeat(8 * 1024)),
                task_kind: Some(ExtractionTaskKind::SessionRollup),
            },
        )?;
    }
    let mut task = claim_rollup_task(&mut conn)?;
    let mut included = 0;
    process_with_summarizer_in_range(&mut conn, &mut task, |prompt| {
        included = prompt.matches("<event id=\"").count();
        assert!(included > 0 && included < 20);
        for id in 1..=included {
            assert!(prompt.contains(&format!("<event id=\"{id}\"")));
        }
        async {
            Ok(xml_response(
                "Summarized the first bounded transcript prefix.",
                "",
            ))
        }
    })
    .await?;
    assert_eq!(task.high_watermark_event_id, Some(included as i64));
    Ok(())
}
