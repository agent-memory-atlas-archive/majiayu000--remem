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

fn write_error_path_transcript(
    directory: &std::path::Path,
    filename: &str,
    session_id: &str,
    text: &str,
) -> Result<std::path::PathBuf> {
    let path = directory.join(filename);
    std::fs::write(
        &path,
        serde_json::json!({
            "type": "assistant",
            "sessionId": session_id,
            "cwd": "/tmp/remem",
            "message": {"content": [{"type": "text", "text": text}]}
        })
        .to_string(),
    )?;
    Ok(path)
}

fn assert_no_rollup_generation_or_checkpoint(conn: &Connection, task_id: i64) -> Result<()> {
    assert_eq!(summary_count(conn), 0);
    let counts: (i64, i64, i64) = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM memory_candidates),
                (SELECT COUNT(*) FROM jobs),
                (SELECT COUNT(*) FROM extraction_tasks WHERE task_kind != 'session_rollup')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(counts, (0, 0, 0));
    let checkpoint: (Option<i64>, Option<i64>) = conn.query_row(
        "SELECT cursor_event_id, completed_event_id FROM extraction_tasks WHERE id = ?1",
        [task_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(checkpoint, (None, None));
    Ok(())
}

#[tokio::test]
async fn rollup_prompt_evidence_failure_archives_only_attempted_prefix_idempotently() -> Result<()>
{
    let data_dir = crate::db::test_support::ScopedTestDataDir::new("rollup-error-archive-prefix");
    std::fs::create_dir_all(&data_dir.path)?;
    let session_id = "rollup-error-archive-prefix";
    let first_text = "Raw history from the failed attempted prefix.";
    let tail_text = "Later raw history must wait for its own attempt.";
    let first = write_error_path_transcript(&data_dir.path, "first.jsonl", session_id, first_text)?;
    let tail = write_error_path_transcript(&data_dir.path, "tail.jsonl", session_id, tail_text)?;
    let mut conn = setup_conn();
    let task_id = capture(
        &conn,
        session_id,
        "session_stop",
        &serde_json::json!({
            "session_id": session_id, "cwd": "/tmp/remem", "transcript_path": first
        })
        .to_string(),
    )?;
    let batch = db::CAPTURED_EVENT_BATCH_LIMIT as i64;
    for index in 1..batch {
        capture(
            &conn,
            session_id,
            "tool_result",
            &format!("prefix event {index}"),
        )?;
    }
    capture(
        &conn,
        session_id,
        "session_stop",
        &serde_json::json!({
            "session_id": session_id, "cwd": "/tmp/remem", "transcript_path": tail
        })
        .to_string(),
    )?;
    let mut task = db::claim_extraction_task_by_id(&mut conn, task_id, "worker-a", 60)?
        .expect("captured rollup task");
    assert_eq!(task.high_watermark_event_id, Some(batch + 1));

    let model_calls = std::cell::Cell::new(0);
    for _ in 0..2 {
        let error = process_with_summarizer_in_range(&mut conn, &mut task, |_prompt| {
            model_calls.set(model_calls.get() + 1);
            async { anyhow::bail!("prompt-evidence rejection must precede generation") }
        })
        .await
        .expect_err("legacy transcript without captured fallback remains invalid model input");
        assert!(
            error.to_string().contains("transcript_byte_len"),
            "{error:#}"
        );
        assert_eq!(
            db::classify_failure_error(&error),
            db::FailureClass::Permanent
        );
        assert_eq!(model_calls.get(), 0);
        assert_eq!(task.high_watermark_event_id, Some(batch));
        assert_no_rollup_generation_or_checkpoint(&conn, task_id)?;
        let archived: (i64, i64) = conn.query_row(
            "SELECT SUM(content = ?1), SUM(content = ?2) FROM raw_messages",
            params![first_text, tail_text],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(archived, (1, 0));
        let durable_target: Option<i64> = conn.query_row(
            "SELECT high_watermark_event_id FROM extraction_tasks WHERE id = ?1",
            [task_id],
            |row| row.get(0),
        )?;
        assert_eq!(durable_target, Some(batch + 1));
    }
    Ok(())
}

#[tokio::test]
async fn rollup_prompt_evidence_failure_keeps_archive_error_visible_and_permanent() -> Result<()> {
    let data_dir = crate::db::test_support::ScopedTestDataDir::new("rollup-error-archive-failure");
    std::fs::create_dir_all(&data_dir.path)?;
    let session_id = "rollup-error-archive-failure";
    let transcript = write_error_path_transcript(
        &data_dir.path,
        "failed.jsonl",
        session_id,
        "Raw archive insertion is deliberately blocked.",
    )?;
    let mut conn = setup_conn();
    let task_id = capture(
        &conn,
        session_id,
        "session_stop",
        &serde_json::json!({
            "session_id": session_id, "cwd": "/tmp/remem", "transcript_path": transcript
        })
        .to_string(),
    )?;
    let mut task = db::claim_extraction_task_by_id(&mut conn, task_id, "worker-a", 60)?
        .expect("captured rollup task");
    conn.execute_batch(
        "CREATE TRIGGER fail_prompt_error_archive BEFORE INSERT ON raw_messages
         BEGIN SELECT RAISE(FAIL, 'forced prompt-error raw archive failure'); END;",
    )?;
    let model_calls = std::cell::Cell::new(0);
    let error = process_with_summarizer_in_range(&mut conn, &mut task, |_prompt| {
        model_calls.set(model_calls.get() + 1);
        async { anyhow::bail!("prompt-evidence rejection must precede generation") }
    })
    .await
    .expect_err("archive failure must not replace the original missing-evidence error");
    assert!(
        error.to_string().starts_with("missing evidence:"),
        "{error:#}"
    );
    assert_eq!(
        db::classify_failure_error(&error),
        db::FailureClass::Permanent
    );
    assert_eq!(model_calls.get(), 0);
    assert_no_rollup_generation_or_checkpoint(&conn, task_id)?;
    let failures: i64 = conn.query_row(
        "SELECT COUNT(*) FROM raw_ingest_failures
         WHERE session_id = ?1 AND insert_errors = 1",
        [session_id],
        |row| row.get(0),
    )?;
    assert_eq!(failures, 1);
    let log = std::fs::read_to_string(data_dir.path.join("remem.log"))?;
    assert!(
        log.contains("[ERROR] [session-rollup] raw archive preservation failed after prompt evidence rejection"),
        "{log}"
    );
    assert!(log.contains("raw archive ingest incomplete"), "{log}");
    Ok(())
}
