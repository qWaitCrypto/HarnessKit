use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::checks::{push_check, IndexCheck};
use crate::discover::is_markdown;
use crate::external_refs::resolve_mention;
use crate::fs_utils::{now_unix, read_text_lossy};
use crate::index::IndexedDoc;
use crate::lifecycle::{
    banner_status, doc_lifecycle, historical_status, is_active_plan, path_status, successor_map,
};
use crate::parse::{markdown_prose_lines, parse_doc_text, parse_heading};
use crate::schema::Schema;
use crate::Result;

pub(super) fn add_lifecycle_checks(
    schema: &Schema,
    root: &Path,
    docs: &[IndexedDoc],
    checks: &mut Vec<IndexCheck>,
) -> Result<()> {
    let successors = successor_map(docs);
    let max_lines = schema.rules.entrypoint_max_lines.unwrap_or(120) as usize;
    let stale_days = u64::from(schema.rules.stale_active_plan_days.unwrap_or(30));
    let project_map = schema
        .entrypoints
        .project_map
        .as_deref()
        .unwrap_or("docs/project.md");
    let now = now_unix();
    for doc in docs {
        if schema.entrypoints.agents.as_deref() == Some(&doc.path)
            || schema.entrypoints.claude.as_deref() == Some(&doc.path)
        {
            let lines = read_text_lossy(&root.join(&doc.path))?.lines().count();
            if lines > max_lines {
                emit(checks, "entrypoint-too-long", &doc.path,
                    &format!("{} has {} lines (limit {}).", doc.path, lines, max_lines),
                    &format!("Move progress and current-plan details from {} to {}; keep the entrypoint as a routing table.", doc.path, project_map));
            }
        }
        let explicit_status = doc.frontmatter_keys.iter().any(|key| key == "status");
        if explicit_status
            && doc
                .status
                .as_deref()
                .map(|status| status.eq_ignore_ascii_case("active"))
                .unwrap_or(false)
            && (path_status(&doc.path)
                .map(historical_status)
                .unwrap_or(false)
                || banner_status(&doc.summary).is_some())
        {
            emit(checks, "active-but-historical", &doc.path,
                "Explicit active status conflicts with a historical path or opening banner.",
                &format!("Review {}: mark status as superseded/completed/archived, or remove the obsolete banner and move the current document to an active path.", doc.path));
        }
        for old in &doc.supersedes {
            if let Some(previous) = docs.iter().find(|candidate| candidate.path == *old) {
                let marked = previous.frontmatter_keys.iter().any(|key| key == "status")
                    && previous
                        .status
                        .as_deref()
                        .map(|status| status.eq_ignore_ascii_case("superseded"))
                        .unwrap_or(false);
                if !marked {
                    emit(checks, "superseded-not-marked", old,
                        &format!("{} is superseded by {} but has no explicit superseded status.", old, doc.path),
                        &format!("Add `status: superseded` and `superseded_by: {}` to the frontmatter of {}.", doc.path, old));
                }
            }
        }
        if is_active_plan(
            doc,
            successors.get(&doc.path).map(Vec::as_slice).unwrap_or(&[]),
        ) && now.saturating_sub(doc.mtime_unix) > stale_days * 86400
        {
            emit(checks, "stale-active-plan", &doc.path,
                &format!("Active plan {} has not changed for {} days (limit {}).", doc.path, now.saturating_sub(doc.mtime_unix) / 86400, stale_days),
                &format!("Review and update the remaining acceptance items in {}, or mark the plan completed/superseded if it has ended.", doc.path));
        }
    }
    add_pointer_checks(root, docs, &successors, project_map, checks);
    Ok(())
}

fn emit(checks: &mut Vec<IndexCheck>, rule: &str, path: &str, message: &str, hint: &str) {
    push_check(
        checks,
        rule,
        "info",
        path,
        message,
        &[("doc", path), ("hint", hint)],
    );
}

fn current_plan_label(text: &str) -> bool {
    let lower = text.to_lowercase();
    (lower.contains("current") && lower.contains("plan"))
        || (lower.contains("当前") && lower.contains("计划"))
}

fn add_pointer_checks(
    root: &Path,
    docs: &[IndexedDoc],
    successors: &BTreeMap<String, Vec<String>>,
    project_map: &str,
    checks: &mut Vec<IndexCheck>,
) {
    let next = |doc: &IndexedDoc| successors.get(&doc.path).map(Vec::as_slice).unwrap_or(&[]);
    let plans = docs
        .iter()
        .filter(|doc| is_active_plan(doc, next(doc)))
        .map(|doc| doc.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut pointers = BTreeMap::<String, BTreeSet<String>>::new();
    for doc in docs.iter().filter(|doc| {
        doc_lifecycle(
            &doc.path,
            doc.status.as_deref().unwrap_or(""),
            &doc.summary,
            next(doc),
        ) == "current"
    }) {
        let mut current_heading = false;
        for line in markdown_prose_lines(&doc.content_text) {
            if let Some((_, heading)) = parse_heading(line) {
                current_heading = current_plan_label(&heading);
            }
            if !current_heading && !current_plan_label(line) {
                continue;
            }
            let parsed = parse_doc_text(&doc.path, line);
            let targets = parsed
                .links
                .into_iter()
                .chain(
                    parsed
                        .path_mentions
                        .iter()
                        .filter_map(|mention| resolve_mention(root, &doc.path, mention)),
                )
                .chain(line.split_whitespace().filter_map(|token| {
                    let path = token.trim_matches(|ch| matches!(ch, '`' | '(' | ')' | ',' | ';'));
                    is_markdown(path)
                        .then(|| resolve_mention(root, &doc.path, path))
                        .flatten()
                }));
            for target in targets.filter(|target| plans.contains(target.as_str())) {
                pointers.entry(target).or_default().insert(doc.path.clone());
            }
        }
    }
    for (plan, sources) in &pointers {
        if sources.len() >= 3 {
            let paths = sources.iter().cloned().collect::<Vec<_>>().join(", ");
            emit(checks, "duplicated-current-pointer", sources.first().unwrap(),
                &format!("Current plan {} is repeated in {} files: {}.", plan, sources.len(), paths),
                &format!("Keep the current-plan pointer in {} and replace the repeated pointers in {} with a link to that project map.", project_map, paths));
        }
    }
    let sources = pointers.values().flatten().collect::<BTreeSet<_>>();
    if pointers.len() > 1 && sources.len() > 1 {
        let targets = pointers
            .iter()
            .map(|(plan, sources)| {
                format!(
                    "{} -> {}",
                    sources.iter().cloned().collect::<Vec<_>>().join(", "),
                    plan
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        emit(checks, "duplicated-current-pointer", sources.first().unwrap(),
            &format!("Current-plan pointers disagree across files: {}.", targets),
            &format!("Reconcile the current-plan pointers in {} through {}; list concurrent plans there explicitly if both remain active.", sources.into_iter().cloned().collect::<Vec<_>>().join(", "), project_map));
    }
}
