mod args;

use args::{
    option_value, CheckArgs, CheckFormat, Cli, Command, ContextArgs, DiffArgs, DoctorArgs,
    HistoryCommand, InputArgs, QueryArgs, ShowArgs, SnapshotArgs,
};
pub(crate) use args::{CommonArgs, TargetArgs};
use clap::{CommandFactory, Parser};

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::{env, fs};

use crate::checks::{check_baseline, filter_and_suppress_checks, BaselineDelta, CheckView};
use crate::context::{
    build_current_context, build_handoff_context, context_packet_for_doc, doc_exists,
};
use crate::discover::{discover_docs, orphan_state_hints, project_hint};
use crate::doctor::build_doctor_report;
use crate::fs_utils::{ensure_dir, now_unix, validate_target_dir};
use crate::git::ensure_host_git_exclude;
use crate::history::{
    apply_restore_plan, compute_history_status, create_history_snapshot, diff_snapshot_to_worktree,
    diff_snapshots, export_history, history_file_content, import_history, is_history_dirty,
    list_history_snapshots, resolve_snapshot_arg, restore_plan,
};
use crate::index::{open_fact_store, refresh_fact_store, scalar_count, EngineContext};
use crate::output::{eprintln, finish_write, println};
use crate::query::{
    diagnose_query, diagnostic_hints, query_candidates, title_suggestions, DEFAULT_LIMIT,
};
use crate::render::{
    print_check_output, print_context_packet, print_current_context, print_doctor_report,
    print_handoff_context, print_history_diff, print_history_list, print_history_snapshot_result,
    print_history_status, print_index_result, print_init_preview, print_init_result,
    print_init_summary, print_inspect_output, print_list_docs_rows, print_query_candidates,
    print_rank_rows, print_refs_output, print_restore_plan, render_check_json,
    render_context_packet_json, render_doctor_json, render_query_json,
};
use crate::schema::{
    default_context_schema_arg, load_schema, parse_schema, render_schema_copy,
    rewrite_docs_root_bound_paths, Schema, SchemaParseError, BUNDLED_SCHEMA_PATH,
};
use crate::templates::{materialize_from_schema, validate_init_templates};
use crate::update::{detect_release_target, update_from_release};
use crate::usage;
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
    usage::target(&target_dir);
    validate_target_dir(&target_dir, true)?;
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
    print_project_hint(&target_dir, &schema);
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
    let target_dir = &context.target_dir;
    let db = open_fact_store(target_dir)?;
    let doc_count = scalar_count(&db, "SELECT COUNT(*) FROM docs;")?;
    let relation_count = scalar_count(&db, "SELECT COUNT(*) FROM relations;")?;

    let artifact = crate::index::read_index_artifact_from_fact_store(&db)?;
    usage::results(doc_count);
    usage::coverage(artifact.coverage.ratio);
    if args.options.json {
        println!("{}", crate::render::render_index_json(&artifact));
    } else {
        print_index_result(target_dir, doc_count, relation_count);
        print_coverage(&artifact.coverage, &artifact.hints);
    }
    Ok(())
}

fn run_check(args: &CheckArgs) -> Result<()> {
    let format = args.format.unwrap_or(if args.target.options.json {
        CheckFormat::Json
    } else {
        CheckFormat::Summary
    });
    let context = match load_engine_context(&args.target) {
        Ok(context) => context,
        Err(error) => {
            if let Some(error) = error.downcast_ref::<SchemaParseError>() {
                let checks = vec![CheckView {
                    severity: "error".to_string(),
                    rule_id: "schema-parse-error".to_string(),
                    subject_path: error.path.clone(),
                    message: error.message.clone(),
                    hint: Some(format!(
                        "Repair the schema syntax in {} and rerun check.",
                        error.path
                    )),
                    fixable: false,
                    evidence_json: serde_json::json!({"error": error.message}).to_string(),
                    suppressed: false,
                    suppression_reason: None,
                }];
                let target = Path::new(args.target.target_dir.as_deref().unwrap_or("."));
                let hints = vec![
                    "Schema parsing failed; coverage and baseline were not evaluated or updated."
                        .to_string(),
                ];
                if format == CheckFormat::Json {
                    println!(
                        "{}",
                        render_check_json(
                            target,
                            0,
                            0,
                            &checks,
                            &Default::default(),
                            &hints,
                            &BaselineDelta::default(),
                            args.all
                        )
                    );
                } else {
                    print_check_output(
                        target,
                        0,
                        0,
                        &checks,
                        &BaselineDelta::default(),
                        args.all || format == CheckFormat::Full,
                    );
                    println!("Hint: {}", hints[0]);
                }
                usage::results(1);
                usage::exit(1);
            }
            return Err(error);
        }
    };
    let options = command_options(&args.target.options, Some(&context.schema));
    let coverage = discover_docs(&context.target_dir, &context.schema)?;
    let orphan_hints = orphan_state_hints(&context.target_dir, &context.schema)?;
    refresh_fact_store(&context)?;
    let target_dir = &context.target_dir;
    let db = open_fact_store(target_dir)?;
    let mut hints = indexed_hints(&context, &db)?;
    hints.extend(orphan_hints.iter().cloned());
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
    let all_checks =
        filter_and_suppress_checks(&check_rows, &context.schema, &CommandOptions::default());
    let baseline = check_baseline(target_dir, &all_checks, &options, args.update_baseline)?;
    usage::results(checks.iter().filter(|check| !check.suppressed).count());
    usage::coverage(coverage.ratio);
    usage::check_new_findings(baseline.new_count);
    let warning_count = checks
        .iter()
        .filter(|check| !check.suppressed && check.severity == "warning")
        .count();
    let error_count = checks
        .iter()
        .filter(|check| !check.suppressed && check.severity == "error")
        .count();
    if format == CheckFormat::Json {
        println!(
            "{}",
            render_check_json(
                target_dir,
                docs_count,
                relation_count,
                &checks,
                &coverage,
                &hints,
                &baseline,
                args.all
            )
        );
        if error_count > 0 || (options.strict && warning_count > 0) {
            usage::exit(1);
        }
        return Ok(());
    }

    let full = args.all || format == CheckFormat::Full;
    print_check_output(
        target_dir,
        docs_count,
        relation_count,
        &checks,
        &baseline,
        full,
    );
    if full {
        print_coverage(&coverage, &hints);
    } else {
        println!("{}", coverage.summary());
        for hint in orphan_hints.iter().take(3) {
            println!("Hint: {}", hint);
        }
        if orphan_hints.len() > 3 {
            println!(
                "Hint: {} more orphan-state hints; use check --all to review.",
                orphan_hints.len() - 3
            );
        }
    }

    if error_count > 0 || (options.strict && warning_count > 0) {
        usage::exit(1);
    }

    Ok(())
}

fn run_doctor(doctor_args: &DoctorArgs) -> Result<()> {
    let args = &doctor_args.target;
    let context = load_engine_context(args)?;
    let json = args.options.json;
    let mut report = build_doctor_report(&context.target_dir);
    report.hints = orphan_state_hints(&context.target_dir, &context.schema)?;
    report.coverage = discover_docs(&context.target_dir, &context.schema)?;
    usage::coverage(report.coverage.ratio);
    report.hints.extend(project_hints(&context)?);
    if doctor_args.usage {
        let usage = usage::summarize(doctor_args.days);
        report.hints.extend(usage.hints.iter().cloned());
        report.usage = Some(usage);
    }
    if args.options.write_schema {
        let mut schema = context.schema.clone();
        for mapping in &report.coverage.mappings {
            if mapping.status == "managed" && !mapping.reason.starts_with("schema") {
                if let Some(collection) = &mapping.collection {
                    schema.include.push(crate::schema::IncludeSpec {
                        pattern: globset::escape(&mapping.path),
                        collection: collection.clone(),
                    });
                }
            }
        }
        let text = render_schema_copy(&schema);
        let path = context.target_dir.join(".harnesskit/schema.yaml");
        if args.options.preview || (path.exists() && !args.options.force) {
            report.hints.push("Schema preview only; use doctor --layout --write-schema --force to replace the existing schema.".to_string());
        } else {
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(&path, &text)?;
            report.schema_written = true;
            report.hints.push("Wrote .harnesskit/schema.yaml with the inferred mappings; run check to review the resulting coverage.".to_string());
        }
        report.schema_preview = Some(text);
    }

    if json {
        println!("{}", render_doctor_json(&report));
    } else {
        print_doctor_report(&report);
        println!("{}", report.coverage.summary());
        if let Some(usage) = &report.usage {
            usage::print_report(usage);
        }
        if args.options.layout || args.options.write_schema {
            for mapping in &report.coverage.mappings {
                println!(
                    "- {} [{}] {}: {}",
                    mapping.path,
                    mapping.status,
                    mapping.role.as_deref().unwrap_or("-"),
                    mapping.reason
                );
            }
        }
        if let Some(preview) = &report.schema_preview {
            println!("{}", preview);
        }
    }
    if !report.state_writable {
        return Err(report
            .state_error
            .unwrap_or_else(|| "HarnessKit state is not writable".to_string())
            .into());
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

    let artifact = crate::index::read_index_artifact_from_fact_store(&db)?;
    usage::results(result.rows.len());
    usage::coverage(artifact.coverage.ratio);
    if args.options.json {
        println!(
            "{}",
            crate::render::render_covered_json(
                &serde_json::json!({"docs": artifact.docs}),
                &artifact.coverage,
                &artifact.hints
            )
        );
    } else {
        print_list_docs_rows(&result);
        print_coverage(&artifact.coverage, &artifact.hints);
    }
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

fn run_query(query_args: &QueryArgs) -> Result<()> {
    let args = &query_args.input;
    let query_terms = args.input.clone().ok_or("query requires <terms>")?;
    usage::query_text(&query_terms);
    let context = load_engine_context(&args.target)?;
    let options = command_options(&args.target.options, Some(&context.schema));
    refresh_fact_store(&context)?;
    let db = open_fact_store(&context.target_dir)?;

    let candidates = query_candidates(
        &db,
        &context.target_dir,
        &query_terms,
        query_args.limit as usize,
    )?;
    let coverage = discover_docs(&context.target_dir, &context.schema)?;
    usage::coverage(coverage.ratio);
    usage::query_results(
        candidates.len(),
        candidates.iter().any(|candidate| {
            candidate
                .match_info
                .as_ref()
                .map(|info| info.mode == "relaxed")
                .unwrap_or(false)
        }),
    );
    let diagnostics = if candidates.is_empty() {
        Some(diagnose_query(
            &db,
            &context.target_dir,
            &query_terms,
            &coverage,
        )?)
    } else {
        None
    };
    let suggestions = if candidates.is_empty() {
        title_suggestions(&db, &query_terms)?
    } else {
        Vec::new()
    };
    let mut query_hints = diagnostics
        .as_ref()
        .map(diagnostic_hints)
        .unwrap_or_default();
    if candidates.iter().any(|candidate| {
        candidate
            .match_info
            .as_ref()
            .map(|info| info.mode == "relaxed")
            .unwrap_or(false)
    }) {
        query_hints.push("Fewer than three strict matches; partial matches are included with their missing terms.".to_string());
    }
    for suggestion in &suggestions {
        query_hints.push(format!(
            "Did you mean `{}`? See {}.",
            suggestion.title, suggestion.path
        ));
    }
    let mut hints = indexed_hints(&context, &db)?;
    hints.extend(query_hints.iter().cloned());

    if options.json {
        println!(
            "{}",
            render_query_json(
                &query_terms,
                &candidates,
                &coverage,
                &hints,
                diagnostics.as_ref(),
                &suggestions,
            )
        );
    } else {
        print_query_candidates(&query_terms, &candidates);
        for hint in &query_hints {
            println!("Hint: {}", hint);
        }
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
            render_context_packet_json(
                "graph",
                &doc_path,
                &packet,
                &discover_docs(&context.target_dir, &context.schema)?,
                &indexed_hints(&context, &db)?
            )
        );
    } else {
        print_context_packet("Graph", &doc_path, &packet);
    }
    Ok(())
}

fn run_context(args: &ContextArgs) -> Result<()> {
    if args.current || args.since.is_some() || args.input.input.is_none() {
        // In these modes the sole positional is TARGET_DIR; legacy INPUT TARGET_DIR
        // remains unchanged when retrieving a document or a search anchor.
        let mut target = args.input.target.clone();
        if let Some(path) = &args.input.input {
            if target.target_dir.is_some() {
                return Err("context --current/--since accepts only one target_dir".into());
            }
            target.target_dir = Some(path.clone());
        }
        let context = load_engine_context(&target)?;
        refresh_fact_store(&context)?;
        let db = open_fact_store(&context.target_dir)?;
        let coverage = discover_docs(&context.target_dir, &context.schema)?;
        usage::coverage(coverage.ratio);
        let mut hints = indexed_hints(&context, &db)?;
        if let Some(since) = &args.since {
            let packet = build_handoff_context(&db, &context.target_dir, &context.schema, since)?;
            if packet.cutoff_unix.is_some() {
                hints.push("Date cutoffs use UTC modification times; additions are included under modified because mtime cannot identify creation. Use a snapshot ID or latest for content-based additions, modifications and removals.".to_string());
            } else {
                hints.push("Review each changed document with context <path>; snapshot comparisons use content hashes, including edits whose mtime was preserved.".to_string());
            }
            if target.options.json {
                println!(
                    "{}",
                    crate::render::render_covered_json(&packet, &coverage, &hints)
                );
            } else {
                print_handoff_context(&packet, &coverage, hints.last().unwrap());
            }
        } else {
            let packet = build_current_context(&db, &context.target_dir, &context.schema)?;
            let next_step = if packet.latest_snapshot.is_some() {
                "Use context <path> for details, --json for all current pointers, or context --since latest for changed documents."
            } else {
                "Use context <path> for details or --json for all current pointers; create a handoff baseline with history snapshot -m <message>."
            };
            hints.push(next_step.to_string());
            if target.options.json {
                println!(
                    "{}",
                    crate::render::render_covered_json(&packet, &coverage, &hints)
                );
            } else {
                print_current_context(&packet, &coverage, hints.last().unwrap());
            }
        }
        return Ok(());
    }
    let args = &args.input;
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
        let candidates = query_candidates(&db, &context.target_dir, &input, DEFAULT_LIMIT)?;
        candidates
            .first()
            .map(|candidate| candidate.path.clone())
            .ok_or_else(|| format!("no context candidate found for query: {}", input))?
    };
    let packet = context_packet_for_doc(&db, &context.target_dir, &anchor_path)?;

    if options.json {
        println!(
            "{}",
            render_context_packet_json(
                "context",
                &input,
                &packet,
                &discover_docs(&context.target_dir, &context.schema)?,
                &indexed_hints(&context, &db)?
            )
        );
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
        Some(HistoryCommand::Export(args)) => run_history_transfer(&args, false),
        Some(HistoryCommand::Import(args)) => run_history_transfer(&args, true),
        Some(HistoryCommand::Show(args)) => run_history_show(&args),
        Some(HistoryCommand::Unknown(args)) => {
            Err(format!("unknown history subcommand: {}", args[0]).into())
        }
        None => Err("history requires a subcommand: snapshot, list, status, diff, restore, export, import, show".into()),
    }
}

fn run_history_snapshot(args: &SnapshotArgs) -> Result<()> {
    let message = option_value(&args.target.options.short_message, "-m")
        .or_else(|| option_value(&args.target.options.message, "--message"))
        .unwrap_or_else(|| "snapshot".to_string());
    let context = load_engine_context(&args.target)?;
    let outcome = create_history_snapshot(
        &context.target_dir,
        &context.schema,
        &message,
        args.allow_empty,
    )?;
    let hints = if outcome.skipped {
        vec!["Snapshot skipped: managed file contents are unchanged. Use --allow-empty to force a new snapshot.".to_string()]
    } else {
        Vec::new()
    };
    if args.target.options.json {
        let mut report = serde_json::to_value(&outcome.snapshot)?;
        // Report current coverage when skipped, even for a legacy snapshot.
        report.as_object_mut().unwrap().remove("coverage");
        report["skipped"] = outcome.skipped.into();
        print_history_json(&context, &report, &hints)?;
    } else {
        if outcome.skipped {
            println!("{}", hints[0]);
        } else {
            print_history_snapshot_result(&outcome.snapshot);
        }
        print_coverage(
            &discover_docs(&context.target_dir, &context.schema)?,
            &project_hints(&context)?,
        );
    }
    Ok(())
}

fn run_history_list(args: &TargetArgs) -> Result<()> {
    let context = load_engine_context(args)?;
    let snapshots = list_history_snapshots(&context.target_dir)?;
    usage::results(snapshots.len());
    if args.options.json {
        print_history_json(&context, &serde_json::json!({"snapshots": snapshots}), &[])?;
    } else {
        print_history_list(&snapshots);
    }
    Ok(())
}

fn run_history_status(args: &TargetArgs) -> Result<()> {
    let context = load_engine_context(args)?;
    let status = compute_history_status(&context.target_dir, &context.schema)?;
    if args.options.json {
        let mut report = serde_json::to_value(&status)?;
        report["dirty"] = is_history_dirty(&status).into();
        print_history_json(&context, &report, &status.hints())?;
    } else {
        print_history_status(&status);
        for hint in status.hints() {
            println!("Hint: {}", hint);
        }
    }
    Ok(())
}

fn run_history_diff(args: &DiffArgs) -> Result<()> {
    let positionals = &args.positionals;
    if positionals.is_empty() {
        return Err("history diff requires <snapshot-id|latest> or <a> <b>".into());
    }
    if positionals.len() > 3 {
        return Err("history diff accepts <snapshot> [target_dir] or <a> <b> [target_dir]".into());
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
    if args.options.json {
        print_history_json(&context, &diff, &[])?;
    } else {
        print_history_diff(&diff);
    }
    Ok(())
}

fn history_diff_snapshot_args(positionals: &[String]) -> Vec<String> {
    if positionals.len() <= 1 {
        return positionals.to_vec();
    }
    let second = Path::new(&positionals[1]);
    // With an explicit third argument the first two values are snapshot IDs.
    // Otherwise preserve existing IDs (including legacy IDs), and validate an
    // unknown second argument as a target instead of silently creating state.
    let snapshot_exists = Path::new(".harnesskit/history/snapshots")
        .join(format!("{}.json", positionals[1]))
        .is_file();
    if positionals.len() == 2
        && (second.is_dir() || (positionals[1] != "latest" && !snapshot_exists))
    {
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
    if args.target.options.json {
        print_history_json(
            &context,
            &serde_json::json!({
                "snapshot": snapshot.id, "apply": apply,
                "restore_files": plan.restore_files, "remove_paths": plan.remove_paths
            }),
            &[],
        )?;
    } else {
        print_restore_plan(&snapshot.id, &plan, apply);
    }
    if apply {
        apply_restore_plan(&context.target_dir, &plan)?;
    }
    Ok(())
}

fn run_history_transfer(args: &InputArgs, importing: bool) -> Result<()> {
    let path = args
        .input
        .as_deref()
        .ok_or("history export/import requires <file.tar.gz>")?;
    let context = load_engine_context(&args.target)?;
    let summary = if importing {
        import_history(&context.target_dir, Path::new(path))?
    } else {
        export_history(&context.target_dir, Path::new(path))?
    };
    if args.target.options.json {
        print_history_json(&context, &summary, &[])?;
    } else {
        println!("{} HarnessKit history: {} snapshots, {} objects, {} existing snapshot IDs skipped ({})",
            if importing { "Imported" } else { "Exported" }, summary.snapshots, summary.objects,
            summary.skipped_snapshots, path);
    }
    Ok(())
}

fn run_history_show(args: &ShowArgs) -> Result<()> {
    let context = load_engine_context(&args.target)?;
    let snapshot = resolve_snapshot_arg(&context.target_dir, &args.snapshot)?;
    let content = history_file_content(&context.target_dir, &snapshot, &args.path)?;
    if args.target.options.json {
        print_history_json(
            &context,
            &serde_json::json!({
                "snapshot": snapshot.id, "path": args.path,
                "content": String::from_utf8_lossy(&content)
            }),
            &[],
        )?;
    } else {
        let mut stdout = std::io::stdout().lock();
        finish_write(stdout.write_all(&content).and_then(|_| stdout.flush()));
    }
    Ok(())
}

fn print_history_json<T: serde::Serialize>(
    context: &EngineContext,
    report: &T,
    extra_hints: &[String],
) -> Result<()> {
    let coverage = discover_docs(&context.target_dir, &context.schema)?;
    usage::coverage(coverage.ratio);
    let mut hints = project_hints(context)?;
    hints.extend_from_slice(extra_hints);
    println!(
        "{}",
        crate::render::render_covered_json(report, &coverage, &hints)
    );
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
    usage::target(&target_dir);
    validate_target_dir(&target_dir, false)?;
    let docs_root_override = option_value(&args.options.docs_root, "--docs-root");
    let schema_arg = option_value(&args.options.schema, "--schema")
        .unwrap_or_else(|| default_context_schema_arg(&target_dir));

    let schema_load = load_schema(&target_dir, &schema_arg)?;
    let mut schema = parse_schema(&schema_load.text).map_err(|error| SchemaParseError {
        path: schema_load.source.clone(),
        message: error.to_string(),
    })?;
    schema.inferred_layout =
        args.options.schema.is_empty() && !target_dir.join(".harnesskit/schema.yaml").exists();
    let original_managed_root = schema.managed_root.clone();
    if let Some(ref docs_root) = docs_root_override {
        schema.managed_root = docs_root.clone();
        rewrite_docs_root_bound_paths(&mut schema, &original_managed_root, docs_root);
    }
    if args.options.schema.is_empty()
        && docs_root_override.is_none()
        && !target_dir.join(".harnesskit/schema.yaml").exists()
        && !target_dir.join(&schema.managed_root).exists()
    {
        let old_root = schema.managed_root.clone();
        schema.inferred_root = Some(old_root.clone());
        schema.managed_root = ".".to_string();
        rewrite_docs_root_bound_paths(&mut schema, &old_root, ".");
    }
    print_project_hint(&target_dir, &schema);

    Ok(EngineContext { target_dir, schema })
}

fn print_project_hint(target_dir: &Path, schema: &Schema) {
    if let Some(hint) = project_hint(target_dir, schema) {
        eprintln!("Hint: {}", hint);
    }
}

fn project_hints(context: &EngineContext) -> Result<Vec<String>> {
    let mut hints = discover_docs(&context.target_dir, &context.schema)?.hints();
    hints.extend(project_hint(&context.target_dir, &context.schema));
    Ok(hints)
}

fn indexed_hints(
    context: &EngineContext,
    db: &crate::sqlite::SqliteConnection,
) -> Result<Vec<String>> {
    let mut hints: Vec<String> = crate::index::meta_value(db, "coverage_hints")?
        .map(|text| serde_json::from_str(&text))
        .transpose()?
        .unwrap_or_default();
    hints.extend(project_hint(&context.target_dir, &context.schema));
    Ok(hints)
}

fn print_coverage(coverage: &crate::discover::Coverage, hints: &[String]) {
    usage::coverage(coverage.ratio);
    println!("{}", coverage.summary());
    for hint in hints {
        println!("Hint: {}", hint);
    }
}

pub(crate) fn run() -> Result<()> {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) => {
            finish_write(error.print());
            usage::exit(error.exit_code());
        }
    };
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
            usage::exit(2);
        }
    }
}
