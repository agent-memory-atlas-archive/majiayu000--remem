use super::UsageObservation;
use crate::ai::pricing::estimate_observed_cost_usd;
use crate::ai::types::{AiCallResult, UsageContext};
use crate::runtime_config::{MemoryAiExecutor, ResolvedMemoryAiProfile};

/// Own the attempt boundary, including cancellation of the awaiting caller.
/// No detached recorder is spawned: dropping the backend first leaves this
/// guard alive long enough to persist the terminal counters already read.
pub(super) struct UsageAttemptGuard<'a> {
    ctx: UsageContext<'a>,
    state: std::sync::Mutex<AttemptState>,
    armed: bool,
}

#[derive(Clone)]
struct AttemptState {
    evidence: AiCallResult,
    pending_line: Vec<u8>,
}

impl<'a> UsageAttemptGuard<'a> {
    pub fn new(ctx: UsageContext<'a>, profile: &ResolvedMemoryAiProfile) -> Self {
        let (executor, model, source) = match profile.executor {
            MemoryAiExecutor::Http => (
                "http",
                super::config::resolve_model_for_api(profile.model.as_deref().unwrap_or("haiku"))
                    .to_string(),
                Some("anthropic_usage"),
            ),
            MemoryAiExecutor::CodexCli => (
                "codex-cli",
                profile
                    .model
                    .clone()
                    .unwrap_or_else(|| "codex-default".into()),
                Some("codex_log"),
            ),
            MemoryAiExecutor::ClaudeCli => (
                "cli",
                profile.model.clone().unwrap_or_else(|| "haiku".into()),
                None,
            ),
        };
        Self {
            ctx,
            state: std::sync::Mutex::new(AttemptState {
                evidence: AiCallResult {
                    text: String::new(),
                    executor,
                    model,
                    usage: None,
                    usage_source: source,
                },
                pending_line: Vec::new(),
            }),
            armed: true,
        }
    }

    pub fn observe_codex_bytes(&self, bytes: &[u8]) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        state.pending_line.extend_from_slice(bytes);
        if let Some(end) = state.pending_line.iter().rposition(|byte| *byte == b'\n') {
            let lines: Vec<u8> = state.pending_line.drain(..=end).collect();
            merge_codex_events(&mut state.evidence, &lines);
        }
    }

    pub fn snapshot(&self) -> AiCallResult {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        // No mutex is held while producing or recording the snapshot. A
        // complete final JSON event needs no newline; partial JSON adds none.
        merge_codex_events(&mut state.evidence, &state.pending_line);
        state.evidence
    }

    pub fn complete(&mut self, result: &AiCallResult, outcome: &str, input: i64, output: i64) {
        self.armed = false;
        record_usage(self.ctx, result, outcome, input, output);
    }
}

fn merge_codex_events(evidence: &mut AiCallResult, bytes: &[u8]) {
    match super::codex_usage::parse_codex_json_events(bytes, None) {
        Ok(Some(event)) => match &mut evidence.usage {
            Some(usage) => usage.merge(event.usage),
            None => evidence.usage = Some(event.usage),
        },
        Ok(None) => {}
        Err(error) => crate::log::error("ai", &format!("codex usage observation failed: {error}")),
    }
}

impl Drop for UsageAttemptGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.armed = false;
            record_usage(self.ctx, &self.snapshot(), "failed", 0, 0);
        }
    }
}

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
