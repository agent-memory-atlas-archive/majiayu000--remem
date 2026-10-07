use super::types::{Check, Status};

pub(super) fn check_capture_capabilities() -> Vec<Check> {
    let (hosts, codex_error) = super::environment::active_host_names();
    let mut checks = if hosts.is_empty() && codex_error.is_some() {
        Vec::new()
    } else {
        check_capture_capabilities_for(&hosts)
    };
    if let Some(error) = codex_error {
        checks.push(Check::new(
            "Capture capability (codex)",
            Status::Fail,
            format!("invalid Codex home: {error}"),
        ));
    }
    checks
}

fn check_capture_capabilities_for(hosts: &[&'static str]) -> Vec<Check> {
    if hosts.is_empty() {
        return vec![Check::new(
            "Capture capability",
            Status::Warn,
            "capture=none; no supported host detected",
        )];
    }
    hosts.iter().map(|host| capture_capability(host)).collect()
}

fn capture_capability(host: &'static str) -> Check {
    match host {
        "claude" => Check::new(
            "Capture capability (claude)",
            Status::Ok,
            "capture=full; PostToolUse observe plus Stop/PreCompact transcript drain",
        ),
        "codex" => Check::new(
            "Capture capability (codex)",
            Status::Ok,
            "capture=drain-only; SessionStart context, UserPromptSubmit prompt capture/candidate index, and Stop transcript drain; PostToolUse observe is intentionally unsupported",
        ),
        _ => Check::new(
            "Capture capability",
            Status::Warn,
            "capture=none; unsupported host",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_supported_host_capture_capabilities() {
        let checks = check_capture_capabilities_for(&["claude", "codex"]);

        assert_eq!(checks.len(), 2);
        assert_eq!(checks[0].name, "Capture capability (claude)");
        assert!(matches!(checks[0].status, Status::Ok));
        assert!(checks[0].detail.contains("capture=full"));
        assert_eq!(checks[1].name, "Capture capability (codex)");
        assert!(matches!(checks[1].status, Status::Ok));
        assert!(checks[1].detail.contains("capture=drain-only"));
    }

    #[test]
    fn labels_absent_hosts_as_no_capture() {
        let checks = check_capture_capabilities_for(&[]);

        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].name, "Capture capability");
        assert!(matches!(checks[0].status, Status::Warn));
        assert!(checks[0].detail.contains("capture=none"));
    }
}
