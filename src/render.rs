mod json;

pub(crate) use json::{
    render_check_json, render_context_packet_json, render_covered_json, render_doctor_json,
    render_history_snapshot_json, render_index_json, render_query_json, serialize_evidence,
    serialize_inline,
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
use crate::query::QueryCandidate;
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

pub(crate) fn print_query_candidates(query_terms: &str, candidates: &[QueryCandidate]) {
    println!("Query: {}", query_terms);
    if candidates.is_empty() {
        println!("No candidate docs found.");
        return;
    }
    for candidate in candidates {
        println!(
            "- {} [{}] status={} lifecycle={} score={} :: {}",
            candidate.path,
            candidate.role,
            blank_as_dash(&candidate.status),
            candidate.lifecycle,
            candidate.score,
            candidate.title
        );
        println!("  why: {}", candidate.why);
        if let Some(info) = &candidate.match_info {
            println!(
                "  match: {} (matched: {}; missing: {})",
                info.mode,
                info.matched_terms.join(", "),
                info.missing_terms.join(", ")
            );
        }
        if !candidate.superseded_by.is_empty() {
            println!("  superseded by: {}", candidate.superseded_by.join(", "));
        }
        if !candidate.summary.is_empty() {
            println!("  summary: {}", candidate.summary);
        }
        if !candidate.snippet.is_empty() {
            println!("  snippet: {}", candidate.snippet);
            if !candidate.snippet_heading.is_empty() {
                println!("  heading: {}", candidate.snippet_heading);
            }
            if !candidate.snippet_location.is_empty() {
                println!("  location: {}", candidate.snippet_location);
            }
        }
        if !candidate.neighbors.is_empty() {
            println!("  neighbors: {}", candidate.neighbors.join(" | "));
        }
    }
}

pub(crate) fn print_context_packet(label: &str, input: &str, packet: &ContextPacket) {
    println!("{}: {}", label, input);
    println!(
        "Anchor: {} [{}] :: {}",
        packet.anchor.path, packet.anchor.role, packet.anchor.title
    );
    if !packet.anchor.summary.is_empty() {
        println!("Summary: {}", packet.anchor.summary);
    }
    if !packet.anchor.snippet.is_empty() {
        println!("Snippet: {}", packet.anchor.snippet);
        if !packet.anchor.snippet_location.is_empty() {
            println!("Location: {}", packet.anchor.snippet_location);
        }
    }

    println!("\nIncoming refs:");
    print_relation_list(&packet.incoming, true);
    println!("\nOutgoing refs:");
    print_relation_list(&packet.outgoing, false);
    println!("\nCollection siblings:");
    print_path_list(&packet.collection_siblings);
    println!("\nSame-scope docs:");
    print_path_list(&packet.same_scope_docs);
    println!("\nRecommended reading order:");
    print_path_list(&packet.recommended_reading_order);
}

fn short_line(text: &str, limit: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > limit {
        text.chars().take(limit).collect::<String>() + "…"
    } else {
        text
    }
}

fn print_context_pointer(doc: &ContextDoc) {
    println!(
        "- {} [status={}, lifecycle={}] :: {}",
        doc.path,
        doc.status.as_deref().unwrap_or("default"),
        doc.lifecycle,
        short_line(&doc.title, 96)
    );
}

pub(crate) fn print_current_context(packet: &CurrentContext, coverage: &Coverage, hint: &str) {
    println!("Current context:");
    println!("Entrypoints:");
    for doc in &packet.entrypoints {
        print_context_pointer(doc);
    }
    if packet.entrypoints.is_empty() {
        println!("- none");
    }
    println!("Project map:");
    if let Some(doc) = &packet.project_map {
        print_context_pointer(doc);
    } else {
        println!("- none");
    }
    println!("Active plans: {} (newest first)", packet.active_plans.len());
    for doc in packet.active_plans.iter().take(8) {
        print_context_pointer(doc);
    }
    if packet.active_plans.len() > 8 {
        println!(
            "- {} more active plans; use --json",
            packet.active_plans.len() - 8
        );
    }
    println!("Recent current decisions:");
    for doc in packet.recent_decisions.iter().take(3) {
        print_context_pointer(doc);
    }
    println!(
        "Latest snapshot: {}",
        packet
            .latest_snapshot
            .as_ref()
            .map(|snapshot| snapshot.id.as_str())
            .unwrap_or("none")
    );
    if let Some(changes) = &packet.changes_since_snapshot {
        println!(
            "Since snapshot: {} added, {} modified, {} removed documents",
            changes.added.len(),
            changes.modified.len(),
            changes.removed.len()
        );
    }
    println!(
        "Unverified: {} unchecked items in {} current documents",
        packet.unverified_items,
        packet.unverified_documents.len()
    );
    println!("{}", coverage.summary());
    println!("Hint: {}", hint);
}

pub(crate) fn print_handoff_context(packet: &HandoffContext, coverage: &Coverage, hint: &str) {
    println!("Context since {}:", packet.since);
    for (label, docs) in [("Added", &packet.added), ("Modified", &packet.modified)] {
        println!("{}: {}", label, docs.len());
        for doc in docs {
            println!(
                "- {} [status={}, lifecycle={}] :: {} | {}",
                doc.path,
                doc.status.as_deref().unwrap_or("default"),
                doc.lifecycle,
                short_line(&doc.title, 96),
                short_line(&doc.summary, 180)
            );
        }
    }
    println!("Removed: {}", packet.removed.len());
    for path in &packet.removed {
        println!("- {}", path);
    }
    println!("{}", coverage.summary());
    println!("Hint: {}", hint);
}

fn print_relation_list(relations: &[DocRelation], incoming: bool) {
    if relations.is_empty() {
        println!("- none");
        return;
    }
    for relation in relations {
        let path = if incoming {
            &relation.src_path
        } else {
            &relation.dst_path
        };
        println!(
            "- {} via {} ({}, confidence={:.2})",
            path, relation.relation_type, relation.reason, relation.confidence
        );
    }
}

fn print_path_list(paths: &[String]) {
    if paths.is_empty() {
        println!("- none");
        return;
    }
    for path in paths {
        println!("- {}", path);
    }
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
) {
    let summary = CheckSummary::from_checks(checks);
    let active_count = checks.iter().filter(|check| !check.suppressed).count();

    println!(
        "HarnessKit checks for {}: docs {}, relations {}, checks {}",
        target_dir.display(),
        docs_count,
        relation_count,
        active_count
    );

    println!(
        "Severity summary: error {}, warning {}, info {}, suppressed {}",
        summary.error, summary.warning, summary.info, summary.suppressed
    );
    if baseline.available {
        println!(
            "Baseline: {}new {}, resolved {}{}",
            if baseline.created { "created; " } else { "" },
            baseline.new_count,
            baseline.resolved_count,
            if baseline.updated { "; updated" } else { "" }
        );
    } else {
        println!("Baseline: not evaluated");
    }
    for rule in &summary.by_rule {
        println!(
            "- {}: error {}, warning {}, info {}, suppressed {}",
            rule.rule_id, rule.error, rule.warning, rule.info, rule.suppressed
        );
    }
    for (label, changes) in [("new", &baseline.new), ("resolved", &baseline.resolved)] {
        for finding in changes.iter().take(if full { usize::MAX } else { 3 }) {
            println!(
                "[{}] {} {} - {}",
                label,
                finding.rule_id,
                finding.subject_path,
                if full {
                    finding.message.clone()
                } else {
                    check_example(&finding.message)
                }
            );
        }
    }
    let examples = checks
        .iter()
        .filter(|check| full || !check.suppressed)
        .take(if full { usize::MAX } else { 3 });
    for check in examples {
        if check.suppressed {
            println!(
                "[suppressed] {} {} - {}",
                check.rule_id,
                check.subject_path,
                check.suppression_reason.as_deref().unwrap_or("suppressed")
            );
            continue;
        }
        println!(
            "[{}] {} {} - {}",
            check.severity,
            check.rule_id,
            check.subject_path,
            if full {
                check.message.clone()
            } else {
                check_example(&check.message)
            }
        );
        if let Some(hint) = &check.hint {
            if hint.contains('\n') {
                println!("  Hint:\n{}", hint);
            } else {
                println!(
                    "  Hint: {}",
                    if full {
                        hint.clone()
                    } else {
                        check_example(hint)
                    }
                );
            }
        }
        if full && !check.evidence_json.is_empty() && check.evidence_json != "{}" {
            println!("  evidence: {}", check.evidence_json);
        }
    }
    if !full {
        println!("Hint: Use check --all for all findings; --update-baseline accepts the current findings as the baseline.");
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
