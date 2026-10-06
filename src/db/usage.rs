use anyhow::Result;
use rusqlite::{params, Connection};

/// Storage-owned projection. AI adapters supply evidence without making
/// persistence depend on an application-layer usage type.
pub(crate) struct AiUsageRecord<'a> {
    pub project: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub operation: &'a str,
    pub executor: &'a str,
    pub model: Option<&'a str>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    pub raw_input_tokens: i64,
    pub raw_output_tokens: i64,
    pub total_tokens: i64,
    pub estimated_cost_usd: f64,
    pub usage_source: &'a str,
    pub pricing_source: &'a str,
    pub usage_status: &'a str,
    pub attempt_outcome: &'a str,
    pub cost_status: &'a str,
    pub usage_details_json: &'a str,
}

/// Record one dispatched AI attempt, including failed/missing observations.
pub(crate) fn record_ai_usage_observation(
    conn: &Connection,
    record: &AiUsageRecord<'_>,
) -> Result<()> {
    let now = chrono::Utc::now();
    let created_at = now.to_rfc3339();
    let created_at_epoch = now.timestamp();
    anyhow::ensure!(
        record.estimated_cost_usd.is_finite() && record.estimated_cost_usd >= 0.0,
        "AI usage cost must be finite and non-negative"
    );

    conn.execute(
        "INSERT INTO ai_usage_events \
         (created_at, created_at_epoch, project, session_id, operation, executor, model, \
          input_tokens, output_tokens, reasoning_tokens, cache_creation_tokens, \
          cache_read_tokens, raw_input_tokens, raw_output_tokens, total_tokens, \
          estimated_cost_usd, usage_source, pricing_source, usage_status, attempt_outcome, \
          cost_status, usage_details_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
        params![created_at, created_at_epoch,
            record.project,
            record.session_id,
            record.operation,
            record.executor,
            record.model,
            record.input_tokens,
            record.output_tokens,
            record.reasoning_tokens,
            record.cache_creation_tokens,
            record.cache_read_tokens,
            record.raw_input_tokens,
            record.raw_output_tokens,
            record.total_tokens,
            record.estimated_cost_usd,
            record.usage_source,
            record.pricing_source,
            record.usage_status,
            record.attempt_outcome,
            record.cost_status,
            record.usage_details_json,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn record_ai_usage_persists_session_id() -> Result<()> {
        let conn = Connection::open_in_memory()?;
        crate::migrate::run_migrations(&conn)?;
        let record = AiUsageRecord {
            project: Some("/repo"),
            session_id: Some("sess-status-spend"),
            operation: "summarize",
            executor: "codex-cli",
            model: Some("codex-default"),
            input_tokens: 100,
            output_tokens: 25,
            reasoning_tokens: 0,
            cache_creation_tokens: 0,
            cache_read_tokens: 0,
            raw_input_tokens: 100,
            raw_output_tokens: 25,
            total_tokens: 125,
            estimated_cost_usd: 0.001,
            usage_source: "text_estimate",
            pricing_source: "remem_static",
            usage_status: "estimated",
            attempt_outcome: "success",
            cost_status: "complete",
            usage_details_json: "{}",
        };
        record_ai_usage_observation(&conn, &record)?;

        let stored: (Option<String>, i64) = conn.query_row(
            "SELECT session_id, total_tokens FROM ai_usage_events",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(stored, (Some("sess-status-spend".to_string()), 125));
        Ok(())
    }
}
