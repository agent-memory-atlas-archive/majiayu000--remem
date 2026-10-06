use super::types::LatestSessionMemorySpendStatus;

pub(super) fn print_latest_session_spend(spend: &LatestSessionMemorySpendStatus) {
    println!();
    println!("Latest session memory footprint:");
    println!("  Session:      {}", spend.session_id);
    println!("  Project:      {}", spend.project);
    println!(
        "  Context now:  {:>6} chars (~{} tokens)",
        spend.context_output_chars, spend.context_estimated_tokens
    );
    println!(
        "  Context runs: {:>6} emitted, {:>6} suppressed",
        spend.context_emit_count, spend.context_suppress_count
    );
    if spend.relevance_state == "unavailable" {
        println!("  Relevance:    unavailable on legacy context rows");
    } else {
        let threshold = spend
            .relevance_threshold
            .map(|value| format!("{value:.3}"))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  Relevance:    {} (k={}, threshold={}, {}/{} injected/eligible)",
            spend.relevance_state,
            spend.relevance_k.unwrap_or(0),
            threshold,
            spend.relevance_final_injected_count,
            spend.relevance_eligible_count
        );
        println!(
            "  Relevance drops: {} low, {} k-limit, {} section, {} total-limit",
            spend.relevance_below_threshold_count,
            spend.relevance_k_limited_count,
            spend.relevance_section_budget_count,
            spend.relevance_total_char_limit_count
        );
    }
    match spend.ai_usage_attribution.as_str() {
        "attributed" => {
            println!(
                "  AI usage:     {:>6} calls, {:>6} tokens, ${:.4} known estimate",
                spend.ai_calls, spend.ai_total_tokens, spend.ai_estimated_cost_usd
            );
        }
        "partial" => {
            println!(
                "  AI usage:     {:>6} attributed calls, {:>6} tokens, ${:.4} known estimate",
                spend.ai_calls, spend.ai_total_tokens, spend.ai_estimated_cost_usd
            );
            println!(
                "  AI legacy:    {:>6} unattributed calls not assigned to a session",
                spend.ai_unattributed_legacy_calls
            );
        }
        _ => {
            println!("  AI usage:     unavailable on legacy rows without session_id");
        }
    }
    if spend.ai_usage_attribution != "unavailable" {
        println!(
            "  AI coverage:  {} cost-incomplete calls ({} unpriced), {} failed attempts",
            spend.ai_usage_coverage.cost_incomplete_calls,
            spend.ai_usage_coverage.unpriced_calls,
            spend.ai_usage_coverage.failed_calls
        );
    }
}
