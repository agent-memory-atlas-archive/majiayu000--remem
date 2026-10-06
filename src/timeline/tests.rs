use rusqlite::{params, Connection};

use super::{generate_timeline_report, generate_timeline_report_data};

fn setup_test_db(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE observations (
            id INTEGER PRIMARY KEY,
            memory_session_id TEXT NOT NULL,
            project TEXT,
            type TEXT NOT NULL,
            title TEXT,
            subtitle TEXT,
            narrative TEXT,
            facts TEXT,
            concepts TEXT,
            files_read TEXT,
            files_modified TEXT,
            prompt_number INTEGER,
            created_at TEXT,
            created_at_epoch INTEGER,
            discovery_tokens INTEGER DEFAULT 0,
            status TEXT DEFAULT 'active',
            last_accessed_epoch INTEGER
        );
        CREATE TABLE session_summaries (
            id INTEGER PRIMARY KEY,
            memory_session_id TEXT NOT NULL,
            project TEXT,
            request TEXT,
            completed TEXT,
            decisions TEXT,
            learned TEXT,
            next_steps TEXT,
            preferences TEXT,
            prompt_number INTEGER,
            created_at TEXT,
            created_at_epoch INTEGER,
            discovery_tokens INTEGER DEFAULT 0
        );
        CREATE TABLE memories (
            id INTEGER PRIMARY KEY,
            session_id TEXT,
            project TEXT NOT NULL,
            topic_key TEXT,
            title TEXT NOT NULL,
            content TEXT NOT NULL,
            memory_type TEXT NOT NULL,
            files TEXT,
            created_at_epoch INTEGER NOT NULL,
            updated_at_epoch INTEGER NOT NULL,
            status TEXT NOT NULL DEFAULT 'active'
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
            total_tokens INTEGER NOT NULL,
            estimated_cost_usd REAL NOT NULL
        );",
    )
    .unwrap();
}

#[test]
fn empty_project_produces_report() {
    let conn = Connection::open_in_memory().unwrap();
    setup_test_db(&conn);

    let report = generate_timeline_report(&conn, "tools/remem", false).unwrap();
    assert!(report.contains("# Journey Into tools/remem"));
    assert!(report.contains("Total observations: 0"));
}

#[test]
fn usage_timeline_reports_known_portion_and_coverage_with_matching_project() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    crate::migrate::run_migrations(&conn)?;
    for (project, pricing, cost_status, cost) in [
        ("/wanted", "remem_static", "complete", 0.123),
        ("/wanted", "unknown_pricing", "unpriced", 0.0),
        ("/other", "unknown_pricing", "unpriced", 0.0),
    ] {
        conn.execute("INSERT INTO ai_usage_events(created_at,created_at_epoch,project,operation,executor,input_tokens,output_tokens,total_tokens,estimated_cost_usd,usage_source,pricing_source,usage_status,attempt_outcome,cost_status)
            VALUES('fixture',1767571200,?1,'fixture','codex-cli',100,40,140,?2,'codex_log',?3,'complete','success',?4)", params![project,cost,pricing,cost_status])?;
    }
    let json = serde_json::to_value(generate_timeline_report_data(&conn, "/wanted", true)?)?;
    assert_eq!(json["token_economics"]["total_ai_cost"], 0.123);
    assert_eq!(
        json["token_economics"]["ai_usage_coverage"]["unpriced_calls"],
        1
    );
    assert_eq!(
        json["monthly_breakdown"][0]["ai_usage_coverage"]["cost_incomplete_calls"],
        1
    );
    let text = generate_timeline_report(&conn, "/wanted", true)?;
    assert!(text.contains("Known AI cost estimate: $0.12"));
    assert!(text.contains("1 incomplete calls, including 1 unpriced"));
    Ok(())
}

#[test]
fn summary_report_excludes_timeline() {
    let conn = Connection::open_in_memory().unwrap();
    setup_test_db(&conn);
    let now = chrono::Utc::now().timestamp();

    conn.execute(
        "INSERT INTO observations (memory_session_id, project, type, title, created_at_epoch, discovery_tokens) \
         VALUES ('s1', 'tools/remem', 'decision', 'Test observation', ?1, 100)",
        params![now],
    )
    .unwrap();

    let report = generate_timeline_report(&conn, "tools/remem", false).unwrap();
    assert!(report.contains("Total observations: 1"));
    assert!(!report.contains("## Timeline"));
    assert!(!report.contains("## Monthly Breakdown"));
}

#[test]
fn json_report_data_exposes_structured_fields() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    setup_test_db(&conn);
    let now = chrono::Utc::now().timestamp();

    conn.execute(
        "INSERT INTO observations (memory_session_id, project, type, title, created_at_epoch, discovery_tokens) \
         VALUES ('s1', 'tools/remem', 'decision', 'Structured report', ?1, 100)",
        params![now],
    )?;

    let report = generate_timeline_report_data(&conn, "tools/remem", true)?;
    let json = serde_json::to_value(report)?;

    assert_eq!(json["project"], "tools/remem");
    assert_eq!(json["overview"]["total_observations"], 1);
    assert_eq!(json["activity_by_type"][0]["obs_type"], "decision");
    assert_eq!(json["token_economics"]["total_discovery_tokens"], 100);
    assert!(json["recent_timeline"].is_array());
    assert!(json["monthly_breakdown"].is_array());
    Ok(())
}

#[test]
fn overview_counts_semantic_rollup_summary_rows_as_sessions() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory()?;
    setup_test_db(&conn);
    let now = chrono::Utc::now().timestamp();

    conn.execute(
        "INSERT INTO session_summaries
         (memory_session_id, project, request, completed, decisions, preferences,
          created_at, created_at_epoch)
         VALUES ('capture-rollup-1', 'tools/remem', 'retire legacy summary writer',
                 'SessionRollup wrote semantic fields',
                 'SessionRollup owns structured fields',
                 'Preserve preferences in rollup rows',
                 '2026-07-07', ?1)",
        params![now],
    )?;

    let report = generate_timeline_report_data(&conn, "tools/remem", false)?;
    let json = serde_json::to_value(report)?;

    assert_eq!(json["overview"]["total_sessions"], 1);
    Ok(())
}

#[test]
fn full_report_includes_timeline_and_monthly() {
    let conn = Connection::open_in_memory().unwrap();
    setup_test_db(&conn);
    let now = chrono::Utc::now().timestamp();

    conn.execute(
        "INSERT INTO observations (memory_session_id, project, type, title, created_at_epoch, discovery_tokens) \
         VALUES ('s1', 'tools/remem', 'decision', 'FTS5 switch', ?1, 500)",
        params![now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO observations (memory_session_id, project, type, title, created_at_epoch, discovery_tokens) \
         VALUES ('s1', 'tools/remem', 'bugfix', 'Fix search', ?1, 300)",
        params![now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO session_summaries (memory_session_id, project, request, created_at, created_at_epoch) \
         VALUES ('s1', 'tools/remem', 'analyze search', '2026-03-19', ?1)",
        params![now],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO memories (session_id, project, title, content, memory_type, created_at_epoch, updated_at_epoch) \
         VALUES ('s1', 'tools/remem', 'Test memory', 'content', 'decision', ?1, ?1)",
        params![now],
    )
    .unwrap();

    let report = generate_timeline_report(&conn, "tools/remem", true).unwrap();
    assert!(report.contains("## Timeline (recent first)"));
    assert!(report.contains("[decision] FTS5 switch"));
    assert!(report.contains("[bugfix] Fix search"));
    assert!(report.contains("## Monthly Breakdown"));
    assert!(report.contains("Total observations: 2"));
    assert!(report.contains("Total sessions: 1"));
    assert!(report.contains("Total memories: 1"));
}
