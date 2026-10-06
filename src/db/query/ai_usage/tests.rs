use anyhow::Result;
use rusqlite::{params, Connection};

use super::*;

fn insert(
    conn: &Connection,
    project: &str,
    epoch: i64,
    status: &str,
    outcome: &str,
    pricing: &str,
    cost_status: &str,
    cost: f64,
) -> Result<()> {
    conn.execute("INSERT INTO ai_usage_events(created_at,created_at_epoch,project,operation,executor,model,input_tokens,output_tokens,total_tokens,estimated_cost_usd,usage_source,pricing_source,usage_status,attempt_outcome,cost_status)
        VALUES('fixture',?1,?2,'fixture','codex-cli','fixture-model',100,40,140,?3,'codex_log',?4,?5,?6,?7)",
        params![epoch, project, cost, pricing, status, outcome, cost_status])?;
    Ok(())
}

#[test]
fn usage_coverage_follows_identical_project_time_and_group_filters() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    crate::migrate::run_migrations(&conn)?;
    let epoch = 1_767_571_200;
    for (status, outcome, pricing, cost_status, cost) in [
        ("complete", "success", "remem_static", "complete", 0.123),
        ("complete", "success", "unknown_pricing", "unpriced", 0.0),
        ("partial", "success", "remem_static", "partial", 0.001),
        ("missing", "failed", "remem_static", "partial", 0.0),
        ("invalid", "failed", "remem_static", "partial", 0.0),
        ("estimated", "success", "remem_static", "complete", 0.005),
        (
            "legacy_unverified",
            "unknown",
            "remem_static",
            "legacy_unverified",
            0.007,
        ),
    ] {
        insert(
            &conn,
            "/wanted",
            epoch,
            status,
            outcome,
            pricing,
            cost_status,
            cost,
        )?;
    }
    insert(
        &conn,
        "/other",
        epoch,
        "complete",
        "success",
        "remem_static",
        "complete",
        7.0,
    )?;
    insert(
        &conn,
        "/wanted",
        epoch - 100_000,
        "complete",
        "success",
        "remem_static",
        "complete",
        5.0,
    )?;
    let totals = query_ai_usage_totals(&conn, Some(epoch), Some("/wanted"))?;
    assert_eq!(totals.calls, 7);
    assert!((totals.estimated_cost_usd - 0.136).abs() < 1e-12);
    let coverage = &totals.coverage;
    assert_eq!(coverage.complete_calls, 2);
    assert_eq!(
        (
            coverage.partial_calls,
            coverage.missing_calls,
            coverage.invalid_calls
        ),
        (1, 1, 1)
    );
    assert_eq!(
        (
            coverage.estimated_calls,
            coverage.legacy_unverified_calls,
            coverage.failed_calls
        ),
        (1, 1, 2)
    );
    assert_eq!(
        (coverage.unpriced_calls, coverage.cost_incomplete_calls),
        (1, 5)
    );
    assert!(!coverage.cost_complete());
    let daily = query_daily_ai_usage(&conn, epoch, Some("/wanted"), 10)?;
    let weekly = query_weekly_ai_usage(&conn, epoch, Some("/wanted"), 10)?;
    assert_eq!(&daily[0].coverage, coverage);
    assert_eq!(&weekly[0].coverage, coverage);
    let sources = query_ai_usage_source_totals(&conn, Some(epoch), Some("/wanted"))?;
    let unknown = sources
        .iter()
        .find(|source| source.pricing_source == "unknown_pricing")
        .unwrap();
    assert_eq!(unknown.estimated_cost_usd, 0.0);
    assert_eq!(unknown.coverage.unpriced_calls, 1);
    assert!(!unknown.coverage.cost_complete());
    let breakdown = query_ai_usage_breakdown(&conn, Some(epoch), Some("/wanted"), 10)?;
    assert_eq!(
        breakdown
            .iter()
            .map(|row| row.coverage.cost_incomplete_calls)
            .sum::<i64>(),
        5
    );
    let empty = query_ai_usage_totals(&conn, Some(epoch), Some("/absent"))?;
    assert_eq!(empty.calls, 0);
    assert!(empty.coverage.cost_complete());
    Ok(())
}

#[test]
fn usage_coverage_migration_preserves_legacy_cost_without_inventing_completeness() -> Result<()> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch("CREATE TABLE ai_usage_events(id INTEGER PRIMARY KEY, total_tokens INTEGER, estimated_cost_usd REAL, usage_source TEXT, pricing_source TEXT);
        INSERT INTO ai_usage_events VALUES(1,140,0.123,'codex_log','remem_static');")?;
    let legacy = ai_usage_coverage_select(&conn)?;
    let coverage = conn.query_row(
        &format!("SELECT {legacy} FROM ai_usage_events"),
        [],
        |row| ai_usage_coverage_from_row(row, 0),
    )?;
    assert_eq!(
        (
            coverage.legacy_unverified_calls,
            coverage.cost_incomplete_calls
        ),
        (1, 1)
    );
    conn.execute_batch(include_str!(
        "../../../migrations/v096_ai_usage_observation.sql"
    ))?;
    let stored: (i64, f64, String, String) = conn.query_row("SELECT total_tokens, estimated_cost_usd, usage_status, attempt_outcome FROM ai_usage_events", [],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
    assert_eq!(
        stored,
        (140, 0.123, "legacy_unverified".into(), "unknown".into())
    );
    let current = ai_usage_coverage_select(&conn)?;
    assert_eq!(
        coverage,
        conn.query_row(
            &format!("SELECT {current} FROM ai_usage_events"),
            [],
            |row| ai_usage_coverage_from_row(row, 0)
        )?
    );
    assert!(conn
        .execute("UPDATE ai_usage_events SET usage_status='fabricated'", [])
        .is_err());
    assert!(conn
        .execute("UPDATE ai_usage_events SET usage_details_json='[]'", [])
        .is_err());
    Ok(())
}
