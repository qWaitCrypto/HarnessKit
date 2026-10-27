pub(crate) mod cache;
mod code_context;
mod contract;
mod json;

pub(crate) use code_context::{print_code_context_with_options, print_diff_context_with_options};
pub(crate) use contract::{
    compact, constraint_score, cursor_for_offset, cursor_offset, edge_badge, freshness_badge,
    highlight, kwic_excerpt, local_anchor_names, merge_names, paginate_lines, preferred_excerpt,
    short_hint, token_cost, RenderLine, RenderOptions, RenderedPage,
};

#[allow(unused_imports)]
pub(crate) use json::{
    render_check_json, render_check_json_page, render_context_packet_json,
    render_context_packet_json_page, render_covered_json, render_covered_json_page,
    render_doctor_json, render_history_snapshot_json, render_index_json, render_query_json,
    render_query_json_page, serialize_evidence, serialize_inline,
};

use std::collections::BTreeMap;
use std::path::Path;

use crate::checks::{BaselineDelta, CheckSummary, CheckView};
use crate::cli::InitMode;
use crate::context::{ContextDoc, ContextPacket, CurrentContext, HandoffContext};
use crate::discover::Coverage;
use crate::doctor::{CommandCheck, DoctorReport};
use crate::git::{find_host_git_dir, git_dirty_label};
use crate::history::{is_history_dirty, HistoryDiff, HistorySnapshot, HistoryStatus, RestorePlan};
use crate::index::{cell, DocRelation, IndexArtifact};
use crate::output::println;
use crate::query::{kwic_for_doc, BundleNode, QueryBundle, QueryCandidate};
use crate::schema::Schema;
use crate::sqlite::QueryResult;
use crate::templates::{init_plan_paths, InitStats};

fn init_mode_label(mode: InitMode) -> &'static str {
    match mode {
        InitMode::Local => "local",
        InitMode::Tracked => "tracked",
    }
}

pub(crate) fn print_init_preview(
    target_dir: &Path,
    schema: &Schema,
    schema_source: &str,
    mode: InitMode,
    force: bool,
) {
    println!("HarnessKit init preview");
    println!("Target: {}", target_dir.display());
    println!("Schema: {}", schema_source);
    println!("Mode: {}", init_mode_label(mode));
    println!("Docs root: {}", schema.managed_root);
    println!("Force overwrite: {}", yes_no_bool(force));
    let host_git_dir = find_host_git_dir(target_dir);
    println!(
        "Git repository: {}",
        host_git_dir
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "not found".to_string())
    );
    println!();
    println!("Would create or update:");
    for path in init_plan_paths(schema) {
        let abs = target_dir.join(&path);
        let status = if abs.exists() {
            "skip existing"
        } else {
            "create"
        };
        println!("- {} ({})", path, status);
    }
    println!();
    match mode {
        InitMode::Local => {
            if host_git_dir.is_some() {
                println!("Git exclude: would add HarnessKit-managed paths to .git/info/exclude.");
                println!("Local mode keeps generated docs out of git status. Use --tracked for team-visible repo memory.");
            } else {
                println!("Git exclude: no git repository found; would skip .git/info/exclude.");
                println!("Local mode can still generate the docs scaffold without git local exclude entries.");
            }
        }
        InitMode::Tracked => {
            println!("Git exclude: would not write HarnessKit-managed paths to .git/info/exclude.");
            println!("Tracked mode leaves generated docs visible to git status so they can be committed.");
        }
    }
    println!();
    println!("Preview only: no files were written.");
}

pub(crate) fn print_init_summary(
    target_dir: &Path,
    schema: &Schema,
    mode: InitMode,
    exclude_stats: Option<(usize, usize)>,
) {
    println!("Mode: {}", init_mode_label(mode));
    println!("Docs root: {}", schema.managed_root);
    println!("Entrypoints:");
    if let Some(path) = &schema.entrypoints.agents {
        println!("- {}", path);
    }
    if let Some(path) = &schema.entrypoints.claude {
        println!("- {}", path);
    }
    if let Some(path) = &schema.entrypoints.architecture {
        println!("- {}", path);
    }
    println!(
        ".harnesskit/state: derived fact/index layer; safe to rebuild with `harnesskit index`."
    );
    println!(".harnesskit/history: local doc checkpoints; do not delete as cache.");

    match mode {
        InitMode::Local => match exclude_stats {
            Some((added, existing)) => {
                println!(
                    "Git visibility: local mode updated .git/info/exclude (added {}, already present {}).",
                    added, existing
                );
                println!("Generated docs are local project memory and will not appear in git status. Use `harnesskit init --tracked` for team-visible docs.");
            }
            None => {
                println!("Git visibility: No git repository found; generated local docs without git exclude entries.");
                println!("Generated docs are local files in this directory. Run `git init` first or rerun in a git repository if you want HarnessKit to add local exclude rules.");
            }
        },
        InitMode::Tracked => {
            println!("Git visibility: tracked mode selected; .git/info/exclude was not changed.");
            println!("Generated docs will appear in git status and can be committed as team-visible context harness files.");
        }
    }

    println!("Next steps:");
    println!("- Agent-first: ask your agent to read `AGENTS.md` or `CLAUDE.md` and follow the reading path.");
    println!("- Manual: `harnesskit index {}`", target_dir.display());
    println!("- Manual: `harnesskit doctor {}`", target_dir.display());
    println!(
        "- Manual: `harnesskit check {} --json`",
        target_dir.display()
    );
}

pub(crate) fn print_doctor_report(report: &DoctorReport) {
    println!("HarnessKit doctor");
    println!("CLI version: {}", report.cli_version);
    println!("Platform: {} {}", report.os, report.arch);
    println!("Target dir: {}", report.target_dir);
    println!("Current dir: {}", report.current_dir);
    println!("PWD: {}", report.pwd.as_deref().unwrap_or("(not set)"));
    println!(
        "PWD differs from current_dir: {}",
        yes_no_bool(report.pwd_differs_from_current_dir)
    );
    println!("git: {}", command_check_label(&report.git));
    println!(
        "Inside git repo: {}",
        report.inside_git_repo.map(yes_no_bool).unwrap_or("unknown")
    );
    if let Some(error) = &report.git_repo_error {
        println!("Git repo check: {}", error);
    }
    println!(
        ".harnesskit/state exists: {}",
        yes_no_bool(report.state_exists)
    );
    println!(
        ".harnesskit/state writable or creatable: {}",
        yes_no_bool(report.state_writable)
    );
    if let Some(error) = &report.state_error {
        println!("State write check: {}", error);
        println!("Hint: if diagnostics do not point at the intended writable project root, pass the project root explicitly, for example `harnesskit doctor /path/to/repo --json` or `harnesskit index /path/to/repo`.");
    }
    println!(
        ".harnesskit/history exists: {}",
        yes_no_bool(report.history_exists)
    );
    println!(
        ".harnesskit/schema.yaml exists: {}",
        yes_no_bool(report.schema_exists)
    );
    println!(
        "Claude skill: {} ({})",
        installed_label(report.claude_skill_installed),
        report.claude_skill_path
    );
    println!(
        "Codex skill: {} ({})",
        installed_label(report.codex_skill_installed),
        report.codex_skill_path
    );
    for skill in &report.skills {
        println!(
            "{} skill version: {} (bundled {}; {})",
            skill.agent,
            skill.version.as_deref().unwrap_or("unknown"),
            report.bundled_skill_version,
            if skill.matches_bundled {
                "current"
            } else {
                "sync needed"
            }
        );
    }
    println!(
        "Current executable: {}",
        report.current_exe.as_deref().unwrap_or("(unknown)")
    );
    println!(
        "Current executable directory on PATH: {}",
        yes_no_bool(report.path_contains_cli_dir)
    );
    if let Some(code) = &report.code {
        println!(
            "Code: {} files, {} symbols ({})",
            code.files, code.symbols, code.scan_mode
        );
        for language in &code.languages {
            println!(
                "- {}: {} files, {} symbols",
                language.lang, language.files, language.symbols
            );
        }
    }
    for hint in &report.hints {
        println!("Hint: {}", hint);
    }
    println!("Note: `.harnesskit/state` is derived and rebuildable; `.harnesskit/history` stores local doc checkpoints.");
}

fn command_check_label(check: &CommandCheck) -> String {
    if check.present {
        match (&check.version, &check.error) {
            (Some(version), _) if !version.is_empty() => format!("ok ({})", version),
            (_, Some(error)) => format!("present but failed ({})", error),
            _ => "ok".to_string(),
        }
    } else {
        format!(
            "missing ({})",
            check.error.as_deref().unwrap_or("not found")
        )
    }
}

fn installed_label(installed: bool) -> &'static str {
    if installed {
        "installed"
    } else {
        "missing"
    }
}

pub(crate) fn print_history_status(status: &HistoryStatus) {
    println!(
        "History status: latest={} files={} dirty={}",
        status.latest_snapshot.as_deref().unwrap_or("-"),
        status.tracked_files_count,
        yes_no_bool(is_history_dirty(status))
    );
    print_named_path_group("Added", &status.added);
    print_named_path_group("Changed", &status.changed);
    print_named_path_group("Removed", &status.removed);
    println!("Schema changed: {}", yes_no_bool(status.schema_changed));
    println!(
        "Days since latest snapshot: {}",
        status
            .days_since_snapshot
            .map(|days| days.to_string())
            .unwrap_or_else(|| "-".to_string())
    );
    println!(
        "Changed files: {}; unmanaged files: {}",
        status.changed_files_count, status.unmanaged_files_count
    );
    println!(
        "Docs tracked by git: {} ({} tracked, {} untracked); host git dirty: {}",
        yes_no_bool(status.docs_git_tracked),
        status.git_tracked_docs_count,
        status.git_untracked_docs_count,
        git_dirty_label(status.host_git_dirty)
    );
}

fn yes_no_bool(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

pub(crate) fn print_history_diff(diff: &HistoryDiff) {
    print_named_path_group("Added", &diff.added);
    print_named_path_group("Changed", &diff.changed);
    print_named_path_group("Removed", &diff.removed);
}

fn print_named_path_group(label: &str, paths: &[String]) {
    println!("{}:", label);
    if paths.is_empty() {
        println!("- none");
    } else {
        for path in paths {
            println!("- {}", path);
        }
    }
}

pub(crate) fn print_restore_plan(snapshot_id: &str, plan: &RestorePlan, apply: bool) {
    println!(
        "Restore {} snapshot {}:",
        if apply { "applying" } else { "preview for" },
        snapshot_id
    );
    println!("Files to restore:");
    for file in &plan.restore_files {
        println!("- {}", file.path);
    }
    print_named_path_group("Files to remove", &plan.remove_paths);
    if !apply {
        println!("Preview only. Re-run with --apply to write files.");
    }
}

pub(crate) fn render_index_summary(artifact: &IndexArtifact) -> String {
    let mut out = String::new();
    out.push_str("# HarnessKit Doc Index Summary\n\n");
    out.push_str(&format!("- Managed root: `{}`\n", artifact.managed_root));
    out.push_str(&format!("- Documents indexed: `{}`\n", artifact.docs.len()));
    out.push_str(&format!(
        "- History latest snapshot: `{}`\n",
        artifact.history_latest_snapshot.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!("- History dirty: `{}`\n", artifact.history_dirty));
    out.push_str(&format!(
        "- Host git dirty: `{}`\n",
        git_dirty_label(artifact.host_git_dirty)
    ));
    out.push_str(&format!(
        "- Relations built: `{}`\n\n",
        artifact.relations.len()
    ));

    out.push_str("## Top Anchor Docs\n\n");
    let mut anchor_docs = artifact
        .docs
        .iter()
        .filter(|doc| doc.is_anchor_candidate || doc.is_entrypoint)
        .collect::<Vec<_>>();
    anchor_docs.sort_by(|a, b| {
        b.incoming_links
            .cmp(&a.incoming_links)
            .then_with(|| b.outgoing_links.cmp(&a.outgoing_links))
            .then_with(|| a.path.cmp(&b.path))
    });
    for doc in anchor_docs.into_iter().take(12) {
        out.push_str(&format!(
            "- `{}` - {} (incoming {}, outgoing {})\n",
            doc.path, doc.title, doc.incoming_links, doc.outgoing_links
        ));
    }

    out.push_str("\n## Relation Types\n\n");
    let mut relation_counts = BTreeMap::<String, usize>::new();
    for relation in &artifact.relations {
        *relation_counts
            .entry(relation.relation_type.clone())
            .or_insert(0) += 1;
    }
    for (relation_type, count) in relation_counts {
        out.push_str(&format!("- `{}`: `{}`\n", relation_type, count));
    }

    out.push_str("\n## Collections\n\n");
    let mut counts = BTreeMap::<String, usize>::new();
    for doc in &artifact.docs {
        if let Some(collection) = &doc.collection {
            *counts.entry(collection.clone()).or_insert(0) += 1;
        }
    }
    for (collection, count) in counts {
        out.push_str(&format!("- `{}`: `{}` docs\n", collection, count));
    }

    if !artifact.checks.is_empty() {
        out.push_str("\n## Checks\n\n");
        for check in &artifact.checks {
            out.push_str(&format!(
                "- `[{}]` `{}` - {}\n",
                check.severity, check.subject_path, check.message
            ));
        }
    }

    out
}

pub(crate) fn print_rank_rows(result: &QueryResult) {
    println!("Top ranked docs:");
    for row in &result.rows {
        let path = cell(row, 0);
        let title = cell(row, 1);
        let score = cell(row, 2);
        let role = cell(row, 3);
        let summary = cell(row, 4);
        println!("- {} [{}] score={} :: {}", path, role, score, title);
        if !summary.is_empty() {
            println!("  summary: {}", summary);
        }
    }
}

pub(crate) fn print_list_docs_rows(result: &QueryResult) {
    println!("Managed docs:");
    for row in &result.rows {
        println!(
            "- {} [{}] status={} authority={} collection={} incoming={} outgoing={} :: {}",
            cell(row, 0),
            cell(row, 2),
            blank_as_dash(cell(row, 3)),
            blank_as_dash(cell(row, 4)),
            blank_as_dash(cell(row, 7)),
            cell(row, 5),
            cell(row, 6),
            cell(row, 1)
        );
    }
}

pub(crate) fn print_inspect_output(
    doc: &QueryResult,
    outgoing: &QueryResult,
    incoming: &QueryResult,
    checks: &QueryResult,
) {
    let row = &doc.rows[0];
    println!("Path: {}", cell(row, 0));
    println!("Title: {}", cell(row, 1));
    println!("Role: {}", cell(row, 2));
    println!("Status: {}", blank_as_dash(cell(row, 3)));
    println!("Authority: {}", blank_as_dash(cell(row, 4)));
    println!("Collection: {}", blank_as_dash(cell(row, 5)));
    println!("Incoming links: {}", cell(row, 7));
    println!("Outgoing links: {}", cell(row, 8));
    println!("Entrypoint: {}", yes_no(cell(row, 9)));
    println!("Anchor candidate: {}", yes_no(cell(row, 10)));
    println!("Generated: {}", yes_no(cell(row, 11)));
    println!("Reference: {}", yes_no(cell(row, 12)));
    println!("Size bytes: {}", cell(row, 13));
    println!("Modified: {}", cell(row, 14));
    if !cell(row, 6).is_empty() {
        println!("Summary: {}", cell(row, 6));
    }

    println!("\nOutgoing relations:");
    if outgoing.rows.is_empty() {
        println!("- none");
    } else {
        for relation in &outgoing.rows {
            println!(
                "- {} -> {} ({}, confidence={:.2})",
                cell(relation, 0),
                cell(relation, 1),
                cell(relation, 2),
                cell(relation, 3).parse::<f32>().unwrap_or(0.0)
            );
        }
    }

    println!("\nIncoming relations:");
    if incoming.rows.is_empty() {
        println!("- none");
    } else {
        for relation in &incoming.rows {
            println!(
                "- {} <- {} ({}, confidence={:.2})",
                cell(relation, 0),
                cell(relation, 1),
                cell(relation, 2),
                cell(relation, 3).parse::<f32>().unwrap_or(0.0)
            );
        }
    }

    println!("\nChecks:");
    if checks.rows.is_empty() {
        println!("- none");
    } else {
        for check in &checks.rows {
            println!(
                "- [{}] {} :: {}",
                cell(check, 0),
                cell(check, 1),
                cell(check, 2)
            );
            if !cell(check, 3).is_empty() && cell(check, 3) != "{}" {
                println!("  evidence: {}", cell(check, 3));
            }
        }
    }
}

pub(crate) fn print_refs_output(doc_path: &str, result: &QueryResult) {
    println!("Refs for {}:", doc_path);
    if result.rows.is_empty() {
        println!("- none");
        return;
    }
    for row in &result.rows {
        println!(
            "- {} via {} ({}, confidence={:.2})",
            cell(row, 0),
            cell(row, 1),
            cell(row, 2),
            cell(row, 3).parse::<f32>().unwrap_or(0.0)
        );
    }
}

pub(crate) fn print_query_candidates_with_options(
    target_dir: &Path,
    query_terms: &str,
    candidates: &[QueryCandidate],
    extra_lines: &[String],
    options: &RenderOptions,
) -> RenderedPage {
    let mut lines = Vec::new();
    if candidates.is_empty() {
        lines.push(RenderLine::content("No matching documentation found."));
    }
    for (index, candidate) in candidates.iter().enumerate() {
        let badge = if candidate.lifecycle == "historical" {
            " superseded"
        } else {
            ""
        };
        lines.push(RenderLine::content(format!(
            "{}. {} §{}{}",
            index + 1,
            candidate.path,
            candidate.title,
            badge
        )));
        let excerpt = kwic_for_doc(
            target_dir,
            &candidate.path,
            query_terms,
            (!candidate.snippet_heading.is_empty()).then_some(candidate.snippet_heading.as_str()),
        );
        let excerpt = if excerpt.is_empty() {
            kwic_excerpt(&candidate.summary, query_terms, &candidate.title, 160)
        } else {
            excerpt
        };
        lines.push(RenderLine::content(format!("   {}", excerpt)));
        if options.full || options.verbose {
            if let Some(info) = &candidate.match_info {
                lines.push(RenderLine::related(format!(
                    "   match {}: {}{}",
                    info.mode,
                    info.matched_terms.join(", "),
                    if info.missing_terms.is_empty() {
                        String::new()
                    } else {
                        format!("; missing {}", info.missing_terms.join(", "))
                    }
                )));
            }
        }
    }
    for line in extra_lines.iter().take(2) {
        if line.starts_with("Coverage:") || line.starts_with('…') {
            lines.push(RenderLine::related(short_hint(line)));
        } else {
            lines.push(RenderLine::hint(format!("Hint: {}", short_hint(line))));
        }
    }
    let page = paginate_lines(lines, options, "query");
    for line in &page.lines {
        println!("{}", line);
    }
    page
}

fn compact_line(text: &str, max_chars: usize) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

fn bundle_node_label(node: &BundleNode) -> String {
    let location = match node.kind.as_str() {
        "symbol" => format!("{}: {}", node.path, node.name),
        "section" => format!("{} §{}", node.path, node.heading),
        _ => node.path.clone(),
    };
    compact_line(&format!("{} {}", node.role, location), 240)
}

pub(crate) fn print_query_bundles_with_options(
    target_dir: &Path,
    query: &str,
    bundles: &[QueryBundle],
    extra_lines: &[String],
    options: &RenderOptions,
) -> RenderedPage {
    let bundles = merge_document_bundles(bundles);
    let mut lines = Vec::new();
    if bundles.is_empty() {
        lines.push(RenderLine::content("No matching knowledge bundles found."));
    }
    for (index, bundle) in bundles.iter().enumerate() {
        let node = &bundle.primary;
        let badge = freshness_badge(&bundle.freshness.status);
        let badge = if badge.is_empty() {
            if node.lifecycle == "historical" {
                "superseded"
            } else {
                ""
            }
        } else {
            badge
        };
        let primary_issue = bundle
            .freshness
            .issues
            .iter()
            .filter_map(|issue| issue["kind"].as_str())
            .next()
            .unwrap_or("");
        let badge = if badge.is_empty() {
            primary_issue.is_empty().then_some("").unwrap_or("")
        } else {
            badge
        };
        let suffix = if badge.is_empty() {
            String::new()
        } else if primary_issue.is_empty() {
            format!(" {badge}")
        } else {
            format!(" {badge} {primary_issue}")
        };
        let label = bundle_node_label(node);
        let detail = if node.kind == "section" || node.kind == "doc" {
            kwic_for_doc(
                target_dir,
                &node.path,
                query,
                (!node.heading.is_empty()).then(|| {
                    node.heading
                        .split("; ")
                        .next()
                        .unwrap_or(node.heading.as_str())
                }),
            )
        } else if !node.signature.is_empty() {
            kwic_excerpt(&node.signature, query, &node.name, 160)
        } else {
            kwic_excerpt(&node.summary, query, &node.name, 160)
        };
        let suffix = if suffix.is_empty() {
            suffix
        } else {
            format!(" [{}]", suffix.trim())
        };
        lines.push(RenderLine::content(format!(
            "{}. {}{} — {}",
            index + 1,
            label,
            suffix,
            detail
        )));
        if options.expand == Some(index + 1) {
            if !node.summary.is_empty() {
                lines.push(RenderLine::content(format!(
                    "   {}",
                    compact(&node.summary, 600)
                )));
            }
            if !node.signature.is_empty() && !rendered_text_matches(&detail, &node.signature) {
                lines.push(RenderLine::content(format!(
                    "   {}",
                    compact(&node.signature, 600)
                )));
            }
        }
        for related in &bundle.related {
            let link = &related.link;
            if link.edge.weight < 0.05 && link.edge.confidence == "INFERRED" {
                continue;
            }
            let links = std::iter::once(link).chain(related.additional_symbols.iter());
            let label = if related.additional_symbols.is_empty() {
                bundle_node_label(&link.node)
            } else {
                let names = links
                    .clone()
                    .filter(|link| link.node.kind == "symbol")
                    .map(|link| link.node.name.as_str())
                    .collect::<Vec<_>>();
                format!(
                    "{}: {}",
                    link.node.path,
                    merge_names(names.into_iter().map(str::to_string), 3)
                )
            };
            let mut badges = Vec::new();
            let edge_badge = edge_badge(&link.edge.confidence);
            if !edge_badge.is_empty() {
                badges.push(edge_badge);
            }
            for related_link in links.clone() {
                let badge = freshness_badge(&related_link.freshness.status);
                if !badge.is_empty() && !badges.contains(&badge) {
                    badges.push(badge);
                }
            }
            let issue_kind = links
                .clone()
                .flat_map(|item| item.freshness.issues.iter())
                .filter_map(|issue| issue["kind"].as_str())
                .next()
                .unwrap_or("");
            let suffix = if badges.is_empty() && issue_kind.is_empty() {
                String::new()
            } else if issue_kind.is_empty() {
                format!(" {}", badges.join(" "))
            } else {
                format!(" {} {}", badges.join(" "), issue_kind)
            };
            let evidence = links
                .clone()
                .map(|item| {
                    if !item.node.signature.is_empty() {
                        item.node.signature.clone()
                    } else {
                        local_anchor_names(&item.node.summary, &node.path)
                    }
                })
                .find(|text| !text.is_empty())
                .map(|text| kwic_excerpt(&text, query, &label, 140));
            if evidence.is_none() && !options.full && options.expand != Some(index + 1) {
                continue;
            }
            let detail = evidence.map(|text| format!(": {text}")).unwrap_or_default();
            lines.push(RenderLine::related(format!(
                "   ↳ {}{}{}",
                label, suffix, detail
            )));
            if options.expand == Some(index + 1) {
                for related_link in links {
                    if !related_link.node.signature.is_empty() {
                        lines.push(RenderLine::related(format!(
                            "      {}",
                            compact(&related_link.node.signature, 600)
                        )));
                    }
                }
            }
        }
    }
    for line in extra_lines.iter().take(2) {
        if line.starts_with("Coverage:") || line.starts_with('…') {
            lines.push(RenderLine::related(short_hint(line)));
        } else {
            lines.push(RenderLine::hint(format!("Hint: {}", short_hint(line))));
        }
    }
    let page = paginate_lines(lines, options, "query");
    for line in &page.lines {
        println!("{}", line);
    }
    page
}

fn merge_document_bundles(bundles: &[QueryBundle]) -> Vec<QueryBundle> {
    let mut merged = Vec::new();
    for bundle in bundles {
        {
            if let Some(existing) = merged.iter_mut().find(|candidate: &&mut QueryBundle| {
                matches!(candidate.primary.kind.as_str(), "doc" | "section")
                    == matches!(bundle.primary.kind.as_str(), "doc" | "section")
                    && candidate.primary.path == bundle.primary.path
            }) {
                if bundle.primary.kind == "symbol" {
                    existing.primary.name = merge_names(
                        [existing.primary.name.clone(), bundle.primary.name.clone()],
                        usize::MAX,
                    );
                    append_signature(&mut existing.primary.signature, &bundle.primary.signature);
                }
                if !bundle.primary.heading.is_empty()
                    && !existing.primary.heading.contains(&bundle.primary.heading)
                {
                    if !existing.primary.heading.is_empty() {
                        existing.primary.heading.push_str("; ");
                    }
                    existing.primary.heading.push_str(&bundle.primary.heading);
                }
                if !bundle.primary.summary.is_empty()
                    && !existing.primary.summary.contains(&bundle.primary.summary)
                {
                    existing.primary.summary.push_str(" ");
                    existing.primary.summary.push_str(&bundle.primary.summary);
                }
                existing.related.extend(bundle.related.clone());
                continue;
            }
        }
        merged.push(bundle.clone());
    }
    for bundle in &mut merged {
        let mut related = Vec::<crate::query::RelatedNode>::new();
        for mut item in std::mem::take(&mut bundle.related) {
            if let Some(existing) = related
                .iter_mut()
                .find(|existing| existing.link.node.path == item.link.node.path)
            {
                if existing.link.node.id == item.link.node.id {
                    existing
                        .additional_symbols
                        .append(&mut item.additional_symbols);
                    continue;
                }
                if matches!(item.link.node.kind.as_str(), "doc" | "section") {
                    existing
                        .link
                        .node
                        .heading
                        .push_str(&format!("; {}", item.link.node.heading));
                    existing
                        .link
                        .node
                        .summary
                        .push_str(&format!(" {}", item.link.node.summary));
                } else {
                    existing.additional_symbols.push(item.link);
                    existing
                        .additional_symbols
                        .append(&mut item.additional_symbols);
                }
            } else {
                related.push(item);
            }
        }
        for item in &mut related {
            let mut seen = std::collections::BTreeSet::from([item.link.node.id.clone()]);
            item.additional_symbols
                .retain(|link| seen.insert(link.node.id.clone()));
        }
        bundle.related = related;
    }
    merged
}

fn append_signature(existing: &mut String, signature: &str) {
    let signature = signature.trim();
    if signature.is_empty() || existing.split("; ").any(|part| part.trim() == signature) {
        return;
    }
    if !existing.is_empty() {
        existing.push_str("; ");
    }
    existing.push_str(signature);
}

fn rendered_text_matches(rendered: &str, source: &str) -> bool {
    let normalize = |text: &str| {
        text.replace(['«', '»'], "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    normalize(rendered) == normalize(source)
}

pub(crate) fn print_context_packet(label: &str, input: &str, packet: &ContextPacket) {
    let _ = print_context_packet_with_options(label, input, packet, &RenderOptions::default(), &[]);
}

pub(crate) fn print_context_packet_with_options(
    label: &str,
    input: &str,
    packet: &ContextPacket,
    options: &RenderOptions,
    hints: &[String],
) -> RenderedPage {
    let mut lines = vec![RenderLine::content(format!("{}: {}", label, input))];
    lines.push(RenderLine::content(format!(
        "Anchor: {} [{}] :: {}",
        packet.anchor.path, packet.anchor.role, packet.anchor.title
    )));
    let excerpt = if input == packet.anchor.path && !packet.anchor.summary.is_empty() {
        compact(&packet.anchor.summary, 160)
    } else if !packet.anchor.snippet.is_empty() {
        highlight(&packet.anchor.snippet, input)
    } else {
        kwic_excerpt(&packet.anchor.summary, input, &packet.anchor.title, 160)
    };
    if !excerpt.is_empty() {
        lines.push(RenderLine::content(format!("{}", excerpt)));
    }
    if !packet.incoming.is_empty() {
        lines.push(RenderLine::related("Incoming refs:"));
        lines.extend(relation_lines(&packet.incoming, true, &packet.related_text));
    }
    if !packet.outgoing.is_empty() {
        lines.push(RenderLine::related("Outgoing refs:"));
        lines.extend(relation_lines(
            &packet.outgoing,
            false,
            &packet.related_text,
        ));
    }
    if !packet.collection_siblings.is_empty() {
        lines.push(RenderLine::related(format!(
            "Collection siblings: {}",
            packet.collection_siblings.join(", ")
        )));
    }
    if !packet.same_scope_docs.is_empty() {
        lines.push(RenderLine::related(format!(
            "Same-scope docs: {}",
            packet.same_scope_docs.join(", ")
        )));
    }
    if (options.verbose || options.full) && !packet.recommended_reading_order.is_empty() {
        lines.push(RenderLine::related(format!(
            "Reading order: {}",
            packet.recommended_reading_order.join(" -> ")
        )));
    }
    if options.verbose || options.full {
        for hint in hints.iter().take(1) {
            lines.push(RenderLine::hint(format!("Hint: {}", short_hint(hint))));
        }
    }
    let page = paginate_lines(lines, options, "context");
    for line in &page.lines {
        println!("{}", line);
    }
    page
}

fn short_line(text: &str, limit: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > limit {
        text.chars().take(limit).collect::<String>() + "…"
    } else {
        text
    }
}

pub(crate) fn print_current_context_with_options(
    packet: &CurrentContext,
    coverage: &Coverage,
    hint: &str,
    options: &RenderOptions,
) -> RenderedPage {
    let mut lines = vec![RenderLine::content("Current context:")];
    if !packet.entrypoints.is_empty() {
        lines.extend(
            packet.entrypoints.iter().map(|doc| {
                RenderLine::content(format!("Entrypoints: {}", context_pointer_line(doc)))
            }),
        );
    }
    if let Some(doc) = &packet.project_map {
        lines.push(RenderLine::content(format!(
            "Project map: {}",
            context_pointer_line(doc)
        )));
    }
    if !packet.active_plans.is_empty() {
        lines.extend(packet.active_plans.iter().take(8).map(|doc| {
            RenderLine::content(format!("Active plans: {}", context_pointer_line(doc)))
        }));
        if packet.active_plans.len() > 8 {
            lines.push(RenderLine::related(format!(
                "- {} more active plans; use --json",
                packet.active_plans.len() - 8
            )));
        }
    }
    if !packet.recent_decisions.is_empty() {
        lines.extend(packet.recent_decisions.iter().take(3).map(|doc| {
            RenderLine::content(format!("Current decisions: {}", context_pointer_line(doc)))
        }));
    }
    if let Some(snapshot) = &packet.latest_snapshot {
        lines.push(RenderLine::content(format!(
            "Latest snapshot: {}",
            snapshot.id
        )));
    }
    if let Some(changes) = &packet.changes_since_snapshot {
        lines.push(RenderLine::content(format!(
            "Since snapshot: {} added, {} modified, {} removed documents",
            changes.added.len(),
            changes.modified.len(),
            changes.removed.len()
        )));
    }
    lines.extend(code_context::code_changes_lines(
        &packet.code_changes_since_snapshot,
    ));
    if packet.unverified_items > 0 {
        lines.push(RenderLine::content(format!(
            "Unverified: {} unchecked items in {} current documents",
            packet.unverified_items,
            packet.unverified_documents.len()
        )));
    }
    if options.verbose {
        lines.push(RenderLine::related(coverage.summary()));
    }
    if !hint.is_empty()
        && (options.verbose
            || packet.entrypoints.is_empty()
            || hint == "Run harnesskit migrate --plan")
    {
        lines.push(RenderLine::hint(format!("Hint: {}", short_hint(hint))));
    }
    let page = paginate_lines(lines, options, "context");
    for line in &page.lines {
        println!("{}", line);
    }
    page
}

fn context_pointer_line(doc: &ContextDoc) -> String {
    let badge = freshness_badge(&doc.lifecycle);
    if badge.is_empty() {
        format!(
            "- {} :: {} — {}",
            doc.path,
            short_line(&doc.title, 96),
            short_line(&doc.summary, 180)
        )
    } else {
        format!(
            "- {} {} :: {} — {}",
            doc.path,
            badge,
            short_line(&doc.title, 96),
            short_line(&doc.summary, 180)
        )
    }
}

pub(crate) fn print_handoff_context(packet: &HandoffContext, coverage: &Coverage, hint: &str) {
    println!("Context since {}:", packet.since);
    for (label, docs) in [("Added", &packet.added), ("Modified", &packet.modified)] {
        if docs.is_empty() {
            continue;
        }
        println!("{}:", label);
        for doc in docs {
            println!(
                "- {} :: {} | {}",
                doc.path,
                short_line(&doc.title, 96),
                short_line(&doc.summary, 180)
            );
        }
    }
    if !packet.removed.is_empty() {
        println!("Removed:");
        for path in &packet.removed {
            println!("- {}", path);
        }
    }
    let _ = coverage;
    if !hint.is_empty() {
        println!("Hint: {}", short_hint(hint));
    }
}

fn relation_lines(
    relations: &[DocRelation],
    incoming: bool,
    text: &BTreeMap<String, String>,
) -> Vec<RenderLine> {
    relations
        .iter()
        .map(|relation| {
            let path = if incoming {
                &relation.src_path
            } else {
                &relation.dst_path
            };
            let badge = if relation.explicit { "" } else { " ~" };
            RenderLine::related(format!(
                "- {}{} :: {}",
                path,
                badge,
                compact(
                    text.get(path)
                        .map(String::as_str)
                        .unwrap_or(&relation.reason),
                    160
                )
            ))
        })
        .collect()
}

fn blank_as_dash(value: &str) -> &str {
    if value.is_empty() {
        "-"
    } else {
        value
    }
}

fn yes_no(value: &str) -> &str {
    if value == "1" {
        "yes"
    } else {
        "no"
    }
}

pub(crate) fn print_init_result(target_dir: &Path, schema_source: &str, stats: &InitStats) {
    println!(
        "Initialized HarnessKit scaffold at {} using schema {} (created {}, skipped {})",
        target_dir.display(),
        schema_source,
        stats.created_files,
        stats.skipped_files
    );
}

pub(crate) fn print_index_result(target_dir: &Path, doc_count: usize, relation_count: usize) {
    println!(
        "Indexed HarnessKit docs at {} (docs {}, relations {})",
        target_dir.display(),
        doc_count,
        relation_count
    );
}

pub(crate) fn print_check_output(
    target_dir: &Path,
    docs_count: usize,
    relation_count: usize,
    checks: &[CheckView],
    baseline: &BaselineDelta,
    full: bool,
    fallback_hint: Option<&str>,
    options: &RenderOptions,
) {
    let summary = CheckSummary::from_checks(checks);
    let active_count = checks.iter().filter(|check| !check.suppressed).count();

    let mut lines = vec![RenderLine::content(format!(
        "HarnessKit checks for {}: docs {}, relations {}, checks {}",
        target_dir.display(),
        docs_count,
        relation_count,
        active_count
    ))];

    if full {
        lines.push(RenderLine::content(format!(
            "Severity summary: error {}, warning {}, info {}, suppressed {}",
            summary.error, summary.warning, summary.info, summary.suppressed
        )));
        if baseline.available {
            lines.push(RenderLine::content(format!(
                "Baseline: {}new {}, resolved {}{}",
                if baseline.created { "created; " } else { "" },
                baseline.new_count,
                baseline.resolved_count,
                if baseline.updated { "; updated" } else { "" }
            )));
        } else {
            lines.push(RenderLine::content("Baseline: not evaluated"));
        }
        for rule in &summary.by_rule {
            lines.push(RenderLine::content(format!(
                "- {}: error {}, warning {}, info {}, suppressed {}",
                rule.rule_id, rule.error, rule.warning, rule.info, rule.suppressed
            )));
        }
    } else if let Some(summary_line) = lines.first_mut() {
        summary_line.text.push_str(&format!(
            "; error {}, warning {}, info {}; new {}, resolved {}",
            summary.error,
            summary.warning,
            summary.info,
            baseline.new_count,
            baseline.resolved_count
        ));
    }
    for (label, changes) in [("new", &baseline.new), ("resolved", &baseline.resolved)] {
        for finding in changes.iter().take(if full { usize::MAX } else { 3 }) {
            lines.push(RenderLine::related(format!(
                "[{}] {} {} - {}",
                label,
                finding.rule_id,
                finding.subject_path,
                if full {
                    finding.message.clone()
                } else {
                    check_example(&finding.message)
                }
            )));
        }
    }
    let examples = checks
        .iter()
        .filter(|check| full || !check.suppressed)
        .filter(|check| {
            !baseline
                .new
                .iter()
                .take(if full { usize::MAX } else { 3 })
                .any(|finding| {
                    finding.rule_id == check.rule_id
                        && finding.subject_path == check.subject_path
                        && finding.message == check.message
                })
        })
        .take(if full { usize::MAX } else { 3 });
    let mut first_hint = baseline.new.iter().find_map(|finding| finding.hint.clone());
    for check in examples {
        if check.suppressed {
            lines.push(RenderLine::related(format!(
                "[suppressed] {} {} - {}",
                check.rule_id,
                check.subject_path,
                check.suppression_reason.as_deref().unwrap_or("suppressed")
            )));
            continue;
        }
        lines.push(RenderLine::content(format!(
            "[{}] {} {} - {}",
            check.severity,
            check.rule_id,
            check.subject_path,
            if full {
                check.message.clone()
            } else {
                check_example(&check.message)
            }
        )));
        if first_hint.is_none() {
            first_hint = check.hint.clone();
        }
        if full && !check.evidence_json.is_empty() && check.evidence_json != "{}" {
            lines.push(RenderLine::related(format!(
                "  evidence: {}",
                check.evidence_json
            )));
        }
    }
    if let Some(hint) = fallback_hint.map(str::to_string).or(first_hint) {
        lines.push(RenderLine::hint(format!("Hint: {}", short_hint(&hint))));
    } else if !full {
        lines.push(RenderLine::hint("Hint: Use check --all for all findings; --update-baseline accepts the current findings as the baseline."));
    }
    let mut page_options = options.clone();
    if full {
        // `check --all` promises every finding.  Keep each finding's evidence
        // attached while lifting the compact text budget for this explicit
        // full view.
        page_options.budget = usize::MAX;
        page_options.full = true;
    }
    let page = paginate_lines(lines, &page_options, "check");
    for line in page.lines {
        println!("{}", line);
    }
}

fn check_example(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut example = line.chars().take(240).collect::<String>();
    if line.chars().count() > 240 {
        example.push_str("...");
    }
    example
}

pub(crate) fn print_history_snapshot_result(snapshot: &HistorySnapshot) {
    println!(
        "Created HarnessKit history snapshot {} (files {}, message: {})",
        snapshot.id,
        snapshot.files.len(),
        snapshot.message
    );
}

pub(crate) fn print_history_list(snapshots: &[HistorySnapshot]) {
    if snapshots.is_empty() {
        println!("No HarnessKit history snapshots found.");
        return;
    }

    println!("HarnessKit history snapshots:");
    for snapshot in snapshots {
        println!(
            "- {} files={} created_at={} message={}",
            snapshot.id,
            snapshot.files.len(),
            snapshot.created_at_unix,
            snapshot.message
        );
    }
}
