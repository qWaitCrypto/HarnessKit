use std::collections::BTreeSet;
use std::path::Path;

use serde_json::json;

use crate::checks::{complete_check, IndexCheck};
use crate::index::cell;
use crate::schema::Schema;
use crate::sqlite::SqliteConnection;
use crate::Result;

const DEFAULT_MIN_COMMITS: u32 = 3;
const DEFAULT_MAX_HOTSPOTS: usize = 3;

/// Report frequently cochanged production files that have no document edge.
/// The relation graph is already derived by W4, so this check adds no Git
/// traversal to ordinary `check` invocations.
pub(super) fn add_undocumented_hotspot_checks(
    db: &SqliteConnection,
    schema: &Schema,
    checks: &mut Vec<IndexCheck>,
) -> Result<()> {
    let threshold = schema
        .rules
        .undocumented_hotspot_min_commits
        .unwrap_or(DEFAULT_MIN_COMMITS);
    let limit = schema
        .rules
        .undocumented_hotspot_max
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_MAX_HOTSPOTS);
    if limit == 0 {
        return Ok(());
    }

    let covered = db.query(
        "SELECT DISTINCT target.path
         FROM edges e
         JOIN nodes source ON source.id = e.src
         JOIN nodes target ON target.id = e.dst
         WHERE source.kind IN ('doc', 'section')
           AND e.kind IN ('anchor', 'mention')
           AND target.kind IN ('file', 'symbol');",
        [],
    )?;
    let covered_paths = covered
        .rows
        .iter()
        .map(|row| cell(row, 0).to_string())
        .collect::<BTreeSet<_>>();

    let rows = db.query(
        "SELECT files.path, files.commit_count
         FROM cochange_files files
         JOIN nodes node ON node.kind = 'file' AND node.path = files.path
         WHERE files.commit_count >= ?1
         ORDER BY files.commit_count DESC, files.path ASC;",
        [threshold.to_string()],
    )?;
    for row in rows.rows.iter().filter(|row| !covered_paths.contains(cell(row, 0))) {
        let path = cell(row, 0);
        if excluded_hotspot_path(path) {
            continue;
        }
        let commits = cell(row, 1).parse::<u32>().unwrap_or(0);
        let mut check = IndexCheck {
            rule_id: "undocumented-hotspot".to_string(),
            severity: "info".to_string(),
            subject_path: path.to_string(),
            message: format!(
                "{} changed in {} same-commit history entries but has no document edge.",
                path, commits
            ),
            hint: Some(
                "Review the change history and add a precise code anchor to the relevant decision or active plan with `harnesskit suggest-anchors`.".to_string(),
            ),
            fixable: false,
            evidence_json: json!({
                "path": path,
                "commit_count": commits,
                "threshold": threshold,
                "excluded": false,
                "fixable": false,
            })
            .to_string(),
        };
        complete_check(&mut check);
        checks.push(check);
        if checks
            .iter()
            .filter(|check| check.rule_id == "undocumented-hotspot")
            .count()
            >= limit
        {
            break;
        }
    }
    Ok(())
}

fn excluded_hotspot_path(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.iter().any(|part| {
        matches!(
            *part,
            "test" | "tests" | "__tests__" | "testing" | "fixtures" | "fixture"
                | "generated" | "vendor" | "target" | "build" | "dist"
        )
    }) {
        return true;
    }
    let filename = Path::new(&path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    filename.starts_with("test_")
        || filename.contains(".test.")
        || filename.contains(".spec.")
        || filename.contains(".generated.")
        || filename.ends_with("_generated.rs")
}

