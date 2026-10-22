use clap::{Args, Parser, Subcommand};

use crate::output::eprintln;

#[derive(Parser)]
#[command(name = "harnesskit", version, about)]
pub(super) struct Cli {
    #[command(subcommand)]
    pub(super) command: Option<Command>,
}

#[derive(Subcommand)]
pub(super) enum Command {
    /// Create a documentation scaffold.
    Init(TargetArgs),
    /// Refresh the derived document index.
    Index(TargetArgs),
    /// Check managed documentation.
    Check(TargetArgs),
    /// Rank documents by incoming links.
    Rank(TargetArgs),
    /// List managed documents.
    ListDocs(TargetArgs),
    /// Inspect one document's metadata and relations.
    Inspect(InputArgs),
    /// List incoming document references.
    Refs(InputArgs),
    /// Search managed documents.
    Query(InputArgs),
    /// Show a document's relation graph.
    Graph(InputArgs),
    /// Retrieve context for terms or a document path.
    Context(InputArgs),
    /// Manage local documentation checkpoints.
    History {
        #[command(subcommand)]
        command: Option<HistoryCommand>,
    },
    /// Diagnose the local environment.
    Doctor(TargetArgs),
    /// Update from a release archive.
    Update(CommonArgs),
    #[command(external_subcommand)]
    Unknown(Vec<String>),
}

#[derive(Subcommand)]
pub(super) enum HistoryCommand {
    /// Save a documentation checkpoint.
    Snapshot(TargetArgs),
    /// List checkpoints.
    List(TargetArgs),
    /// Compare current documents with the latest checkpoint.
    Status(TargetArgs),
    /// Compare a checkpoint with current documents or another checkpoint.
    Diff(DiffArgs),
    /// Preview or apply a checkpoint restore.
    Restore(InputArgs),
    #[command(external_subcommand)]
    Unknown(Vec<String>),
}

#[derive(Args, Clone, Default)]
pub(crate) struct TargetArgs {
    #[arg(value_name = "TARGET_DIR")]
    pub(crate) target_dir: Option<String>,
    #[command(flatten)]
    pub(crate) options: CommonArgs,
}

#[derive(Args)]
pub(super) struct InputArgs {
    #[arg(value_name = "INPUT", allow_hyphen_values = true)]
    pub(super) input: Option<String>,
    #[command(flatten)]
    pub(super) target: TargetArgs,
}

#[derive(Args)]
pub(super) struct DiffArgs {
    #[arg(value_name = "SNAPSHOT_OR_TARGET")]
    pub(super) positionals: Vec<String>,
    #[command(flatten)]
    pub(super) options: CommonArgs,
}

// The old CLI accepted these options on every command, even when unused.
// Vec values preserve duplicate warnings and the existing last-value policy.
#[derive(Args, Clone, Default)]
pub(crate) struct CommonArgs {
    #[arg(long, value_name = "PATH", allow_hyphen_values = true)]
    pub(crate) schema: Vec<String>,
    #[arg(long, value_name = "PATH", allow_hyphen_values = true)]
    pub(crate) docs_root: Vec<String>,
    #[arg(long, value_name = "LIST", allow_hyphen_values = true)]
    pub(crate) rules: Vec<String>,
    #[arg(short = 'm', value_name = "MESSAGE", allow_hyphen_values = true)]
    pub(crate) short_message: Vec<String>,
    #[arg(long, value_name = "MESSAGE", allow_hyphen_values = true)]
    pub(crate) message: Vec<String>,
    #[arg(long, value_name = "TAG", allow_hyphen_values = true)]
    pub(crate) version: Vec<String>,
    #[arg(long, value_name = "OWNER/NAME", allow_hyphen_values = true)]
    pub(crate) repo: Vec<String>,
    #[arg(long, value_name = "URL", allow_hyphen_values = true)]
    pub(crate) asset_base_url: Vec<String>,
    #[arg(long, action = clap::ArgAction::SetTrue, overrides_with = "json")]
    pub(crate) json: bool,
    #[arg(long, action = clap::ArgAction::SetTrue, overrides_with = "strict")]
    pub(crate) strict: bool,
    #[arg(long, action = clap::ArgAction::SetTrue, overrides_with = "no_strict")]
    pub(crate) no_strict: bool,
    #[arg(long, action = clap::ArgAction::SetTrue, overrides_with = "force")]
    pub(crate) force: bool,
    #[arg(long, action = clap::ArgAction::SetTrue, overrides_with = "apply")]
    pub(crate) apply: bool,
    #[arg(long, action = clap::ArgAction::SetTrue, overrides_with = "local")]
    pub(crate) local: bool,
    #[arg(long, action = clap::ArgAction::SetTrue, overrides_with = "tracked")]
    pub(crate) tracked: bool,
    #[arg(long, action = clap::ArgAction::SetTrue, overrides_with = "preview")]
    pub(crate) preview: bool,
    #[arg(long, hide = true, action = clap::ArgAction::SetTrue, overrides_with = "cli_only")]
    cli_only: bool,
    #[arg(long, hide = true, action = clap::ArgAction::SetTrue, overrides_with = "no_claude")]
    no_claude: bool,
    #[arg(long, hide = true, action = clap::ArgAction::SetTrue, overrides_with = "no_codex")]
    no_codex: bool,
    #[arg(long, hide = true, action = clap::ArgAction::SetTrue, overrides_with = "with_codex_plugin")]
    with_codex_plugin: bool,
}

pub(super) fn option_value(values: &[String], flag: &str) -> Option<String> {
    if values.len() > 1 {
        eprintln!("warning: duplicate {}; using last value", flag);
    }
    values.last().cloned()
}
