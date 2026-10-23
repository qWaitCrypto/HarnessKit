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
    build_code_context, build_current_context, build_diff_context, build_handoff_context,
    context_packet_for_doc, doc_exists,
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
    diagnose_query, diagnostic_hints, query_bundles, query_candidates, title_suggestions,
    QuerySide, DEFAULT_BUNDLE_LIMIT, DEFAULT_LIMIT,
};
use crate::render::{
    print_check_output, print_context_packet, print_current_context, print_doctor_report,
    print_handoff_context, print_history_diff, print_history_list, print_history_snapshot_result,
    print_history_status, print_index_result, print_init_preview, print_init_result,
    print_init_summary, print_inspect_output, print_list_docs_rows, print_query_bundles,
    print_query_candidates, print_rank_rows, print_refs_output, print_restore_plan,
    render_check_json, render_context_packet_json, render_doctor_json, render_query_json,
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

    let mut artifact = crate::index::read_index_artifact_from_fact_store(&db)?;
    artifact.hints = output_hints(&context, &args.options, &artifact.coverage, artifact.hints);
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
    refresh_fact_store(&context)?;
    // W3 maintenance actions operate on the durable lock after the normal
    // refresh.  A second refresh makes the final report reflect any fix or
    // acknowledgement in the same invocation.
    let mut maintenance_changed = false;
    if args.fix {
        let db = open_fact_store(&context.target_dir)?;
        maintenance_changed = crate::knowledge::fix_moved(&context.target_dir, &db)?;
    }
    if maintenance_changed {
        refresh_fact_store(&context)?;
    }
    if !args.ack.is_empty() {
        let db = open_fact_store(&context.target_dir)?;
        maintenance_changed |= crate::knowledge::acknowledge(&context.target_dir, &db, &args.ack)?;
    }
    if maintenance_changed {
        refresh_fact_store(&context)?;
    }
    let options = command_options(&args.target.options, Some(&context.schema));
    let coverage = discover_docs(&context.target_dir, &context.schema)?;
    let orphan_hints = orphan_state_hints(&context.target_dir, &context.schema)?;
    refresh_fact_store(&context)?;
    let target_dir = &context.target_dir;
    let db = open_fact_store(target_dir)?;
    let mut hint_options = args.target.options.clone();
    hint_options.json = format == CheckFormat::Json;
    let mut hints = indexed_hints(&context, &db, &hint_options)?;
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
        let explanations = coverage.explanation_hints();
        for hint in hints.iter().filter(|hint| explanations.contains(hint)) {
            println!("Hint: {}", hint);
        }
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
    if doctor_args.code {
        refresh_fact_store(&context)?;
    }
    let json = args.options.json;
    let mut report = build_doctor_report(&context.target_dir);
    report
        .hints
        .extend(orphan_state_hints(&context.target_dir, &context.schema)?);
    report.coverage = discover_docs(&context.target_dir, &context.schema)?;
    usage::coverage(report.coverage.ratio);
    report.hints.extend(project_hints(&context, &args.options)?);
    if doctor_args.code {
        let code = crate::code::code_report(&open_fact_store(&context.target_dir)?)?;
        if code.scan_mode == "git-unavailable" {
            report.hints.push("Code index was not refreshed because Git could not inspect this repository; counts may be stale. Check that git is available and can access this worktree, then rerun `doctor --code`.".to_string());
        }
        report.code = Some(code);
    }
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

    let mut artifact = crate::index::read_index_artifact_from_fact_store(&db)?;
    artifact.hints = output_hints(&context, &args.options, &artifact.coverage, artifact.hints);
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

    let has_code = scalar_count(
        &db,
        "SELECT COUNT(*) FROM nodes WHERE kind IN ('file', 'symbol');",
    )? > 0;
    let side = if query_args.docs {
        QuerySide::Docs
    } else if query_args.code {
        QuerySide::Code
    } else {
        QuerySide::Mixed
    };
    let legacy = !has_code && side != QuerySide::Code;
    let limit = query_args
        .limit
        .map(|limit| limit as usize)
        .unwrap_or(if legacy {
            DEFAULT_LIMIT
        } else {
            DEFAULT_BUNDLE_LIMIT
        });
    // Preserve the established document schema/ranking for existing clients;
    // bundles contain the mixed ranking and code-only results.
    let candidates = if side == QuerySide::Code {
        Vec::new()
    } else {
        query_candidates(&db, &context.target_dir, &query_terms, limit)?
    };
    let bundle_limit = query_args
        .limit
        .map(|limit| limit as usize)
        .unwrap_or(DEFAULT_BUNDLE_LIMIT);
    let bundles = query_bundles(
        &db,
        &query_terms,
        side,
        bundle_limit,
        legacy.then_some(candidates.as_slice()),
    )?;
    let relaxed = if legacy {
        candidates.iter().any(|candidate| {
            candidate
                .match_info
                .as_ref()
                .map(|info| info.mode == "relaxed")
                .unwrap_or(false)
        })
    } else {
        bundles.iter().any(|bundle| {
            bundle
                .primary
                .match_info
                .as_ref()
                .map(|info| info.mode == "relaxed")
                .unwrap_or(false)
        })
    };
    let empty = if legacy {
        candidates.is_empty()
    } else {
        bundles.is_empty()
    };
    let coverage = discover_docs(&context.target_dir, &context.schema)?;
    usage::coverage(coverage.ratio);
    usage::query_results(
        if legacy {
            candidates.len()
        } else {
            bundles.len()
        },
        relaxed,
    );
    let diagnostics = if empty {
        Some(diagnose_query(
            &db,
            &context.target_dir,
            &query_terms,
            &coverage,
            if legacy { QuerySide::Docs } else { side },
        )?)
    } else {
        None
    };
    let suggestions = if empty && side != QuerySide::Code {
        title_suggestions(&db, &query_terms)?
    } else {
        Vec::new()
    };
    let mut query_hints = diagnostics
        .as_ref()
        .map(diagnostic_hints)
        .unwrap_or_default();
    if relaxed {
        query_hints.push("Fewer than three strict matches; partial matches are included with their missing terms.".to_string());
    }
    for suggestion in &suggestions {
        query_hints.push(format!(
            "Did you mean `{}`? See {}.",
            suggestion.title, suggestion.path
        ));
    }
    if options.json {
        let mut hints = indexed_hints(&context, &db, &args.target.options)?;
        hints.extend(query_hints.iter().cloned());
        println!(
            "{}",
            render_query_json(
                &query_terms,
                &candidates,
                &bundles,
                &coverage,
                &hints,
                diagnostics.as_ref(),
                &suggestions,
            )
        );
    } else if legacy {
        print_query_candidates(&query_terms, &candidates);
        if args.target.options.verbose {
            print_coverage(
                &coverage,
                &indexed_hints(&context, &db, &args.target.options)?,
            );
        }
        for hint in &query_hints {
            println!("Hint: {}", hint);
        }
    } else {
        let mut extra_lines = Vec::new();
        if args.target.options.verbose {
            extra_lines.push(format!(
                "Coverage: {}/{} managed Markdown documents ({:.1}%).",
                coverage.managed,
                coverage.markdown_total,
                coverage.ratio * 100.0
            ));
            extra_lines.extend(
                indexed_hints(&context, &db, &args.target.options)?
                    .iter()
                    .map(|hint| format!("Hint: {hint}")),
            );
        }
        extra_lines.extend(query_hints.iter().map(|hint| format!("Hint: {hint}")));
        print_query_bundles(&query_terms, &bundles, &extra_lines);
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
                &indexed_hints(&context, &db, &args.target.options)?
            )
        );
    } else {
        print_context_packet("Graph", &doc_path, &packet);
        if args.target.options.verbose {
            print_coverage(
                &discover_docs(&context.target_dir, &context.schema)?,
                &indexed_hints(&context, &db, &args.target.options)?,
            );
        }
    }
    Ok(())
}

fn run_context(args: &ContextArgs) -> Result<()> {
    if args.code_for.is_some() || args.diff.is_some() {
        let mut target = args.input.target.clone();
        if let Some(path) = &args.input.input {
            if target.target_dir.is_some() {
                return Err("context --for/--diff accepts only one target_dir".into());
            }
            target.target_dir = Some(path.clone());
        }
        let context = load_engine_context(&target)?;
        if args.diff.is_some() {
            crate::context::validate_diff_repository(&context.target_dir)?;
        }
        let locks_before_index = crate::knowledge::read_anchor_lock(&context.target_dir)?;
        refresh_fact_store(&context)?;
        let db = open_fact_store(&context.target_dir)?;
        let coverage = discover_docs(&context.target_dir, &context.schema)?;
        usage::coverage(coverage.ratio);
        let mut hints = indexed_hints(&context, &db, &target.options)?;
        hints.truncate(2);
        hints.push("覆盖指 current 文档的锚点或无歧义提及；同提交关系仅是推断线索。".to_string());
        if let Some(input) = &args.code_for {
            let packet = build_code_context(
                &db,
                &context.target_dir,
                &context.schema,
                input,
                &crate::knowledge::read_anchor_lock(&context.target_dir)?,
            )?;
            if packet.message.is_some() {
                let example = packet
                    .targets
                    .first()
                    .map(|target| {
                        format!(
                            "{}#{}",
                            target.path,
                            if target.name.is_empty() {
                                "symbol"
                            } else {
                                &target.name
                            }
                        )
                    })
                    .unwrap_or_else(|| "src/file.rs#symbol".to_string());
                hints.push(format!("suggest: 在决策或经验章节中用反引号写代码锚点，例如 `{example}`（目录可写 `src/`）。"));
            }
            if packet.ambiguous {
                hints.push(
                    "符号匹配到多处，已汇总所有候选；使用 --for <path#symbol> 缩小范围。"
                        .to_string(),
                );
            }
            if target.options.json {
                println!(
                    "{}",
                    crate::render::render_covered_json(&packet, &coverage, &hints)
                );
            } else {
                crate::render::print_code_context(&packet, &coverage, &hints);
            }
        } else {
            let revision = args.diff.as_deref().filter(|revision| !revision.is_empty());
            let packet = build_diff_context(
                &db,
                &context.target_dir,
                &context.schema,
                revision,
                &locks_before_index,
            )?;
            hints.push(
                "漂移预测比较命令开始时的 knowledge.lock；文档与代码共改仍按 W3 自动复核。"
                    .to_string(),
            );
            if target.options.json {
                println!(
                    "{}",
                    crate::render::render_covered_json(&packet, &coverage, &hints)
                );
            } else {
                crate::render::print_diff_context(&packet, &coverage, &hints);
            }
        }
        return Ok(());
    }
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
        let mut hints = indexed_hints(&context, &db, &target.options)?;
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
                for hint in &hints[..hints.len() - 1] {
                    println!("Hint: {}", hint);
                }
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
                for hint in &hints[..hints.len() - 1] {
                    println!("Hint: {}", hint);
                }
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
                &indexed_hints(&context, &db, &args.target.options)?
            )
        );
    } else {
        print_context_packet("Context", &input, &packet);
        if args.target.options.verbose {
            print_coverage(
                &discover_docs(&context.target_dir, &context.schema)?,
                &indexed_hints(&context, &db, &args.target.options)?,
            );
        }
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
        print_history_json(&context, &args.target.options, &report, &hints)?;
    } else {
        if outcome.skipped {
            println!("{}", hints[0]);
        } else {
            print_history_snapshot_result(&outcome.snapshot);
        }
        print_coverage(
            &discover_docs(&context.target_dir, &context.schema)?,
            &project_hints(&context, &args.target.options)?,
        );
    }
    Ok(())
}

fn run_history_list(args: &TargetArgs) -> Result<()> {
    let context = load_engine_context(args)?;
    let snapshots = list_history_snapshots(&context.target_dir)?;
    usage::results(snapshots.len());
    if args.options.json {
        print_history_json(
            &context,
            &args.options,
            &serde_json::json!({"snapshots": snapshots}),
            &[],
        )?;
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
        print_history_json(&context, &args.options, &report, &status.hints())?;
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
        print_history_json(&context, &args.options, &diff, &[])?;
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
            &args.target.options,
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
        print_history_json(&context, &args.target.options, &summary, &[])?;
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
            &args.target.options,
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
    options: &CommonArgs,
    report: &T,
    extra_hints: &[String],
) -> Result<()> {
    let coverage = discover_docs(&context.target_dir, &context.schema)?;
    usage::coverage(coverage.ratio);
    let mut hints = project_hints(context, options)?;
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

fn project_hints(context: &EngineContext, options: &CommonArgs) -> Result<Vec<String>> {
    let coverage = discover_docs(&context.target_dir, &context.schema)?;
    let mut hints = coverage.hints();
    hints.extend(project_hint(&context.target_dir, &context.schema));
    Ok(output_hints(context, options, &coverage, hints))
}

fn indexed_hints(
    context: &EngineContext,
    db: &crate::sqlite::SqliteConnection,
    options: &CommonArgs,
) -> Result<Vec<String>> {
    let mut hints: Vec<String> = crate::index::meta_value(db, "coverage_hints")?
        .map(|text| serde_json::from_str(&text))
        .transpose()?
        .unwrap_or_default();
    hints.extend(project_hint(&context.target_dir, &context.schema));
    let coverage = discover_docs(&context.target_dir, &context.schema)?;
    Ok(output_hints(context, options, &coverage, hints))
}

fn output_hints(
    context: &EngineContext,
    options: &CommonArgs,
    coverage: &crate::discover::Coverage,
    mut hints: Vec<String>,
) -> Vec<String> {
    let record = context
        .target_dir
        .join(".harnesskit/state/layout-hints-shown");
    let layout = format!(
        "{}\n{:?}\n",
        context.schema.managed_root, context.schema.inferred_root
    );
    // Explicit explanation requests remain read-only (notably doctor --layout).
    // Only an automatic first-run explanation records that it was shown.
    let first_inference = !options.verbose
        && !options.layout
        && context.schema.inferred_layout
        && fs::read_to_string(&record).ok().as_deref() != Some(layout.as_str());
    if !options.verbose && !options.layout && !first_inference {
        let explanations = coverage.explanation_hints();
        hints.retain(|hint| !explanations.contains(hint));
    }
    if first_inference {
        // Muting a repeated explanation must not prevent doctor from reporting
        // an unwritable state directory, or fail an otherwise valid command.
        let _ =
            fs::create_dir_all(record.parent().unwrap()).and_then(|_| fs::write(record, layout));
    }
    hints.retain(|hint| !hint.starts_with("0 external file reference(s)"));
    if hints.is_empty() && options.json {
        hints.push(
            "Use doctor --layout to review coverage mappings, or --verbose for scope details."
                .to_string(),
        );
    }
    hints
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
