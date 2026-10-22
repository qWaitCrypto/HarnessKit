use std::collections::BTreeMap;

use crate::checks::{push_check, IndexCheck};
use crate::index::IndexedDoc;

pub(super) fn add_duplicate_checks(docs: &[IndexedDoc], checks: &mut Vec<IndexCheck>) {
    let mut by_hash = BTreeMap::<String, Vec<&IndexedDoc>>::new();
    let mut by_fingerprint = BTreeMap::<String, Vec<&IndexedDoc>>::new();

    for doc in docs {
        by_hash
            .entry(doc.content_hash.clone())
            .or_default()
            .push(doc);
        let fingerprint = doc_structure_fingerprint(doc);
        if !fingerprint.is_empty() {
            by_fingerprint.entry(fingerprint).or_default().push(doc);
        }
    }

    for group in by_hash.values().filter(|group| group.len() > 1) {
        let paths = group
            .iter()
            .map(|doc| doc.path.clone())
            .collect::<Vec<_>>()
            .join(" | ");
        for doc in group {
            push_check(
                checks,
                "duplicate-content",
                "warning",
                &doc.path,
                &format!("{} has identical content with {}", doc.path, paths),
                &[("paths", &paths), ("content_hash", &doc.content_hash)],
            );
        }
    }

    for group in by_fingerprint.values().filter(|group| group.len() > 1) {
        let paths = group
            .iter()
            .map(|doc| doc.path.clone())
            .collect::<Vec<_>>()
            .join(" | ");
        for doc in group {
            push_check(
                checks,
                "overlap-heading-fingerprint",
                "info",
                &doc.path,
                &format!(
                    "{} has a similar title/headings structure with {}",
                    doc.path, paths
                ),
                &[
                    ("paths", &paths),
                    ("fingerprint", &doc_structure_fingerprint(doc)),
                ],
            );
        }
    }
}

fn doc_structure_fingerprint(doc: &IndexedDoc) -> String {
    let title = normalize_fingerprint_text(&doc.title);
    let headings = doc
        .headings
        .iter()
        .take(6)
        .map(|heading| normalize_fingerprint_text(heading))
        .filter(|heading| !heading.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    if title.is_empty() || headings.is_empty() {
        String::new()
    } else {
        format!("{}::{}", title, headings)
    }
}

fn normalize_fingerprint_text(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
