use std::fs;
use std::path::Path;
use std::time::UNIX_EPOCH;

use crate::checks::{push_check, IndexCheck};
use crate::index::IndexedDoc;

pub(super) fn add_stale_anchor_checks(
    repo_root: &Path,
    docs: &[IndexedDoc],
    checks: &mut Vec<IndexCheck>,
) {
    for doc in docs {
        for scope_path in &doc.scope_paths {
            let scoped_mtime = newest_existing_path_mtime(&repo_root.join(scope_path));
            if let Some(scoped_mtime) = scoped_mtime {
                if scoped_mtime > doc.mtime_unix {
                    push_check(
                        checks,
                        "stale-explicit-anchor",
                        "warning",
                        &doc.path,
                        &format!(
                            "{} may be stale: scoped path {} is newer than the doc",
                            doc.path, scope_path
                        ),
                        &[
                            ("doc", &doc.path),
                            ("scope_path", scope_path),
                            ("doc_mtime_unix", &doc.mtime_unix.to_string()),
                            ("scope_mtime_unix", &scoped_mtime.to_string()),
                        ],
                    );
                }
            } else {
                push_check(
                    checks,
                    "missing-scope-path",
                    "info",
                    &doc.path,
                    &format!("{} scopes missing path {}", doc.path, scope_path),
                    &[("doc", &doc.path), ("scope_path", scope_path)],
                );
            }
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
