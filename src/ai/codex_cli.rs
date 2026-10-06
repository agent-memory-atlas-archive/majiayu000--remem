use std::ffi::OsString;
use std::path::Path;

use anyhow::{Context, Result};
use tokio::process::Command;

use crate::ai::types::{AiCallFailure, AiCallResult, AI_TIMEOUT_SECS};
use crate::runtime_config::ResolvedMemoryAiProfile;

pub(super) async fn call_codex_cli(
    system: &str,
    user_message: &str,
    profile: &ResolvedMemoryAiProfile,
) -> Result<AiCallResult> {
    call_codex_cli_with_timeout(
        system,
        user_message,
        profile,
        std::time::Duration::from_secs(AI_TIMEOUT_SECS),
    )
    .await
}

pub(super) async fn call_codex_cli_with_timeout(
    system: &str,
    user_message: &str,
    profile: &ResolvedMemoryAiProfile,
    timeout: std::time::Duration,
) -> Result<AiCallResult> {
    let codex = profile.cli_path.as_deref().unwrap_or("codex");
    let model = profile.model.clone();
    let reasoning_effort = profile.reasoning_effort.as_deref();
    let output_path = std::env::temp_dir().join(format!(
        "remem-codex-summary-{}-{}.txt",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let prompt = build_prompt(system, user_message);
    let working_dir = super::stable_working_dir();

    let mut command = Command::new(codex);
    command.args(build_codex_args(
        &output_path,
        model.as_deref(),
        reasoning_effort,
    ));
    command
        .current_dir(&working_dir)
        .env("REMEM_DISABLE_HOOKS", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn '{}' - is Codex CLI installed?", codex))?;

    // The buffers outlive the cancellable wait, preserving terminal events
    // already read when the process times out or a pipe fails.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stdin = child.stdin.take().context("Codex stdin pipe missing")?;
    let mut stdout = child.stdout.take().context("Codex stdout pipe missing")?;
    let mut stderr = child.stderr.take().context("Codex stderr pipe missing")?;
    let mut stdout_bytes = Vec::new();
    let mut stderr_bytes = Vec::new();
    let mut stdin_error = None;
    let mut stdout_error = None;
    let mut stderr_error = None;
    let exit = tokio::time::timeout(timeout, async {
        // Join rather than try_join: an input error must not cancel a
        // still-readable terminal usage event on stdout. The same deadline
        // covers writing a large prompt and draining the child's output.
        let (_, _, _, status) = tokio::join!(
            async {
                stdin_error = stdin.write_all(prompt.as_bytes()).await.err();
                drop(stdin);
            },
            async {
                stdout_error = stdout.read_to_end(&mut stdout_bytes).await.err();
            },
            async {
                stderr_error = stderr.read_to_end(&mut stderr_bytes).await.err();
            },
            child.wait(),
        );
        status
    })
    .await;
    let mut exit = match exit {
        Ok(result) => result.map_err(anyhow::Error::from),
        Err(_) => {
            let _ = child.start_kill();
            Err(anyhow::anyhow!(
                "codex CLI timed out after {}s",
                timeout.as_secs_f64()
            ))
        }
    };
    if let Some(error) = stdin_error {
        exit = Err(anyhow::Error::from(error).context("failed to write Codex prompt"));
    } else if let Some(error) = stdout_error.or(stderr_error) {
        exit = Err(anyhow::Error::from(error).context("failed to read Codex output"));
    }

    let codex_usage =
        match super::codex_usage::parse_codex_json_events(&stdout_bytes, model.clone()) {
            Ok(usage) => usage,
            Err(error) => {
                crate::log::warn("ai", &format!("codex usage parse failed: {}", error));
                None
            }
        };
    if codex_usage.is_none() {
        crate::log::warn("ai", "codex JSON usage parse found no turn.completed usage");
    }
    let usage = codex_usage
        .as_ref()
        .map(|run_usage| run_usage.usage.clone());
    let usage_model = codex_usage.and_then(|run_usage| run_usage.model);

    let evidence = AiCallResult {
        text: String::new(),
        executor: "codex-cli",
        model: usage_model
            .or(model)
            .unwrap_or_else(|| "codex-default".to_string()),
        usage,
        usage_source: Some("codex_log"),
    };
    let text = match exit {
        Ok(status) if status.success() => std::fs::read_to_string(&output_path)
            .with_context(|| format!("failed to read Codex output {}", output_path.display())),
        Ok(status) => Err(anyhow::anyhow!(
            "codex CLI exited {}: {}",
            status,
            String::from_utf8_lossy(&stderr_bytes)
        )),
        Err(error) => Err(error),
    };
    let _ = std::fs::remove_file(&output_path);
    finish_output(evidence, text)
}

fn finish_output(mut evidence: AiCallResult, text: Result<String>) -> Result<AiCallResult> {
    let text = text.and_then(|text| {
        let text = text.trim().to_string();
        if text.is_empty() {
            anyhow::bail!("codex CLI returned empty response");
        }
        Ok(text)
    });
    match text {
        Ok(text) => {
            evidence.text = text;
            Ok(evidence)
        }
        Err(error) => Err(AiCallFailure::with_evidence(error, evidence)),
    }
}

fn build_codex_args(
    output_path: &Path,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "--ask-for-approval",
        "never",
        "exec",
        "--ephemeral",
        "--ignore-user-config",
        "--ignore-rules",
        "--skip-git-repo-check",
        "--sandbox",
        "read-only",
        "--json",
        "--output-last-message",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();

    args.push(output_path.as_os_str().to_owned());
    if let Some(model) = model {
        args.push(OsString::from("--model"));
        args.push(OsString::from(model));
    }
    if let Some(reasoning_effort) = reasoning_effort {
        args.push(OsString::from("-c"));
        args.push(OsString::from(format!(
            "model_reasoning_effort=\"{}\"",
            reasoning_effort
        )));
    }
    args.push(OsString::from("-"));
    args
}

fn build_prompt(system: &str, user_message: &str) -> String {
    format!(
        "You are running as remem's Codex CLI summarization backend.\n\
         Follow the system instructions exactly and return only the requested output.\n\n\
         <system>\n{}\n</system>\n\n\
         <input>\n{}\n</input>\n",
        system, user_message
    )
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::build_codex_args;

    #[test]
    fn codex_args_put_global_approval_before_exec() {
        let args = build_codex_args(
            Path::new("/tmp/remem-out.txt"),
            Some("gpt-test"),
            Some("low"),
        );
        let rendered: Vec<String> = args
            .iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect();

        assert_eq!(&rendered[..3], ["--ask-for-approval", "never", "exec"]);
        for isolation_arg in ["--ephemeral", "--ignore-user-config", "--ignore-rules"] {
            assert!(
                rendered.iter().any(|arg| arg == isolation_arg),
                "{rendered:?}"
            );
        }
        assert!(rendered.iter().any(|arg| arg == "--json"), "{rendered:?}");
        assert!(
            rendered
                .windows(2)
                .any(|pair| pair[0] == "--output-last-message" && pair[1] == "/tmp/remem-out.txt"),
            "{rendered:?}"
        );
        assert!(
            rendered
                .windows(2)
                .any(|pair| pair[0] == "--model" && pair[1] == "gpt-test"),
            "{rendered:?}"
        );
        assert!(
            rendered
                .windows(2)
                .any(|pair| pair[0] == "-c" && pair[1] == "model_reasoning_effort=\"low\""),
            "{rendered:?}"
        );
        assert_eq!(rendered.last().map(String::as_str), Some("-"));
    }
}
