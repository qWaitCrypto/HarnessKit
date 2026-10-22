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
use crate::index::{
    open_fact_store, refresh_fact_store, scalar_count, sql_string_literal, EngineContext,
};
use crate::query::{query_candidates, query_rows_to_candidates};
use crate::render::{
    print_check_output, print_context_packet, print_doctor_report, print_history_diff,
    print_history_list, print_history_snapshot_result, print_history_status, print_index_result,
    print_init_preview, print_init_result, print_init_summary, print_inspect_output,
    print_list_docs_rows, print_query_candidates, print_rank_rows, print_refs_output,
    print_restore_plan, render_check_json, render_context_packet_json, render_doctor_json,
    render_query_json, usage,
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

fn run_init(args: &[String]) -> Result<()> {
    let target_dir = PathBuf::from(positional_or_default(args, 0, "."));
    let docs_root_override = option_value(args, "--docs-root");
    let schema_arg =
        option_value(args, "--schema").unwrap_or_else(|| BUNDLED_SCHEMA_PATH.to_string());
    let force = has_flag(args, "--force");
    let preview = has_flag(args, "--preview");
    let local = has_flag(args, "--local");
    let tracked = has_flag(args, "--tracked");
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

fn run_index(args: &[String]) -> Result<()> {
    let context = load_engine_context(args, 0)?;
    refresh_fact_store(&context)?;
    let target_dir = context.target_dir;
    let db = open_fact_store(&target_dir)?;
    let doc_count = scalar_count(&db, "SELECT COUNT(*) FROM docs;")?;
    let relation_count = scalar_count(&db, "SELECT COUNT(*) FROM relations;")?;

    print_index_result(&target_dir, doc_count, relation_count);
    Ok(())
}

fn run_check(args: &[String]) -> Result<()> {
    let context = load_engine_context(args, 0)?;
    let options = command_options(args, Some(&context.schema));
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

fn run_doctor(args: &[String]) -> Result<()> {
    let target_dir = PathBuf::from(positional_or_default(args, 0, "."));
    let json = has_flag(args, "--json");
    let report = build_doctor_report(&target_dir);

    if json {
        println!("{}", render_doctor_json(&report));
    } else {
        print_doctor_report(&report);
    }
    Ok(())
}

fn run_update(args: &[String]) -> Result<()> {
    let version = option_value(args, "--version").unwrap_or_else(|| "latest".to_string());
    let repo = option_value(args, "--repo").unwrap_or_else(|| "qWaitCrypto/HarnessKit".to_string());
    let asset_base_url_arg = option_value(args, "--asset-base-url");
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

fn run_rank(args: &[String]) -> Result<()> {
    let context = load_engine_context(args, 0)?;
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
    )?;

    print_rank_rows(&result);
    Ok(())
}

fn run_list_docs(args: &[String]) -> Result<()> {
    let context = load_engine_context(args, 0)?;
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;
    let result = db.query(
        "SELECT path, title, role, COALESCE(status, ''), COALESCE(authority, ''), incoming_links, outgoing_links, COALESCE(collection_name, '')
         FROM docs
         ORDER BY path ASC;",
    )?;

    print_list_docs_rows(&result);
    Ok(())
}

fn run_inspect(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("inspect requires <doc_path>".into());
    }
    let doc_path = args[0].clone();
    let context = load_engine_context(&args[1..], 0)?;
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;

    let escaped = sql_string_literal(&doc_path);
    let doc = db.query(&format!(
        "SELECT path, title, role, COALESCE(status, ''), COALESCE(authority, ''), COALESCE(collection_name, ''), summary, incoming_links, outgoing_links, is_entrypoint, is_anchor_candidate, is_generated, is_reference, file_size_bytes, mtime_unix
         FROM docs WHERE path = {};",
        escaped
    ))?;
    if doc.rows.is_empty() {
        return Err(format!("doc not found in fact store: {}", doc_path).into());
    }

    let outgoing = db.query(&format!(
        "SELECT relation_type, dst_path, reason, confidence
         FROM relations
         WHERE src_path = {}
         ORDER BY relation_type ASC, dst_path ASC;",
        escaped
    ))?;
    let incoming = db.query(&format!(
        "SELECT relation_type, src_path, reason, confidence
         FROM relations
         WHERE dst_path = {}
         ORDER BY relation_type ASC, src_path ASC;",
        escaped
    ))?;
    let checks = db.query(&format!(
        "SELECT severity, rule_id, message, evidence_json
         FROM checks
         WHERE subject_path = {}
         ORDER BY severity DESC, rule_id ASC;",
        escaped
    ))?;

    print_inspect_output(&doc, &outgoing, &incoming, &checks);
    Ok(())
}

fn run_refs(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("refs requires <doc_path>".into());
    }
    let doc_path = args[0].clone();
    let context = load_engine_context(&args[1..], 0)?;
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;
    let result = db.query(&format!(
        "SELECT src_path, relation_type, reason, confidence
         FROM relations
         WHERE dst_path = {}
         ORDER BY relation_type ASC, src_path ASC;",
        sql_string_literal(&doc_path)
    ))?;

    print_refs_output(&doc_path, &result);
    Ok(())
}

fn run_query(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("query requires <terms>".into());
    }

    let query_terms = args[0].clone();
    let context = load_engine_context(&args[1..], 0)?;
    let options = command_options(args, Some(&context.schema));
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

fn run_graph(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("graph requires <doc_path>".into());
    }
    let doc_path = args[0].clone();
    let context = load_engine_context(&args[1..], 0)?;
    let options = command_options(args, Some(&context.schema));
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

fn run_context(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("context requires <terms-or-doc-path>".into());
    }
    let input = args[0].clone();
    let context = load_engine_context(&args[1..], 0)?;
    let options = command_options(args, Some(&context.schema));
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

fn run_history(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("history requires a subcommand: snapshot, list, status, diff, restore".into());
    }

    match args[0].as_str() {
        "snapshot" => run_history_snapshot(&args[1..]),
        "list" => run_history_list(&args[1..]),
        "status" => run_history_status(&args[1..]),
        "diff" => run_history_diff(&args[1..]),
        "restore" => run_history_restore(&args[1..]),
        other => Err(format!("unknown history subcommand: {}", other).into()),
    }
}

fn run_history_snapshot(args: &[String]) -> Result<()> {
    let message = option_value(args, "-m")
        .or_else(|| option_value(args, "--message"))
        .unwrap_or_else(|| "snapshot".to_string());
    let context = load_engine_context(args, 0)?;
    let snapshot = create_history_snapshot(&context.target_dir, &context.schema, &message)?;
    print_history_snapshot_result(&snapshot);
    Ok(())
}

fn run_history_list(args: &[String]) -> Result<()> {
    let context = load_engine_context(args, 0)?;
    let snapshots = list_history_snapshots(&context.target_dir)?;
    print_history_list(&snapshots);
    Ok(())
}

fn run_history_status(args: &[String]) -> Result<()> {
    let context = load_engine_context(args, 0)?;
    let status = compute_history_status(&context.target_dir, &context.schema)?;
    print_history_status(&status);
    Ok(())
}

fn run_history_diff(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("history diff requires <snapshot-id|latest> or <a> <b>".into());
    }
    let positionals = positional_args(args);
    if positionals.is_empty() {
        return Err("history diff requires <snapshot-id|latest> or <a> <b>".into());
    }
    let snapshot_args = history_diff_snapshot_args(&positionals);
    let context = load_engine_context(args, snapshot_args.len())?;
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

fn run_history_restore(args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Err("history restore requires <snapshot-id|latest>".into());
    }
    let apply = has_flag(args, "--apply");
    let force = has_flag(args, "--force");
    let positionals = positional_args(args);
    if positionals.is_empty() {
        return Err("history restore requires <snapshot-id|latest>".into());
    }
    let context = load_engine_context(args, 1)?;
    let snapshot = resolve_snapshot_arg(&context.target_dir, &positionals[0])?;
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

fn positional_or_default(args: &[String], index: usize, default: &str) -> String {
    positional_args(args)
        .get(index)
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn positional_args(args: &[String]) -> Vec<String> {
    let mut values = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        match arg.as_str() {
            "--schema" | "--docs-root" | "--rules" | "--message" | "-m" | "--version"
            | "--repo" | "--asset-base-url" => {
                skip_next = true;
            }
            "--force"
            | "--json"
            | "--strict"
            | "--no-strict"
            | "--apply"
            | "--cli-only"
            | "--no-claude"
            | "--no-codex"
            | "--with-codex-plugin"
            | "--local"
            | "--tracked"
            | "--preview" => {}
            _ if arg.starts_with("--") => {}
            _ if arg.starts_with('-') => {}
            _ => values.push(arg.clone()),
        }
    }
    values
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

pub(crate) fn command_options(args: &[String], schema: Option<&Schema>) -> CommandOptions {
    let schema_strict = schema
        .and_then(|schema| schema.rules.strict_checks)
        .unwrap_or(false);
    CommandOptions {
        json: has_flag(args, "--json"),
        strict: (has_flag(args, "--strict") || schema_strict) && !has_flag(args, "--no-strict"),
        rules: option_value(args, "--rules").map(|value| {
            value
                .split(',')
                .map(|item| item.trim())
                .filter(|item| !item.is_empty())
                .map(|item| item.to_string())
                .collect::<BTreeSet<_>>()
        }),
    }
}

fn option_value(args: &[String], flag: &str) -> Option<String> {
    let values = args
        .windows(2)
        .filter(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .collect::<Vec<_>>();
    if values.len() > 1 {
        eprintln!("warning: duplicate {}; using last value", flag);
    }
    values.last().cloned()
}

pub(crate) fn load_engine_context(
    args: &[String],
    positional_index: usize,
) -> Result<EngineContext> {
    let target_dir = PathBuf::from(positional_or_default(args, positional_index, "."));
    let docs_root_override = option_value(args, "--docs-root");
    let schema_arg =
        option_value(args, "--schema").unwrap_or_else(|| default_context_schema_arg(&target_dir));

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
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        usage();
        std::process::exit(2);
    }

    match args[1].as_str() {
        "init" => run_init(&args[2..])?,
        "index" => run_index(&args[2..])?,
        "check" => run_check(&args[2..])?,
        "rank" => run_rank(&args[2..])?,
        "list-docs" => run_list_docs(&args[2..])?,
        "inspect" => run_inspect(&args[2..])?,
        "refs" => run_refs(&args[2..])?,
        "query" => run_query(&args[2..])?,
        "graph" => run_graph(&args[2..])?,
        "context" => run_context(&args[2..])?,
        "history" => run_history(&args[2..])?,
        "doctor" => run_doctor(&args[2..])?,
        "update" => run_update(&args[2..])?,
        "help" | "--help" | "-h" => usage(),
        "--version" | "-V" => println!("harnesskit {}", env!("CARGO_PKG_VERSION")),
        _ => {
            usage();
            std::process::exit(2);
        }
    }

    Ok(())
}
