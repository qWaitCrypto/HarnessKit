use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::checks::{push_check, IndexCheck};
use crate::discover::{discover_docs, include_hint};
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
    }
    for (dir, paths) in unmanaged {
        let hint = include_hint(&dir);
        checks.push(IndexCheck {
            rule_id: "unmanaged-docs".to_string(), severity: "warning".to_string(),
            subject_path: dir.clone(),
            hint: None,
            fixable: false,
            message: format!("{} Markdown file(s) in {} are not managed. Merge the suggested include mapping into .harnesskit/schema.yaml.", paths.len(), dir),
            evidence_json: serde_json::json!({"directory": dir, "paths": paths, "hint": hint}).to_string(),
        });
    }
    Ok(())
}
