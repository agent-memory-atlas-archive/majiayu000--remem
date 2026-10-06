mod cli;
mod codex_cli;
mod codex_usage;
mod config;
mod http;
mod pricing;
#[cfg(test)]
mod tests;
mod types;
mod usage;
mod usage_observation;
#[cfg(test)]
mod usage_tests;

use cli::call_cli;
use codex_cli::call_codex_cli;
use http::call_http;
use pricing::estimate_tokens;
use usage::record_usage;

tokio::task_local! {
    static RESOLVED_PROFILE_OVERRIDE: crate::runtime_config::ResolvedMemoryAiProfile;
}

pub(crate) use types::TokenUsage;
pub use types::UsageContext;
pub(crate) use usage_observation::{UsageObservation, UsageStatus};

/// AI call with timeout. Executor/model/path are resolved from remem config.
pub async fn call_ai(
    system: &str,
    user_message: &str,
    ctx: UsageContext<'_>,
) -> anyhow::Result<String> {
    let profile = match RESOLVED_PROFILE_OVERRIDE.try_with(Clone::clone) {
        Ok(profile) => profile,
        Err(_) => crate::runtime_config::resolve_memory_ai_profile(
            crate::runtime_config::MemoryAiSelection {
                host: ctx.host,
                profile: ctx.profile,
            },
        )?,
    };
    let result = match profile.executor {
        crate::runtime_config::MemoryAiExecutor::Http => {
            call_http(system, user_message, &profile).await
        }
        crate::runtime_config::MemoryAiExecutor::ClaudeCli => {
            call_cli(system, user_message, &profile).await
        }
        crate::runtime_config::MemoryAiExecutor::CodexCli => {
            call_codex_cli(system, user_message, &profile).await
        }
    };

    let input_tokens = estimate_tokens(system) + estimate_tokens(user_message);
    match result {
        Ok(result) => {
            let output_tokens = estimate_tokens(&result.text);
            record_usage(ctx, &result, "success", input_tokens, output_tokens);
            Ok(result.text)
        }
        Err(error) => {
            let fallback;
            let evidence = if let Some(failure) = error.downcast_ref::<types::AiCallFailure>() {
                &failure.evidence
            } else {
                let (executor, model, source) = match profile.executor {
                    crate::runtime_config::MemoryAiExecutor::Http => (
                        "http",
                        config::resolve_model_for_api(profile.model.as_deref().unwrap_or("haiku"))
                            .to_string(),
                        Some("anthropic_usage"),
                    ),
                    crate::runtime_config::MemoryAiExecutor::CodexCli => (
                        "codex-cli",
                        profile
                            .model
                            .clone()
                            .unwrap_or_else(|| "codex-default".into()),
                        Some("codex_log"),
                    ),
                    crate::runtime_config::MemoryAiExecutor::ClaudeCli => (
                        "cli",
                        profile.model.clone().unwrap_or_else(|| "haiku".into()),
                        None,
                    ),
                };
                fallback = types::AiCallResult {
                    text: String::new(),
                    executor,
                    model,
                    usage: None,
                    usage_source: source,
                };
                &fallback
            };
            record_usage(ctx, evidence, "failed", 0, 0);
            Err(error)
        }
    }
}

pub(crate) async fn with_resolved_profile<T>(
    profile: crate::runtime_config::ResolvedMemoryAiProfile,
    future: impl std::future::Future<Output = T>,
) -> T {
    RESOLVED_PROFILE_OVERRIDE.scope(profile, future).await
}

fn stable_working_dir() -> std::path::PathBuf {
    let data_dir = match crate::db::try_data_dir() {
        Ok(path) => path,
        Err(error) => {
            crate::log::error("ai", &format!("cannot resolve AI working dir: {error}"));
            return std::env::temp_dir();
        }
    };
    match std::fs::create_dir_all(&data_dir) {
        Ok(()) => data_dir,
        Err(err) => {
            crate::log::warn(
                "ai",
                &format!(
                    "failed to create AI working dir {}: {}; falling back to temp dir",
                    data_dir.display(),
                    err
                ),
            );
            std::env::temp_dir()
        }
    }
}
