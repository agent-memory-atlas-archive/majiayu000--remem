use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};

use super::super::helpers::{error_response, open_request_db};
use super::super::types::{DbState, StatsResponse, TypeCount};

pub(in crate::api) async fn handle_stats(State(_state): State<DbState>) -> impl IntoResponse {
    let conn = match open_request_db() {
        Ok(conn) => conn,
        Err(response) => return response,
    };

    let result = (|| -> anyhow::Result<StatsResponse> {
        let current_filter =
            crate::memory::memory_current_filter_sql("status", "expires_at_epoch", false);
        let total_memories: i64 =
            conn.query_row("SELECT COUNT(*) FROM memories", [], |row| row.get(0))?;
        let active_memories: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM memories WHERE {current_filter}"),
            [],
            |row| row.get(0),
        )?;
        let pending_candidates: i64 = conn.query_row(
            "SELECT COUNT(*) FROM memory_candidates WHERE review_status = 'pending_review'",
            [],
            |row| row.get(0),
        )?;
        let captured_events: i64 =
            conn.query_row("SELECT COUNT(*) FROM captured_events", [], |row| row.get(0))?;
        let pending_extraction_tasks: i64 = conn.query_row(
            "SELECT COUNT(*) FROM extraction_tasks WHERE status = 'pending'",
            [],
            |row| row.get(0),
        )?;
        let usage = crate::db::query_ai_usage_totals(&conn, None, None)?;

        let mut stmt = conn.prepare(&format!(
            "SELECT memory_type, COUNT(*) FROM memories WHERE {current_filter} \
             GROUP BY memory_type ORDER BY COUNT(*) DESC"
        ))?;
        let type_distribution: Vec<TypeCount> = stmt
            .query_map([], |row| {
                Ok(TypeCount {
                    memory_type: row.get(0)?,
                    count: row.get(1)?,
                })
            })?
            .collect::<Result<Vec<_>, rusqlite::Error>>()?;

        Ok(StatsResponse {
            active_memories,
            total_memories,
            pending_candidates,
            captured_events,
            pending_extraction_tasks,
            ai_calls: usage.calls,
            ai_cost_usd: usage.estimated_cost_usd,
            ai_total_tokens: usage.total_tokens,
            ai_cost_complete: usage.coverage.cost_complete(),
            ai_usage_coverage: usage.coverage,
            type_distribution,
        })
    })();

    match result {
        Ok(stats) => Json(stats).into_response(),
        Err(err) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "stats_failed",
            &err.to_string(),
        )
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn usage_stats_api_distinguishes_unpriced_zero_and_known_cost_portion(
    ) -> anyhow::Result<()> {
        let _scope = crate::db::test_support::ScopedTestDataDir::new("usage-stats-api");
        let conn = crate::db::open_db()?;
        conn.execute("INSERT INTO ai_usage_events(created_at,created_at_epoch,operation,executor,input_tokens,output_tokens,total_tokens,estimated_cost_usd,usage_source,pricing_source,usage_status,attempt_outcome,cost_status)
            VALUES('fixture',1,'fixture','codex-cli',100,40,140,0,'codex_log','unknown_pricing','complete','success','unpriced')", [])?;
        for (expected_calls, expected_cost) in [(1, 0.0), (2, 0.123)] {
            if expected_calls == 2 {
                conn.execute("INSERT INTO ai_usage_events(created_at,created_at_epoch,operation,executor,input_tokens,output_tokens,total_tokens,estimated_cost_usd,usage_source,pricing_source,usage_status,attempt_outcome,cost_status)
                    VALUES('fixture',2,'fixture','codex-cli',100,40,140,0.123,'codex_log','remem_static','complete','success','complete')", [])?;
            }
            let response = handle_stats(State(DbState)).await.into_response();
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), 1_048_576).await?;
            let json: serde_json::Value = serde_json::from_slice(&body)?;
            assert_eq!(json["ai_calls"], expected_calls);
            assert_eq!(json["ai_cost_usd"], expected_cost);
            assert_eq!(json["ai_cost_complete"], false);
            assert_eq!(json["ai_usage_coverage"]["unpriced_calls"], 1);
            assert_eq!(json["ai_usage_coverage"]["cost_incomplete_calls"], 1);
        }
        Ok(())
    }
}
