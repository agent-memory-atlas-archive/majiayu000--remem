use super::{CandidateSourceBatch, ParsedUserContextCandidate};
use crate::user_context::non_retention::research;

pub(super) fn is_supported(
    candidate: &ParsedUserContextCandidate,
    batch: &CandidateSourceBatch,
) -> bool {
    if candidate.source_kind != "explicit_user_statement"
        || !matches!(
            candidate.claim_type,
            super::super::claims::UserContextClaimType::Activity
                | super::super::claims::UserContextClaimType::Role
                | super::super::claims::UserContextClaimType::Skill
        )
    {
        return false;
    }
    let Some(key) = research::activity_key(&candidate.claim_text) else {
        return false;
    };
    let events = batch.events_for_candidate(candidate);
    !events.is_empty()
        && events.iter().all(|event| {
            batch.event_is_user_authored(event.id)
                && event.tool_name.is_none()
                && matches!(event.event_type.as_str(), "message" | "user_prompt_submit")
                && research::activity_key(&event.content) == Some(key)
        })
}

pub(super) fn block_reason(
    candidate: &ParsedUserContextCandidate,
    batch: &CandidateSourceBatch,
) -> Option<&'static str> {
    (research::activity_key(&candidate.claim_text).is_some() && !is_supported(candidate, batch))
        .then_some("no_supporting_user_source_event")
}

#[cfg(test)]
mod tests;
