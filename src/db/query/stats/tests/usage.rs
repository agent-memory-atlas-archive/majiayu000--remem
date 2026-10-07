use super::setup_stats_schema;
use crate::db::{
    query_ai_usage_breakdown, query_ai_usage_source_totals, query_ai_usage_totals,
    query_daily_ai_usage, query_weekly_ai_usage, AiUsageBreakdown, AiUsageSourceTotals,
    AiUsageTotals, DailyAiUsage, WeeklyAiUsage,
};
use rusqlite::Connection;

fn insert_usage(
    conn: &Connection,
    project: &str,
    created_at_epoch: i64,
    input_tokens: i64,
    output_tokens: i64,
    reasoning_tokens: i64,
    cache_read_tokens: i64,
    estimated_cost_usd: f64,
) {
    insert_usage_with_source(
        conn,
        Some(project),
        created_at_epoch,
        "codex-cli",
        input_tokens,
        output_tokens,
        reasoning_tokens,
        cache_read_tokens,
        estimated_cost_usd,
        "codex_log",
        "remem_static",
    );
}

fn insert_usage_with_source(
    conn: &Connection,
    project: Option<&str>,
    created_at_epoch: i64,
    executor: &str,
    input_tokens: i64,
    output_tokens: i64,
    reasoning_tokens: i64,
    cache_read_tokens: i64,
    estimated_cost_usd: f64,
    usage_source: &str,
    pricing_source: &str,
) {
    conn.execute(
        "INSERT INTO ai_usage_events
         (created_at, created_at_epoch, project, operation, executor, model,
          input_tokens, output_tokens, reasoning_tokens, cache_read_tokens, total_tokens,
          estimated_cost_usd, usage_source, pricing_source)
         VALUES ('2026-01-01T00:00:00Z', ?1, ?2, 'summary', ?3, 'codex-default',
                 ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        rusqlite::params![
            created_at_epoch,
            project,
            executor,
            input_tokens,
            output_tokens,
            reasoning_tokens,
            cache_read_tokens,
            input_tokens + output_tokens + reasoning_tokens + cache_read_tokens,
            estimated_cost_usd,
            usage_source,
            pricing_source
        ],
    )
    .expect("usage insert should succeed");
}

#[test]
fn query_ai_usage_groups_daily_and_weekly_token_costs() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory().expect("in-memory db should open");
    setup_stats_schema(&conn);

    let jan_05_2026 = 1_767_571_200;
    let jan_06_2026 = 1_767_657_600;
    let jan_12_2026 = 1_768_176_000;

    insert_usage(&conn, "alpha", jan_05_2026, 100, 40, 10, 50, 0.001);
    insert_usage(&conn, "alpha", jan_05_2026 + 60, 200, 60, 20, 80, 0.002);
    insert_usage(&conn, "alpha", jan_06_2026, 300, 80, 30, 120, 0.003);
    insert_usage(&conn, "beta", jan_12_2026, 500, 100, 40, 160, 0.005);

    let alpha_totals = query_ai_usage_totals(&conn, Some(jan_05_2026), Some("alpha"))
        .expect("usage totals should load");
    assert_eq!(
        alpha_totals,
        AiUsageTotals {
            calls: 3,
            input_tokens: 600,
            output_tokens: 180,
            reasoning_tokens: 60,
            cache_creation_tokens: 0,
            cache_read_tokens: 250,
            total_tokens: 1090,
            estimated_cost_usd: 0.006,
            coverage: legacy_coverage(3),
        }
    );

    let alpha_sources = query_ai_usage_source_totals(&conn, Some(jan_05_2026), Some("alpha"))
        .expect("usage source totals should load");
    assert_eq!(
        alpha_sources,
        vec![AiUsageSourceTotals {
            usage_source: "codex_log".to_string(),
            pricing_source: "remem_static".to_string(),
            calls: 3,
            total_tokens: 1090,
            estimated_cost_usd: 0.006,
            coverage: legacy_coverage(3),
        }]
    );

    let alpha_breakdown = query_ai_usage_breakdown(&conn, Some(jan_05_2026), Some("alpha"), 10)?;
    assert_eq!(
        alpha_breakdown,
        vec![AiUsageBreakdown {
            project: Some("alpha".to_string()),
            executor: "codex-cli".to_string(),
            usage_source: "codex_log".to_string(),
            pricing_source: "remem_static".to_string(),
            calls: 3,
            total_tokens: 1090,
            estimated_cost_usd: 0.006,
            coverage: legacy_coverage(3),
        }]
    );

    let daily = query_daily_ai_usage(&conn, jan_05_2026, Some("alpha"), 14)
        .expect("daily usage should load");
    assert_eq!(
        daily,
        vec![
            DailyAiUsage {
                day: "2026-01-06".to_string(),
                calls: 1,
                input_tokens: 300,
                output_tokens: 80,
                reasoning_tokens: 30,
                cache_creation_tokens: 0,
                cache_read_tokens: 120,
                total_tokens: 530,
                estimated_cost_usd: 0.003,
                coverage: legacy_coverage(1),
            },
            DailyAiUsage {
                day: "2026-01-05".to_string(),
                calls: 2,
                input_tokens: 300,
                output_tokens: 100,
                reasoning_tokens: 30,
                cache_creation_tokens: 0,
                cache_read_tokens: 130,
                total_tokens: 560,
                estimated_cost_usd: 0.003,
                coverage: legacy_coverage(2),
            },
        ]
    );

    let weekly =
        query_weekly_ai_usage(&conn, jan_05_2026, None, 8).expect("weekly usage should load");
    assert_eq!(
        weekly,
        vec![
            WeeklyAiUsage {
                week: "2026-W02".to_string(),
                calls: 1,
                input_tokens: 500,
                output_tokens: 100,
                reasoning_tokens: 40,
                cache_creation_tokens: 0,
                cache_read_tokens: 160,
                total_tokens: 800,
                estimated_cost_usd: 0.005,
                coverage: legacy_coverage(1),
            },
            WeeklyAiUsage {
                week: "2026-W01".to_string(),
                calls: 3,
                input_tokens: 600,
                output_tokens: 180,
                reasoning_tokens: 60,
                cache_creation_tokens: 0,
                cache_read_tokens: 250,
                total_tokens: 1090,
                estimated_cost_usd: 0.006,
                coverage: legacy_coverage(3),
            },
        ]
    );
    Ok(())
}

#[test]
fn query_ai_usage_breakdown_exposes_project_executor_and_source() -> anyhow::Result<()> {
    let conn = Connection::open_in_memory().expect("in-memory db should open");
    setup_stats_schema(&conn);

    let jan_05_2026 = 1_767_571_200;
    insert_usage_with_source(
        &conn,
        Some("/Users/lifcc/.remem"),
        jan_05_2026,
        "cli",
        900,
        100,
        0,
        0,
        0.003,
        "text_estimate",
        "remem_static",
    );
    insert_usage_with_source(
        &conn,
        Some("alpha"),
        jan_05_2026 + 60,
        "codex-cli",
        100,
        50,
        0,
        25,
        0.001,
        "codex_log",
        "remem_static",
    );
    insert_usage_with_source(
        &conn,
        None,
        jan_05_2026 + 120,
        "http",
        80,
        20,
        0,
        0,
        0.0005,
        "anthropic_usage",
        "remem_static",
    );

    let breakdown = query_ai_usage_breakdown(&conn, Some(jan_05_2026), None, 10)?;

    assert_eq!(
        breakdown,
        vec![
            AiUsageBreakdown {
                project: Some("/Users/lifcc/.remem".to_string()),
                executor: "cli".to_string(),
                usage_source: "text_estimate".to_string(),
                pricing_source: "remem_static".to_string(),
                calls: 1,
                total_tokens: 1000,
                estimated_cost_usd: 0.003,
                coverage: legacy_coverage(1),
            },
            AiUsageBreakdown {
                project: Some("alpha".to_string()),
                executor: "codex-cli".to_string(),
                usage_source: "codex_log".to_string(),
                pricing_source: "remem_static".to_string(),
                calls: 1,
                total_tokens: 175,
                estimated_cost_usd: 0.001,
                coverage: legacy_coverage(1),
            },
            AiUsageBreakdown {
                project: None,
                executor: "http".to_string(),
                usage_source: "anthropic_usage".to_string(),
                pricing_source: "remem_static".to_string(),
                calls: 1,
                total_tokens: 100,
                estimated_cost_usd: 0.0005,
                coverage: legacy_coverage(1),
            },
        ]
    );

    let limited = query_ai_usage_breakdown(&conn, Some(jan_05_2026), None, 1)?;
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].project.as_deref(), Some("/Users/lifcc/.remem"));

    let empty = query_ai_usage_breakdown(&conn, Some(jan_05_2026), None, 0)?;
    assert!(empty.is_empty());
    Ok(())
}

fn legacy_coverage(calls: i64) -> crate::db::AiUsageCoverage {
    crate::db::AiUsageCoverage {
        legacy_unverified_calls: calls,
        cost_incomplete_calls: calls,
        ..Default::default()
    }
}
