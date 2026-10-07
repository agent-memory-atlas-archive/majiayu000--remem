use super::*;

#[tokio::test]
async fn malformed_source_evidence_is_not_a_model_output_retry() -> Result<()> {
    let mut conn = setup_conn();
    let task = setup_task(&mut conn, "sess-candidate-invalid-evidence")?;
    insert_source_observation(&conn, &task, "An otherwise valid observation.")?;
    conn.execute(
        "UPDATE observations SET evidence_event_ids = 'not-json' WHERE session_row_id = ?1",
        params![task.session_row_id],
    )?;
    let error = process_with_generator(&mut conn, &task, |_prompt| async {
        Err(anyhow::anyhow!(
            "source failure must prevent the model call"
        ))
    })
    .await
    .expect_err("invalid source evidence must fail before generation");
    assert!(error.to_string().contains("malformed evidence_event_ids"));
    assert_eq!(
        db::classify_failure_error(&error),
        db::FailureClass::Permanent
    );
    db::mark_claimed_extraction_task_error_or_retry(&conn, &task, "worker-a", &error, 1)?;
    let (state, attempts, class): (String, i64, String) = conn.query_row(
        "SELECT status, attempts, failure_class FROM extraction_tasks WHERE id = ?1",
        [task.id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(
        (state.as_str(), attempts, class.as_str()),
        ("failed", 1, "permanent")
    );
    Ok(())
}
