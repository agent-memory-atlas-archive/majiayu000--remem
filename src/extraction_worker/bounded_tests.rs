use std::cell::RefCell;

use super::*;

fn task() -> db::ExtractionTask {
    db::ExtractionTask {
        id: 1,
        task_kind: db::ExtractionTaskKind::ObservationExtract,
        host_id: 1,
        workspace_id: 1,
        project_id: 1,
        session_row_id: Some(1),
        host: "codex-cli".into(),
        project: "/tmp/remem-exact-chunks".into(),
        session_id: Some("exact".into()),
        ai_profile: Some("explicit".into()),
        priority: 1,
        cursor_event_id: Some(0),
        high_watermark_event_id: Some(3),
        attempts: 0,
        replay_range_id: Some(1),
    }
}

#[tokio::test]
async fn exact_chunks_checkpoint_prefix_before_attempting_next_chunk() -> Result<()> {
    let mut task = task();
    let checkpoints = RefCell::new(Vec::new());
    let seen = RefCell::new(Vec::new());
    let outcome = process_exact_chunks_with(
        &mut task,
        async |task| {
            let start = task.cursor_event_id.unwrap() + 1;
            seen.borrow_mut().push(start);
            assert_eq!(task.ai_profile.as_deref(), Some("explicit"));
            assert_eq!(task.high_watermark_event_id, Some(3));
            task.high_watermark_event_id = Some(start);
            Ok(ExtractionTaskOutcome::Done { to_event_id: None })
        },
        |task, end| {
            assert_eq!(task.cursor_event_id, Some(end - 1));
            checkpoints.borrow_mut().push(end);
            Ok(())
        },
    )
    .await?;
    assert_eq!(seen.into_inner(), vec![1, 2, 3]);
    assert_eq!(checkpoints.into_inner(), vec![1, 2]);
    assert_eq!(
        outcome,
        ExtractionTaskOutcome::Done {
            to_event_id: Some(3)
        }
    );
    assert_eq!(task.cursor_event_id, Some(2));
    Ok(())
}

#[tokio::test]
async fn exact_chunk_failure_keeps_actual_attempt_and_only_successful_prefix() {
    let mut task = task();
    let checkpoints = RefCell::new(Vec::new());
    let error = process_exact_chunks_with(
        &mut task,
        async |task| {
            let end = task.cursor_event_id.unwrap() + 1;
            task.high_watermark_event_id = Some(end);
            if end == 2 {
                anyhow::bail!("transient model timeout");
            }
            Ok(ExtractionTaskOutcome::Done { to_event_id: None })
        },
        |_, end| {
            checkpoints.borrow_mut().push(end);
            Ok(())
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("timeout"));
    assert_eq!(checkpoints.into_inner(), vec![1]);
    assert_eq!(task.cursor_event_id, Some(1));
    assert_eq!(task.high_watermark_event_id, Some(2));
}

#[tokio::test]
async fn exact_chunks_share_one_total_timeout() {
    let mut task = task();
    let checkpoints = RefCell::new(Vec::new());
    let result = tokio::time::timeout(
        Duration::from_millis(50),
        process_exact_chunks_with(
            &mut task,
            async |task| {
                let end = task.cursor_event_id.unwrap() + 1;
                task.high_watermark_event_id = Some(end);
                if end == 2 {
                    std::future::pending::<()>().await;
                }
                Ok(ExtractionTaskOutcome::Done { to_event_id: None })
            },
            |_, end| {
                checkpoints.borrow_mut().push(end);
                Ok(())
            },
        ),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(checkpoints.into_inner(), vec![1]);
    assert_eq!(
        (task.cursor_event_id, task.high_watermark_event_id),
        (Some(1), Some(2))
    );
}

#[tokio::test]
async fn exact_chunks_never_skip_after_failed_checkpoint() {
    let mut task = task();
    let calls = RefCell::new(0);
    let result = process_exact_chunks_with(
        &mut task,
        async |task| {
            *calls.borrow_mut() += 1;
            task.high_watermark_event_id = Some(1);
            Ok(ExtractionTaskOutcome::Done { to_event_id: None })
        },
        |_, _| anyhow::bail!("stale lease owner"),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(*calls.borrow(), 1);
    assert_eq!(task.cursor_event_id, Some(0));
}
