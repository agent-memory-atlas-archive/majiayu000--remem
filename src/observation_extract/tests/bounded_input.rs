use super::*;

#[tokio::test]
async fn observation_input_budget_splits_backlog_and_covers_each_event_once() -> Result<()> {
    let mut conn = setup_conn();
    for index in 0..40 {
        capture(
            &conn,
            "budget",
            &format!("evidence-{index:03} {}", "x".repeat(28 * 1024)),
        )?;
    }
    let mut covered = Vec::new();
    let mut chunks = 0;
    while let Some(mut task) = db::claim_next_extraction_task(&mut conn, "worker-a", 60)? {
        let mut seen = Vec::new();
        process_with_extractor_in_range(&mut conn, &mut task, |prompt| {
            let payload: serde_json::Value = serde_json::from_str(&prompt).unwrap();
            assert!(
                db::extraction_input_bytes(OBSERVATION_EXTRACT_SYSTEM, &prompt)
                    <= db::EXTRACTION_INPUT_MAX_BYTES
            );
            seen = payload["covered_events"]["event_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_i64().unwrap())
                .collect();
            assert!(!seen.is_empty());
            assert!(seen.len() <= db::CAPTURED_EVENT_BATCH_LIMIT);
            assert_eq!(
                payload["content_truncated_event_ids"]
                    .as_array()
                    .unwrap()
                    .len(),
                seen.len()
            );
            async { Ok(no_observations_response("bounded evidence inspected")) }
        })
        .await?;
        assert_eq!(task.high_watermark_event_id, seen.last().copied());
        covered.extend(seen);
        db::mark_extraction_task_done(&conn, task.id, "worker-a", task.high_watermark_event_id)?;
        chunks += 1;
    }
    assert!(chunks > 1);
    assert_eq!(covered, (1..=40).collect::<Vec<i64>>());
    let raw_count: i64 =
        conn.query_row("SELECT COUNT(*) FROM captured_events", [], |row| row.get(0))?;
    assert_eq!(raw_count, 40);
    Ok(())
}

#[tokio::test]
async fn observation_count_limit_applies_even_to_tiny_events() -> Result<()> {
    let mut conn = setup_conn();
    for index in 0..70 {
        capture(&conn, "tiny", &format!("tiny-{index}"))?;
    }
    let mut task = claim_extract_task(&mut conn)?;
    process_with_extractor_in_range(&mut conn, &mut task, |prompt| async move {
        let payload: serde_json::Value = serde_json::from_str(&prompt)?;
        assert_eq!(
            payload["covered_events"]["event_ids"]
                .as_array()
                .unwrap()
                .len(),
            64
        );
        assert!(!prompt.contains("tiny-64"));
        Ok(no_observations_response("first prefix inspected"))
    })
    .await?;
    assert_eq!(task.high_watermark_event_id, Some(64));
    Ok(())
}

#[tokio::test]
async fn bounded_malformed_output_exhaustion_does_not_archive_unattempted_tail() -> Result<()> {
    let mut conn = setup_conn();
    for index in 0..20 {
        capture(
            &conn,
            "bad-batch",
            &format!("{index:03} {}", "x".repeat(24 * 1024)),
        )?;
    }
    let mut attempted_end = None;
    for attempt in 0..db::EXTRACTION_TASK_MAX_ATTEMPTS {
        conn.execute("UPDATE extraction_tasks SET next_retry_epoch = 0", [])?;
        let mut task = claim_extract_task(&mut conn)?;
        assert_eq!(task.attempts, attempt);
        let error = process_with_extractor_in_range(&mut conn, &mut task, |_| async {
            Ok("malformed model output".into())
        })
        .await
        .expect_err("generated output remains fail closed");
        attempted_end = task.high_watermark_event_id;
        db::mark_claimed_extraction_task_error_or_retry(&conn, &task, "worker-a", &error, 1)?;
    }
    let range = db::list_extraction_replay_ranges(&conn, None, 10)?.remove(0);
    assert_eq!(range.from_event_id, 1);
    assert_eq!(Some(range.to_event_id), attempted_end);
    assert!(range.to_event_id < 20);
    let next = claim_extract_task(&mut conn)?;
    assert_eq!(next.cursor_event_id, attempted_end);
    assert_eq!(next.high_watermark_event_id, Some(20));
    assert_eq!(next.attempts, 0);
    Ok(())
}

#[test]
fn extraction_budget_includes_system_and_wrapper_not_only_user_content() {
    let user = "x".repeat(db::EXTRACTION_INPUT_MAX_BYTES - db::EXTRACTION_WRAPPER_RESERVE_BYTES);
    assert!(db::extraction_prompt_fits("", &user));
    assert!(!db::extraction_prompt_fits("system", &user));
}

#[tokio::test]
async fn single_event_with_oversized_prompt_metadata_fails_before_model_call() -> Result<()> {
    let mut conn = setup_conn();
    capture(&conn, "oversized-metadata", "one preserved event")?;
    let mut task = claim_extract_task(&mut conn)?;
    task.project = "x".repeat(db::EXTRACTION_INPUT_MAX_BYTES);
    let error = process_with_extractor_in_range(&mut conn, &mut task, |_| async {
        anyhow::bail!("provider must not run for oversized input")
    })
    .await
    .expect_err("one event still cannot fit the complete prompt");
    assert!(error.to_string().contains("exceeds"));
    assert_eq!(task.high_watermark_event_id, Some(1));
    let cursor: Option<i64> = conn.query_row(
        "SELECT cursor_event_id FROM extraction_tasks WHERE id = ?1",
        [task.id],
        |row| row.get(0),
    )?;
    assert_eq!(cursor, None);
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM captured_events", [], |row| row
            .get::<_, i64>(0))?,
        1
    );
    Ok(())
}
