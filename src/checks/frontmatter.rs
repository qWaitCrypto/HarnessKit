use std::collections::BTreeSet;
use std::path::Path;

use crate::checks::{push_check, IndexCheck};
use crate::discover::collection_lookup;
use crate::index::IndexedDoc;
use crate::schema::Schema;

pub(super) fn add_frontmatter_parse_checks(
    root: &Path,
    docs: &[IndexedDoc],
    checks: &mut Vec<IndexCheck>,
) -> crate::Result<()> {
    for doc in docs {
        let text = crate::fs_utils::read_text_lossy(&root.join(&doc.path))?;
        if let Some(error) = crate::parse::frontmatter_error(&text) {
            push_check(
                checks,
                "frontmatter-parse-error",
                "error",
                &doc.path,
                &format!("Cannot parse frontmatter in {}: {}", doc.path, error),
                &[("doc", &doc.path), ("error", &error)],
            );
        }
    }
    Ok(())
}

pub(super) fn add_frontmatter_constraint_checks(
    schema: &Schema,
    docs: &[IndexedDoc],
    checks: &mut Vec<IndexCheck>,
) {
    let collection_by_name = collection_lookup(schema);
    for doc in docs {
        let Some(collection_name) = &doc.collection else {
            continue;
        };
        let Some(spec) = collection_by_name.get(collection_name) else {
            continue;
        };
        let allowed = spec
            .allowed_frontmatter
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        for key in &doc.frontmatter_keys {
            // These additive fields also work with pre-lifecycle project schemas.
            if matches!(key.as_str(), "superseded_by" | "supersedes_parts_of")
                || (key == "status"
                    && (doc.role.starts_with("exec_plans") || doc.role.starts_with("worklog")))
            {
                continue;
            }
            if !allowed.contains(key) {
                push_check(
                    checks,
                    "frontmatter-field-not-allowed",
                    "info",
                    &doc.path,
                    &format!(
                        "{} uses frontmatter field `{}` outside this collection's allowlist",
                        doc.path, key
                    ),
                    &[
                        ("doc", &doc.path),
                        ("field", key),
                        ("collection", collection_name),
                    ],
                );
            }
        }
    }
}
