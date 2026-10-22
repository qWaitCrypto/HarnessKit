use std::collections::BTreeSet;

use crate::checks::{push_check, IndexCheck};
use crate::discover::collection_lookup;
use crate::index::IndexedDoc;
use crate::schema::Schema;

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
