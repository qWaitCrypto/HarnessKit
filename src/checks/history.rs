use std::path::Path;

use crate::checks::{push_check, IndexCheck};
use crate::git::{find_host_git_dir, ignored_paths, tracked_doc_paths};
use crate::history::{
    compute_history_status, has_history_doc_changes, history_dir, latest_history_snapshot_id,
};
use crate::index::IndexedDoc;
use crate::schema::Schema;

pub(super) fn add_history_checks(
    schema: &Schema,
    repo_root: &Path,
    docs: &[IndexedDoc],
    checks: &mut Vec<IndexCheck>,
) -> crate::Result<()> {
    // Merely inspecting an inferred layout does not opt it into local isolation
    // or mandatory checkpoints. Existing history is still checked normally.
    if !schema.inferred_layout && find_host_git_dir(repo_root).is_some() {
        let paths = docs.iter().map(|doc| doc.path.clone()).collect::<Vec<_>>();
        if tracked_doc_paths(repo_root, &paths)?.is_empty() {
            let mut expected = paths;
            expected.push(".harnesskit/".to_string());
            let ignored = ignored_paths(repo_root, &expected)?;
            for path in expected.iter().filter(|path| !ignored.contains(path)) {
                push_check(
                    checks,
                    "managed-file-not-excluded",
                    "warning",
                    path,
                    &format!(
                        "{} is neither tracked nor covered by git ignore rules",
                        path
                    ),
                    &[("path", path)],
                );
            }
        }
    }
    if schema.inferred_layout && !history_dir(repo_root).exists() {
        return Ok(());
    }

    if !history_dir(repo_root).exists() || latest_history_snapshot_id(repo_root).is_none() {
        push_check(
            checks,
            "history-missing",
            "warning",
            ".harnesskit/history",
            "no HarnessKit history snapshot exists",
            &[("path", ".harnesskit/history")],
        );
        return Ok(());
    }

    if let Ok(status) = compute_history_status(repo_root, schema) {
        if has_history_doc_changes(&status)
            && (status
                .snapshot_age_seconds
                .is_some_and(|age| age > 7 * 86_400)
                || status.changed_files_count > 20)
        {
            push_check(
                checks,
                "history-dirty-since-snapshot",
                "warning",
                ".harnesskit/history",
                "managed docs differ from a snapshot older than 7 days or exceed 20 changed files",
                &[
                    (
                        "latest_snapshot",
                        status.latest_snapshot.as_deref().unwrap_or(""),
                    ),
                    ("added", &status.added.len().to_string()),
                    ("changed", &status.changed.len().to_string()),
                    ("removed", &status.removed.len().to_string()),
                    (
                        "days_since_snapshot",
                        &status.days_since_snapshot.unwrap_or(0).to_string(),
                    ),
                    (
                        "changed_files_count",
                        &status.changed_files_count.to_string(),
                    ),
                ],
            );
        }
        if status.schema_changed {
            push_check(
                checks,
                "history-schema-not-snapshotted",
                "warning",
                ".harnesskit/schema.yaml",
                "HarnessKit schema differs from latest history snapshot",
                &[("path", ".harnesskit/schema.yaml")],
            );
        }
    }
    Ok(())
}
