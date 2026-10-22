mod args;

use args::{option_value, Cli, Command, DiffArgs, HistoryCommand, InputArgs};
pub(crate) use args::{CommonArgs, TargetArgs};
use clap::{CommandFactory, Parser};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::{env, fs};

use crate::checks::filter_and_suppress_checks;
use crate::context::{context_packet_for_doc, doc_exists};
use crate::doctor::build_doctor_report;
use crate::fs_utils::{ensure_dir, now_unix};
use crate::git::ensure_host_git_exclude;
use crate::history::{
    apply_restore_plan, compute_history_status, create_history_snapshot, diff_snapshot_to_worktree,
    diff_snapshots, is_history_dirty, list_history_snapshots, resolve_snapshot_arg, restore_plan,
};
use crate::index::{open_fact_store, refresh_fact_store, scalar_count, EngineContext};
use crate::query::{query_candidates, query_rows_to_candidates};
use crate::render::{
    print_check_output, print_context_packet, print_doctor_report, print_history_diff,
    print_history_list, print_history_snapshot_result, print_history_status, print_index_result,
    print_init_preview, print_init_result, print_init_summary, print_inspect_output,
    print_list_docs_rows, print_query_candidates, print_rank_rows, print_refs_output,
    print_restore_plan, render_check_json, render_context_packet_json, render_doctor_json,
    render_query_json,
};
use crate::schema::{
    default_context_schema_arg, load_schema, parse_schema, render_schema_copy,
    rewrite_docs_root_bound_paths, Schema, BUNDLED_SCHEMA_PATH,
};
use crate::templates::{materialize_from_schema, validate_init_templates};
use crate::update::{detect_release_target, update_from_release};
use crate::Result;

#[derive(Default)]
pub(crate) struct CommandOptions {
    pub(crate) json: bool,
    pub(crate) strict: bool,
    pub(crate) rules: Option<BTreeSet<String>>,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum InitMode {
    Local,
    Tracked,
}

fn run_init(args: &TargetArgs) -> Result<()> {
    let target_dir = PathBuf::from(args.target_dir.as_deref().unwrap_or("."));
    let docs_root_override = option_value(&args.options.docs_root, "--docs-root");
    let schema_arg = option_value(&args.options.schema, "--schema")
        .unwrap_or_else(|| BUNDLED_SCHEMA_PATH.to_string());
    let force = args.options.force;
    let preview = args.options.preview;
    let local = args.options.local;
    let tracked = args.options.tracked;
    if local && tracked {
        return Err("harnesskit init accepts only one of --local or --tracked".into());
    }
    let mode = if tracked {
        InitMode::Tracked
    } else {
        InitMode::Local
    };

    let schema_load = load_schema(&target_dir, &schema_arg)?;
    let mut schema = parse_schema(&schema_load.text)?;
    let original_managed_root = schema.managed_root.clone();
    if let Some(ref docs_root) = docs_root_override {
        schema.managed_root = docs_root.clone();
        rewrite_docs_root_bound_paths(&mut schema, &original_managed_root, docs_root);
    }
    let schema_copy_text = render_schema_copy(&schema);
    validate_init_templates(&schema)?;

    if preview {
        print_init_preview(&target_dir, &schema, &schema_load.source, mode, force);
        return Ok(());
    }

    ensure_dir(&target_dir)?;

    let stats = materialize_from_schema(&target_dir, &schema, &schema_copy_text, force)?;
    let exclude_stats = if mode == InitMode::Local {
        ensure_host_git_exclude(&target_dir, &schema)?
    } else {
        None
    };

    print_init_result(&target_dir, &schema_load.source, &stats);
    print_init_summary(&target_dir, &schema, mode, exclude_stats);
    Ok(())
}

fn run_index(args: &TargetArgs) -> Result<()> {
    let context = load_engine_context(args)?;
    refresh_fact_store(&context)?;
    let target_dir = context.target_dir;
    let db = open_fact_store(&target_dir)?;
    let doc_count = scalar_count(&db, "SELECT COUNT(*) FROM docs;")?;
    let relation_count = scalar_count(&db, "SELECT COUNT(*) FROM relations;")?;

    print_index_result(&target_dir, doc_count, relation_count);
    Ok(())
}

fn run_check(args: &TargetArgs) -> Result<()> {
    let context = load_engine_context(args)?;
    let options = command_options(&args.options, Some(&context.schema));
    refresh_fact_store(&context)?;
    let target_dir = context.target_dir;
    let db = open_fact_store(&target_dir)?;
    let docs_count = scalar_count(&db, "SELECT COUNT(*) FROM docs;")?;
    let relation_count = scalar_count(&db, "SELECT COUNT(*) FROM relations;")?;
    let check_rows = db.query(
        "SELECT severity, rule_id, subject_path, message, evidence_json FROM checks ORDER BY
            CASE severity WHEN 'error' THEN 0 WHEN 'warning' THEN 1 ELSE 2 END,
            subject_path ASC,
            rule_id ASC;",
        [],
    )?;
    let checks = filter_and_suppress_checks(&check_rows, &context.schema, &options);
    let warning_count = checks
        .iter()
        .filter(|check| !check.suppressed && check.severity == "warning")
        .count();
    let error_count = checks
        .iter()
        .filter(|check| !check.suppressed && check.severity == "error")
        .count();
    if options.json {
        println!(
            "{}",
            render_check_json(&target_dir, docs_count, relation_count, &checks)
        );
        if error_count > 0 || (options.strict && warning_count > 0) {
            std::process::exit(1);
        }
        return Ok(());
    }

    print_check_output(&target_dir, docs_count, relation_count, &checks);

    if error_count > 0 || (options.strict && warning_count > 0) {
        std::process::exit(1);
    }

    Ok(())
}

fn run_doctor(args: &TargetArgs) -> Result<()> {
    let target_dir = PathBuf::from(args.target_dir.as_deref().unwrap_or("."));
    let json = args.options.json;
    let report = build_doctor_report(&target_dir);

    if json {
        println!("{}", render_doctor_json(&report));
    } else {
        print_doctor_report(&report);
    }
    Ok(())
}

fn run_update(args: &CommonArgs) -> Result<()> {
    let version = option_value(&args.version, "--version").unwrap_or_else(|| "latest".to_string());
    let repo =
        option_value(&args.repo, "--repo").unwrap_or_else(|| "qWaitCrypto/HarnessKit".to_string());
    let asset_base_url_arg = option_value(&args.asset_base_url, "--asset-base-url");
    if !repo.contains('/') {
        return Err("--repo must use owner/name format".into());
    }

    let target = detect_release_target()?;
    let package = format!("harnesskit-{}", target);
    let archive = format!("{}.tar.gz", package);
    let asset_base_url = asset_base_url_arg.unwrap_or_else(|| {
        if version == "latest" {
            format!("https://github.com/{}/releases/latest/download", repo)
        } else {
            format!("https://github.com/{}/releases/download/{}", repo, version)
        }
    });

    let current_exe = env::current_exe()?;
    let tmp_dir = env::temp_dir().join(format!(
        "harnesskit-update-{}-{}",
        std::process::id(),
        now_unix()
    ));
    fs::create_dir_all(&tmp_dir)?;
    let result = update_from_release(
        &asset_base_url,
        &archive,
        &package,
        &tmp_dir,
        &current_exe,
        &version,
        &target,
    );
    let _ = fs::remove_dir_all(&tmp_dir);
    result
}

fn run_rank(args: &TargetArgs) -> Result<()> {
    let context = load_engine_context(args)?;
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;
    let result = db.query(
        "SELECT d.path, d.title, COALESCE(SUM(CASE
            WHEN r.relation_type = 'doc_indexes_doc' THEN 3
            WHEN r.relation_type = 'doc_links_doc' THEN 1
            WHEN r.relation_type = 'supersedes' THEN 1
            ELSE 0 END), 0) AS score,
            d.role,
            d.summary
         FROM docs d
         LEFT JOIN relations r ON r.dst_path = d.path
         GROUP BY d.path, d.title, d.role, d.summary
         ORDER BY score DESC, d.path ASC
         LIMIT 20;",
        [],
    )?;

    print_rank_rows(&result);
    Ok(())
}

fn run_list_docs(args: &TargetArgs) -> Result<()> {
    let context = load_engine_context(args)?;
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;
    let result = db.query(
        "SELECT path, title, role, COALESCE(status, ''), COALESCE(authority, ''), incoming_links, outgoing_links, COALESCE(collection_name, '')
         FROM docs
         ORDER BY path ASC;", [])?;

    print_list_docs_rows(&result);
    Ok(())
}

fn run_inspect(args: &InputArgs) -> Result<()> {
    let doc_path = args.input.clone().ok_or("inspect requires <doc_path>")?;
    let context = load_engine_context(&args.target)?;
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;

    let doc = db.query(
        "SELECT path, title, role, COALESCE(status, ''), COALESCE(authority, ''), COALESCE(collection_name, ''), summary, incoming_links, outgoing_links, is_entrypoint, is_anchor_candidate, is_generated, is_reference, file_size_bytes, mtime_unix
         FROM docs WHERE path = ?1;",
        [&doc_path],
    )?;
    if doc.rows.is_empty() {
        return Err(format!("doc not found in fact store: {}", doc_path).into());
    }

    let outgoing = db.query(
        "SELECT relation_type, dst_path, reason, confidence
         FROM relations
         WHERE src_path = ?1
         ORDER BY relation_type ASC, dst_path ASC;",
        [&doc_path],
    )?;
    let incoming = db.query(
        "SELECT relation_type, src_path, reason, confidence
         FROM relations
         WHERE dst_path = ?1
         ORDER BY relation_type ASC, src_path ASC;",
        [&doc_path],
    )?;
    let checks = db.query(
        "SELECT severity, rule_id, message, evidence_json
         FROM checks
         WHERE subject_path = ?1
         ORDER BY severity DESC, rule_id ASC;",
        [&doc_path],
    )?;

    print_inspect_output(&doc, &outgoing, &incoming, &checks);
    Ok(())
}

fn run_refs(args: &InputArgs) -> Result<()> {
    let doc_path = args.input.clone().ok_or("refs requires <doc_path>")?;
    let context = load_engine_context(&args.target)?;
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;
    let result = db.query(
        "SELECT src_path, relation_type, reason, confidence
         FROM relations
         WHERE dst_path = ?1
         ORDER BY relation_type ASC, src_path ASC;",
        [&doc_path],
    )?;

    print_refs_output(&doc_path, &result);
    Ok(())
}

fn run_query(args: &InputArgs) -> Result<()> {
    let query_terms = args.input.clone().ok_or("query requires <terms>")?;
    let context = load_engine_context(&args.target)?;
    let options = command_options(&args.target.options, Some(&context.schema));
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;

    let result = query_candidates(&db, &context.target_dir, &query_terms)?;
    let candidates = query_rows_to_candidates(&result, &context.target_dir, &query_terms);

    if options.json {
        println!("{}", render_query_json(&query_terms, &candidates));
    } else {
        print_query_candidates(&query_terms, &candidates);
    }
    Ok(())
}

fn run_graph(args: &InputArgs) -> Result<()> {
    let doc_path = args.input.clone().ok_or("graph requires <doc_path>")?;
    let context = load_engine_context(&args.target)?;
    let options = command_options(&args.target.options, Some(&context.schema));
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;
    let packet = context_packet_for_doc(&db, &context.target_dir, &doc_path)?;

    if options.json {
        println!(
            "{}",
            render_context_packet_json("graph", &doc_path, &packet)
        );
    } else {
        print_context_packet("Graph", &doc_path, &packet);
    }
    Ok(())
}

fn run_context(args: &InputArgs) -> Result<()> {
    let input = args
        .input
        .clone()
        .ok_or("context requires <terms-or-doc-path>")?;
    let context = load_engine_context(&args.target)?;
    let options = command_options(&args.target.options, Some(&context.schema));
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;
    let anchor_path = if doc_exists(&db, &input)? {
        input.clone()
    } else {
        let rows = query_candidates(&db, &context.target_dir, &input)?;
        let candidates = query_rows_to_candidates(&rows, &context.target_dir, &input);
        candidates
            .first()
            .map(|candidate| candidate.path.clone())
            .ok_or_else(|| format!("no context candidate found for query: {}", input))?
    };
    let packet = context_packet_for_doc(&db, &context.target_dir, &anchor_path)?;

    if options.json {
        println!("{}", render_context_packet_json("context", &input, &packet));
    } else {
        print_context_packet("Context", &input, &packet);
    }
    Ok(())
}

fn run_history(command: Option<HistoryCommand>) -> Result<()> {
    match command {
        Some(HistoryCommand::Snapshot(args)) => run_history_snapshot(&args),
        Some(HistoryCommand::List(args)) => run_history_list(&args),
        Some(HistoryCommand::Status(args)) => run_history_status(&args),
        Some(HistoryCommand::Diff(args)) => run_history_diff(&args),
        Some(HistoryCommand::Restore(args)) => run_history_restore(&args),
        Some(HistoryCommand::Unknown(args)) => {
            Err(format!("unknown history subcommand: {}", args[0]).into())
        }
        None => Err("history requires a subcommand: snapshot, list, status, diff, restore".into()),
    }
}

fn run_history_snapshot(args: &TargetArgs) -> Result<()> {
    let message = option_value(&args.options.short_message, "-m")
        .or_else(|| option_value(&args.options.message, "--message"))
        .unwrap_or_else(|| "snapshot".to_string());
    let context = load_engine_context(args)?;
    let snapshot = create_history_snapshot(&context.target_dir, &context.schema, &message)?;
    print_history_snapshot_result(&snapshot);
    Ok(())
}

fn run_history_list(args: &TargetArgs) -> Result<()> {
    let context = load_engine_context(args)?;
    let snapshots = list_history_snapshots(&context.target_dir)?;
    print_history_list(&snapshots);
    Ok(())
}

fn run_history_status(args: &TargetArgs) -> Result<()> {
    let context = load_engine_context(args)?;
    let status = compute_history_status(&context.target_dir, &context.schema)?;
    print_history_status(&status);
    Ok(())
}

fn run_history_diff(args: &DiffArgs) -> Result<()> {
    let positionals = &args.positionals;
    if positionals.is_empty() {
        return Err("history diff requires <snapshot-id|latest> or <a> <b>".into());
    }
    let snapshot_args = history_diff_snapshot_args(positionals);
    let target = TargetArgs {
        target_dir: positionals.get(snapshot_args.len()).cloned(),
        options: args.options.clone(),
    };
    let context = load_engine_context(&target)?;
    let first = resolve_snapshot_arg(&context.target_dir, &positionals[0])?;
    let second = if snapshot_args.len() >= 2 {
        Some(resolve_snapshot_arg(
            &context.target_dir,
            &snapshot_args[1],
        )?)
    } else {
        None
    };

    let diff = if let Some(second) = second {
        diff_snapshots(&first, &second)
    } else {
        diff_snapshot_to_worktree(&context.target_dir, &context.schema, &first)?
    };
    print_history_diff(&diff);
    Ok(())
}

fn history_diff_snapshot_args(positionals: &[String]) -> Vec<String> {
    if positionals.len() <= 1 {
        return positionals.to_vec();
    }
    let second = Path::new(&positionals[1]);
    if second.exists() && second.is_dir() {
        vec![positionals[0].clone()]
    } else {
        vec![positionals[0].clone(), positionals[1].clone()]
    }
}

fn run_history_restore(args: &InputArgs) -> Result<()> {
    let snapshot_arg = args
        .input
        .as_deref()
        .ok_or("history restore requires <snapshot-id|latest>")?;
    let apply = args.target.options.apply;
    let force = args.target.options.force;
    let context = load_engine_context(&args.target)?;
    let snapshot = resolve_snapshot_arg(&context.target_dir, snapshot_arg)?;
    let dirty = compute_history_status(&context.target_dir, &context.schema)?;

    if apply && is_history_dirty(&dirty) && !force {
        return Err(
            "current docs differ from latest snapshot; run history snapshot first or use --force"
                .into(),
        );
    }

    let plan = restore_plan(&context.target_dir, &context.schema, &snapshot)?;
    print_restore_plan(&snapshot.id, &plan, apply);
    if apply {
        apply_restore_plan(&context.target_dir, &plan)?;
    }
    Ok(())
}

pub(crate) fn command_options(args: &CommonArgs, schema: Option<&Schema>) -> CommandOptions {
    let schema_strict = schema
        .and_then(|schema| schema.rules.strict_checks)
        .unwrap_or(false);
    CommandOptions {
        json: args.json,
        strict: (args.strict || schema_strict) && !args.no_strict,
        rules: option_value(&args.rules, "--rules").map(|value| {
            value
                .split(',')
                .map(|item| item.trim())
                .filter(|item| !item.is_empty())
                .map(|item| item.to_string())
                .collect::<BTreeSet<_>>()
        }),
    }
}

pub(crate) fn load_engine_context(args: &TargetArgs) -> Result<EngineContext> {
    let target_dir = PathBuf::from(args.target_dir.as_deref().unwrap_or("."));
    let docs_root_override = option_value(&args.options.docs_root, "--docs-root");
    let schema_arg = option_value(&args.options.schema, "--schema")
        .unwrap_or_else(|| default_context_schema_arg(&target_dir));

    let schema_load = load_schema(&target_dir, &schema_arg)?;
    let mut schema = parse_schema(&schema_load.text)?;
    let original_managed_root = schema.managed_root.clone();
    if let Some(ref docs_root) = docs_root_override {
        schema.managed_root = docs_root.clone();
        rewrite_docs_root_bound_paths(&mut schema, &original_managed_root, docs_root);
    }

    Ok(EngineContext { target_dir, schema })
}

pub(crate) fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Init(args)) => run_init(&args),
        Some(Command::Index(args)) => run_index(&args),
        Some(Command::Check(args)) => run_check(&args),
        Some(Command::Rank(args)) => run_rank(&args),
        Some(Command::ListDocs(args)) => run_list_docs(&args),
        Some(Command::Inspect(args)) => run_inspect(&args),
        Some(Command::Refs(args)) => run_refs(&args),
        Some(Command::Query(args)) => run_query(&args),
        Some(Command::Graph(args)) => run_graph(&args),
        Some(Command::Context(args)) => run_context(&args),
        Some(Command::History { command }) => run_history(command),
        Some(Command::Doctor(args)) => run_doctor(&args),
        Some(Command::Update(args)) => run_update(&args),
        Some(Command::Unknown(_)) | None => {
            Cli::command().write_help(&mut std::io::stderr())?;
            eprintln!();
            std::process::exit(2);
        }
    }
}
