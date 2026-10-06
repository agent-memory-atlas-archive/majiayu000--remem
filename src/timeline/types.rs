use serde::Serialize;

#[derive(Debug, Serialize)]
pub(super) struct Overview {
    pub first_date: String,
    pub last_date: String,
    pub days_span: i64,
    pub total_observations: i64,
    pub total_sessions: i64,
    pub total_memories: i64,
}

#[derive(Debug, Serialize)]
pub(super) struct TypeCount {
    pub obs_type: String,
    pub count: i64,
}

#[derive(Debug, Serialize)]
pub(super) struct MonthRow {
    pub month: String,
    pub observations: i64,
    pub sessions: i64,
    pub ai_cost: f64,
    pub ai_usage_coverage: crate::db::AiUsageCoverage,
}

#[derive(Debug, Serialize)]
pub(super) struct TokenEcon {
    pub total_ai_cost: f64,
    pub ai_usage_coverage: crate::db::AiUsageCoverage,
    pub total_discovery_tokens: i64,
    pub sessions_with_context: i64,
}

#[derive(Debug, Serialize)]
pub(super) struct RecentObservation {
    pub id: i64,
    pub obs_type: String,
    pub title: Option<String>,
    pub created_at_epoch: i64,
}
