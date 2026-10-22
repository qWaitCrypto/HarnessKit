use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::checks::{push_check, IndexCheck};
use crate::discover::{discover_docs, include_hint, is_ignored, is_markdown};
use crate::external_refs::resolve_mention;
use crate::index::IndexedDoc;
use crate::schema::Schema;
use crate::Result;

pub(super) fn add_coverage_checks(
    schema: &Schema,
    root: &Path,
    docs: &[IndexedDoc],
    checks: &mut Vec<IndexCheck>,
) -> Result<()> {
    let coverage = discover_docs(root, schema)?;
    let mut unmanaged = BTreeMap::<String, BTreeSet<String>>::new();
    let managed = docs
        .iter()
        .map(|doc| doc.path.as_str())
        .collect::<BTreeSet<_>>();
    let excluded = coverage
        .mappings
        .iter()
        .filter(|item| item.status == "excluded")
        .map(|item| item.path.as_str())
        .collect::<BTreeSet<_>>();
    let mut add_unmanaged = |path: &str| {
        let dir = Path::new(path)
            .parent()
            .and_then(|dir| dir.to_str())
            .filter(|dir| !dir.is_empty())
            .unwrap_or(".");
        unmanaged
            .entry(dir.to_string())
            .or_default()
            .insert(path.to_string());
    };
    for mapping in &coverage.mappings {
        if mapping.status == "unmanaged" {
            add_unmanaged(&mapping.path);
        }
    }
    for doc in docs {
        for link in &doc.links {
            if !root.join(link).exists() {
                push_check(
                    checks,
                    "broken-link",
                    "error",
                    &doc.path,
                    &format!(
                        "{} links to missing target {}; fix or remove this link.",
                        doc.path, link
                    ),
                    &[("source", &doc.path), ("target", link)],
                );
            }
        }
        for path in doc.links.iter().cloned().chain(
            doc.path_mentions
                .iter()
                .filter_map(|mention| resolve_mention(root, &doc.path, mention)),
        ) {
            if is_markdown(&path)
                && root.join(&path).is_file()
                && !managed.contains(path.as_str())
                && !excluded.contains(path.as_str())
                && !path
                    .split('/')
                    .any(|part| part.starts_with('.') || part == "templates")
                && !is_ignored(schema, &path)
            {
                add_unmanaged(&path);
            }
        }
    }
    for (dir, paths) in unmanaged {
        let hint = include_hint(&dir);
        checks.push(IndexCheck {
            rule_id: "unmanaged-docs".to_string(), severity: "warning".to_string(),
            subject_path: dir.clone(),
            message: format!("{} Markdown file(s) in {} are not managed. Merge this mapping into .harnesskit/schema.yaml:\n{}", paths.len(), dir, hint),
            evidence_json: serde_json::json!({"directory": dir, "paths": paths, "hint": hint}).to_string(),
        });
    }
    Ok(())
}
