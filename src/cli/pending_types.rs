use clap::Subcommand;

#[derive(Subcommand)]
#[allow(clippy::enum_variant_names)]
pub(in crate::cli) enum PendingAction {
    /// List failed pending observation rows.
    #[command(alias = "list")]
    ListFailed {
        /// Restrict rows to one project path.
        #[arg(long, short)]
        project: Option<String>,
        /// Maximum failed rows to show.
        #[arg(long, short = 'n', default_value = "20")]
        limit: i64,
        #[arg(long)]
        json: bool,
    },
    /// Move failed legacy pending rows back to pending so migrate-legacy can replay them.
    #[command(alias = "retry")]
    RetryFailed {
        /// Restrict rows to one project path.
        #[arg(long, short)]
        project: Option<String>,
        /// Maximum failed rows to retry.
        #[arg(long, short = 'n', default_value = "100")]
        limit: i64,
        /// Preview retry count without mutating rows.
        #[arg(long)]
        dry_run: bool,
    },
    /// Purge old failed pending observation rows.
    #[command(alias = "purge")]
    PurgeFailed {
        /// Restrict rows to one project path.
        #[arg(long, short)]
        project: Option<String>,
        /// Only purge failed rows older than this many days.
        #[arg(long, default_value = "7")]
        older_than_days: i64,
        /// Preview purge count without deleting rows.
        #[arg(long)]
        dry_run: bool,
    },
    /// Replay legacy pending rows into captured_events/extraction_tasks.
    MigrateLegacy {
        /// Restrict rows to one project path.
        #[arg(long, short)]
        project: Option<String>,
        /// Host to use for rows stored with legacy host=unknown.
        #[arg(long)]
        host: Option<String>,
        /// Maximum pending rows to migrate.
        #[arg(long, short = 'n', default_value = "100")]
        limit: i64,
        /// Preview migration count without mutating rows.
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// Recover exactly one archived failed legacy pending row.
    RecoverArchived {
        /// Archived failed pending observation ID to recover.
        #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
        id: i64,
        /// Capture host fallback required when the archived row has a legacy/unknown host.
        #[arg(long, value_parser = ["claude-code", "codex-cli"])]
        host: Option<String>,
        /// Validate the exact candidate without writing.
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
    /// List exhausted extraction event ranges.
    ListExtractionRanges {
        /// List exactly one range by ID, including terminal replay evidence.
        #[arg(
            long,
            value_parser = clap::value_parser!(i64).range(1..),
            conflicts_with_all = ["project", "limit"]
        )]
        id: Option<i64>,
        #[arg(long, short)]
        project: Option<String>,
        #[arg(long, short = 'n')]
        limit: Option<i64>,
        #[arg(long)]
        json: bool,
    },
    /// Requeue exhausted extraction event ranges.
    RetryExtractionRanges {
        /// Requeue exactly one range by ID.
        #[arg(
            long,
            value_parser = clap::value_parser!(i64).range(1..),
            conflicts_with_all = ["project", "limit"]
        )]
        id: Option<i64>,
        #[arg(long, short)]
        project: Option<String>,
        #[arg(long, short = 'n')]
        limit: Option<i64>,
        /// Explicitly allow retrying one quarantined range.
        #[arg(long, requires = "id")]
        acknowledge_quarantine: bool,
        /// Validate an archived quarantined exact range without mutating it.
        #[arg(
            long,
            requires_all = ["id", "acknowledge_quarantine", "dry_run"]
        )]
        include_archived: bool,
        #[arg(long)]
        dry_run: bool,
    },
    /// Quarantine exhausted extraction event ranges.
    QuarantineExtractionRanges {
        /// Quarantine exactly one range by ID.
        #[arg(
            long,
            value_parser = clap::value_parser!(i64).range(1..),
            conflicts_with_all = ["project", "limit"]
        )]
        id: Option<i64>,
        #[arg(long, short)]
        project: Option<String>,
        #[arg(long, short = 'n')]
        limit: Option<i64>,
        #[arg(long)]
        dry_run: bool,
    },
}
