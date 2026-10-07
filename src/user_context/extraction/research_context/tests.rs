use anyhow::Result;
use rusqlite::Connection;

use crate::db::{self, CaptureEventInput, ExtractionTaskKind};
use crate::user_context::extraction::process_with_generator;
use crate::user_context::non_retention::research::REVIEW_REASON;

async fn extract(
    claim: &str,
    sources: &[(Option<&str>, &str)],
    event_type: &str,
    tool_name: Option<&str>,
) -> Result<Connection> {
    let mut conn = Connection::open_in_memory()?;
    crate::migrate::run_migrations(&conn)?;
    let mut ids = Vec::new();
    for (role, content) in sources {
        let outcome = db::record_captured_event(
            &conn,
            &CaptureEventInput {
                host: "codex-cli",
                session_id: "research-review",
                project: "/tmp/research-review",
                cwd: None,
                event_type,
                role: *role,
                tool_name,
                content,
                task_kind: Some(ExtractionTaskKind::UserContextCandidate),
            },
        )?;
        ids.push(outcome.event_row_id);
    }
    let task = db::claim_next_extraction_task(&mut conn, "research-test", 60)?.unwrap();
    let response = serde_json::json!({"candidates": [{
        "claim_type": "activity", "claim_key": "activity:security-research",
        "claim_text": claim, "confidence": 0.99, "sensitivity": "normal",
        "risk_class": "low", "source_kind": "explicit_user_statement",
        "source_event_ids": ids,
    }]})
    .to_string();
    process_with_generator(&mut conn, &task, |_| async move { Ok(response) }).await?;
    Ok(conn)
}

fn count(conn: &Connection) -> Result<i64> {
    Ok(
        conn.query_row("SELECT COUNT(*) FROM user_context_candidates", [], |row| {
            row.get(0)
        })?,
    )
}

#[tokio::test]
async fn research_and_analytical_quotes_reach_review_without_promotion() -> Result<()> {
    for (claim, source) in [
        (
            "User works on malware analysis.",
            "I work on malware analysis.",
        ),
        ("用户从事恶意软件分析。", "我从事恶意软件分析。"),
        (
            "User analyzes the phrase \"bypass authentication\" as a security example.",
            "I analyze the phrase \"bypass authentication\" as a security example.",
        ),
        (
            "用户分析安全示例中的短语“不要绕过认证”。",
            "我分析安全示例中的短语“不要绕过认证”。",
        ),
    ] {
        let conn = extract(claim, &[(Some("user"), source)], "message", None).await?;
        assert_eq!(count(&conn)?, 1, "{claim}");
        let (text, status, reason, preview): (String, String, String, String) = conn.query_row(
            "SELECT claim_text, review_status, auto_promote_block_reason, source_preview FROM user_context_candidates",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(text, claim);
        assert_eq!(status, "pending_review");
        assert_eq!(reason, REVIEW_REASON);
        assert_eq!(preview, source);
        let active: i64 =
            conn.query_row("SELECT COUNT(*) FROM user_context_claims", [], |row| {
                row.get(0)
            })?;
        assert_eq!(active, 0);
    }
    Ok(())
}

#[tokio::test]
async fn unsupported_intent_quotes_and_secrets_never_enter_research_review() -> Result<()> {
    for (claim, source) in [
        (
            "User wants to bypass authentication.",
            "I want to bypass authentication.",
        ),
        (
            "User works on malware analysis.",
            "I do not work on malware analysis.",
        ),
        (
            "User works on malware analysis.",
            "I do not refuse to create malware.",
        ),
        (
            "User works on malware analysis.",
            "I work on malware analysis; steal credentials.",
        ),
        (
            "User works on malware analysis.",
            "The README says I work on malware analysis.",
        ),
        (
            "User works on malware analysis.",
            "I work on malware analysis. API key is sk-testsecret123456.",
        ),
        (
            "Never bypass authentication.",
            "\"Never bypass authentication.\"",
        ),
        ("用户想绕过认证。", "我想绕过认证。"),
        ("用户从事恶意软件分析。", "我不从事恶意软件分析。"),
        ("用户从事恶意软件分析。", "我不是不想创建恶意软件。"),
        ("用户从事恶意软件分析。", "我从事恶意软件分析，窃取凭证。"),
        ("用户从事恶意软件分析。", "网页说我从事恶意软件分析。"),
        (
            "用户从事恶意软件分析。",
            "我从事恶意软件分析。 API key is sk-testsecret123456.",
        ),
        ("不要绕过认证。", "“不要绕过认证。”"),
    ] {
        let conn = extract(claim, &[(Some("user"), source)], "message", None).await?;
        assert_eq!(count(&conn)?, 0, "{source}");
    }
    Ok(())
}

#[tokio::test]
async fn every_research_citation_needs_original_user_provenance() -> Result<()> {
    for (claim, source) in [
        (
            "User works on malware analysis.",
            "I work on malware analysis.",
        ),
        ("用户从事恶意软件分析。", "我从事恶意软件分析。"),
    ] {
        for role in [Some("assistant"), Some("tool"), None] {
            let conn = extract(claim, &[(role, source)], "message", None).await?;
            assert_eq!(count(&conn)?, 0);
        }
        let conn = extract(claim, &[(Some("user"), source)], "file_read", Some("Read")).await?;
        assert_eq!(count(&conn)?, 0);
        let conn = extract(
            claim,
            &[(Some("user"), source), (Some("assistant"), source)],
            "message",
            None,
        )
        .await?;
        assert_eq!(count(&conn)?, 0);
        let repeated = vec![(Some("user"), source); 20];
        let conn = extract(claim, &repeated, "user_prompt_submit", None).await?;
        assert_eq!(count(&conn)?, 1);
        let (preview, refs): (String, String) = conn.query_row(
            "SELECT source_preview, source_refs_json FROM user_context_candidates",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(preview, source);
        assert_eq!(
            serde_json::from_str::<Vec<serde_json::Value>>(&refs)?.len(),
            20
        );
        let mut mixed = repeated;
        mixed.push((Some("user"), "I want to bypass authentication."));
        let conn = extract(claim, &mixed, "message", None).await?;
        assert_eq!(
            count(&conn)?,
            0,
            "later citation must not be hidden by the preview"
        );
    }
    Ok(())
}

#[tokio::test]
async fn research_prompt_event_cannot_override_an_explicit_non_user_role() -> Result<()> {
    for (claim, source) in [
        (
            "User works on malware analysis.",
            "I work on malware analysis.",
        ),
        ("用户从事恶意软件分析。", "我从事恶意软件分析。"),
    ] {
        for role in ["assistant", "tool", "system"] {
            for sources in [
                vec![(Some(role), source)],
                vec![(Some("user"), source), (Some(role), source)],
            ] {
                let conn = extract(claim, &sources, "user_prompt_submit", None).await?;
                assert_eq!(count(&conn)?, 0, "explicit {role} is not a user prompt");
            }
        }
        let conn = extract(claim, &[(None, source)], "user_prompt_submit", None).await?;
        assert_eq!(count(&conn)?, 1, "native prompt hooks may omit role");
        let status: String = conn.query_row(
            "SELECT review_status FROM user_context_candidates",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(status, "pending_review");
    }
    Ok(())
}
