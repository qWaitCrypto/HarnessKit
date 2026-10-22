use std::collections::BTreeMap;
use std::path::Path;

use crate::checks::{rule_group, CheckView};
use crate::cli::InitMode;
use crate::context::ContextPacket;
use crate::doctor::{CommandCheck, DoctorReport};
use crate::fs_utils::absolute_display_path;
use crate::git::find_host_git_dir;
use crate::history::{
    is_history_dirty, parse_json_value_token, HistoryDiff, HistorySnapshot, HistoryStatus,
    JsonCursor, RestorePlan,
};
use crate::index::{cell, DocRelation, IndexArtifact};
use crate::query::QueryCandidate;
use crate::schema::Schema;
use crate::sqlite_cli::QueryResult;
use crate::templates::{init_plan_paths, InitStats};

pub(crate) fn usage() {
    eprintln!("harnesskit init [target_dir] [--schema <path>] [--docs-root <path>] [--force] [--local|--tracked] [--preview]");
    eprintln!("harnesskit index [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit rank [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit list-docs [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit inspect <doc_path> [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit refs <doc_path> [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!(
        "harnesskit query <terms> [target_dir] [--schema <path>] [--docs-root <path>] [--json]"
    );
    eprintln!(
        "harnesskit graph <doc_path> [target_dir] [--schema <path>] [--docs-root <path>] [--json]"
    );
    eprintln!("harnesskit context <terms-or-doc-path> [target_dir] [--schema <path>] [--docs-root <path>] [--json]");
    eprintln!("harnesskit check [target_dir] [--schema <path>] [--docs-root <path>] [--rules <list>] [--strict|--no-strict] [--json]");
    eprintln!("harnesskit history snapshot -m <message> [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit history list [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit history status [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit history diff <snapshot-id|latest> [target_dir] [--schema <path>] [--docs-root <path>]");
    eprintln!(
        "harnesskit history diff <a> <b> [target_dir] [--schema <path>] [--docs-root <path>]"
    );
    eprintln!("harnesskit history restore <snapshot-id|latest> [target_dir] [--apply] [--force] [--schema <path>] [--docs-root <path>]");
    eprintln!("harnesskit doctor [target_dir] [--json]");
    eprintln!("harnesskit update [--version <tag>] [--repo <owner/name>] [--asset-base-url <url>]");
    eprintln!("harnesskit --version");
}

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

pub(crate) fn render_history_snapshot_json(snapshot: &HistorySnapshot) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"id\": {},\n", json_string(&snapshot.id)));
    out.push_str(&format!(
        "  \"created_at_unix\": {},\n",
        snapshot.created_at_unix
    ));
    out.push_str(&format!(
        "  \"message\": {},\n",
        json_string(&snapshot.message)
    ));
    out.push_str(&format!(
        "  \"docs_root\": {},\n",
        json_string(&snapshot.docs_root)
    ));
    out.push_str(&format!(
        "  \"schema_hash\": {},\n",
        json_string(&snapshot.schema_hash)
    ));
    out.push_str(&format!(
        "  \"host_git_head\": {},\n",
        json_opt_string(snapshot.host_git_head.as_deref())
    ));
    out.push_str(&format!(
        "  \"host_git_branch\": {},\n",
        json_opt_string(snapshot.host_git_branch.as_deref())
    ));
    out.push_str(&format!(
        "  \"host_git_dirty\": {},\n",
        json_bool(snapshot.host_git_dirty)
    ));
    out.push_str("  \"files\": [\n");
    for (idx, file) in snapshot.files.iter().enumerate() {
        out.push_str("    {\n");
        out.push_str(&format!("      \"path\": {},\n", json_string(&file.path)));
        out.push_str(&format!("      \"hash\": {},\n", json_string(&file.hash)));
        out.push_str(&format!("      \"size\": {},\n", file.size));
        out.push_str(&format!("      \"mtime_unix\": {}\n", file.mtime_unix));
        out.push_str("    }");
        if idx + 1 != snapshot.files.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
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
    println!("sqlite3: {}", command_check_label(&report.sqlite3));
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
    println!(
        "Current executable: {}",
        report.current_exe.as_deref().unwrap_or("(unknown)")
    );
    println!(
        "Current executable directory on PATH: {}",
        yes_no_bool(report.path_contains_cli_dir)
    );
    if !report.sqlite3.present {
        println!("Hint: HarnessKit alpha requires sqlite3 CLI on PATH for fact-store-backed commands such as index, query, check, context, graph, inspect, refs, rank, and list-docs.");
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

pub(crate) fn render_doctor_json(report: &DoctorReport) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!(
        "  \"cli_version\": {},\n",
        json_string(&report.cli_version)
    ));
    out.push_str(&format!("  \"os\": {},\n", json_string(&report.os)));
    out.push_str(&format!("  \"arch\": {},\n", json_string(&report.arch)));
    out.push_str(&format!(
        "  \"target_dir\": {},\n",
        json_string(&report.target_dir)
    ));
    out.push_str(&format!(
        "  \"current_dir\": {},\n",
        json_string(&report.current_dir)
    ));
    out.push_str(&format!(
        "  \"pwd\": {},\n",
        json_opt_string(report.pwd.as_deref())
    ));
    out.push_str(&format!(
        "  \"pwd_differs_from_current_dir\": {},\n",
        json_bool(report.pwd_differs_from_current_dir)
    ));
    out.push_str(&format!(
        "  \"sqlite3\": {},\n",
        render_command_check_json(&report.sqlite3)
    ));
    out.push_str(&format!(
        "  \"git\": {},\n",
        render_command_check_json(&report.git)
    ));
    out.push_str(&format!(
        "  \"inside_git_repo\": {},\n",
        report.inside_git_repo.map(json_bool).unwrap_or("null")
    ));
    out.push_str(&format!(
        "  \"git_repo_error\": {},\n",
        json_opt_string(report.git_repo_error.as_deref())
    ));
    out.push_str(&format!(
        "  \"state_exists\": {},\n",
        json_bool(report.state_exists)
    ));
    out.push_str(&format!(
        "  \"state_writable\": {},\n",
        json_bool(report.state_writable)
    ));
    out.push_str(&format!(
        "  \"state_error\": {},\n",
        json_opt_string(report.state_error.as_deref())
    ));
    out.push_str(&format!(
        "  \"history_exists\": {},\n",
        json_bool(report.history_exists)
    ));
    out.push_str(&format!(
        "  \"schema_exists\": {},\n",
        json_bool(report.schema_exists)
    ));
    out.push_str(&format!(
        "  \"claude_skill_path\": {},\n",
        json_string(&report.claude_skill_path)
    ));
    out.push_str(&format!(
        "  \"claude_skill_installed\": {},\n",
        json_bool(report.claude_skill_installed)
    ));
    out.push_str(&format!(
        "  \"codex_skill_path\": {},\n",
        json_string(&report.codex_skill_path)
    ));
    out.push_str(&format!(
        "  \"codex_skill_installed\": {},\n",
        json_bool(report.codex_skill_installed)
    ));
    out.push_str(&format!(
        "  \"current_exe\": {},\n",
        json_opt_string(report.current_exe.as_deref())
    ));
    out.push_str(&format!(
        "  \"path_contains_cli_dir\": {},\n",
        json_bool(report.path_contains_cli_dir)
    ));
    out.push_str("  \"notes\": [\n");
    out.push_str(
        "    \"HarnessKit alpha requires sqlite3 CLI on PATH for fact-store-backed commands.\",\n",
    );
    out.push_str("    \".harnesskit/state is derived and rebuildable; .harnesskit/history stores local doc checkpoints.\",\n");
    out.push_str("    \"If diagnostics do not point at the intended writable project root, pass the project root explicitly.\"\n");
    out.push_str("  ]\n");
    out.push('}');
    out
}

fn render_command_check_json(check: &CommandCheck) -> String {
    format!(
        "{{\"present\":{},\"version\":{},\"error\":{}}}",
        json_bool(check.present),
        json_opt_string(check.version.as_deref()),
        json_opt_string(check.error.as_deref())
    )
}

pub(crate) fn render_index_json(artifact: &IndexArtifact) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!(
        "  \"repo_root\": {},\n",
        json_string(&artifact.repo_root)
    ));
    out.push_str(&format!(
        "  \"managed_root\": {},\n",
        json_string(&artifact.managed_root)
    ));
    out.push_str(&format!(
        "  \"generated_at_unix\": {},\n",
        artifact.generated_at_unix
    ));
    out.push_str(&format!(
        "  \"history_latest_snapshot\": {},\n",
        json_opt_string(artifact.history_latest_snapshot.as_deref())
    ));
    out.push_str(&format!(
        "  \"history_dirty\": {},\n",
        json_bool(artifact.history_dirty)
    ));
    out.push_str(&format!(
        "  \"history_tracked_files_count\": {},\n",
        artifact.history_tracked_files_count
    ));
    out.push_str(&format!(
        "  \"host_git_head\": {},\n",
        json_opt_string(artifact.host_git_head.as_deref())
    ));
    out.push_str(&format!(
        "  \"host_git_branch\": {},\n",
        json_opt_string(artifact.host_git_branch.as_deref())
    ));
    out.push_str(&format!(
        "  \"host_git_dirty\": {},\n",
        json_bool(artifact.host_git_dirty)
    ));
    out.push_str("  \"docs\": [\n");
    for (idx, doc) in artifact.docs.iter().enumerate() {
        out.push_str("    {\n");
        out.push_str(&format!("      \"path\": {},\n", json_string(&doc.path)));
        out.push_str(&format!("      \"title\": {},\n", json_string(&doc.title)));
        out.push_str(&format!(
            "      \"summary\": {},\n",
            json_string(&doc.summary)
        ));
        out.push_str(&format!("      \"role\": {},\n", json_string(&doc.role)));
        out.push_str(&format!(
            "      \"status\": {},\n",
            json_opt_string(doc.status.as_deref())
        ));
        out.push_str(&format!(
            "      \"authority\": {},\n",
            json_opt_string(doc.authority.as_deref())
        ));
        out.push_str(&format!(
            "      \"collection\": {},\n",
            json_opt_string(doc.collection.as_deref())
        ));
        out.push_str(&format!(
            "      \"headings\": {},\n",
            json_string_array(&doc.headings)
        ));
        out.push_str(&format!(
            "      \"links\": {},\n",
            json_string_array(&doc.links)
        ));
        out.push_str(&format!(
            "      \"scope_paths\": {},\n",
            json_string_array(&doc.scope_paths)
        ));
        out.push_str(&format!(
            "      \"supersedes\": {},\n",
            json_string_array(&doc.supersedes)
        ));
        out.push_str(&format!(
            "      \"content_hash\": {},\n",
            json_string(&doc.content_hash)
        ));
        out.push_str(&format!(
            "      \"file_size_bytes\": {},\n",
            doc.file_size_bytes
        ));
        out.push_str(&format!(
            "      \"incoming_links\": {},\n",
            doc.incoming_links
        ));
        out.push_str(&format!(
            "      \"outgoing_links\": {},\n",
            doc.outgoing_links
        ));
        out.push_str(&format!(
            "      \"is_entrypoint\": {},\n",
            json_bool(doc.is_entrypoint)
        ));
        out.push_str(&format!(
            "      \"is_anchor_candidate\": {},\n",
            json_bool(doc.is_anchor_candidate)
        ));
        out.push_str(&format!(
            "      \"is_generated\": {},\n",
            json_bool(doc.is_generated)
        ));
        out.push_str(&format!(
            "      \"is_reference\": {},\n",
            json_bool(doc.is_reference)
        ));
        out.push_str(&format!("      \"mtime_unix\": {}\n", doc.mtime_unix));
        out.push_str("    }");
        if idx + 1 != artifact.docs.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ],\n");
    out.push_str("  \"relations\": [\n");
    for (idx, relation) in artifact.relations.iter().enumerate() {
        out.push_str("    {\n");
        out.push_str(&format!(
            "      \"src_path\": {},\n",
            json_string(&relation.src_path)
        ));
        out.push_str(&format!(
            "      \"dst_path\": {},\n",
            json_string(&relation.dst_path)
        ));
        out.push_str(&format!(
            "      \"relation_type\": {},\n",
            json_string(&relation.relation_type)
        ));
        out.push_str(&format!(
            "      \"reason\": {},\n",
            json_string(&relation.reason)
        ));
        out.push_str(&format!(
            "      \"explicit\": {},\n",
            json_bool(relation.explicit)
        ));
        out.push_str(&format!(
            "      \"confidence\": {:.2},\n",
            relation.confidence
        ));
        out.push_str(&format!(
            "      \"evidence_json\": {}\n",
            json_value_or_string(&relation.evidence_json)
        ));
        out.push_str("    }");
        if idx + 1 != artifact.relations.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ],\n");
    out.push_str("  \"checks\": [\n");
    for (idx, check) in artifact.checks.iter().enumerate() {
        out.push_str("    {\n");
        out.push_str(&format!(
            "      \"rule_id\": {},\n",
            json_string(&check.rule_id)
        ));
        out.push_str(&format!(
            "      \"severity\": {},\n",
            json_string(&check.severity)
        ));
        out.push_str(&format!(
            "      \"subject_path\": {},\n",
            json_string(&check.subject_path)
        ));
        out.push_str(&format!(
            "      \"message\": {}\n",
            json_string(&check.message)
        ));
        out.push_str("    }");
        if idx + 1 != artifact.checks.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n");
    out.push_str("}\n");
    out
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
        artifact.host_git_dirty
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
            "- {} [{}] status={} score={} :: {}",
            candidate.path,
            candidate.role,
            blank_as_dash(&candidate.status),
            candidate.score,
            candidate.title
        );
        println!("  why: {}", candidate.why);
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

pub(crate) fn render_check_json(
    target_dir: &Path,
    docs_count: usize,
    relation_count: usize,
    checks: &[CheckView],
) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!(
        "  \"target_dir\": {},\n",
        json_string(&absolute_display_path(target_dir))
    ));
    out.push_str(&format!("  \"docs_count\": {},\n", docs_count));
    out.push_str(&format!("  \"relation_count\": {},\n", relation_count));
    out.push_str("  \"checks\": [\n");
    for (idx, check) in checks.iter().enumerate() {
        out.push_str("    {\n");
        out.push_str(&format!(
            "      \"severity\": {},\n",
            json_string(&check.severity)
        ));
        out.push_str(&format!(
            "      \"rule_id\": {},\n",
            json_string(&check.rule_id)
        ));
        out.push_str(&format!(
            "      \"rule_group\": {},\n",
            json_string(&rule_group(&check.rule_id))
        ));
        out.push_str(&format!(
            "      \"subject_path\": {},\n",
            json_string(&check.subject_path)
        ));
        out.push_str(&format!(
            "      \"message\": {},\n",
            json_string(&check.message)
        ));
        out.push_str(&format!(
            "      \"evidence_json\": {},\n",
            json_value_or_string(&check.evidence_json)
        ));
        out.push_str(&format!(
            "      \"suppressed\": {},\n",
            json_bool(check.suppressed)
        ));
        out.push_str(&format!(
            "      \"suppression_reason\": {}\n",
            json_opt_string(check.suppression_reason.as_deref())
        ));
        out.push_str("    }");
        if idx + 1 != checks.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

pub(crate) fn render_query_json(query_terms: &str, candidates: &[QueryCandidate]) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"query\": {},\n", json_string(query_terms)));
    out.push_str("  \"candidates\": [\n");
    for (idx, candidate) in candidates.iter().enumerate() {
        out.push_str(&render_query_candidate_json(candidate, 4));
        if idx + 1 != candidates.len() {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("  ]\n}\n");
    out
}

pub(crate) fn render_context_packet_json(
    kind: &str,
    input: &str,
    packet: &ContextPacket,
) -> String {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(&format!("  \"kind\": {},\n", json_string(kind)));
    out.push_str(&format!("  \"input\": {},\n", json_string(input)));
    out.push_str("  \"anchor\": ");
    out.push_str(&render_query_candidate_json(&packet.anchor, 2));
    out.push_str(",\n");
    out.push_str(&format!(
        "  \"incoming\": {},\n",
        render_relations_json(&packet.incoming)
    ));
    out.push_str(&format!(
        "  \"outgoing\": {},\n",
        render_relations_json(&packet.outgoing)
    ));
    out.push_str(&format!(
        "  \"collection_siblings\": {},\n",
        json_string_array(&packet.collection_siblings)
    ));
    out.push_str(&format!(
        "  \"same_scope_docs\": {},\n",
        json_string_array(&packet.same_scope_docs)
    ));
    out.push_str(&format!(
        "  \"recommended_reading_order\": {}\n",
        json_string_array(&packet.recommended_reading_order)
    ));
    out.push_str("}\n");
    out
}

fn render_query_candidate_json(candidate: &QueryCandidate, indent: usize) -> String {
    let pad = " ".repeat(indent);
    let inner = " ".repeat(indent + 2);
    format!(
        "{pad}{{\n{inner}\"path\": {},\n{inner}\"title\": {},\n{inner}\"role\": {},\n{inner}\"status\": {},\n{inner}\"score\": {},\n{inner}\"why\": {},\n{inner}\"summary\": {},\n{inner}\"neighbors\": {},\n{inner}\"snippet_heading\": {},\n{inner}\"snippet_location\": {},\n{inner}\"snippet\": {}\n{pad}}}",
        json_string(&candidate.path),
        json_string(&candidate.title),
        json_string(&candidate.role),
        json_string(&candidate.status),
        json_string(&candidate.score),
        json_string(&candidate.why),
        json_string(&candidate.summary),
        json_string_array(&candidate.neighbors),
        json_string(&candidate.snippet_heading),
        json_string(&candidate.snippet_location),
        json_string(&candidate.snippet),
        pad = pad,
        inner = inner
    )
}

fn render_relations_json(relations: &[DocRelation]) -> String {
    let mut out = String::from("[");
    for (idx, relation) in relations.iter().enumerate() {
        if idx > 0 {
            out.push_str(", ");
        }
        out.push_str(&format!(
            "{{\"src_path\":{},\"dst_path\":{},\"relation_type\":{},\"reason\":{},\"confidence\":{:.2},\"explicit\":{}}}",
            json_string(&relation.src_path),
            json_string(&relation.dst_path),
            json_string(&relation.relation_type),
            json_string(&relation.reason),
            relation.confidence,
            json_bool(relation.explicit)
        ));
    }
    out.push(']');
    out
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

fn json_string_array(values: &[String]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(|value| json_string(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn json_value_or_string(value: &str) -> String {
    let trimmed = value.trim();
    if parse_json_value_token(trimmed, &mut JsonCursor::default()).is_ok() {
        trimmed.to_string()
    } else {
        json_string(value)
    }
}

fn json_opt_string(value: Option<&str>) -> String {
    value.map(json_string).unwrap_or_else(|| "null".to_string())
}

fn json_bool(value: bool) -> &'static str {
    if value {
        "true"
    } else {
        "false"
    }
}

pub(crate) fn json_string(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\u{08}' => escaped.push_str("\\b"),
            '\u{0c}' => escaped.push_str("\\f"),
            ch if ch < '\u{20}' => escaped.push_str(&format!("\\u{:04x}", ch as u32)),
            _ => escaped.push(ch),
        }
    }
    format!("\"{}\"", escaped)
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
) {
    let warning_count = checks
        .iter()
        .filter(|check| !check.suppressed && check.severity == "warning")
        .count();
    let error_count = checks
        .iter()
        .filter(|check| !check.suppressed && check.severity == "error")
        .count();
    let info_count = checks
        .iter()
        .filter(|check| !check.suppressed && check.severity == "info")
        .count();
    let active_count = checks.iter().filter(|check| !check.suppressed).count();

    println!(
        "HarnessKit checks for {}: docs {}, relations {}, checks {}",
        target_dir.display(),
        docs_count,
        relation_count,
        active_count
    );

    if checks.iter().all(|check| check.suppressed) {
        println!("No checks emitted.");
        return;
    }

    println!(
        "Severity summary: error {}, warning {}, info {}",
        error_count, warning_count, info_count
    );
    for check in checks {
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
            check.severity, check.rule_id, check.subject_path, check.message
        );
        if !check.evidence_json.is_empty() && check.evidence_json != "{}" {
            println!("  evidence: {}", check.evidence_json);
        }
    }
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
