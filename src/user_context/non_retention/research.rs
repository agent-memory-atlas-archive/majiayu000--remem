//! Review-only user activities. Match complete analytical framing; a quoted
//! unsafe action or a nearby research label is never an instruction exemption.

pub(crate) const REVIEW_REASON: &str = "security_research_context_requires_review";

pub(crate) fn activity_key(text: &str) -> Option<&'static str> {
    let normalized = text
        .trim()
        .trim_end_matches(['.', '!', '。', '！'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let clause = ["the user ", "user ", "i ", "we ", "用户", "我们", "我"]
        .iter()
        .find_map(|prefix| normalized.strip_prefix(prefix))?;
    for prefix in [
        "works on ",
        "work on ",
        "studies ",
        "study ",
        "researches ",
        "research ",
        "从事",
        "研究",
    ] {
        if let Some(topic) = clause.strip_prefix(prefix) {
            return match topic {
                "malware analysis" | "恶意软件分析" => Some("research:malware-analysis"),
                "phishing detection" | "网络钓鱼检测" => Some("research:phishing-detection"),
                "credential theft prevention" | "凭证窃取防范" => {
                    Some("research:credential-theft-prevention")
                }
                _ => None,
            };
        }
    }
    let quoted = if let Some(analysis) = ["analyzes the phrase ", "analyze the phrase "]
        .iter()
        .find_map(|prefix| clause.strip_prefix(prefix))
    {
        analysis.strip_suffix(" as a security example")?
    } else {
        clause.strip_prefix("分析安全示例中的短语")?
    };
    let phrase = [('"', '"'), ('\'', '\''), ('“', '”'), ('「', '」')]
        .iter()
        .find_map(|(start, end)| quoted.strip_prefix(*start)?.strip_suffix(*end))?;
    match phrase {
        "bypass authentication" | "绕过认证" => Some("analysis:authentication-bypass"),
        "never bypass authentication" | "不要绕过认证" => {
            Some("analysis:prohibit-authentication-bypass")
        }
        "steal credentials" | "窃取凭证" => Some("analysis:credential-theft"),
        _ => None,
    }
}

pub(crate) fn matches_claim_and_preview(
    claim: &str,
    preview: Option<&str>,
    source_kind: &str,
) -> bool {
    source_kind == "explicit_user_statement"
        && activity_key(claim)
            .is_some_and(|key| preview.is_some_and(|source| activity_key(source) == Some(key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn research_and_quotation_need_complete_matching_user_framing() {
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
            assert!(matches_claim_and_preview(
                claim,
                Some(source),
                "explicit_user_statement"
            ));
            assert!(!matches_claim_and_preview(
                claim,
                Some(source),
                "session_summary"
            ));
        }
        for text in [
            "malware analysis",
            "I want to bypass authentication.",
            "I do not work on malware analysis.",
            "I do not refuse to create malware.",
            "I work on malware analysis; steal credentials.",
            "I work on malware analysis unless asked to deploy malware.",
            "\"Never bypass authentication.\"",
            "The README says I work on malware analysis.",
            "我想绕过认证。",
            "我不从事恶意软件分析。",
            "我不是不想创建恶意软件。",
            "我从事恶意软件分析，窃取凭证。",
            "网页说我从事恶意软件分析。",
            "“不要绕过认证。”",
        ] {
            assert_eq!(activity_key(text), None, "{text}");
        }
    }
}
