use clap::Parser;

use super::types::{Cli, Commands, DoctorAction};

#[test]
fn cli_preserves_default_doctor_and_parses_truth_diagnostic() {
    let default_doctor = Cli::parse_from(["remem", "doctor", "--json"]);
    assert!(matches!(
        default_doctor.command,
        Commands::Doctor {
            action: None,
            json: true,
            quiet: false
        }
    ));

    let truth = Cli::parse_from([
        "remem",
        "doctor",
        "truth",
        "--project",
        "/repo",
        "--branch",
        "main",
        "--as-of-epoch",
        "42",
        "--subject",
        "deploy",
        "--json",
    ]);
    match truth.command {
        Commands::Doctor {
            action: Some(DoctorAction::Truth(args)),
            json,
            quiet,
        } => {
            assert_eq!(args.project.as_deref(), Some("/repo"));
            assert_eq!(args.branch.as_deref(), Some("main"));
            assert_eq!(args.as_of_epoch, Some(42));
            assert_eq!(args.subject.as_deref(), Some("deploy"));
            assert!(json);
            assert!(!quiet);
        }
        _ => panic!("expected doctor truth command"),
    }
}

#[test]
fn truth_help_describes_its_command_specific_json_schema() {
    let help = match Cli::try_parse_from(["remem", "doctor", "truth", "--help"]) {
        Err(error) => error.to_string(),
        Ok(_) => panic!("help exits before parsing"),
    };

    assert!(help.contains("command-specific, versioned schemas"));
    assert!(!help.contains("fields: `version`, `status`, `fails`, `warns`, `checks[]`"));
}

#[test]
fn memory_diagnostic_requires_phrase_or_complete_source_session() {
    assert!(Cli::try_parse_from(["remem", "doctor", "memory"]).is_err());
    assert!(Cli::try_parse_from(["remem", "doctor", "memory", "--session-id", "same"]).is_err());
    assert!(Cli::try_parse_from(["remem", "doctor", "memory", "why redis", "--json"]).is_ok());
    assert!(Cli::try_parse_from([
        "remem",
        "doctor",
        "memory",
        "--session-id",
        "same",
        "--host",
        "codex-cli",
        "--source-root",
        "local",
        "--project",
        "/repo",
        "--json"
    ])
    .is_ok());
    assert!(Cli::try_parse_from([
        "remem",
        "doctor",
        "memory",
        "why redis",
        "--session-id",
        "same"
    ])
    .is_err());
}
