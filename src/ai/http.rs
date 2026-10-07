use std::sync::OnceLock;

use anyhow::{Context, Result};

use super::usage_observation::{Counter, UsageObservation};
use crate::ai::config::resolve_model_for_api;
use crate::ai::types::{AiCallFailure, AiCallResult, AI_TIMEOUT_SECS};
use crate::runtime_config::ResolvedMemoryAiProfile;

/// Process-wide HTTP client shared across AI calls.
///
/// `reqwest::Client` owns a connection pool and is designed to be reused; a
/// fresh client per call threw away keep-alive connections and rebuilt the TLS
/// configuration every time. The timeout is a compile-time constant, so one
/// client serves every call. The client is cheap to reuse across base URLs
/// because the pool is keyed by host.
fn shared_client() -> Result<&'static reqwest::Client> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(AI_TIMEOUT_SECS))
        .build()
        .context("build shared AI HTTP client")?;
    // A concurrent caller may win the race; `set` fails in that case and we
    // return whichever client is now stored. Either way the client is built at
    // most a handful of times, then reused for the process lifetime.
    let _ = CLIENT.set(client);
    Ok(CLIENT
        .get()
        .expect("shared HTTP client was just initialized"))
}

pub(super) async fn call_http(
    system: &str,
    user_message: &str,
    profile: &ResolvedMemoryAiProfile,
) -> Result<AiCallResult> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .or_else(|_| std::env::var("ANTHROPIC_AUTH_TOKEN"))
        .context("ANTHROPIC_API_KEY not set")?;
    let raw = profile.model.as_deref().unwrap_or("haiku");
    let model = resolve_model_for_api(raw);
    let base_url = profile
        .base_url
        .as_deref()
        .unwrap_or("https://api.anthropic.com");

    let body = serde_json::json!({
        "model": model,
        "max_tokens": 4096,
        "system": [{"type": "text", "text": system}],
        "messages": [{"role": "user", "content": user_message}]
    });

    let client = shared_client()?;

    let resp = client
        .post(format!("{}/v1/messages", base_url.trim_end_matches('/')))
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp
            .text()
            .await
            .unwrap_or_else(|error| format!("<body read error: {error}>"));
        let usage = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .as_ref()
            .and_then(extract_usage);
        return Err(AiCallFailure::with_evidence(
            anyhow::anyhow!("Anthropic API error {}: {}", status, body),
            AiCallResult {
                text: String::new(),
                executor: "http",
                model: model.to_string(),
                usage,
                usage_source: Some("anthropic_usage"),
            },
        ));
    }
    let data: serde_json::Value = resp.json().await?;
    parse_response(&data, model)
}

fn parse_response(data: &serde_json::Value, model: &str) -> Result<AiCallResult> {
    let mut evidence = AiCallResult {
        text: String::new(),
        executor: "http",
        model: model.to_string(),
        usage: extract_usage(data),
        usage_source: Some("anthropic_usage"),
    };
    match extract_text(data) {
        Ok(text) => {
            evidence.text = text;
            Ok(evidence)
        }
        Err(error) => Err(AiCallFailure::with_evidence(error, evidence)),
    }
}

fn extract_text(data: &serde_json::Value) -> Result<String> {
    let text = data["content"]
        .as_array()
        .and_then(|arr| arr.first())
        .and_then(|content| content["text"].as_str())
        .ok_or_else(|| {
            let snippet: String = serde_json::to_string(data)
                .unwrap_or_default()
                .chars()
                .take(512)
                .collect();
            anyhow::anyhow!("Anthropic response missing content[0].text: {}", snippet)
        })?
        .to_string();

    if text.trim().is_empty() {
        anyhow::bail!("Anthropic returned empty text body");
    }
    Ok(text)
}

fn extract_usage(data: &serde_json::Value) -> Option<UsageObservation> {
    let usage = data.get("usage")?;
    if usage.is_null() {
        return Some(UsageObservation::missing());
    }
    if !usage.is_object() {
        return Some(UsageObservation::from_counters(
            [Counter::Invalid; 7],
            false,
        ));
    }
    let input = Counter::read(usage, "input_tokens");
    let output = Counter::read(usage, "output_tokens");
    let creation = Counter::optional_zero(usage, "cache_creation_input_tokens");
    let cache_read = Counter::optional_zero(usage, "cache_read_input_tokens");
    let reported = [
        "input_tokens",
        "output_tokens",
        "cache_creation_input_tokens",
        "cache_read_input_tokens",
    ]
    .into_iter()
    .any(|key| Counter::read(usage, key).is_reported());
    Some(UsageObservation::from_counters(
        [
            input,
            output,
            Counter::Known(0),
            creation,
            cache_read,
            Counter::sum(&[input, creation, cache_read]),
            output,
        ],
        reported,
    ))
}

#[cfg(test)]
mod http_tests {
    use super::{extract_text, extract_usage, parse_response, shared_client};
    use serde_json::json;

    #[test]
    fn usage_observation_http_preserves_missing_invalid_zero_and_failed_text() {
        use crate::ai::UsageStatus;
        let zero = extract_usage(&json!({"usage":{"input_tokens":0,"output_tokens":0}})).unwrap();
        assert_eq!(zero.status, UsageStatus::Complete);
        assert!(zero.has_reported_counters);
        for value in [
            json!({"input_tokens":100}),
            json!({"input_tokens":100,"output_tokens":null}),
        ] {
            let usage = extract_usage(&json!({"usage":value})).unwrap();
            assert_eq!(usage.status, UsageStatus::Partial);
            assert_eq!(usage.known_total_tokens, 100);
        }
        for value in [
            json!({"input_tokens":100,"output_tokens":"40"}),
            json!({"input_tokens":100,"output_tokens":-1}),
        ] {
            assert_eq!(
                extract_usage(&json!({"usage":value})).unwrap().status,
                UsageStatus::Invalid
            );
        }
        let error = parse_response(
            &json!({"content":[],"usage":{"input_tokens":100,"output_tokens":40}}),
            "haiku",
        )
        .unwrap_err();
        let failure = error
            .downcast_ref::<crate::ai::types::AiCallFailure>()
            .unwrap();
        assert_eq!(
            failure.evidence.usage.as_ref().unwrap().known_total_tokens,
            140
        );
    }

    #[test]
    fn shared_client_is_reused_across_calls() {
        let first = shared_client().expect("client builds");
        let second = shared_client().expect("client builds");
        assert!(
            std::ptr::eq(first, second),
            "shared_client must return the same process-wide client instance"
        );
    }

    #[test]
    fn extracts_text_from_valid_response() {
        let data = json!({
            "content": [{"type": "text", "text": "hello"}]
        });
        assert_eq!(extract_text(&data).unwrap(), "hello");
    }

    #[test]
    fn errors_on_tool_use_response_without_text_field() {
        let data = json!({
            "content": [{"type": "tool_use", "id": "abc", "name": "x", "input": {}}]
        });
        let err = extract_text(&data).unwrap_err().to_string();
        assert!(err.contains("missing content[0].text"), "got: {err}");
    }

    #[test]
    fn errors_on_missing_content_array() {
        let data = json!({"id": "msg_1"});
        let err = extract_text(&data).unwrap_err().to_string();
        assert!(err.contains("missing content[0].text"), "got: {err}");
    }

    #[test]
    fn errors_on_empty_content_array() {
        let data = json!({"content": []});
        assert!(extract_text(&data).is_err());
    }

    #[test]
    fn errors_on_whitespace_only_text() {
        let data = json!({"content": [{"type": "text", "text": "   \n"}]});
        let err = extract_text(&data).unwrap_err().to_string();
        assert!(err.contains("empty text body"), "got: {err}");
    }

    #[test]
    fn errors_on_empty_string_text() {
        let data = json!({"content": [{"type": "text", "text": ""}]});
        let err = extract_text(&data).unwrap_err().to_string();
        assert!(err.contains("empty text body"), "got: {err}");
    }

    #[test]
    fn extracts_anthropic_usage_breakdown() {
        let data = json!({
            "usage": {
                "input_tokens": 100,
                "output_tokens": 40,
                "cache_creation_input_tokens": 20,
                "cache_read_input_tokens": 300
            }
        });
        let usage = extract_usage(&data).expect("usage should parse").tokens;
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 40);
        assert_eq!(usage.cache_creation_tokens, 20);
        assert_eq!(usage.cache_read_tokens, 300);
        assert_eq!(usage.raw_input_tokens, 420);
        assert_eq!(usage.total_tokens(), 460);
    }
}
