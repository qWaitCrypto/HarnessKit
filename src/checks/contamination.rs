use std::collections::BTreeMap;

use crate::checks::{push_check, IndexCheck};
use crate::index::{DocRelation, IndexedDoc};

pub(super) fn add_contamination_checks(
    docs: &[IndexedDoc],
    relations: &[DocRelation],
    checks: &mut Vec<IndexCheck>,
) {
    let doc_map: BTreeMap<String, &IndexedDoc> =
        docs.iter().map(|doc| (doc.path.clone(), doc)).collect();
    for relation in relations {
        let Some(src) = doc_map.get(&relation.src_path) else {
            continue;
        };
        let Some(dst) = doc_map.get(&relation.dst_path) else {
            continue;
        };
        if src.authority.as_deref() == Some("canonical") && (dst.is_generated || dst.is_reference) {
            let rule = if dst.is_generated {
                "canonical-links-generated"
            } else {
                "canonical-links-reference"
            };
            push_check(
                checks,
                rule,
                "warning",
                &src.path,
                &format!(
                    "canonical doc {} links to non-source doc {}",
                    src.path, dst.path
                ),
                &[
                    ("source", &src.path),
                    ("target", &dst.path),
                    ("relation_type", &relation.relation_type),
                ],
            );
        }
    }
}
