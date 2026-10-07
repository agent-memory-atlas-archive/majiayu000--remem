use anyhow::Result;
use rusqlite::{Connection, Row};

use crate::db::AiUsageCoverage;

const COVERAGE_SELECT: &str = "
    COALESCE(SUM(usage_status = 'complete'), 0),
    COALESCE(SUM(usage_status = 'partial'), 0),
    COALESCE(SUM(usage_status = 'missing'), 0),
    COALESCE(SUM(usage_status = 'invalid'), 0),
    COALESCE(SUM(usage_status = 'estimated'), 0),
    COALESCE(SUM(usage_status = 'legacy_unverified'), 0),
    COALESCE(SUM(attempt_outcome = 'failed'), 0),
    COALESCE(SUM(cost_status = 'unpriced' OR pricing_source IN ('unknown_pricing', 'invalid_pricing')), 0),
    COALESCE(SUM(cost_status <> 'complete' OR pricing_source IN ('unknown_pricing', 'invalid_pricing')
        OR usage_status IN ('estimated', 'missing', 'invalid', 'legacy_unverified')), 0)";

/// Read-only legacy surfaces can predate v096. Preserve their costs, but do
/// not infer complete observations from old provenance labels.
pub(crate) fn ai_usage_coverage_select(conn: &Connection) -> Result<String> {
    let columns = conn
        .prepare("PRAGMA table_info(ai_usage_events)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let present = ["usage_status", "attempt_outcome", "cost_status"]
        .iter()
        .filter(|name| columns.iter().any(|column| column == **name))
        .count();
    if present == 3 {
        return Ok(COVERAGE_SELECT.into());
    }
    anyhow::ensure!(present == 0, "AI usage observation schema is incomplete");
    let unpriced = if columns.iter().any(|column| column == "pricing_source") {
        "COALESCE(SUM(pricing_source IN ('unknown_pricing', 'invalid_pricing')), 0)"
    } else {
        "0"
    };
    Ok(format!("0, 0, 0, 0, 0, COUNT(*), 0, {unpriced}, COUNT(*)"))
}

pub(crate) fn ai_usage_coverage_from_row(
    row: &Row<'_>,
    start: usize,
) -> rusqlite::Result<AiUsageCoverage> {
    Ok(AiUsageCoverage {
        complete_calls: row.get(start)?,
        partial_calls: row.get(start + 1)?,
        missing_calls: row.get(start + 2)?,
        invalid_calls: row.get(start + 3)?,
        estimated_calls: row.get(start + 4)?,
        legacy_unverified_calls: row.get(start + 5)?,
        failed_calls: row.get(start + 6)?,
        unpriced_calls: row.get(start + 7)?,
        cost_incomplete_calls: row.get(start + 8)?,
    })
}
