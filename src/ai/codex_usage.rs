use anyhow::Result;
use serde_json::Value;

use super::usage_observation::{Counter, UsageObservation};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CodexRunUsage {
    pub usage: UsageObservation,
    pub model: Option<String>,
}

fn cached_counter(value: &Value) -> Counter {
    let current = Counter::read(value, "cached_input_tokens");
    let alias = Counter::read(value, "cache_read_input_tokens");
    match (current, alias) {
        (Counter::Invalid, _) | (_, Counter::Invalid) => Counter::Invalid,
        (Counter::Known(a), Counter::Known(b)) if a != b => Counter::Invalid,
        (Counter::Known(value), _) | (_, Counter::Known(value)) => Counter::Known(value),
        _ => Counter::Missing,
    }
}

fn extract_usage(value: Option<&Value>) -> UsageObservation {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return UsageObservation::missing();
    };
    if !value.is_object() {
        return UsageObservation::from_counters([Counter::Invalid; 7], false);
    }
    let raw_input = Counter::read(value, "input_tokens");
    let raw_output = Counter::read(value, "output_tokens");
    let cached = cached_counter(value);
    let reasoning = Counter::read(value, "reasoning_output_tokens");
    let input = raw_input.subtract(cached);
    let output = raw_output.subtract(reasoning);
    UsageObservation::from_counters(
        [
            input,
            output,
            reasoning,
            Counter::Known(0),
            cached,
            raw_input,
            raw_output,
        ],
        [raw_input, raw_output, cached, reasoning]
            .iter()
            .any(|counter| counter.is_reported()),
    )
}

pub(super) fn parse_codex_json_events(
    events: &[u8],
    model: Option<String>,
) -> Result<Option<CodexRunUsage>> {
    let mut usage: Option<UsageObservation> = None;
    // A malformed unrelated log line must not erase counters parsed earlier.
    for line in events.split(|byte| *byte == b'\n') {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("turn.completed") {
            continue;
        }
        let observed = extract_usage(value.get("usage"));
        match &mut usage {
            Some(usage) => usage.merge(observed),
            None => usage = Some(observed),
        }
    }
    Ok(usage.map(|usage| CodexRunUsage { usage, model }))
}

#[cfg(test)]
mod tests {
    use anyhow::{Context, Result};

    use super::parse_codex_json_events;

    #[test]
    fn parses_codex_exec_json_turn_completed_usage() -> Result<()> {
        let log = r#"
{"type":"thread.started","thread_id":"019e2f80-913f-7452-97ed-6340c56b2bd4"}
{"type":"turn.started"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"OK"}}
{"type":"turn.completed","usage":{"input_tokens":1000,"cached_input_tokens":200,"output_tokens":500,"reasoning_output_tokens":150}}
"#;
        let parsed = parse_codex_json_events(log.as_bytes(), Some("gpt-5.2".to_string()))
            .and_then(|usage| usage.context("usage should parse"))?;
        assert_eq!(parsed.model.as_deref(), Some("gpt-5.2"));
        assert_eq!(parsed.usage.tokens.input_tokens, 800);
        assert_eq!(parsed.usage.tokens.cache_read_tokens, 200);
        assert_eq!(parsed.usage.tokens.output_tokens, 350);
        assert_eq!(parsed.usage.tokens.reasoning_tokens, 150);
        assert_eq!(parsed.usage.tokens.total_tokens(), 1500);
        Ok(())
    }

    #[test]
    fn parses_multiple_codex_exec_json_turns() -> Result<()> {
        let log = r#"
{"type":"turn.completed","usage":{"input_tokens":100,"cached_input_tokens":80,"output_tokens":20,"reasoning_output_tokens":5}}
{"type":"turn.completed","usage":{"input_tokens":50,"cached_input_tokens":0,"output_tokens":10,"reasoning_output_tokens":0}}
"#;
        let parsed = parse_codex_json_events(log.as_bytes(), None)
            .and_then(|usage| usage.context("usage should parse"))?;
        assert_eq!(parsed.usage.tokens.input_tokens, 70);
        assert_eq!(parsed.usage.tokens.cache_read_tokens, 80);
        assert_eq!(parsed.usage.tokens.output_tokens, 25);
        assert_eq!(parsed.usage.tokens.reasoning_tokens, 5);
        assert_eq!(parsed.usage.tokens.total_tokens(), 180);
        Ok(())
    }

    #[test]
    fn returns_none_without_turn_completed_usage() -> Result<()> {
        let log = r#"
{"type":"thread.started","thread_id":"019e2f80-913f-7452-97ed-6340c56b2bd4"}
{"type":"item.completed","item":{"id":"item_0","type":"agent_message","text":"OK"}}
"#;
        let parsed = parse_codex_json_events(log.as_bytes(), Some("gpt-5.2".to_string()))?;
        assert!(parsed.is_none());
        Ok(())
    }
}
