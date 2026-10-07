use super::*;

#[tokio::test]
async fn malformed_generated_output_retries_the_same_evidence_then_completes() -> Result<()> {
    let mut conn = setup_conn();
    let task_id = capture(&conn, "sess-model-retry", "Keep the original evidence.")?;
    let task = claim_extract_task(&mut conn)?;
    let error = process_with_extractor(&mut conn, &task, |_prompt| async {
        Ok("{ incomplete JSON".to_string())
    })
    .await
    .expect_err("invalid generated JSON must fail closed");
    db::mark_claimed_extraction_task_error_or_retry(&conn, &task, "worker-a", &error, 1)?;
    let (state, attempts, cursor): (String, i64, Option<i64>) = conn.query_row(
        "SELECT status, attempts, cursor_event_id FROM extraction_tasks WHERE id = ?1",
        [task_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!((state.as_str(), attempts, cursor), ("pending", 1, None));
    assert!(db::list_extraction_replay_ranges(&conn, None, 10)?.is_empty());

    conn.execute(
        "UPDATE extraction_tasks SET next_retry_epoch = 0 WHERE id = ?1",
        [task_id],
    )?;
    let retried = claim_extract_task(&mut conn)?;
    assert_eq!(retried.cursor_event_id, task.cursor_event_id);
    assert_eq!(
        retried.high_watermark_event_id,
        task.high_watermark_event_id
    );
    let result = process_with_extractor(&mut conn, &retried, |prompt| async move {
        assert!(prompt.contains("Keep the original evidence."));
        Ok(no_observations_response(
            "The evidence is a request, not a completed finding.",
        ))
    })
    .await?;
    assert_eq!(result, ObservationExtractResult::NoObservations);
    db::mark_extraction_task_done(
        &conn,
        retried.id,
        "worker-a",
        retried.high_watermark_event_id,
    )?;
    let (state, cursor): (String, Option<i64>) = conn.query_row(
        "SELECT status, cursor_event_id FROM extraction_tasks WHERE id = ?1",
        [task_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(state, "done");
    assert_eq!(cursor, task.high_watermark_event_id);
    let raw_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM captured_events", [], |row| row.get(0))?;
    assert_eq!(raw_count, 1);
    Ok(())
}

#[tokio::test]
async fn repeated_invalid_generated_output_exhausts_into_transient_replay_evidence() -> Result<()> {
    let mut conn = setup_conn();
    let task_id = capture(&conn, "sess-model-exhaust", "Preserve this raw evidence.")?;
    for attempt in 0..db::EXTRACTION_TASK_MAX_ATTEMPTS {
        conn.execute(
            "UPDATE extraction_tasks SET next_retry_epoch = 0 WHERE id = ?1",
            [task_id],
        )?;
        let task = claim_extract_task(&mut conn)?;
        assert_eq!(task.attempts, attempt);
        let error = process_with_extractor(&mut conn, &task, |_prompt| async {
            Ok("not a JSON object".to_string())
        })
        .await
        .expect_err("repeated invalid output must never persist");
        db::mark_claimed_extraction_task_error_or_retry(&conn, &task, "worker-a", &error, 1)?;
    }
    let (state, attempts, class): (String, i64, String) = conn.query_row(
        "SELECT status, attempts, failure_class FROM extraction_tasks WHERE id = ?1",
        [task_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(
        (state.as_str(), attempts, class.as_str()),
        ("failed", db::EXTRACTION_TASK_MAX_ATTEMPTS, "transient")
    );
    let (ranges, replay_class): (i64, String) = conn.query_row(
        "SELECT COUNT(*), failure_class FROM extraction_replay_ranges WHERE source_task_id = ?1",
        [task_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!((ranges, replay_class.as_str()), (1, "transient"));
    let counts: (i64, i64) = conn.query_row(
        "SELECT (SELECT COUNT(*) FROM captured_events), (SELECT COUNT(*) FROM observations)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(counts, (1, 0));
    Ok(())
}

#[tokio::test]
async fn observation_extract_malformed_output_fails_closed() -> Result<()> {
    let mut conn = setup_conn();
    capture(&conn, "sess-bad", "important output")?;
    let task = claim_extract_task(&mut conn)?;

    let err = process_with_extractor(&mut conn, &task, |_prompt| async {
        Ok("not json".to_string())
    })
    .await
    .expect_err("malformed output should fail");

    assert!(err.to_string().contains("malformed observation_extract"));
    assert_eq!(
        db::classify_failure_error(&err),
        db::FailureClass::Transient
    );
    Ok(())
}
