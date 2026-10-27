use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde::Serialize;

use super::code::{
    cap, document_paths, state_affects, CodeDocument, CodeGraph, DOC_LIMIT, REGION_LIMIT,
};
use crate::code::{regions_for_text, CodeRegion};
use crate::discover::{is_ignored, is_markdown};
use crate::git::{git_repo_check, ignored_paths, run_git_capture};
use crate::history::HistorySnapshot;
use crate::knowledge::{read_anchor_lock, AnchorState, LockEntry};
use crate::parse::parse_doc_text;
use crate::schema::Schema;
use crate::sqlite::SqliteConnection;
use crate::Result;

const FILE_LIMIT: usize = 5;

struct RawChange {
    path: String,
    change: &'static str,
    structural_change: bool,
    regions: Vec<CodeRegion>,
}

#[derive(Serialize)]
pub(crate) struct RegionImpact {
    pub(crate) region: CodeRegion,
    pub(crate) covered: bool,
    pub(crate) documents: Vec<CodeDocument>,
    pub(crate) documents_total: usize,
}

#[derive(Serialize)]
pub(crate) struct FileImpact {
    pub(crate) path: String,
    pub(crate) change: &'static str,
    pub(crate) structural_change: bool,
    pub(crate) regions: Vec<RegionImpact>,
    pub(crate) regions_total: usize,
    pub(crate) documents: Vec<CodeDocument>,
    pub(crate) documents_total: usize,
    pub(crate) decisions: Vec<String>,
    pub(crate) coverage: &'static str,
}

#[derive(Default, Serialize)]
pub(crate) struct ImpactTotals {
    pub(crate) files: usize,
    pub(crate) regions: usize,
    pub(crate) covered: usize,
    pub(crate) uncovered: usize,
    pub(crate) drifting_anchors: usize,
    pub(crate) accepted_decisions_changed: usize,
}

#[derive(Serialize)]
pub(crate) struct DiffContext {
    pub(crate) kind: &'static str,
    pub(crate) revision: Option<String>,
    pub(crate) baseline: String,
    pub(crate) includes_worktree: bool,
    pub(crate) files: Vec<FileImpact>,
    pub(crate) predicted_drift: Vec<AnchorState>,
    pub(crate) accepted_decision_changes: Vec<AcceptedDecisionChange>,
    pub(crate) uncovered_regions: Vec<CodeRegion>,
    pub(crate) totals: ImpactTotals,
    pub(crate) truncated: BTreeMap<String, usize>,
}

#[derive(Clone, Serialize)]
pub(crate) struct AcceptedDecisionChange {
    pub(crate) path: String,
    pub(crate) message: &'static str,
}

pub(crate) fn build_diff_context(
    db: &SqliteConnection,
    root: &Path,
    schema: &Schema,
    revision: Option<&str>,
    locks: &[LockEntry],
) -> Result<DiffContext> {
    let graph = CodeGraph::load(db, locks)?;
    let (baseline, changes, mut accepted_decision_changes) = git_changes(root, schema, revision)?;
    let mut files = Vec::new();
    let mut uncovered_regions = Vec::new();
    let mut affected = Vec::new();
    let mut totals = ImpactTotals::default();
    let mut truncated = BTreeMap::new();
    for change in changes {
        let mut regions = Vec::new();
        let docs = graph.documents_for(&change.regions, false);
        let decisions = document_paths(&docs, |doc| doc.constrains);
        for region in change.regions {
            let mut documents = graph.documents_for(std::slice::from_ref(&region), false);
            let covered = documents.iter().any(CodeDocument::covers);
            if !covered {
                uncovered_regions.push(region.clone());
            }
            affected.push(region.clone());
            let documents_total = documents.len();
            documents.truncate(DOC_LIMIT);
            regions.push(RegionImpact {
                region,
                covered,
                documents,
                documents_total,
            });
        }
        let regions_total = regions.len();
        let covered = regions.iter().filter(|region| region.covered).count();
        totals.regions += regions_total;
        totals.covered += covered;
        totals.uncovered += regions_total - covered;
        cap(
            &mut regions,
            REGION_LIMIT,
            &format!("{}:regions", change.path),
            &mut truncated,
        );
        let documents_total = docs.len();
        let mut documents = docs;
        cap(
            &mut documents,
            DOC_LIMIT,
            &format!("{}:documents", change.path),
            &mut truncated,
        );
        files.push(FileImpact {
            path: change.path,
            change: change.change,
            structural_change: change.structural_change,
            regions,
            regions_total,
            documents,
            documents_total,
            decisions,
            coverage: if covered == regions_total {
                "covered"
            } else if covered == 0 {
                "uncovered"
            } else {
                "partial"
            },
        });
    }
    let mut predicted_drift = graph
        .states
        .iter()
        .filter(|state| {
            state.status != "current"
                && affected
                    .iter()
                    .any(|region| state_affects(state, region, false))
        })
        .cloned()
        .collect::<Vec<_>>();
    totals.files = files.len();
    totals.drifting_anchors = predicted_drift.len();
    totals.accepted_decisions_changed = accepted_decision_changes.len();
    cap(
        &mut accepted_decision_changes,
        DOC_LIMIT,
        "accepted_decision_changes",
        &mut truncated,
    );
    cap(&mut files, FILE_LIMIT, "files", &mut truncated);
    cap(
        &mut uncovered_regions,
        REGION_LIMIT,
        "uncovered_regions",
        &mut truncated,
    );
    cap(
        &mut predicted_drift,
        REGION_LIMIT,
        "predicted_drift",
        &mut truncated,
    );
    Ok(DiffContext {
        kind: "diff",
        revision: revision.map(str::to_string),
        baseline,
        includes_worktree: true,
        files,
        predicted_drift,
        accepted_decision_changes,
        uncovered_regions,
        totals,
        truncated,
    })
}

pub(crate) fn validate_diff_repository(root: &Path) -> Result<()> {
    if git_repo_check(root).0 != Some(true) {
        return Err("context --diff requires a Git working tree; use context --for <path|symbol|dir> instead".into());
    }
    Ok(())
}

fn git_changes(
    root: &Path,
    schema: &Schema,
    revision: Option<&str>,
) -> Result<(String, Vec<RawChange>, Vec<AcceptedDecisionChange>)> {
    validate_diff_repository(root)?;
    let baseline = match revision {
        Some(revision) => run_git_capture(
            root,
            &[
                "rev-parse",
                "--verify",
                "--end-of-options",
                &format!("{revision}^{{commit}}"),
            ],
        )?,
        None => run_git_capture(root, &["rev-parse", "--verify", "HEAD"])
            .or_else(|_| run_git_capture(root, &["hash-object", "-t", "tree", "--stdin"]))?,
    };
    let output = Command::new("git")
        .args([
            "--literal-pathspecs",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--relative",
            "--no-renames",
            "--name-status",
            "-z",
            &baseline,
            "--",
            ".",
        ])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr)
            .trim()
            .to_string()
            .into());
    }
    let prefix = run_git_capture(root, &["rev-parse", "--show-prefix"])?;
    let records = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let paths = records
        .chunks_exact(2)
        .map(|record| String::from_utf8_lossy(record[1]).into_owned())
        .collect::<Vec<_>>();
    let ignored = ignored_paths(root, &paths)?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let mut changes = Vec::new();
    let mut accepted_decision_changes = Vec::new();
    for record in records.chunks_exact(2) {
        let path = String::from_utf8_lossy(record[1]).into_owned();
        if is_ignored(schema, &path)
            || ignored.contains(&path)
            || path
                .split('/')
                .any(|part| matches!(part, ".git" | ".harnesskit"))
        {
            continue;
        }
        if is_markdown(&path) {
            if accepted_decision_body_changed(root, schema, &baseline, &prefix, &path)? {
                accepted_decision_changes.push(AcceptedDecisionChange {
                    path,
                    message: crate::lifecycle::ACCEPTED_DECISION_CHANGED,
                });
            }
            continue;
        }
        let old_bytes = Command::new("git")
            .args(["show", &format!("{baseline}:{prefix}{path}")])
            .current_dir(root)
            .output()?;
        let old = old_bytes.status.success().then_some(old_bytes.stdout);
        let regular = std::fs::symlink_metadata(root.join(&path))
            .map(|metadata| metadata.is_file())
            .unwrap_or(false);
        let new = if record[0] == b"D" || !regular {
            None
        } else {
            std::fs::read(root.join(&path)).ok()
        };
        // Match W1's text-only scope; never feed a binary blob into a parser.
        let text = |bytes: &Option<Vec<u8>>| {
            bytes
                .as_ref()
                .map(|bytes| {
                    if bytes.contains(&0) {
                        None
                    } else {
                        std::str::from_utf8(bytes).ok().map(str::to_string)
                    }
                })
                .unwrap_or_else(|| Some(String::new()))
        };
        let (Some(old_text), Some(new_text)) = (text(&old), text(&new)) else {
            continue;
        };
        let before = if old.is_some() {
            regions_for_text(&path, &old_text)?
        } else {
            Vec::new()
        };
        let after = if new.is_some() {
            regions_for_text(&path, &new_text)?
        } else {
            Vec::new()
        };
        let patch = run_git_capture(
            root,
            &[
                "--literal-pathspecs",
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-renames",
                "--unified=0",
                &baseline,
                "--",
                &path,
            ],
        )?;
        let changed_symbols = changed_fingerprints(&before, &after)
            .into_iter()
            .filter(|region| region.kind == "symbol")
            .collect::<Vec<_>>();
        let changed_ids = changed_symbols
            .iter()
            .map(|region| region.id.as_str())
            .collect::<BTreeSet<_>>();
        let mut regions = changed_symbols
            .iter()
            .filter(|region| {
                !changed_symbols.iter().any(|inner| {
                    inner.span_start >= region.span_start
                        && inner.span_end <= region.span_end
                        && inner.span_end - inner.span_start < region.span_end - region.span_start
                })
            })
            .map(|region| (region.id.clone(), region.clone()))
            .collect::<BTreeMap<_, _>>();
        for line in patch.lines().filter(|line| line.starts_with("@@ ")) {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if let (Some(old_range), Some(new_range)) = (fields.get(1), fields.get(2)) {
                touched_regions(&before, old_range, &changed_ids, &mut regions);
                touched_regions(&after, new_range, &changed_ids, &mut regions);
            }
        }
        if regions.is_empty() {
            if let Some(file) = after.first().or_else(|| before.first()) {
                regions.insert(file.id.clone(), file.clone());
            }
        }
        let mut regions = regions.into_values().collect::<Vec<_>>();
        regions.sort_by(|a, b| (a.span_start, &a.name).cmp(&(b.span_start, &b.name)));
        changes.push(RawChange {
            path,
            change: if old.is_none() {
                "added"
            } else if new.is_none() {
                "removed"
            } else {
                "modified"
            },
            structural_change: before.first().map(|region| &region.fingerprint)
                != after.first().map(|region| &region.fingerprint),
            regions,
        });
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((baseline, changes, accepted_decision_changes))
}

fn accepted_decision_body_changed(
    root: &Path,
    schema: &Schema,
    baseline: &str,
    prefix: &str,
    path: &str,
) -> Result<bool> {
    let old_output = Command::new("git")
        .args(["show", &format!("{baseline}:{prefix}{path}")])
        .current_dir(root)
        .output()?;
    let old = old_output.status.success().then_some(old_output.stdout);
    let new = std::fs::read(root.join(path)).ok();
    let decode = |bytes: &Option<Vec<u8>>| {
        bytes.as_ref().and_then(|bytes| {
            if bytes.contains(&0) {
                None
            } else {
                std::str::from_utf8(bytes).ok().map(str::to_string)
            }
        })
    };
    let Some(old_text) = decode(&old) else {
        return Ok(false);
    };
    let Some(new_text) = (if new.is_none() {
        Some(String::new())
    } else {
        decode(&new)
    }) else {
        return Ok(false);
    };
    let old_doc = parse_doc_text(path, &old_text);
    let new_doc = parse_doc_text(path, &new_text);
    if old_doc.body_text.trim_start_matches('\n') == new_doc.body_text.trim_start_matches('\n') {
        return Ok(false);
    }
    Ok(is_accepted_decision(schema, path, &old_text)
        || is_accepted_decision(schema, path, &new_text))
}

fn is_accepted_decision(schema: &Schema, path: &str, text: &str) -> bool {
    let parsed = parse_doc_text(path, text);
    parsed
        .frontmatter
        .get("status")
        .map(|status| status.eq_ignore_ascii_case("accepted"))
        .unwrap_or(false)
        && crate::discover::infer_index_role(
            schema,
            path,
            crate::discover::collection_name_for_path(schema, path).as_deref(),
        ) == "decisions"
}

fn touched_regions(
    nodes: &[CodeRegion],
    range: &str,
    changed_ids: &BTreeSet<&str>,
    found: &mut BTreeMap<String, CodeRegion>,
) {
    let range = range.trim_start_matches(['-', '+']);
    let (start, count) = range.split_once(',').unwrap_or((range, "1"));
    let (Ok(start), Ok(count)) = (start.parse::<usize>(), count.parse::<usize>()) else {
        return;
    };
    let Some(file) = nodes.first() else {
        return;
    };
    for line in start..start.saturating_add(count) {
        let symbol = nodes
            .iter()
            .filter(|node| {
                node.kind == "symbol" && node.span_start <= line && node.span_end >= line
            })
            .min_by_key(|node| {
                (
                    !changed_ids.contains(node.id.as_str()),
                    node.span_end - node.span_start,
                )
            });
        let mut region = symbol.unwrap_or(file).clone();
        if symbol.is_none() {
            // A file-level remainder must not borrow a sibling symbol's docs.
            region.id = format!("{}#outside-symbols", file.id);
            region.span_start = line;
            region.span_end = line;
        }
        found
            .entry(region.id.clone())
            .and_modify(|previous| {
                if region.kind == "file" {
                    previous.span_start = previous.span_start.min(line);
                    previous.span_end = previous.span_end.max(line);
                } else {
                    *previous = region.clone();
                }
            })
            .or_insert(region);
    }
}

#[derive(Serialize)]
pub(crate) struct ChangedCodeArea {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) status: &'static str,
}

#[derive(Serialize)]
pub(crate) struct CodeChangesSummary {
    pub(crate) available: bool,
    pub(crate) baseline: &'static str,
    pub(crate) covered: usize,
    pub(crate) uncovered: usize,
    pub(crate) drifted: usize,
    pub(crate) areas: Vec<ChangedCodeArea>,
    pub(crate) truncated: usize,
    pub(crate) hint: Option<&'static str>,
}

pub(super) fn snapshot_code_changes(
    db: &SqliteConnection,
    root: &Path,
    schema: &Schema,
    snapshot: Option<&HistorySnapshot>,
) -> Result<CodeChangesSummary> {
    let mut summary = CodeChangesSummary {
        available: false,
        baseline: "none",
        covered: 0,
        uncovered: 0,
        drifted: 0,
        areas: Vec::new(),
        truncated: 0,
        hint: None,
    };
    let Some(snapshot) = snapshot else {
        summary.hint = Some(
            "No snapshot yet; run history snapshot -m <message> to establish a code baseline.",
        );
        return Ok(summary);
    };
    let graph = CodeGraph::load(db, &read_anchor_lock(root)?)?;
    let regions = if let Some(before) = &snapshot.code_regions {
        summary.baseline = "snapshot-code-fingerprints";
        changed_fingerprints(before, &graph.regions)
    } else if let Some(head) = &snapshot.host_git_head {
        if git_repo_check(root).0 != Some(true) {
            summary.hint = Some("The old snapshot has no code fingerprints and this is not a Git repository; run history snapshot --allow-empty to create a new baseline.");
            return Ok(summary);
        }
        summary.baseline = "legacy-git-head";
        summary.hint =
            Some("The old snapshot has no code fingerprints; falling back to its Git HEAD may include edits that were already uncommitted at snapshot time.");
        match git_changes(root, schema, Some(head)) {
            Ok((_, changes, _)) => changes
                .into_iter()
                .flat_map(|change| change.regions)
                .collect(),
            Err(_) => {
                summary.hint = Some(
                    "The old snapshot Git HEAD is unavailable; run history snapshot --allow-empty to create a new baseline.",
                );
                return Ok(summary);
            }
        }
    } else {
        summary.hint = Some(
            "The old snapshot has no code baseline; run history snapshot --allow-empty -m <message> to create a new snapshot.",
        );
        return Ok(summary);
    };
    summary.available = true;
    for region in regions {
        let status = if graph
            .states
            .iter()
            .any(|state| state.status != "current" && state_affects(state, &region, false))
        {
            summary.drifted += 1;
            "drifted"
        } else if graph
            .documents_for(std::slice::from_ref(&region), false)
            .iter()
            .any(CodeDocument::covers)
        {
            summary.covered += 1;
            "covered"
        } else {
            summary.uncovered += 1;
            "uncovered"
        };
        summary.areas.push(ChangedCodeArea {
            path: region.path,
            name: region.name,
            status,
        });
    }
    summary.truncated = summary.areas.len().saturating_sub(5);
    summary.areas.truncate(5);
    Ok(summary)
}

fn changed_fingerprints(before: &[CodeRegion], after: &[CodeRegion]) -> Vec<CodeRegion> {
    let old = before
        .iter()
        .map(|region| (&region.id, region))
        .collect::<BTreeMap<_, _>>();
    let new = after
        .iter()
        .map(|region| (&region.id, region))
        .collect::<BTreeMap<_, _>>();
    let mut files = BTreeSet::new();
    let mut regions = BTreeMap::new();
    for id in old.keys().chain(new.keys()) {
        if old.get(id).map(|region| &region.fingerprint)
            == new.get(id).map(|region| &region.fingerprint)
        {
            continue;
        }
        let region = new.get(id).or_else(|| old.get(id)).unwrap();
        if region.kind == "symbol" {
            regions.insert(region.id.clone(), (*region).clone());
        }
        files.insert(region.path.clone());
    }
    for path in files {
        if regions.values().any(|region| region.path == path) {
            continue;
        }
        if let Some(region) = after
            .iter()
            .chain(before)
            .find(|region| region.path == path && region.kind == "file")
        {
            regions.insert(region.id.clone(), region.clone());
        }
    }
    regions.into_values().collect()
}
