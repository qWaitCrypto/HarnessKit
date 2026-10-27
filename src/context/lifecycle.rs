use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;

use crate::discover::is_markdown;
use crate::history::{
    diff_snapshot_to_worktree, latest_history_snapshot_id, resolve_snapshot_arg, HistorySnapshot,
};
use crate::index::{read_index_artifact_from_fact_store, IndexedDoc};
use crate::lifecycle::{doc_lifecycle, is_active_plan, successor_map};
use crate::parse::markdown_prose_lines;
use crate::schema::Schema;
use crate::sqlite::SqliteConnection;
use crate::Result;

#[derive(Serialize)]
pub(crate) struct ContextDoc {
    pub(crate) path: String,
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) status: Option<String>,
    pub(crate) lifecycle: String,
    pub(crate) mtime_unix: u64,
}

impl ContextDoc {
    fn new(doc: &IndexedDoc, successors: &[String]) -> Self {
        Self {
            path: doc.path.clone(),
            title: doc.title.clone(),
            summary: doc.summary.clone(),
            status: doc.status.clone(),
            lifecycle: doc_lifecycle(
                &doc.path,
                doc.status.as_deref().unwrap_or(""),
                &doc.summary,
                successors,
            ),
            mtime_unix: doc.mtime_unix,
        }
    }
}

#[derive(Serialize)]
pub(crate) struct SnapshotPointer {
    pub(crate) id: String,
    pub(crate) created_at_unix: u64,
    pub(crate) message: String,
}

impl From<&HistorySnapshot> for SnapshotPointer {
    fn from(snapshot: &HistorySnapshot) -> Self {
        Self {
            id: snapshot.id.clone(),
            created_at_unix: snapshot.created_at_unix,
            message: snapshot.message.clone(),
        }
    }
}

#[derive(Serialize)]
pub(crate) struct ContextChanges {
    pub(crate) added: Vec<String>,
    pub(crate) modified: Vec<String>,
    pub(crate) removed: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct UnverifiedDoc {
    pub(crate) path: String,
    pub(crate) count: usize,
}

#[derive(Serialize)]
pub(crate) struct CurrentContext {
    pub(crate) kind: &'static str,
    pub(crate) entrypoints: Vec<ContextDoc>,
    pub(crate) project_map: Option<ContextDoc>,
    pub(crate) active_plans: Vec<ContextDoc>,
    pub(crate) recent_decisions: Vec<ContextDoc>,
    pub(crate) latest_snapshot: Option<SnapshotPointer>,
    pub(crate) changes_since_snapshot: Option<ContextChanges>,
    pub(crate) code_changes_since_snapshot: super::CodeChangesSummary,
    pub(crate) unverified_items: usize,
    pub(crate) unverified_documents: Vec<UnverifiedDoc>,
}

pub(crate) fn build_current_context(
    db: &SqliteConnection,
    root: &Path,
    schema: &Schema,
) -> Result<CurrentContext> {
    let artifact = read_index_artifact_from_fact_store(db)?;
    let successors = successor_map(&artifact.docs);
    let next = |doc: &IndexedDoc| successors.get(&doc.path).map(Vec::as_slice).unwrap_or(&[]);
    let current = artifact
        .docs
        .iter()
        .filter(|doc| {
            doc_lifecycle(
                &doc.path,
                doc.status.as_deref().unwrap_or(""),
                &doc.summary,
                next(doc),
            ) == "current"
        })
        .collect::<Vec<_>>();
    let entrypoints = [
        &schema.entrypoints.agents,
        &schema.entrypoints.claude,
        &schema.entrypoints.architecture,
        &schema.entrypoints.manifest,
    ]
    .into_iter()
    .flatten()
    .filter_map(|path| current.iter().find(|doc| doc.path == *path))
    .map(|doc| ContextDoc::new(doc, next(doc)))
    .collect();
    let project_map = current
        .iter()
        .find(|doc| schema.entrypoints.project_map.as_deref() == Some(&doc.path))
        .map(|doc| ContextDoc::new(doc, next(doc)));
    let mut plans = current
        .iter()
        .copied()
        .filter(|doc| is_active_plan(doc, next(doc)))
        .collect::<Vec<_>>();
    sort_by_modified(&mut plans);
    let active_plans = plans
        .into_iter()
        .map(|doc| ContextDoc::new(doc, next(doc)))
        .collect();
    let mut decisions = current
        .iter()
        .copied()
        .filter(|doc| doc.role == "decisions")
        .collect::<Vec<_>>();
    sort_by_modified(&mut decisions);
    let recent_decisions = decisions
        .into_iter()
        .take(5)
        .map(|doc| ContextDoc::new(doc, next(doc)))
        .collect();
    let mut unverified_documents = Vec::new();
    for doc in &current {
        let count = unchecked_items(&doc.content_text);
        if count > 0 {
            unverified_documents.push(UnverifiedDoc {
                path: doc.path.clone(),
                count,
            });
        }
    }
    let snapshot = latest_history_snapshot_id(root)
        .map(|id| resolve_snapshot_arg(root, &id))
        .transpose()?;
    let changes_since_snapshot = snapshot
        .as_ref()
        .map(|snapshot| snapshot_changes(root, schema, snapshot))
        .transpose()?;
    Ok(CurrentContext {
        kind: "current",
        entrypoints,
        project_map,
        active_plans,
        recent_decisions,
        latest_snapshot: snapshot.as_ref().map(SnapshotPointer::from),
        changes_since_snapshot,
        code_changes_since_snapshot: super::changes::snapshot_code_changes(
            db,
            root,
            schema,
            snapshot.as_ref(),
        )?,
        unverified_items: unverified_documents.iter().map(|doc| doc.count).sum(),
        unverified_documents,
    })
}

fn sort_by_modified(docs: &mut [&IndexedDoc]) {
    docs.sort_by(|a, b| {
        b.mtime_ns
            .cmp(&a.mtime_ns)
            .then_with(|| a.path.cmp(&b.path))
    });
}

fn unchecked_items(body: &str) -> usize {
    markdown_prose_lines(body)
        .into_iter()
        .filter(|line| {
            let line = line.trim_start().trim_start_matches('>').trim_start();
            let task = ["- ", "* ", "+ "]
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))
                .or_else(|| {
                    line.split_once(' ').and_then(|(number, rest)| {
                        let digits = number
                            .strip_suffix('.')
                            .or_else(|| number.strip_suffix(')'))?;
                        (!digits.is_empty() && digits.chars().all(|ch| ch.is_ascii_digit()))
                            .then_some(rest)
                    })
                });
            task.map(|text| text.starts_with("[ ] ") || text.trim_end() == "[ ]")
                .unwrap_or(false)
        })
        .count()
}

fn snapshot_changes(
    root: &Path,
    schema: &Schema,
    snapshot: &HistorySnapshot,
) -> Result<ContextChanges> {
    let diff = diff_snapshot_to_worktree(root, schema, snapshot)?;
    let markdown_only =
        |paths: Vec<String>| paths.into_iter().filter(|path| is_markdown(path)).collect();
    Ok(ContextChanges {
        added: markdown_only(diff.added),
        modified: markdown_only(diff.changed),
        removed: markdown_only(diff.removed),
    })
}

#[derive(Serialize)]
pub(crate) struct HandoffContext {
    pub(crate) kind: &'static str,
    pub(crate) since: String,
    pub(crate) cutoff_unix: Option<u64>,
    pub(crate) snapshot: Option<SnapshotPointer>,
    pub(crate) added: Vec<ContextDoc>,
    pub(crate) modified: Vec<ContextDoc>,
    pub(crate) removed: Vec<String>,
}

pub(crate) fn build_handoff_context(
    db: &SqliteConnection,
    root: &Path,
    schema: &Schema,
    since: &str,
) -> Result<HandoffContext> {
    let artifact = read_index_artifact_from_fact_store(db)?;
    let successors = successor_map(&artifact.docs);
    let cutoff_unix = date_cutoff(since)?;
    let snapshot = if cutoff_unix.is_some() {
        None
    } else {
        Some(resolve_snapshot_arg(root, since)?)
    };
    let changes = if let Some(snapshot) = &snapshot {
        snapshot_changes(root, schema, snapshot)?
    } else {
        ContextChanges {
            added: Vec::new(),
            modified: artifact
                .docs
                .iter()
                .filter(|doc| doc.mtime_unix >= cutoff_unix.unwrap())
                .map(|doc| doc.path.clone())
                .collect(),
            removed: Vec::new(),
        }
    };
    let docs = artifact
        .docs
        .iter()
        .map(|doc| (doc.path.as_str(), doc))
        .collect::<BTreeMap<_, _>>();
    let describe = |paths: &[String]| {
        let mut selected = paths
            .iter()
            .filter_map(|path| docs.get(path.as_str()).copied())
            .collect::<Vec<_>>();
        sort_by_modified(&mut selected);
        selected
            .into_iter()
            .map(|doc| {
                ContextDoc::new(
                    doc,
                    successors.get(&doc.path).map(Vec::as_slice).unwrap_or(&[]),
                )
            })
            .collect()
    };
    Ok(HandoffContext {
        kind: "since",
        since: since.to_string(),
        cutoff_unix,
        snapshot: snapshot.as_ref().map(SnapshotPointer::from),
        added: describe(&changes.added),
        modified: describe(&changes.modified),
        removed: changes.removed,
    })
}

fn date_cutoff(value: &str) -> Result<Option<u64>> {
    if value.len() != 10 || value.as_bytes()[4] != b'-' || value.as_bytes()[7] != b'-' {
        return Ok(None);
    }
    let invalid = || {
        format!(
            "invalid date `{}`; use YYYY-MM-DD (UTC, year 1970 or later)",
            value
        )
    };
    if !value.is_ascii() {
        return Err(invalid().into());
    }
    let year = value[..4].parse::<u32>().map_err(|_| invalid())?;
    let month = value[5..7].parse::<usize>().map_err(|_| invalid())?;
    let day = value[8..].parse::<u32>().map_err(|_| invalid())?;
    let leap = |year: u32| year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let months = [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if year < 1970 || !(1..=12).contains(&month) || day == 0 || day > months[month - 1] {
        return Err(invalid().into());
    }
    let years: u64 = (1970..year)
        .map(|year| if leap(year) { 366 } else { 365 })
        .sum();
    let days = years
        + months[..month - 1]
            .iter()
            .map(|days| u64::from(*days))
            .sum::<u64>()
        + u64::from(day - 1);
    Ok(Some(days * 86400))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_validate_leap_days_and_use_utc_midnight() {
        assert_eq!(date_cutoff("1970-01-01").unwrap(), Some(0));
        assert_eq!(date_cutoff("2000-02-29").unwrap(), Some(951782400));
        assert_eq!(
            date_cutoff("2100-03-01").unwrap().unwrap()
                - date_cutoff("2100-02-28").unwrap().unwrap(),
            86400
        );
        for date in [
            "2026-02-29",
            "2100-02-29",
            "2026-13-01",
            "2026-00-01",
            "2026-01-00",
            "1969-12-31",
        ] {
            assert!(date_cutoff(date).is_err(), "{}", date);
        }
        assert_eq!(date_cutoff("latest").unwrap(), None);
    }

    #[test]
    fn unchecked_counts_skip_fenced_examples_and_checked_tasks() {
        assert_eq!(unchecked_items("- [ ] first\n  * [ ] nested\n1. [ ] ordered\n+ [x] done\nprose [ ]\n```md\n- [ ] example\n```\n~~~\n- [ ] example\n~~~\n"), 3);
    }
}
