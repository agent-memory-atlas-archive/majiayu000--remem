use super::{CandidateSourceBatch, ParsedUserContextCandidate};
use crate::user_context::non_retention::prevention;

pub(super) fn is_supported(
    candidate: &ParsedUserContextCandidate,
    batch: &CandidateSourceBatch,
) -> bool {
    if candidate.source_kind != "explicit_user_statement"
        || !matches!(
            candidate.claim_type,
            super::super::claims::UserContextClaimType::Preference
                | super::super::claims::UserContextClaimType::Constraint
        )
    {
        return false;
    }
    let Some(key) = prevention::constraint_key(&candidate.claim_text) else {
        return false;
    };
    let events = batch.events_for_candidate(candidate);
    !events.is_empty()
        && events.iter().all(|event| {
            batch.event_is_user_authored(event.id)
                && event.tool_name.is_none()
                && matches!(event.event_type.as_str(), "message" | "user_prompt_submit")
                && prevention::constraint_key(&event.content) == Some(key)
        })
}

pub(super) fn block_reason(
    candidate: &ParsedUserContextCandidate,
    batch: &CandidateSourceBatch,
) -> Option<&'static str> {
    (prevention::constraint_key(&candidate.claim_text).is_some() && !is_supported(candidate, batch))
        .then_some("no_supporting_user_source_event")
}

pub(super) const REVIEW_REASON: &str = "preventive_security_constraint_requires_review";

#[cfg(test)]
mod tests {
    use anyhow::Result;
    use rusqlite::Connection;

    use crate::db::{self, CaptureEventInput, ExtractionTaskKind};
    use crate::user_context::extraction::process_with_generator;

    async fn extract(
        claim: &str,
        sources: &[(Option<&str>, &str)],
        event_type: &str,
        tool_name: Option<&str>,
    ) -> Result<Connection> {
        let mut conn = Connection::open_in_memory()?;
        crate::migrate::run_migrations(&conn)?;
        let mut event_ids = Vec::new();
        for (role, content) in sources {
            let outcome = db::record_captured_event(
                &conn,
                &CaptureEventInput {
                    host: "codex-cli",
                    session_id: "preventive-constraint",
                    project: "/tmp/remem-preventive",
                    cwd: None,
                    event_type,
                    role: *role,
                    tool_name,
                    content,
                    task_kind: Some(ExtractionTaskKind::UserContextCandidate),
                },
            )?;
            event_ids.push(outcome.event_row_id);
        }
        let task = db::claim_next_extraction_task(&mut conn, "preventive-test", 60)?
            .expect("candidate task");
        let response = serde_json::json!({"candidates": [{
            "claim_type": "constraint",
            "claim_key": "constraint:security",
            "claim_text": claim,
            "confidence": 0.99,
            "sensitivity": "normal",
            "risk_class": "low",
            "source_kind": "explicit_user_statement",
            "source_event_ids": event_ids,
        }]})
        .to_string();
        process_with_generator(&mut conn, &task, |_| async move { Ok(response) }).await?;
        Ok(conn)
    }

    fn candidate_count(conn: &Connection) -> Result<i64> {
        Ok(
            conn.query_row("SELECT COUNT(*) FROM user_context_candidates", [], |row| {
                row.get(0)
            })?,
        )
    }

    #[tokio::test]
    async fn explicit_preventive_constraints_reach_review_with_exact_user_evidence() -> Result<()> {
        for (claim, source) in [
            (
                "User must not bypass authentication.",
                "I must not bypass authentication.",
            ),
            (
                "User never wants to bypass authentication.",
                "I never want to bypass authentication.",
            ),
            (
                "Never bypass authentication.",
                "Never bypass authentication.",
            ),
            (
                "User requires agents never to bypass authentication.",
                "I require agents never to bypass authentication.",
            ),
            (
                "User requires malware prevention.",
                "I require malware prevention.",
            ),
            ("用户要求不要绕过认证。", "我要求不要绕过认证。"),
            ("用户禁止窃取凭证。", "我禁止窃取凭证。"),
            ("用户要求防范网络钓鱼。", "我要求防范网络钓鱼。"),
        ] {
            let conn = extract(claim, &[(Some("user"), source)], "message", None).await?;
            assert_eq!(candidate_count(&conn)?, 1, "{claim}");
            let (status, reason, preview): (String, String, String) = conn.query_row(
                "SELECT review_status, auto_promote_block_reason, source_preview FROM user_context_candidates",
                [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(status, "pending_review");
            assert_eq!(reason, super::REVIEW_REASON);
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
    async fn repeated_preventive_sources_keep_a_complete_preview_and_validate_every_citation(
    ) -> Result<()> {
        for (claim, source, unsafe_source) in [
            (
                "Never bypass authentication.",
                "Never bypass authentication.",
                "I want to bypass authentication.",
            ),
            ("不要绕过认证。", "不要绕过认证。", "我想绕过认证。"),
        ] {
            let mut sources = vec![(Some("user"), source); 80];
            assert!(
                sources
                    .iter()
                    .map(|(_, text)| text.chars().count())
                    .sum::<usize>()
                    > 500
            );
            let conn = extract(claim, &sources, "message", None).await?;
            assert_eq!(candidate_count(&conn)?, 1);
            let (status, preview, refs): (String, String, String) = conn.query_row(
                "SELECT review_status, source_preview, source_refs_json FROM user_context_candidates",
                [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(status, "pending_review");
            assert_eq!(preview, source);
            assert_eq!(
                serde_json::from_str::<Vec<serde_json::Value>>(&refs)?.len(),
                sources.len()
            );

            sources.push((Some("user"), unsafe_source));
            let conn = extract(claim, &sources, "message", None).await?;
            assert_eq!(
                candidate_count(&conn)?,
                0,
                "later citations must still be checked"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn harmful_positive_quoted_double_negative_and_secret_sources_create_no_candidate(
    ) -> Result<()> {
        for (claim, source) in [
            (
                "User wants to bypass authentication.",
                "I want to bypass authentication.",
            ),
            (
                "Never bypass authentication.",
                "I do not want to never bypass authentication.",
            ),
            (
                "Never bypass authentication.",
                "\"Never bypass authentication.\"",
            ),
            (
                "Never bypass authentication.",
                "I never bypass authentication unless needed.",
            ),
            (
                "Never bypass authentication.",
                "I never bypass authentication. I want to bypass authentication.",
            ),
            (
                "Never bypass authentication.",
                "I never bypass authentication. API key is sk-testsecret123456.",
            ),
            ("用户想绕过认证。", "我想绕过认证。"),
            ("不要绕过认证。", "我不是不要绕过认证。"),
            ("不要绕过认证。", "“不要绕过认证。”"),
            ("不要绕过认证。", "我不要绕过认证，除非需要。"),
            ("不要绕过认证。", "我不要绕过认证。我想绕过认证。"),
            (
                "不要绕过认证。",
                "不要绕过认证。 API key is sk-testsecret123456.",
            ),
        ] {
            let conn = extract(claim, &[(Some("user"), source)], "message", None).await?;
            assert_eq!(candidate_count(&conn)?, 0, "{source}");
        }
        Ok(())
    }

    #[tokio::test]
    async fn preventive_exception_rejects_external_tool_and_mixed_provenance() -> Result<()> {
        for statement in ["Never bypass authentication.", "不要绕过认证。"] {
            for role in [Some("assistant"), Some("tool"), None] {
                let conn = extract(statement, &[(role, statement)], "message", None).await?;
                assert_eq!(candidate_count(&conn)?, 0);
            }
            let conn = extract(
                statement,
                &[(Some("user"), statement)],
                "file_read",
                Some("Read"),
            )
            .await?;
            assert_eq!(candidate_count(&conn)?, 0, "a file is not a user statement");
            let conn = extract(
                statement,
                &[(Some("user"), statement), (Some("assistant"), statement)],
                "message",
                None,
            )
            .await?;
            assert_eq!(
                candidate_count(&conn)?,
                0,
                "mixed citations must not inherit the exception"
            );
        }
        for (claim, source) in [
            (
                "Never bypass authentication.",
                "The README says: never bypass authentication.",
            ),
            ("不要绕过认证。", "网页说：不要绕过认证。"),
        ] {
            let conn = extract(claim, &[(Some("user"), source)], "message", None).await?;
            assert_eq!(candidate_count(&conn)?, 0);
        }
        Ok(())
    }
}
