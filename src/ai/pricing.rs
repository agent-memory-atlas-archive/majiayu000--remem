use anyhow::Result;

fn parse_env_f64(key: &str) -> Option<f64> {
    std::env::var(key).ok()?.trim().parse::<f64>().ok()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ModelPricing {
    input_per_mtok: f64,
    output_per_mtok: f64,
    reasoning_per_mtok: f64,
    cache_creation_per_mtok: f64,
    cache_read_per_mtok: f64,
    source: &'static str,
}

impl ModelPricing {
    fn new(input: f64, output: f64, cache_creation: f64, cache_read: f64) -> Self {
        Self {
            input_per_mtok: input,
            output_per_mtok: output,
            reasoning_per_mtok: output,
            cache_creation_per_mtok: cache_creation,
            cache_read_per_mtok: cache_read,
            source: "remem_static",
        }
    }

    fn from_rates(rates: crate::runtime_config::PricingRates, source: &'static str) -> Self {
        Self {
            input_per_mtok: rates.input_per_mtok,
            output_per_mtok: rates.output_per_mtok,
            reasoning_per_mtok: rates.reasoning_per_mtok,
            cache_creation_per_mtok: rates.cache_creation_per_mtok,
            cache_read_per_mtok: rates.cache_read_per_mtok,
            source,
        }
    }

    fn rates(self) -> crate::runtime_config::PricingRates {
        crate::runtime_config::PricingRates {
            input_per_mtok: self.input_per_mtok,
            output_per_mtok: self.output_per_mtok,
            reasoning_per_mtok: self.reasoning_per_mtok,
            cache_creation_per_mtok: self.cache_creation_per_mtok,
            cache_read_per_mtok: self.cache_read_per_mtok,
        }
    }

    fn openai(input: f64, output: f64, cache_read: f64) -> Self {
        Self::new(input, output, 0.0, cache_read)
    }
}

pub(super) fn estimate_tokens(text: &str) -> i64 {
    ((text.len() + 3) / 4) as i64
}

#[cfg(test)]
pub(super) fn pricing_for_model(model: &str) -> (f64, f64) {
    pricing_breakdown_for_model(model)
        .expect("pricing config should be readable")
        .map(|pricing| (pricing.input_per_mtok, pricing.output_per_mtok))
        .unwrap_or((0.0, 0.0))
}

pub(super) fn pricing_breakdown_for_model(model: &str) -> Result<Option<ModelPricing>> {
    if let Some(pricing) = env_pricing() {
        return Ok(Some(pricing));
    }
    if let Some(rates) = crate::runtime_config::global_pricing_override()? {
        return Ok(Some(ModelPricing::from_rates(rates, "config_override")));
    }

    let model_lower = model.to_lowercase();
    // The auto preset delegates selection to Codex. This placeholder does
    // not identify an observed GPT-5 model; global overrides above may still
    // price the operator's local estimate.
    if model_lower == "codex-default" {
        return Ok(None);
    }
    // GPT-5.6 Codex subscription models are billed in product credits rather
    // than the generic GPT-5 USD-per-token schedule. Never manufacture a USD
    // estimate for them; an explicit global operator override above remains
    // the only way to opt into a local USD conversion.
    if model_lower.contains("gpt-5.6-luna")
        || model_lower.contains("gpt-5.6-sol")
        || model_lower.contains("gpt-5.6-terra")
    {
        return Ok(None);
    }
    let (default, prefix) = if model_lower.contains("opus-4-7")
        || model_lower.contains("opus-4.7")
        || model_lower.contains("opus-4-6")
        || model_lower.contains("opus-4.6")
        || model_lower.contains("opus-4-5")
        || model_lower.contains("opus-4.5")
    {
        (ModelPricing::new(5.0, 25.0, 6.25, 0.50), "OPUS")
    } else if model_lower.contains("opus") {
        (ModelPricing::new(15.0, 75.0, 18.75, 1.50), "OPUS")
    } else if model_lower.contains("sonnet") {
        (ModelPricing::new(3.0, 15.0, 3.75, 0.30), "SONNET")
    } else if model_lower.contains("haiku") {
        (ModelPricing::new(1.0, 5.0, 1.25, 0.10), "HAIKU")
    } else if model_lower.contains("gpt-5.5") {
        (ModelPricing::openai(5.0, 30.0, 0.50), "GPT55")
    } else if model_lower.contains("gpt-5.4-mini") {
        (ModelPricing::openai(0.75, 4.5, 0.075), "GPT54_MINI")
    } else if model_lower.contains("gpt-5.4-nano") {
        (ModelPricing::openai(0.20, 1.25, 0.020), "GPT54_NANO")
    } else if model_lower.contains("gpt-5.4") {
        (ModelPricing::openai(2.5, 15.0, 0.25), "GPT54")
    } else if model_lower.contains("gpt-5.2") || model_lower.contains("gpt-5.3-codex") {
        (ModelPricing::openai(1.75, 14.0, 0.175), "GPT5_CODEX")
    } else if model_lower.contains("gpt-5-codex") || model_lower.contains("gpt-5.1-codex") {
        (ModelPricing::openai(1.25, 10.0, 0.125), "GPT5_CODEX")
    } else if model_lower.contains("codex-mini") {
        (ModelPricing::openai(1.5, 6.0, 0.375), "CODEX_MINI")
    } else if model_lower.contains("codex") || model_lower.contains("gpt-5") {
        (ModelPricing::openai(1.25, 10.0, 0.125), "GPT5")
    } else if model_lower.contains("gpt-4") {
        (ModelPricing::openai(2.5, 10.0, 0.0), "GPT4")
    } else {
        return Ok(None);
    };

    Ok(Some(apply_family_env(
        apply_family_config(default, prefix)?,
        prefix,
    )))
}

fn env_pricing() -> Option<ModelPricing> {
    let input = parse_env_f64("REMEM_PRICE_INPUT_PER_MTOK")?;
    let output = parse_env_f64("REMEM_PRICE_OUTPUT_PER_MTOK")?;
    Some(ModelPricing {
        input_per_mtok: input,
        output_per_mtok: output,
        reasoning_per_mtok: parse_env_f64("REMEM_PRICE_REASONING_PER_MTOK").unwrap_or(output),
        cache_creation_per_mtok: parse_env_f64("REMEM_PRICE_CACHE_CREATION_PER_MTOK")
            .unwrap_or(input),
        cache_read_per_mtok: parse_env_f64("REMEM_PRICE_CACHE_READ_PER_MTOK").unwrap_or(input),
        source: "env_override",
    })
}

fn apply_family_config(default: ModelPricing, prefix: &str) -> Result<ModelPricing> {
    let (overlay, configured) =
        crate::runtime_config::family_pricing_overlay(prefix, default.rates())?;
    let source = if configured {
        "config_override"
    } else {
        default.source
    };
    Ok(ModelPricing::from_rates(overlay, source))
}

fn apply_family_env(default: ModelPricing, prefix: &str) -> ModelPricing {
    let input_override = parse_env_f64(&format!("REMEM_PRICE_{}_INPUT_PER_MTOK", prefix));
    let output_override = parse_env_f64(&format!("REMEM_PRICE_{}_OUTPUT_PER_MTOK", prefix));
    let reasoning_override = parse_env_f64(&format!("REMEM_PRICE_{}_REASONING_PER_MTOK", prefix));
    let cache_creation_override =
        parse_env_f64(&format!("REMEM_PRICE_{}_CACHE_CREATION_PER_MTOK", prefix));
    let cache_read_override = parse_env_f64(&format!("REMEM_PRICE_{}_CACHE_READ_PER_MTOK", prefix));
    let has_env_override = input_override.is_some()
        || output_override.is_some()
        || reasoning_override.is_some()
        || cache_creation_override.is_some()
        || cache_read_override.is_some();
    let input = input_override.unwrap_or(default.input_per_mtok);
    ModelPricing {
        input_per_mtok: input,
        output_per_mtok: output_override.unwrap_or(default.output_per_mtok),
        reasoning_per_mtok: reasoning_override.unwrap_or(default.reasoning_per_mtok),
        cache_creation_per_mtok: cache_creation_override.unwrap_or(default.cache_creation_per_mtok),
        cache_read_per_mtok: cache_read_override.unwrap_or(default.cache_read_per_mtok),
        source: if has_env_override {
            "env_override"
        } else {
            default.source
        },
    }
}

#[cfg(test)]
pub(super) fn estimate_cost_usd(
    model: &str,
    usage: &crate::ai::TokenUsage,
) -> Result<(f64, &'static str)> {
    let Some(pricing) = pricing_breakdown_for_model(model)? else {
        return Ok((0.0, "unknown_pricing"));
    };

    let cost = (usage.input_tokens as f64 / 1_000_000.0) * pricing.input_per_mtok
        + (usage.output_tokens as f64 / 1_000_000.0) * pricing.output_per_mtok
        + (usage.reasoning_tokens as f64 / 1_000_000.0) * pricing.reasoning_per_mtok
        + (usage.cache_creation_tokens as f64 / 1_000_000.0) * pricing.cache_creation_per_mtok
        + (usage.cache_read_tokens as f64 / 1_000_000.0) * pricing.cache_read_per_mtok;
    Ok((cost, pricing.source))
}

/// Price the known portion independently from counter completeness. A raw
/// total can resolve an absent split only when every possible rate agrees.
pub(super) fn estimate_observed_cost_usd(
    model: &str,
    observation: &super::UsageObservation,
) -> Result<(f64, &'static str, &'static str)> {
    let Some(pricing) = pricing_breakdown_for_model(model)? else {
        return Ok((0.0, "unknown_pricing", "unpriced"));
    };
    price_observation(pricing, observation)
}

fn price_observation(
    pricing: ModelPricing,
    observation: &super::UsageObservation,
) -> Result<(f64, &'static str, &'static str)> {
    for rate in [
        pricing.input_per_mtok,
        pricing.output_per_mtok,
        pricing.reasoning_per_mtok,
        pricing.cache_creation_per_mtok,
        pricing.cache_read_per_mtok,
    ] {
        anyhow::ensure!(
            rate.is_finite() && rate >= 0.0,
            "invalid usage pricing rate"
        );
    }
    if !observation.parts.is_empty() {
        let mut cost = 0.0;
        let mut complete = !matches!(
            observation.status,
            super::UsageStatus::Invalid | super::UsageStatus::Estimated
        );
        for part in &observation.parts {
            let (portion, _, status) = price_observation(pricing, part)?;
            cost += portion;
            complete &= status == "complete";
        }
        anyhow::ensure!(cost.is_finite(), "invalid usage cost calculation");
        return Ok((
            cost,
            pricing.source,
            if complete { "complete" } else { "partial" },
        ));
    }
    if matches!(
        observation.status,
        super::UsageStatus::Missing | super::UsageStatus::Invalid
    ) {
        return Ok((0.0, pricing.source, "partial"));
    }
    let tokens = &observation.tokens;
    let (input, input_complete) = price_group(
        observation,
        "raw_input_tokens",
        tokens.raw_input_tokens,
        &[
            ("input_tokens", tokens.input_tokens, pricing.input_per_mtok),
            (
                "cache_creation_tokens",
                tokens.cache_creation_tokens,
                pricing.cache_creation_per_mtok,
            ),
            (
                "cache_read_tokens",
                tokens.cache_read_tokens,
                pricing.cache_read_per_mtok,
            ),
        ],
    )?;
    let (output, output_complete) = price_group(
        observation,
        "raw_output_tokens",
        tokens.raw_output_tokens,
        &[
            (
                "output_tokens",
                tokens.output_tokens,
                pricing.output_per_mtok,
            ),
            (
                "reasoning_tokens",
                tokens.reasoning_tokens,
                pricing.reasoning_per_mtok,
            ),
        ],
    )?;
    let cost = input + output;
    anyhow::ensure!(
        cost.is_finite() && cost >= 0.0,
        "invalid usage cost calculation"
    );
    Ok((
        cost,
        pricing.source,
        if input_complete && output_complete && observation.status != super::UsageStatus::Estimated
        {
            "complete"
        } else {
            "partial"
        },
    ))
}

fn price_group(
    observation: &super::UsageObservation,
    raw_field: &str,
    raw_tokens: i64,
    categories: &[(&str, i64, f64)],
) -> Result<(f64, bool)> {
    let mut cost = 0.0;
    let mut classified: i64 = 0;
    let mut unknown_rates = Vec::new();
    for (field, tokens, rate) in categories {
        // Partial multi-turn observations can retain a known category portion
        // even when that category was absent from another turn.
        classified = classified
            .checked_add(*tokens)
            .ok_or_else(|| anyhow::anyhow!("usage category total overflow"))?;
        cost += (*tokens as f64 / 1_000_000.0) * rate;
        if !observation.field_known(field) {
            unknown_rates.push(*rate);
        }
    }
    if !observation.field_known(raw_field) {
        return Ok((cost, false));
    }
    anyhow::ensure!(raw_tokens >= classified, "usage subtotal exceeds raw total");
    let remainder = raw_tokens - classified;
    if remainder == 0 {
        return Ok((cost, true));
    }
    if let Some(rate) = unknown_rates
        .first()
        .filter(|first| unknown_rates.iter().all(|rate| rate == *first))
    {
        cost += (remainder as f64 / 1_000_000.0) * rate;
        return Ok((cost, true));
    }
    Ok((cost, false))
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    use crate::ai::codex_usage::parse_codex_json_events;

    #[test]
    fn usage_cost_text_estimate_keeps_amount_and_partial_coverage_in_parts() {
        let pricing = ModelPricing::openai(2.0, 8.0, 0.2);
        let mut estimated = crate::ai::UsageObservation::estimated(1_000_000, 1_000_000);
        assert_eq!(
            price_observation(pricing, &estimated).unwrap(),
            (10.0, "remem_static", "partial")
        );
        let observed = parse_codex_json_events(br#"{"type":"turn.completed","usage":{"input_tokens":1000000,"cached_input_tokens":0,"output_tokens":1000000,"reasoning_output_tokens":0}}"#, None).unwrap().unwrap().usage;
        estimated.merge(observed);
        assert_eq!(estimated.parts.len(), 2);
        assert_eq!(
            price_observation(pricing, &estimated).unwrap(),
            (20.0, "remem_static", "partial")
        );
    }

    #[test]
    fn usage_cost_missing_reasoning_split_requires_equal_category_rates() {
        let usage = parse_codex_json_events(br#"{"type":"turn.completed","usage":{"input_tokens":1000000,"cached_input_tokens":0,"output_tokens":1000000}}"#, None).unwrap().unwrap().usage;
        assert_eq!(usage.status, crate::ai::UsageStatus::Partial);
        let pricing = ModelPricing::openai(2.0, 8.0, 0.2);
        assert_eq!(
            price_observation(pricing, &usage).unwrap(),
            (10.0, "remem_static", "complete")
        );
        let different = ModelPricing {
            reasoning_per_mtok: 12.0,
            ..pricing
        };
        assert_eq!(
            price_observation(different, &usage).unwrap(),
            (2.0, "remem_static", "partial")
        );
    }

    #[test]
    fn usage_cost_never_turns_missing_or_invalid_counts_into_complete_zero() {
        let pricing = ModelPricing::openai(2.0, 8.0, 0.2);
        assert_eq!(
            price_observation(pricing, &crate::ai::UsageObservation::missing()).unwrap(),
            (0.0, "remem_static", "partial")
        );
        let invalid = parse_codex_json_events(br#"{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":200,"output_tokens":0,"reasoning_output_tokens":0}}"#, None).unwrap().unwrap().usage;
        assert_eq!(
            price_observation(pricing, &invalid).unwrap(),
            (0.0, "remem_static", "partial")
        );
        let mut invalid_pricing = pricing;
        invalid_pricing.output_per_mtok = f64::INFINITY;
        assert!(price_observation(
            invalid_pricing,
            &crate::ai::UsageObservation::estimated(1, 1)
        )
        .is_err());
    }
}
