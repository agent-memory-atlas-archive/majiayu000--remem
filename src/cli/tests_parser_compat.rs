use super::types::{Cli, Commands};
use clap::{CommandFactory, Parser};

#[test]
fn cli_builder_keeps_root_metadata_and_existing_argument_group_ids() {
    let command = Cli::command();
    assert_eq!(command.get_name(), "remem");
    assert_eq!(
        command.get_about().unwrap().to_string(),
        "Persistent memory for Claude Code and Codex"
    );
    assert_eq!(command.get_version(), Some(env!("CARGO_PKG_VERSION")));
    assert_eq!(
        command
            .get_groups()
            .map(|group| group.get_id().as_str())
            .collect::<Vec<_>>(),
        ["Cli"]
    );
    for (name, group_id) in [
        ("context", "Context"),
        ("govern", "Govern"),
        ("reroute", "Reroute"),
        ("search", "Search"),
    ] {
        let subcommand = command.find_subcommand(name).unwrap();
        assert_eq!(
            subcommand
                .get_groups()
                .map(|group| group.get_id().as_str())
                .collect::<Vec<_>>(),
            [group_id],
            "argument group changed for {name}"
        );
    }
    assert_eq!(
        command.find_subcommand("eval").is_some(),
        cfg!(feature = "eval")
    );
}

#[test]
fn cli_update_without_subcommand_keeps_existing_command() {
    assert!(Cli::try_parse_from(["remem"]).is_err());
    let mut cli = Cli::try_parse_from([
        "remem",
        "current",
        "original-key",
        "--project",
        "/fixture/project",
        "--json",
    ])
    .unwrap();
    cli.try_update_from(["remem"]).unwrap();
    match cli.command {
        Commands::Current {
            state_key,
            project,
            json,
            ..
        } => {
            assert_eq!(state_key, "original-key");
            assert_eq!(project.as_deref(), Some("/fixture/project"));
            assert!(json);
        }
        _ => panic!("an empty update replaced the original command"),
    }
}

#[test]
fn cli_update_preserves_named_variant_fields_and_accepts_argument_alias() {
    let mut cli = Cli::try_parse_from([
        "remem",
        "current",
        "original-key",
        "--project",
        "/fixture/project",
        "--owner-scope",
        "repo",
        "--owner-key",
        "fixture-owner",
    ])
    .unwrap();
    cli.try_update_from(["remem", "current", "updated-key", "--type", "procedure"])
        .unwrap();
    match cli.command {
        Commands::Current {
            state_key,
            project,
            memory_type,
            owner_scope,
            owner_key,
            ..
        } => {
            assert_eq!(state_key, "updated-key");
            assert_eq!(project.as_deref(), Some("/fixture/project"));
            assert_eq!(memory_type.as_deref(), Some("procedure"));
            assert_eq!(owner_scope.as_deref(), Some("repo"));
            assert_eq!(owner_key.as_deref(), Some("fixture-owner"));
        }
        _ => panic!("a partial named-variant update replaced the command"),
    }
}

#[test]
fn cli_update_preserves_search_fields_and_switches_using_command_alias() {
    let mut cli = Cli::try_parse_from([
        "remem",
        "search",
        "original query",
        "--project",
        "/fixture/project",
        "--type",
        "decision",
    ])
    .unwrap();
    cli.try_update_from(["remem", "search", "updated query", "--branch", "fixture"])
        .unwrap();
    match &cli.command {
        Commands::Search {
            query,
            project,
            memory_type,
            branch,
            ..
        } => {
            assert_eq!(query, "updated query");
            assert_eq!(project.as_deref(), Some("/fixture/project"));
            assert_eq!(memory_type.as_deref(), Some("decision"));
            assert_eq!(branch.as_deref(), Some("fixture"));
        }
        _ => panic!("a partial search update replaced the command"),
    }
    cli.try_update_from([
        "remem",
        "reindex-embeddings",
        "--limit",
        "7",
        "--batch-size",
        "3",
    ])
    .unwrap();
    match cli.command {
        Commands::BackfillEmbeddings { limit, batch_size } => {
            assert_eq!(limit, 7);
            assert_eq!(batch_size, 3);
        }
        _ => panic!("the command alias did not replace the previous variant"),
    }
}
