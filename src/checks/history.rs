use std::fs;
use std::path::Path;

use crate::checks::{push_check, IndexCheck};
use crate::git::find_host_git_dir;
use crate::history::{
    compute_history_status, has_history_doc_changes, history_dir, latest_history_snapshot_id,
};
use crate::schema::{managed_memory_paths, Schema};

pub(super) fn add_history_checks(schema: &Schema, repo_root: &Path, checks: &mut Vec<IndexCheck>) {
    let expected_excludes = managed_memory_paths(schema);
    match find_host_git_dir(repo_root) {
        Some(git_dir) => {
            let exclude_path = git_dir.join("info").join("exclude");
            let exclude_text = fs::read_to_string(exclude_path).unwrap_or_default();
            if exclude_text.trim().is_empty() {
                push_check(
                    checks,
                    "host-git-exclude-missing",
                    "warning",
                    ".git/info/exclude",
                    "host git local exclude does not contain HarnessKit isolation rules",
                    &[("path", ".git/info/exclude")],
                );
            }
            for path in expected_excludes {
                if !exclude_text.lines().any(|line| line.trim() == path) {
                    push_check(
                        checks,
                        "managed-file-not-excluded",
                        "warning",
                        &path,
                        &format!("{} is not listed in host git local exclude", path),
                        &[("path", &path)],
                    );
                }
            }
        }
        None => {
            push_check(
                checks,
                "host-git-exclude-missing",
                "info",
                ".git/info/exclude",
                "host git repo not found; HarnessKit cannot install local exclude rules",
                &[("path", ".git/info/exclude")],
            );
        }
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
        return;
    }

    if let Ok(status) = compute_history_status(repo_root, schema) {
        if has_history_doc_changes(&status) {
            push_check(
                checks,
                "history-dirty-since-snapshot",
                "warning",
                ".harnesskit/history",
                "managed docs differ from latest HarnessKit history snapshot",
                &[
                    (
                        "latest_snapshot",
                        status.latest_snapshot.as_deref().unwrap_or(""),
                    ),
                    ("added", &status.added.len().to_string()),
                    ("changed", &status.changed.len().to_string()),
                    ("removed", &status.removed.len().to_string()),
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
}
