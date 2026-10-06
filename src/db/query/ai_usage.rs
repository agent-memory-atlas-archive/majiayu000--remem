use anyhow::Result;
use rusqlite::{params, Connection};

use super::ai_usage_coverage::{ai_usage_coverage_from_row, ai_usage_coverage_select};
use super::shared::collect_rows;
use crate::db::{
    AiUsageBreakdown, AiUsageSourceTotals, AiUsageTotals, DailyAiUsage, WeeklyAiUsage,
};

pub fn query_ai_usage_totals(
    conn: &Connection,
    since_epoch: Option<i64>,
    project: Option<&str>,
) -> Result<AiUsageTotals> {
    let coverage = ai_usage_coverage_select(conn)?;
    conn.query_row(
        &format!(
            "SELECT COUNT(*),
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(reasoning_tokens), 0),
                COALESCE(SUM(cache_creation_tokens), 0),
                COALESCE(SUM(cache_read_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(estimated_cost_usd), 0.0), {coverage}
         FROM ai_usage_events
         WHERE (?1 IS NULL OR created_at_epoch >= ?1)
           AND (?2 IS NULL OR project = ?2)"
        ),
        params![since_epoch, project],
        |row| {
            Ok(AiUsageTotals {
                calls: row.get(0)?,
                input_tokens: row.get(1)?,
                output_tokens: row.get(2)?,
                reasoning_tokens: row.get(3)?,
                cache_creation_tokens: row.get(4)?,
                cache_read_tokens: row.get(5)?,
                total_tokens: row.get(6)?,
                estimated_cost_usd: row.get(7)?,
                coverage: ai_usage_coverage_from_row(row, 8)?,
            })
        },
    )
    .map_err(Into::into)
}

pub fn query_ai_usage_source_totals(
    conn: &Connection,
    since_epoch: Option<i64>,
    project: Option<&str>,
) -> Result<Vec<AiUsageSourceTotals>> {
    let coverage = ai_usage_coverage_select(conn)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT usage_source,
                pricing_source,
                COUNT(*),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(estimated_cost_usd), 0.0), {coverage}
         FROM ai_usage_events
         WHERE (?1 IS NULL OR created_at_epoch >= ?1)
           AND (?2 IS NULL OR project = ?2)
         GROUP BY usage_source, pricing_source
         ORDER BY SUM(total_tokens) DESC"
    ))?;
    let rows = stmt.query_map(params![since_epoch, project], |row| {
        Ok(AiUsageSourceTotals {
            usage_source: row.get(0)?,
            pricing_source: row.get(1)?,
            calls: row.get(2)?,
            total_tokens: row.get(3)?,
            estimated_cost_usd: row.get(4)?,
            coverage: ai_usage_coverage_from_row(row, 5)?,
        })
    })?;
    collect_rows(rows)
}

pub fn query_ai_usage_breakdown(
    conn: &Connection,
    since_epoch: Option<i64>,
    project: Option<&str>,
    limit: i64,
) -> Result<Vec<AiUsageBreakdown>> {
    if limit <= 0 {
        return Ok(Vec::new());
    }

    let coverage = ai_usage_coverage_select(conn)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT project,
                executor,
                usage_source,
                pricing_source,
                COUNT(*),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(estimated_cost_usd), 0.0), {coverage}
         FROM ai_usage_events
         WHERE (?1 IS NULL OR created_at_epoch >= ?1)
           AND (?2 IS NULL OR project = ?2)
         GROUP BY project, executor, usage_source, pricing_source
         ORDER BY SUM(estimated_cost_usd) DESC,
                  SUM(total_tokens) DESC,
                  COUNT(*) DESC,
                  project ASC,
                  executor ASC
         LIMIT ?3"
    ))?;
    let rows = stmt.query_map(params![since_epoch, project, limit], |row| {
        Ok(AiUsageBreakdown {
            project: row.get(0)?,
            executor: row.get(1)?,
            usage_source: row.get(2)?,
            pricing_source: row.get(3)?,
            calls: row.get(4)?,
            total_tokens: row.get(5)?,
            estimated_cost_usd: row.get(6)?,
            coverage: ai_usage_coverage_from_row(row, 7)?,
        })
    })?;
    collect_rows(rows)
}

pub fn query_daily_ai_usage(
    conn: &Connection,
    since_epoch: i64,
    project: Option<&str>,
    limit: i64,
) -> Result<Vec<DailyAiUsage>> {
    let coverage = ai_usage_coverage_select(conn)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT strftime('%Y-%m-%d', created_at_epoch, 'unixepoch') AS day,
                COUNT(*),
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(reasoning_tokens), 0),
                COALESCE(SUM(cache_creation_tokens), 0),
                COALESCE(SUM(cache_read_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(estimated_cost_usd), 0.0), {coverage}
         FROM ai_usage_events
         WHERE created_at_epoch >= ?1
           AND (?2 IS NULL OR project = ?2)
         GROUP BY day
         ORDER BY day DESC
         LIMIT ?3"
    ))?;
    let rows = stmt.query_map(params![since_epoch, project, limit], |row| {
        Ok(DailyAiUsage {
            day: row.get(0)?,
            calls: row.get(1)?,
            input_tokens: row.get(2)?,
            output_tokens: row.get(3)?,
            reasoning_tokens: row.get(4)?,
            cache_creation_tokens: row.get(5)?,
            cache_read_tokens: row.get(6)?,
            total_tokens: row.get(7)?,
            estimated_cost_usd: row.get(8)?,
            coverage: ai_usage_coverage_from_row(row, 9)?,
        })
    })?;
    collect_rows(rows)
}

pub fn query_weekly_ai_usage(
    conn: &Connection,
    since_epoch: i64,
    project: Option<&str>,
    limit: i64,
) -> Result<Vec<WeeklyAiUsage>> {
    let coverage = ai_usage_coverage_select(conn)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT strftime('%Y-W%W', created_at_epoch, 'unixepoch') AS week,
                COUNT(*),
                COALESCE(SUM(input_tokens), 0),
                COALESCE(SUM(output_tokens), 0),
                COALESCE(SUM(reasoning_tokens), 0),
                COALESCE(SUM(cache_creation_tokens), 0),
                COALESCE(SUM(cache_read_tokens), 0),
                COALESCE(SUM(total_tokens), 0),
                COALESCE(SUM(estimated_cost_usd), 0.0), {coverage}
         FROM ai_usage_events
         WHERE created_at_epoch >= ?1
           AND (?2 IS NULL OR project = ?2)
         GROUP BY week
         ORDER BY week DESC
         LIMIT ?3"
    ))?;
    let rows = stmt.query_map(params![since_epoch, project, limit], |row| {
        Ok(WeeklyAiUsage {
            week: row.get(0)?,
            calls: row.get(1)?,
            input_tokens: row.get(2)?,
            output_tokens: row.get(3)?,
            reasoning_tokens: row.get(4)?,
            cache_creation_tokens: row.get(5)?,
            cache_read_tokens: row.get(6)?,
            total_tokens: row.get(7)?,
            estimated_cost_usd: row.get(8)?,
            coverage: ai_usage_coverage_from_row(row, 9)?,
        })
    })?;
    collect_rows(rows)
}

#[cfg(test)]
mod tests;
