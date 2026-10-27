use clap::{Args, Parser, Subcommand, ValueEnum};

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
    Check(CheckArgs),
    /// Rank documents by incoming links.
    Rank(TargetArgs),
    /// List managed documents.
    ListDocs(TargetArgs),
    /// Inspect one document's metadata and relations.
    Inspect(InputArgs),
    /// List incoming document references.
    Refs(InputArgs),
    /// Search documentation and code, returning connected knowledge bundles.
    Query(QueryArgs),
    /// Show a document's relation graph.
    Graph(InputArgs),
    /// Retrieve project state, code context, change impact, or a document graph.
    Context(ContextArgs),
    /// Manage local documentation checkpoints.
    History {
        #[command(subcommand)]
        command: Option<HistoryCommand>,
    },
    /// Diagnose the local environment.
    Doctor(DoctorArgs),
    /// Plan or apply structured documentation migrations.
    Migrate(MigrateArgs),
    /// Update from a release archive.
    Update(CommonArgs),
    /// Suggest document-to-code anchors from inferred relationships.
    SuggestAnchors(SuggestAnchorsArgs),
    /// Run a lightweight agent edit hook.
    Hook {
        #[command(subcommand)]
        command: Option<HookCommand>,
    },
    /// Print or install agent hook settings.
    Install(InstallArgs),
    #[command(external_subcommand)]
    #[allow(dead_code)]
    Unknown(Vec<String>),
}

#[derive(Subcommand)]
pub(super) enum HistoryCommand {
    /// Save a documentation checkpoint.
    Snapshot(SnapshotArgs),
    /// List checkpoints.
    List(TargetArgs),
    /// Compare current documents with the latest checkpoint.
    Status(TargetArgs),
    /// Compare a checkpoint with current documents or another checkpoint.
    Diff(DiffArgs),
    /// Preview or apply a checkpoint restore.
    Restore(RestoreArgs),
    /// Export snapshots and objects to a portable .tar.gz archive.
    Export(InputArgs),
    /// Merge a history archive, preserving existing snapshot IDs.
    Import(InputArgs),
    /// Print one file from a checkpoint without restoring documents.
    Show(ShowArgs),
    #[command(external_subcommand)]
    Unknown(Vec<String>),
}

#[derive(Args)]
pub(super) struct RestoreArgs {
    #[command(flatten)]
    pub(super) input: InputArgs,
    /// Apply the restore after the preflight preview.
    #[arg(long)]
    pub(super) apply: bool,
}

#[derive(Args)]
pub(super) struct SuggestAnchorsArgs {
    #[command(flatten)]
    pub(super) input: InputArgs,
    /// Apply one or more displayed suggestions (one-based indexes).
    #[arg(
        long,
        value_name = "N",
        num_args = 1..,
        value_delimiter = ',',
        value_parser = clap::value_parser!(usize)
    )]
    pub(super) apply: Vec<usize>,
}

#[derive(Subcommand)]
pub(super) enum HookCommand {
    /// Print the most relevant constraints before an edit.
    PreEdit(HookPathArgs),
    /// Report anchor drift after an edit.
    PostEdit(HookPathArgs),
}

#[derive(Args)]
pub(super) struct HookPathArgs {
    /// File path; when omitted it is read from Claude Code hook JSON on stdin.
    #[arg(value_name = "PATH")]
    pub(super) path: Option<String>,
}

#[derive(Args)]
pub(super) struct InstallArgs {
    /// Print or write Claude Code Edit/Write/MultiEdit hooks.
    #[arg(long)]
    pub(super) hooks: bool,
    /// Write project-local settings after creating a backup.
    #[arg(long)]
    pub(super) write: bool,
    /// Project directory (defaults to the current directory).
    #[arg(value_name = "TARGET_DIR")]
    pub(super) target_dir: Option<String>,
}

#[derive(Args)]
pub(super) struct SnapshotArgs {
    #[command(flatten)]
    pub(super) target: TargetArgs,
    /// Create a checkpoint even when file contents have not changed.
    #[arg(long)]
    pub(super) allow_empty: bool,
}

#[derive(Args)]
pub(super) struct ShowArgs {
    #[arg(value_name = "SNAPSHOT")]
    pub(super) snapshot: String,
    #[arg(value_name = "PATH")]
    pub(super) path: String,
    #[command(flatten)]
    pub(super) target: TargetArgs,
}

#[derive(Args, Clone, Default)]
pub(crate) struct TargetArgs {
    #[arg(value_name = "TARGET_DIR")]
    pub(crate) target_dir: Option<String>,
    #[command(flatten)]
    pub(crate) options: CommonArgs,
}

#[derive(Args)]
pub(super) struct DoctorArgs {
    #[command(flatten)]
    pub(super) target: TargetArgs,
    /// Refresh code nodes and report language, file and symbol counts.
    #[arg(long)]
    pub(super) code: bool,
    /// Summarize local usage across projects (all time unless --days is given).
    #[arg(
        long,
        long_help = "Summarize local usage across projects. Logs default to ~/.harnesskit/usage.jsonl; HARNESSKIT_HOME overrides the directory. HARNESSKIT_USAGE_LOG=off disables writes, full includes query text; the default records only metrics and a project-root hash. Zero-hit and relaxed-hit rates use completed queries as the denominator. A query is relaxed when any displayed result is a partial match."
    )]
    pub(super) usage: bool,
    /// Limit usage to the last N days.
    #[arg(long, value_name = "N", requires = "usage", value_parser = clap::value_parser!(u32).range(1..))]
    pub(super) days: Option<u32>,
}

#[derive(Args)]
pub(super) struct MigrateArgs {
    #[command(flatten)]
    pub(super) target: TargetArgs,
    /// Print the migration plan (the default when neither flag is supplied).
    #[arg(long, conflicts_with = "apply")]
    pub(super) plan: bool,
    /// Apply safe structured migrations after taking a history snapshot.
    #[arg(long, conflicts_with = "plan")]
    pub(super) apply: bool,
    /// Restrict planning or application to one migration category.
    #[arg(long, value_name = "CATEGORY")]
    pub(super) only: Option<String>,
}

#[derive(Copy, Clone, PartialEq, Eq, ValueEnum)]
pub(crate) enum CheckFormat {
    Summary,
    Full,
    Json,
}

#[derive(Args)]
pub(super) struct CheckArgs {
    #[command(flatten)]
    pub(super) target: TargetArgs,
    /// Output format (default: summary); --json remains an alias for json.
    #[arg(long, value_enum, conflicts_with = "json")]
    pub(super) format: Option<CheckFormat>,
    /// Expand every finding and list, including JSON lists.
    #[arg(long)]
    pub(super) all: bool,
    /// Replace the derived baseline after reporting changes against it.
    #[arg(long)]
    pub(super) update_baseline: bool,
    /// Re-seal anchors in a document; may be repeated and accepts globs.
    #[arg(long, value_name = "DOC", action = clap::ArgAction::Append)]
    pub(super) ack: Vec<String>,
    /// Rewrite uniquely moved anchors and refresh their seals.
    #[arg(long)]
    pub(super) fix: bool,
}

#[derive(Args)]
pub(super) struct InputArgs {
    #[arg(value_name = "INPUT", allow_hyphen_values = true)]
    pub(super) input: Option<String>,
    #[command(flatten)]
    pub(super) target: TargetArgs,
}

#[derive(Args)]
pub(super) struct QueryArgs {
    #[command(flatten)]
    pub(super) input: InputArgs,
    /// Maximum bundles (default: 5; document-only repositories retain 8 candidates).
    #[arg(
        long,
        value_name = "N",
        value_parser = clap::value_parser!(u32).range(1..)
    )]
    pub(super) limit: Option<u32>,
    /// Search only documentation; omit code from bundles.
    #[arg(long, conflicts_with = "code")]
    pub(super) docs: bool,
    /// Search only code cards; omit documentation from bundles.
    #[arg(long, conflicts_with = "docs")]
    pub(super) code: bool,
}

#[derive(Args)]
pub(super) struct ContextArgs {
    #[command(flatten)]
    pub(super) input: InputArgs,
    /// Show current routes and active plans (also the default without INPUT).
    #[arg(long, conflicts_with_all = ["since", "code_for", "diff"])]
    pub(super) current: bool,
    /// Show managed docs changed since a snapshot, latest, or YYYY-MM-DD (UTC).
    #[arg(long, value_name = "SNAPSHOT_OR_DATE", conflicts_with_all = ["current", "code_for", "diff"])]
    pub(super) since: Option<String>,
    /// Find chapters, decisions and cochanged files for a code path/symbol/directory.
    #[arg(long = "for", value_name = "PATH_OR_SYMBOL", conflicts_with_all = ["current", "since", "diff"])]
    pub(super) code_for: Option<String>,
    /// Review code changes against HEAD, or REV..HEAD plus staged/unstaged edits.
    #[arg(long, value_name = "REV", num_args = 0..=1, default_missing_value = "", conflicts_with_all = ["current", "since", "code_for"])]
    pub(super) diff: Option<String>,
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
    #[arg(long, action = clap::ArgAction::SetTrue, overrides_with = "local")]
    pub(crate) local: bool,
    #[arg(long, action = clap::ArgAction::SetTrue, overrides_with = "tracked")]
    pub(crate) tracked: bool,
    #[arg(long, action = clap::ArgAction::SetTrue, overrides_with = "preview")]
    pub(crate) preview: bool,
    /// Explain coverage and inferred layout, including previously shown hints.
    #[arg(long)]
    pub(crate) verbose: bool,
    /// Approximate output token budget (default: 1500).
    #[arg(long, value_name = "TOKENS", value_parser = clap::value_parser!(u32).range(1..))]
    pub(crate) budget: Option<u32>,
    /// Expand one result bundle or context section.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub(crate) expand: Option<u32>,
    /// Select a result page (one-based).
    #[arg(long, value_name = "K", value_parser = clap::value_parser!(u32).range(1..))]
    pub(crate) page: Option<u32>,
    /// Continue from an opaque result cursor.
    #[arg(long, value_name = "TOKEN")]
    pub(crate) cursor: Option<String>,
    /// Include verbose evidence fields in JSON output.
    #[arg(long)]
    pub(crate) full: bool,
    /// Explain document layout inference and coverage (doctor).
    #[arg(long)]
    pub(crate) layout: bool,
    /// Persist inferred mappings; existing schemas require --force (doctor).
    #[arg(long)]
    pub(crate) write_schema: bool,
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
