use super::UsageObservation;
use crate::ai::pricing::estimate_observed_cost_usd;
use crate::ai::types::{AiCallResult, UsageContext};

pub(super) fn record_usage(
    ctx: UsageContext<'_>,
    result: &AiCallResult,
    outcome: &str,
    input_tokens: i64,
    output_tokens: i64,
) {
    let operation = if ctx.operation.trim().is_empty() {
        "unknown"
    } else {
        ctx.operation
    };
    let (usage, usage_source) = match &result.usage {
        Some(usage) => (
            usage.clone(),
            result.usage_source.unwrap_or("provider_usage"),
        ),
        None if outcome == "success" && result.usage_source.is_none() => (
            UsageObservation::estimated(input_tokens, output_tokens),
            "text_estimate",
        ),
        None => (
            UsageObservation::missing(),
            result.usage_source.unwrap_or("unavailable"),
        ),
    };
    let (cost, pricing_source, cost_status) =
        match estimate_observed_cost_usd(&result.model, &usage) {
            Ok(estimated) => estimated,
            Err(error) => {
                crate::log::error("ai", &format!("usage cost pricing failed: {error}"));
                (0.0, "invalid_pricing", "unpriced")
            }
        };
    if pricing_source == "unknown_pricing" {
        crate::log::warn(
            "ai",
            &format!("usage cost has unknown pricing for model {}", result.model),
        );
    }
    match crate::db::open_db().and_then(|conn| {
        let details = serde_json::json!({"version": 1, "observation": &usage}).to_string();
        let tokens = &usage.tokens;
        crate::db::record_ai_usage_observation(
            &conn,
            &crate::db::AiUsageRecord {
                project: ctx.project,
                session_id: ctx.session_id,
                operation,
                executor: result.executor,
                model: Some(&result.model),
                input_tokens: tokens.input_tokens,
                output_tokens: tokens.output_tokens,
                reasoning_tokens: tokens.reasoning_tokens,
                cache_creation_tokens: tokens.cache_creation_tokens,
                cache_read_tokens: tokens.cache_read_tokens,
                raw_input_tokens: tokens.raw_input_tokens,
                raw_output_tokens: tokens.raw_output_tokens,
                total_tokens: usage.known_total_tokens,
                estimated_cost_usd: cost,
                usage_source,
                pricing_source,
                usage_status: usage.status.as_str(),
                attempt_outcome: outcome,
                cost_status,
                usage_details_json: &details,
            },
        )?;
        Ok(())
    }) {
        Ok(_) => {}
        Err(error) => crate::log::error("ai", &format!("usage record failed: {}", error)),
    }
}
