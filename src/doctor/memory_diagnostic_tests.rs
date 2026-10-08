use super::*;

fn fixture() -> Result<(Connection, MemoryDiagnosticOptions, i64)> {
    let conn = Connection::open_in_memory()?;
    crate::migrate::run_migrations(&conn)?;
    let event = db::record_captured_event(
        &conn,
        &db::CaptureEventInput {
            host: "codex-cli",
            session_id: "shared-session",
            project: "/repo",
            cwd: None,
            event_type: "user_prompt_submit",
            role: Some("user"),
            tool_name: None,
            content: "remember the deployment decision",
            task_kind: Some(db::ExtractionTaskKind::SessionRollup),
        },
    )?;
    let opts = MemoryDiagnosticOptions {
        project: "/repo".into(),
        query: Some("deployment".into()),
        host: None,
        session_id: None,
        source_root: None,
        injection_run_id: None,
    };
    Ok((conn, opts, event.event_row_id))
}

fn evidence(report: &Value, stage: usize, source: &str) -> Vec<Value> {
    report["stages"][stage]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["source"] == source)
        .cloned()
        .collect()
}

#[test]
fn captured_failure_and_pending_review_are_distinct_read_only_evidence() -> Result<()> {
    let (conn, opts, event_id) = fixture()?;
    conn.execute("UPDATE extraction_tasks SET status='failed',last_error='model unavailable',failure_class='transient'",[])?;
    conn.execute(
        "INSERT INTO memory_candidates
        (project_id,scope,memory_type,topic_key,text,evidence_event_ids,confidence,risk_class,
         review_status,auto_promote_block_reason,created_at_epoch,updated_at_epoch)
        SELECT project_id,'project','decision','deployment','a paraphrased result',?1,0.7,'low',
            'pending_review','confidence_below_threshold',1,1 FROM captured_events WHERE id=?2",
        params![format!("[{event_id}]"), event_id],
    )?;
    conn.execute("INSERT INTO user_context_candidates
        (owner_scope,owner_key,source_project,host,session_id,claim_type,claim_text,
         confidence,sensitivity,risk_class,source_kind,source_refs_json,review_status,
         auto_promote_block_reason,created_at_epoch,updated_at_epoch)
        VALUES ('repo','/repo','/repo','codex-cli','shared-session','preference','a paraphrased preference',
          0.7,'normal','low','explicit_user_statement',?1,'pending_review','manual_review',1,1)",
        [format!(r#"[{{"kind":"captured_event","id":{event_id}}}]"#)])?;
    conn.execute_batch("PRAGMA query_only=ON")?;
    let before = conn.total_changes();
    let report = build_report(&conn, &opts)?;
    assert_eq!(conn.total_changes(), before);
    assert_eq!(evidence(&report, 0, "captured_event").len(), 1);
    assert_eq!(
        evidence(&report, 1, "extraction_task")[0]["last_error"],
        "model unavailable"
    );
    assert_eq!(
        evidence(&report, 2, "memory_candidate")[0]["block_reason"],
        "confidence_below_threshold"
    );
    assert_eq!(
        evidence(&report, 2, "user_context_candidate")[0]["block_reason"],
        "manual_review"
    );
    assert!(evidence(&report, 3, "memory").is_empty());
    assert!(evidence(&report, 4, "context_item").is_empty());
    Ok(())
}

#[test]
fn latest_destination_audit_never_falls_back_to_an_older_memory_hit() -> Result<()> {
    let (conn, mut opts, _) = fixture()?;
    conn.execute("INSERT INTO memories (id,project,topic_key,title,content,memory_type,
        created_at_epoch,updated_at_epoch,status,expires_at_epoch)
        VALUES (42,'/repo','deployment','deployment','deployment decision','decision',1,1,'active',1)",[])?;
    conn.execute(
        "UPDATE memories SET owner_scope='repo',owner_key='/repo',topic_key=NULL WHERE id=42",
        [],
    )?;
    conn.execute_batch(
        "INSERT INTO context_injection_items
        (injection_run_id,host,project,session_id,injection_key,output_mode,decision,item_kind,
         item_id,memory_id,channel,status,drop_reason,injected_at_epoch)
        VALUES ('old','codex-cli','/repo','destination','key','text','emit','memory',42,42,
                'core','dropped','expired',1),
               ('new','codex-cli','/repo','destination','key','text','emit','memory',99,99,
                'core','injected',NULL,2)",
    )?;
    let report = build_report(&conn, &opts)?;
    assert_eq!(
        evidence(&report, 3, "memory")[0]["classification"]["classification"],
        "expired"
    );
    assert_eq!(
        evidence(&report, 3, "memory")[0]["truth"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(evidence(&report, 4, "context_run")[0]["run_id"], "new");
    assert!(evidence(&report, 4, "context_item").is_empty());
    opts.injection_run_id = Some("old".into());
    let report = build_report(&conn, &opts)?;
    assert_eq!(
        evidence(&report, 4, "context_item")[0]["drop_reason"],
        "expired"
    );
    opts.injection_run_id = Some("missing".into());
    assert!(build_report(&conn, &opts).is_err());
    Ok(())
}

#[test]
fn literal_discovery_does_not_infer_missing_capture_or_cross_host_provenance() -> Result<()> {
    let (conn, mut opts, _) = fixture()?;
    db::record_captured_event(
        &conn,
        &db::CaptureEventInput {
            host: "claude-code",
            session_id: "shared-session",
            project: "/repo",
            cwd: None,
            event_type: "user_prompt_submit",
            role: Some("user"),
            tool_name: None,
            content: "deployment decision from the other host",
            task_kind: None,
        },
    )?;
    opts.host = Some("codex-cli".into());
    let report = build_report(&conn, &opts)?;
    assert_eq!(evidence(&report, 0, "captured_event").len(), 1);
    assert_eq!(
        evidence(&report, 0, "captured_event")[0]["host"],
        "codex-cli"
    );
    opts.query = Some("a different paraphrase".into());
    let report = build_report(&conn, &opts)?;
    assert!(report["stages"][0]["evidence"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(report["stages"][0]["note"]
        .as_str()
        .unwrap()
        .contains("unknown"));
    Ok(())
}

#[test]
fn bounds_keep_each_source_visible_and_redact_sensitive_output() {
    let mut entries = (0..21)
        .map(|id| json!({"source":"captured_event","id":id}))
        .collect::<Vec<_>>();
    entries.push(json!({"source":"raw_message","id":1}));
    let stage = Stage::new("capture", entries, "unknown");
    assert!(stage.has_more);
    assert_eq!(stage.evidence.len(), 21);
    let mut text = json!({"error":"Authorization: Bearer example-diagnostic-test-token"});
    redact(&mut text);
    assert!(!text.to_string().contains("example-diagnostic-test-token"));
}

#[test]
fn suppression_and_human_discard_reasons_remain_visible() -> Result<()> {
    let (conn, opts, event_id) = fixture()?;
    conn.execute("INSERT INTO memory_candidates
        (id,project_id,scope,memory_type,topic_key,text,evidence_event_ids,confidence,risk_class,
         review_status,review_reason,review_actor,created_at_epoch,updated_at_epoch)
        SELECT 12,project_id,'project','decision','deployment','deployment',?1,0.9,'low',
            'discarded','outdated deployment choice','reviewer',1,1 FROM captured_events WHERE id=?2",
        params![format!("[{event_id}]"),event_id])?;
    conn.execute(
        "INSERT INTO memories (id,project,topic_key,title,content,memory_type,
        created_at_epoch,updated_at_epoch,status,source_candidate_id)
        VALUES (42,'/repo','deployment','deployment','deployment','decision',1,1,'stale',12)",
        [],
    )?;
    memory::suppression::create_suppression(
        &conn,
        &memory::suppression::SuppressRequest {
            target: memory::suppression::SuppressionTarget {
                kind: "memory".into(),
                id: Some(42),
                value: None,
            },
            reason: Some("no longer relevant"),
            actor: Some("reviewer"),
        },
    )?;
    let report = build_report(&conn, &opts)?;
    assert_eq!(
        evidence(&report, 2, "memory_candidate")[0]["review_reason"],
        "outdated deployment choice"
    );
    assert_eq!(
        evidence(&report, 3, "memory")[0]["suppressions"][0]["reason"],
        "no longer relevant"
    );
    Ok(())
}

#[test]
fn raw_question_locates_metadata_only_stop_and_exact_session_keeps_host() -> Result<()> {
    let (conn, mut opts, _) = fixture()?;
    for (host, path, text) in [
        (
            "codex-cli",
            "/tmp/.codex/sessions/source.jsonl",
            "unique raw evidence",
        ),
        (
            "claude-code",
            "/tmp/.claude/projects/source.jsonl",
            "other host source",
        ),
    ] {
        let raw = memory::raw_archive::insert_raw_message(
            &conn,
            "raw-session",
            "/repo",
            "user",
            text,
            memory::raw_archive::SOURCE_TRANSCRIPT,
            None,
            None,
        )?
        .context("raw row")?;
        conn.execute(
            "INSERT INTO raw_session_identities
            (source_root,transcript_path,host,fallback_session_id,canonical_session_id,
             project,legacy_project,status,contract_version,observed_mtime_ns,observed_size_bytes,
             first_seen_at_epoch,last_seen_at_epoch)
             VALUES ('local',?1,?2,'raw-session','raw-session','/repo','/repo','active',1,1,1,1,1)",
            params![path, host],
        )?;
        let identity = conn.last_insert_rowid();
        conn.execute("UPDATE raw_messages SET transcript_identity_id=?1,transcript_record_ordinal=1 WHERE id=?2",
            params![identity,raw.id])?;
        db::record_captured_event(
            &conn,
            &db::CaptureEventInput {
                host,
                session_id: "raw-session",
                project: "/repo",
                cwd: None,
                event_type: "session_stop",
                role: None,
                tool_name: None,
                content: "{\"transcript_path\":\"/tmp/source.jsonl\"}",
                task_kind: Some(db::ExtractionTaskKind::SessionRollup),
            },
        )?;
    }
    opts.query = Some("unique raw evidence".into());
    let report = build_report(&conn, &opts)?;
    assert_eq!(evidence(&report, 0, "captured_event").len(), 1);
    assert_eq!(
        evidence(&report, 0, "captured_event")[0]["host"],
        "codex-cli"
    );
    assert_eq!(evidence(&report, 1, "extraction_task").len(), 1);
    opts.query = None;
    opts.session_id = Some("raw-session".into());
    opts.host = Some("codex-cli".into());
    opts.source_root = Some("local".into());
    let report = build_report(&conn, &opts)?;
    assert_eq!(evidence(&report, 0, "raw_session")[0]["sample_count"], 1);
    assert_eq!(
        evidence(&report, 0, "raw_session")[0]["preview"],
        "unique raw evidence"
    );
    Ok(())
}
