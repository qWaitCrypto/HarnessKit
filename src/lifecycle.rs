use std::collections::{BTreeMap, BTreeSet};

use crate::index::IndexedDoc;

pub(crate) const ACCEPTED_DECISION_CHANGED: &str =
    "accepted decision changed — confirm with the user / supersede instead";

pub(crate) fn path_status(path: &str) -> Option<&'static str> {
    // A historical directory wins if a nested path also contains `active`.
    let parts = path
        .split('/')
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    if parts.iter().any(|part| part == "completed") {
        Some("completed")
    } else if parts
        .iter()
        .any(|part| matches!(part.as_str(), "archive" | "archived"))
    {
        Some("archived")
    } else if parts
        .iter()
        .any(|part| matches!(part.as_str(), "historical" | "history" | "superseded"))
    {
        Some("historical")
    } else if parts.iter().any(|part| part == "active") {
        Some("active")
    } else {
        None
    }
}

pub(crate) fn banner_status(summary: &str) -> Option<&'static str> {
    let lower = summary.to_lowercase();
    if ["superseded", "已被取代", "已被新路线取代", "不再适用"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        Some("superseded")
    } else if ["历史", "history only"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        Some("historical")
    } else {
        None
    }
}

pub(crate) fn infer_status(
    explicit: Option<&str>,
    path: &str,
    summary: &str,
    default: Option<&str>,
) -> Option<String> {
    explicit
        .or_else(|| path_status(path))
        .or_else(|| banner_status(summary))
        .or(default)
        .map(str::to_string)
}

pub(crate) fn historical_status(status: &str) -> bool {
    matches!(
        status.to_ascii_lowercase().as_str(),
        "superseded"
            | "completed"
            | "complete"
            | "done"
            | "closed"
            | "finished"
            | "archive"
            | "archived"
            | "historical"
    )
}

pub(crate) fn doc_lifecycle(
    path: &str,
    status: &str,
    summary: &str,
    superseded_by: &[String],
) -> String {
    // Keep S5's conservative retrieval behavior even when explicit status is active.
    let historical = !superseded_by.is_empty()
        || historical_status(status)
        || path_status(path).map(historical_status).unwrap_or(false)
        || banner_status(summary).is_some();
    if historical { "historical" } else { "current" }.to_string()
}

pub(crate) fn is_active_plan(doc: &IndexedDoc, successors: &[String]) -> bool {
    doc.role.starts_with("exec_plans")
        && doc
            .status
            .as_deref()
            .map(|status| status.eq_ignore_ascii_case("active"))
            .unwrap_or(false)
        && doc_lifecycle(
            &doc.path,
            doc.status.as_deref().unwrap_or(""),
            &doc.summary,
            successors,
        ) == "current"
}

pub(crate) fn successor_map(docs: &[IndexedDoc]) -> BTreeMap<String, Vec<String>> {
    let mut successors = BTreeMap::<String, BTreeSet<String>>::new();
    for doc in docs {
        successors
            .entry(doc.path.clone())
            .or_default()
            .extend(doc.superseded_by.iter().cloned());
        for old in &doc.supersedes {
            successors
                .entry(old.clone())
                .or_default()
                .insert(doc.path.clone());
        }
    }
    successors
        .into_iter()
        .map(|(path, next)| (path, next.into_iter().collect()))
        .collect()
}
