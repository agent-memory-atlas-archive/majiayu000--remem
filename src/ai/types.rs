/// AI call timeout (seconds)
pub(super) const AI_TIMEOUT_SECS: u64 = 90;

#[derive(Clone, Copy)]
pub struct UsageContext<'a> {
    pub project: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub operation: &'a str,
    pub host: Option<&'a str>,
    pub profile: Option<&'a str>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct TokenUsage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_tokens: i64,
    pub cache_creation_tokens: i64,
    pub cache_read_tokens: i64,
    pub raw_input_tokens: i64,
    pub raw_output_tokens: i64,
}

impl TokenUsage {
    pub fn estimated(input_tokens: i64, output_tokens: i64) -> Self {
        Self {
            input_tokens,
            output_tokens,
            raw_input_tokens: input_tokens,
            raw_output_tokens: output_tokens,
            ..Self::default()
        }
    }

    #[cfg(test)]
    pub fn total_tokens(&self) -> i64 {
        self.checked_total_tokens().unwrap_or(0)
    }

    #[cfg(test)]
    pub fn checked_total_tokens(&self) -> Option<i64> {
        // Raw Codex totals remain useful even when a cache/reasoning split
        // is unavailable. A missing raw total can still have known parts.
        let input = self
            .input_tokens
            .checked_add(self.cache_creation_tokens)?
            .checked_add(self.cache_read_tokens)?;
        let output = self.output_tokens.checked_add(self.reasoning_tokens)?;
        input
            .max(self.raw_input_tokens)
            .checked_add(output.max(self.raw_output_tokens))
    }
}

#[derive(Clone, Debug)]
pub(super) struct AiCallResult {
    pub text: String,
    pub executor: &'static str,
    pub model: String,
    pub usage: Option<super::usage_observation::UsageObservation>,
    pub usage_source: Option<&'static str>,
}

#[derive(Debug)]
pub(super) struct AiCallFailure {
    pub evidence: AiCallResult,
    pub error: anyhow::Error,
}

impl AiCallFailure {
    pub fn with_evidence(error: anyhow::Error, evidence: AiCallResult) -> anyhow::Error {
        Self { evidence, error }.into()
    }
}

impl std::fmt::Display for AiCallFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}

impl std::error::Error for AiCallFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}
