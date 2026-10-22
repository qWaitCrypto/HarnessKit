use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::checks::{push_check, IndexCheck};
use crate::git::last_commit_times;
use crate::index::IndexedDoc;
use crate::lifecycle::{doc_lifecycle, successor_map};

pub(super) fn add_stale_anchor_checks(
    repo_root: &Path,
    docs: &[IndexedDoc],
    checks: &mut Vec<IndexCheck>,
) {
    let successors = successor_map(docs);
    let candidates = docs
        .iter()
        .filter(|doc| {
            !doc.scope_paths.is_empty()
                && doc.collection.as_deref() != Some("decisions")
                && doc_lifecycle(
                    &doc.path,
                    doc.status.as_deref().unwrap_or(""),
                    &doc.summary,
                    successors.get(&doc.path).map(Vec::as_slice).unwrap_or(&[]),
                ) != "historical"
        })
        .collect::<Vec<_>>();
    let paths = candidates
        .iter()
        .flat_map(|doc| std::iter::once(doc.path.clone()).chain(doc.scope_paths.iter().cloned()))
        .collect::<Vec<_>>();
    let committed = last_commit_times(repo_root, &paths);
    for doc in candidates {
        let mut stale_scopes = Vec::new();
        let doc_time = if let Some(times) = &committed {
            times.get(&doc.path).copied().flatten()
        } else {
            Some(doc.mtime_unix)
        };
        for scope_path in &doc.scope_paths {
            if !repo_root.join(scope_path).exists() {
                push_check(
                    checks,
                    "missing-scope-path",
                    "info",
                    &doc.path,
                    &format!("{} scopes missing path {}", doc.path, scope_path),
                    &[("doc", &doc.path), ("scope_path", scope_path)],
                );
                continue;
            }
            let scoped_time = if let Some(times) = &committed {
                times.get(scope_path).copied().flatten()
            } else {
                newest_existing_path_mtime(&repo_root.join(scope_path))
            };
            if matches!((doc_time, scoped_time), (Some(doc), Some(scope)) if scope > doc) {
                stale_scopes.push(scope_path.clone());
            }
        }
        if !stale_scopes.is_empty() {
            checks.push(IndexCheck {
                rule_id: "stale-explicit-anchor".to_string(),
                severity: if committed.is_some() { "warning" } else { "info" }.to_string(),
                subject_path: doc.path.clone(),
                message: format!("{} may be stale: {} scoped path(s) changed after the document ({})", doc.path, stale_scopes.len(), if committed.is_some() { "git commit times" } else { "mtime fallback; no git" }),
                hint: None, fixable: false,
                evidence_json: serde_json::json!({"doc": doc.path, "scope_paths": stale_scopes,
                    "time_source": if committed.is_some() { "git" } else { "mtime" }, "doc_time_unix": doc_time}).to_string(),
            });
        }
    }
}

pub(crate) fn newest_existing_path_mtime(path: &Path) -> Option<u64> {
    if path.is_file() {
        return file_mtime(path);
    }
    if path.is_dir() {
        return newest_dir_mtime(path);
    }
    None
}

fn newest_dir_mtime(path: &Path) -> Option<u64> {
    let mut newest = file_mtime(path);
    let entries = fs::read_dir(path).ok()?;
    for entry in entries.flatten() {
        let child = entry.path();
        let child_mtime = if child.is_dir() {
            newest_dir_mtime(&child)
        } else {
            file_mtime(&child)
        };
        if let Some(value) = child_mtime {
            newest = Some(newest.map(|current| current.max(value)).unwrap_or(value));
        }
    }
    newest
}

fn file_mtime(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .and_then(|meta| meta.modified().ok())
        .and_then(|mtime| mtime.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
}
