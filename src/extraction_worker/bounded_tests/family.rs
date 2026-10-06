use super::*;

#[tokio::test]
async fn exact_family_shares_profile_suppression_and_one_timeout_across_members() {
    let first = task();
    let mut second = task();
    second.id = 2;
    second.cursor_event_id = Some(1);
    let mut tasks = vec![first, second];
    let mut active = 0;
    let checkpoints = RefCell::new(Vec::new());
    let calls = RefCell::new(Vec::new());
    let finished = RefCell::new(false);
    let result = tokio::time::timeout(
        Duration::from_millis(50),
        EXACT_REPLAY_TASK.scope(
            (),
            process_exact_family_with(
                &mut tasks,
                &mut active,
                async |task| {
                    assert!(exact_replay_task_active());
                    assert_eq!(task.ai_profile.as_deref(), Some("explicit"));
                    calls.borrow_mut().push((task.id, task.cursor_event_id));
                    if task.id == 2 {
                        task.high_watermark_event_id = Some(2);
                        std::future::pending::<()>().await;
                    }
                    Ok(ExtractionTaskOutcome::Done { to_event_id: None })
                },
                |task, end| {
                    checkpoints.borrow_mut().push((task.id, end));
                    Ok(())
                },
                || {
                    *finished.borrow_mut() = true;
                    Ok(())
                },
            ),
        ),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(active, 1);
    assert_eq!(checkpoints.into_inner(), vec![(1, 3)]);
    assert_eq!(calls.into_inner(), vec![(1, Some(0)), (2, Some(1))]);
    assert_eq!(tasks[0].cursor_event_id, Some(3));
    assert_eq!(
        (tasks[1].cursor_event_id, tasks[1].high_watermark_event_id),
        (Some(1), Some(2))
    );
    assert!(!finished.into_inner());
}

#[tokio::test]
async fn exact_family_skips_verified_primary_but_completes_its_existing_successor() -> Result<()> {
    let mut first = task();
    first.cursor_event_id = Some(3);
    let mut second = task();
    second.id = 2;
    second.cursor_event_id = Some(1);
    let mut tasks = vec![first, second];
    let mut active = 0;
    let calls = RefCell::new(Vec::new());
    let checkpoints = RefCell::new(Vec::new());
    let finished = RefCell::new(false);
    process_exact_family_with(
        &mut tasks,
        &mut active,
        async |task| {
            calls.borrow_mut().push(task.id);
            Ok(ExtractionTaskOutcome::Done { to_event_id: None })
        },
        |task, end| {
            checkpoints.borrow_mut().push((task.id, end));
            Ok(())
        },
        || {
            *finished.borrow_mut() = true;
            Ok(())
        },
    )
    .await?;
    assert_eq!(calls.into_inner(), vec![2]);
    assert_eq!(checkpoints.into_inner(), vec![(2, 3)]);
    assert!(finished.into_inner());
    Ok(())
}
