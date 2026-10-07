use rusqlite::Connection;

use super::{DailyActivityStats, ProjectCount, SystemStats};
use crate::db::query::{
    query_daily_activity_stats, query_memory_facts_stats, query_system_stats, query_top_projects,
};
use crate::db::PoisoningDefenseStats as PDS;
use crate::db::{FailureLifecycleStats, FailureSurfaceStats};

mod candidate_promotion;
mod legacy_surfaces;
mod usage;

fn setup_stats_schema(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE memories (
            id INTEGER PRIMARY KEY,
            project TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at_epoch INTEGER NOT NULL,
            expires_at_epoch INTEGER
        );
        CREATE TABLE observations (
            id INTEGER PRIMARY KEY,
            project TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at_epoch INTEGER NOT NULL
        );
        CREATE TABLE observations_fts (rowid INTEGER PRIMARY KEY, title TEXT);
        CREATE TABLE session_summaries (id INTEGER PRIMARY KEY, created_at_epoch INTEGER NOT NULL, poisoning_status TEXT NOT NULL DEFAULT 'safe', poisoning_block_count INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE memory_poisoning_injection_drops (id INTEGER PRIMARY KEY, memory_id INTEGER NOT NULL, pattern_id TEXT NOT NULL, pattern_version INTEGER NOT NULL, created_at_epoch INTEGER NOT NULL);
        CREATE TABLE raw_messages (
            id INTEGER PRIMARY KEY,
            created_at_epoch INTEGER NOT NULL
        );
        CREATE TABLE raw_ingest_failures (
            id INTEGER PRIMARY KEY,
            transcript_path TEXT,
            error_kind TEXT NOT NULL,
            error_message TEXT NOT NULL,
            parse_errors INTEGER NOT NULL,
            insert_errors INTEGER NOT NULL,
            created_at_epoch INTEGER NOT NULL
        );
        CREATE TABLE captured_events (
            id INTEGER PRIMARY KEY,
            created_at_epoch INTEGER NOT NULL,
            inserted_at_epoch INTEGER NOT NULL
        );
        CREATE TABLE memory_facts (
            id INTEGER PRIMARY KEY,
            status TEXT NOT NULL,
            valid_from_epoch INTEGER,
            source_memory_id INTEGER
        );
        CREATE TABLE capture_drop_events (
            id INTEGER PRIMARY KEY,
            host_id TEXT,
            session_id TEXT,
            project TEXT,
            tool_name TEXT,
            reason TEXT NOT NULL,
            detail TEXT,
            spill_path TEXT,
            recovered_event_id INTEGER,
            created_at_epoch INTEGER NOT NULL,
            recovered_at_epoch INTEGER
        );
        CREATE TABLE extraction_tasks (
            id INTEGER PRIMARY KEY,
            status TEXT NOT NULL,
            created_at_epoch INTEGER NOT NULL,
            lease_expires_epoch INTEGER,
            replay_range_id INTEGER
        );
        CREATE TABLE extraction_replay_ranges (id INTEGER PRIMARY KEY, status TEXT NOT NULL);
        CREATE TABLE memory_candidates (
            id INTEGER PRIMARY KEY,
            source_kind TEXT NOT NULL DEFAULT 'unattributed',
            review_status TEXT NOT NULL,
            auto_promote_block_reason TEXT,
            created_at_epoch INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE graph_candidates (
            id INTEGER PRIMARY KEY,
            review_status TEXT NOT NULL
        );
        CREATE TABLE pending_observations (
            id INTEGER PRIMARY KEY,
            status TEXT NOT NULL,
            created_at_epoch INTEGER NOT NULL DEFAULT 0,
            updated_at_epoch INTEGER NOT NULL DEFAULT 0,
            next_retry_epoch INTEGER,
            lease_owner TEXT,
            lease_expires_epoch INTEGER
        );
        CREATE TABLE jobs (
            id INTEGER PRIMARY KEY,
            job_type TEXT NOT NULL,
            state TEXT NOT NULL,
            lease_expires_epoch INTEGER,
            created_at_epoch INTEGER NOT NULL,
            updated_at_epoch INTEGER NOT NULL,
            failure_class TEXT,
            failed_at_epoch INTEGER,
            archived_at_epoch INTEGER
        );
        CREATE TABLE worker_heartbeats (
            owner TEXT PRIMARY KEY,
            pid INTEGER,
            started_at_epoch INTEGER NOT NULL,
            updated_at_epoch INTEGER NOT NULL
        );
        CREATE TABLE ai_usage_events (
            id INTEGER PRIMARY KEY,
            created_at TEXT NOT NULL,
            created_at_epoch INTEGER NOT NULL,
            project TEXT,
            operation TEXT NOT NULL,
            executor TEXT NOT NULL,
            model TEXT,
            input_tokens INTEGER NOT NULL,
            output_tokens INTEGER NOT NULL,
            reasoning_tokens INTEGER NOT NULL DEFAULT 0,
            cache_creation_tokens INTEGER NOT NULL DEFAULT 0,
            cache_read_tokens INTEGER NOT NULL DEFAULT 0,
            raw_input_tokens INTEGER NOT NULL DEFAULT 0,
            raw_output_tokens INTEGER NOT NULL DEFAULT 0,
            total_tokens INTEGER NOT NULL,
            estimated_cost_usd REAL NOT NULL,
            usage_source TEXT NOT NULL DEFAULT 'text_estimate',
            pricing_source TEXT NOT NULL DEFAULT 'remem_static'
        );",
    )
    .expect("schema should be created");
}

#[test]
fn query_system_stats_and_related_views_share_one_definition() {
    let conn = Connection::open_in_memory().expect("in-memory db should open");
    setup_stats_schema(&conn);

    conn.execute(
        "INSERT INTO memories (project, status, created_at_epoch) VALUES ('alpha', 'active', 200)",
        [],
    )
    .expect("active memory insert should succeed");
    conn.execute(
        "INSERT INTO memories (project, status, created_at_epoch) VALUES ('alpha', 'archived', 150)",
        [],
    )
    .expect("archived memory insert should succeed");
    conn.execute(
        "INSERT INTO memories (project, status, created_at_epoch) VALUES ('beta', 'active', 300)",
        [],
    )
    .expect("second active memory insert should succeed");
    if let Err(err) = conn.execute(
        "INSERT INTO memories (project, status, created_at_epoch, expires_at_epoch)
         VALUES ('gamma', 'active', 310, strftime('%s', 'now') - 1)",
        [],
    ) {
        panic!("expired active memory insert should succeed: {err}");
    }
    if let Err(err) = conn.execute(
        "INSERT INTO memories (project, status, created_at_epoch, expires_at_epoch)
         VALUES ('delta', 'active', 320, CAST(strftime('%s', 'now') AS INTEGER) + 3600)",
        [],
    ) {
        panic!("future-expiring active memory insert should succeed: {err}");
    }
    conn.execute(
        "INSERT INTO observations (project, status, created_at_epoch) VALUES ('alpha', 'active', 220)",
        [],
    )
    .expect("active observation insert should succeed");
    conn.execute(
        "INSERT INTO observations (project, status, created_at_epoch) VALUES ('beta', 'stale', 140)",
        [],
    )
    .expect("stale observation insert should succeed");
    conn.execute(
        "INSERT INTO observations_fts (rowid, title) VALUES (1, 'active observation')",
        [],
    )
    .expect("observation fts insert should succeed");
    conn.execute(
        "INSERT INTO session_summaries (id, created_at_epoch) VALUES (1, 230)",
        [],
    )
    .expect("summary insert should succeed");
    conn.execute(
        "INSERT INTO raw_ingest_failures
         (transcript_path, error_kind, error_message, parse_errors, insert_errors, created_at_epoch)
         VALUES ('/bad/transcript.jsonl', 'parse_errors', 'bad jsonl', 2, 1, 160)",
        [],
    )
    .expect("raw ingest failure insert should succeed");
    conn.execute(
        "INSERT INTO captured_events (id, created_at_epoch, inserted_at_epoch) VALUES (1, 120, 130)",
        [],
    )
    .expect("captured event insert should succeed");
    conn.execute(
        "INSERT INTO capture_drop_events
         (host_id, session_id, project, tool_name, reason, detail, created_at_epoch)
         VALUES ('codex-cli', 'session-drop', 'alpha', 'Bash', 'codex_bash_disabled',
                 'Codex Bash capture disabled', 170)",
        [],
    )
    .expect("capture drop insert should succeed");
    conn.execute(
        "INSERT INTO extraction_tasks (status, created_at_epoch) VALUES ('pending', 90)",
        [],
    )
    .expect("pending extraction task insert should succeed");
    conn.execute(
        "INSERT INTO extraction_tasks (status, created_at_epoch, lease_expires_epoch)
         VALUES ('processing', 95, strftime('%s', 'now') - 1)",
        [],
    )
    .expect("processing extraction task insert should succeed");
    conn.execute(
        "INSERT INTO extraction_tasks (status, created_at_epoch) VALUES ('failed', 96)",
        [],
    )
    .expect("failed extraction task insert should succeed");
    conn.execute(
        "INSERT INTO extraction_replay_ranges (status) VALUES ('pending'), ('failed'), ('requeued'), ('quarantined')",
        [],
    )
    .expect("extraction replay range insert should succeed");
    conn.execute(
        "INSERT INTO memory_candidates (review_status) VALUES ('pending_review')",
        [],
    )
    .expect("memory candidate insert should succeed");
    conn.execute(
        "INSERT INTO graph_candidates (review_status) VALUES ('pending_review')",
        [],
    )
    .expect("graph candidate insert should succeed");
    if let Err(err) = conn.execute(
        "INSERT INTO graph_candidates (review_status) VALUES ('deferred')",
        [],
    ) {
        panic!("deferred graph candidate insert should succeed: {err}");
    }
    conn.execute(
        "INSERT INTO graph_candidates (review_status) VALUES ('approved')",
        [],
    )
    .expect("approved graph candidate insert should succeed");
    conn.execute_batch(
        "INSERT INTO pending_observations (status, created_at_epoch) VALUES ('pending', 100);
         INSERT INTO pending_observations (status, created_at_epoch) VALUES ('pending', 120);
         UPDATE pending_observations
         SET next_retry_epoch = strftime('%s', 'now') + 3600
         WHERE id = 2;
         INSERT INTO pending_observations (status, created_at_epoch, lease_owner, lease_expires_epoch)
         VALUES ('processing', 130, 'worker-a', strftime('%s', 'now') - 1);
         INSERT INTO pending_observations (status, created_at_epoch) VALUES ('failed', 140);
         INSERT INTO pending_observations (status, created_at_epoch, updated_at_epoch)
         VALUES ('migrated', 500, 0);",
    )
    .expect("pending observation fixtures should insert");
    conn.execute(
        "INSERT INTO jobs (job_type, state, lease_expires_epoch, created_at_epoch, updated_at_epoch)
         VALUES ('compress', 'pending', NULL, 150, 150)",
        [],
    )
    .expect("pending job insert should succeed");
    conn.execute(
        "INSERT INTO jobs (job_type, state, lease_expires_epoch, created_at_epoch, updated_at_epoch)
         VALUES ('summary', 'processing', 0, 260, 265)",
        [],
    )
    .expect("stuck job insert should succeed");
    conn.execute(
        "INSERT INTO jobs (job_type, state, lease_expires_epoch, created_at_epoch, updated_at_epoch)
         VALUES ('summary', 'failed', NULL, 280, 285)",
        [],
    )
    .expect("failed job insert should succeed");
    conn.execute(
        "INSERT INTO jobs
         (job_type, state, lease_expires_epoch, created_at_epoch, updated_at_epoch,
          archived_at_epoch)
         VALUES ('summary', 'failed', NULL, 300, 305, 400)",
        [],
    )
    .expect("archived summary job insert should succeed");
    conn.execute(
        "INSERT INTO worker_heartbeats (owner, pid, started_at_epoch, updated_at_epoch)
         VALUES ('worker-a', ?1, strftime('%s', 'now') - 10, strftime('%s', 'now') - 10)",
        [i64::from(std::process::id())],
    )
    .expect("heartbeat insert should succeed");

    let system = query_system_stats(&conn).expect("system stats should load");
    assert_eq!(
        system,
        SystemStats {
            active_memories: 3,
            active_observations: 1,
            total_observations: 2,
            session_summaries: 1,
            raw_messages: 0,
            raw_ingest_failures: 1,
            raw_ingest_parse_errors: 2,
            raw_ingest_insert_errors: 1,
            latest_raw_ingest_failure_epoch: Some(160),
            latest_raw_ingest_failure_kind: Some("parse_errors".to_string()),
            latest_raw_ingest_failure_path: Some("/bad/transcript.jsonl".to_string()),
            latest_raw_ingest_failure_message: Some("bad jsonl".to_string()),
            captured_events: 1,
            latest_captured_event_epoch: Some(130),
            latest_capture_activity_epoch: Some(170),
            capture_drop_events: 1,
            actionable_capture_drops: 0,
            unrecovered_capture_spills: 0,
            latest_capture_drop_epoch: Some(170),
            latest_capture_drop_reason: Some("codex_bash_disabled".to_string()),
            latest_capture_drop_detail: Some("Codex Bash capture disabled".to_string()),
            pending_extraction_tasks: 1,
            processing_extraction_tasks: 1,
            expired_processing_extraction_tasks: 1,
            failed_extraction_tasks: 1,
            retryable_extraction_replay_ranges: 2,
            active_extraction_replay_ranges: 1,
            quarantined_extraction_replay_ranges: 1,
            oldest_pending_extraction_epoch: Some(90),
            pending_memory_candidates: 1,
            total_memory_candidates: 1,
            promoted_memory_candidates: 0,
            pending_review_memory_candidates: 1,
            pending_graph_candidates: 2,
            pending_observations: 2,
            ready_pending_observations: 1,
            delayed_pending_observations: 1,
            processing_pending_observations: 1,
            expired_processing_pending_observations: 1,
            failed_pending_observations: 1,
            oldest_ready_pending_epoch: Some(100),
            pending_jobs: 1,
            processing_jobs: 1,
            failed_jobs: 1,
            stuck_jobs: 1,
            failure_lifecycle: FailureLifecycleStats {
                pending_observation: FailureSurfaceStats {
                    actionable_total: 1,
                    transient: 1,
                    oldest_actionable_epoch: Some(140),
                    ..FailureSurfaceStats::default()
                },
                extraction_task: FailureSurfaceStats {
                    actionable_total: 1,
                    transient: 1,
                    oldest_actionable_epoch: Some(96),
                    ..FailureSurfaceStats::default()
                },
                extraction_replay_range: FailureSurfaceStats {
                    actionable_total: 3,
                    transient: 3,
                    oldest_actionable_epoch: Some(0),
                    ..FailureSurfaceStats::default()
                },
                job: FailureSurfaceStats {
                    actionable_total: 1,
                    transient: 1,
                    archived: 1,
                    oldest_actionable_epoch: Some(285),
                    ..FailureSurfaceStats::default()
                },
            },
            worker_daemon_healthy: true,
            worker_heartbeat_owner: Some("worker-a".to_string()),
            worker_heartbeat_age_secs: system.worker_heartbeat_age_secs,
            legacy_surfaces: legacy_surfaces::expected_fixture(),
            poisoning_defense: PDS {
                pattern_set_version: 1,
                ..PDS::default()
            },
        }
    );
    assert!(
        system.worker_heartbeat_age_secs.unwrap_or_default() <= 20,
        "heartbeat age should be recent"
    );

    let daily = query_daily_activity_stats(&conn, 180).expect("daily stats should load");
    assert_eq!(
        daily,
        DailyActivityStats {
            memories: 4,
            observations: 1,
        }
    );

    let top_projects = query_top_projects(&conn, 5).expect("top projects should load");
    assert_eq!(
        top_projects,
        vec![
            ProjectCount {
                project: "alpha".to_string(),
                count: 1,
            },
            ProjectCount {
                project: "beta".to_string(),
                count: 1,
            },
            ProjectCount {
                project: "delta".to_string(),
                count: 1,
            },
        ]
    );
}

#[test]
fn query_memory_facts_stats_excludes_expired_source_memories() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    setup_stats_schema(&conn);

    conn.execute_batch(
        "INSERT INTO memories (id, project, status, created_at_epoch, expires_at_epoch)
         VALUES
            (1, 'alpha', 'active', 100, NULL),
            (2, 'alpha', 'active', 110, CAST(strftime('%s', 'now') AS INTEGER) + 3600),
            (3, 'alpha', 'active', 120, CAST(strftime('%s', 'now') AS INTEGER) - 1),
            (4, 'alpha', 'archived', 130, NULL);
         INSERT INTO memory_facts (status, valid_from_epoch, source_memory_id)
         VALUES
            ('active', 100, 1),
            ('active', 110, 2),
            ('active', 120, 3),
            ('active', 130, 4),
            ('active', NULL, 1),
            ('stale', 140, 1);",
    )?;

    let stats = query_memory_facts_stats(&conn)?;

    assert!(stats.table_exists);
    assert_eq!(stats.total, 6);
    assert_eq!(stats.active_memories, 2);
    assert_eq!(stats.retrieval_eligible, 2);
    Ok(())
}

#[test]
fn query_memory_facts_stats_excludes_invalidated_active_facts() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    setup_stats_schema(&conn);
    conn.execute_batch("ALTER TABLE memory_facts ADD COLUMN invalidated_at_epoch INTEGER;")?;

    conn.execute_batch(
        "INSERT INTO memories (id, project, status, created_at_epoch, expires_at_epoch)
         VALUES (1, 'alpha', 'active', 100, NULL);
         INSERT INTO memory_facts
            (status, valid_from_epoch, source_memory_id, invalidated_at_epoch)
         VALUES
            ('active', 100, 1, NULL),
            ('active', 100, 1, 150);",
    )?;

    let stats = query_memory_facts_stats(&conn)?;

    assert_eq!(stats.total, 2);
    assert_eq!(stats.retrieval_eligible, 1);
    Ok(())
}

#[test]
fn query_system_stats_reports_daemon_heartbeat_not_once() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    setup_stats_schema(&conn);
    let now = chrono::Utc::now().timestamp();
    conn.execute(
        "INSERT INTO worker_heartbeats (owner, pid, started_at_epoch, updated_at_epoch)
         VALUES ('worker-daemon-stats', ?1, ?2, ?2)",
        (i64::from(std::process::id()), now - 10),
    )?;
    conn.execute(
        "INSERT INTO worker_heartbeats (owner, pid, started_at_epoch, updated_at_epoch)
         VALUES ('worker-once-stats', ?1, ?2, ?2)",
        (i64::from(std::process::id()), now),
    )?;

    let system = query_system_stats(&conn)?;

    assert!(system.worker_daemon_healthy);
    assert_eq!(
        system.worker_heartbeat_owner.as_deref(),
        Some("worker-daemon-stats")
    );
    assert!(
        system.worker_heartbeat_age_secs.unwrap_or_default() >= 10,
        "reported age should come from daemon heartbeat"
    );
    Ok(())
}

#[test]
fn query_system_stats_defaults_raw_ingest_failures_when_table_is_absent() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    setup_stats_schema(&conn);
    conn.execute("DROP TABLE raw_ingest_failures", [])?;

    let system = query_system_stats(&conn)?;

    assert_eq!(system.raw_ingest_failures, 0);
    assert_eq!(system.raw_ingest_parse_errors, 0);
    assert_eq!(system.raw_ingest_insert_errors, 0);
    assert_eq!(system.latest_raw_ingest_failure_epoch, None);
    Ok(())
}

#[test]
fn query_system_stats_defaults_capture_drops_when_table_is_absent() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    setup_stats_schema(&conn);
    conn.execute("DROP TABLE capture_drop_events", [])?;

    let system = query_system_stats(&conn)?;

    assert_eq!(system.capture_drop_events, 0);
    assert_eq!(system.actionable_capture_drops, 0);
    assert_eq!(system.unrecovered_capture_spills, 0);
    assert_eq!(system.latest_capture_drop_epoch, None);
    assert_eq!(system.latest_capture_drop_reason, None);
    Ok(())
}

#[test]
fn query_system_stats_defaults_replay_ranges_when_table_is_absent() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    setup_stats_schema(&conn);
    conn.execute("DROP TABLE extraction_replay_ranges", [])?;

    let system = query_system_stats(&conn)?;

    assert_eq!(system.retryable_extraction_replay_ranges, 0);
    assert_eq!(system.active_extraction_replay_ranges, 0);
    assert_eq!(system.quarantined_extraction_replay_ranges, 0);
    Ok(())
}
