use std::collections::{BTreeMap, BTreeSet};

use crate::checks::{push_check, IndexCheck};
use crate::index::{is_navigation_doc, DocRelation, IndexedDoc};
use crate::schema::Schema;

fn add_manifest_constraint_checks(
    schema: &Schema,
    docs: &[IndexedDoc],
    relations: &[DocRelation],
    doc_map: &BTreeMap<String, &IndexedDoc>,
    checks: &mut Vec<IndexCheck>,
) {
    let manifest_path = schema.entrypoints.manifest.as_deref().unwrap_or("");
    if manifest_path.is_empty() || !doc_map.contains_key(manifest_path) {
        return;
    }

    let manifest_links = relations
        .iter()
        .filter(|relation| relation.src_path == manifest_path && relation.dst_path.ends_with(".md"))
        .map(|relation| relation.dst_path.clone())
        .collect::<BTreeSet<_>>();

    for doc in docs {
        if doc.path == manifest_path
            || doc.is_entrypoint
            || doc
                .path
                .starts_with(&format!("{}/templates/", schema.managed_root))
        {
            continue;
        }
        let is_core = schema.core_files.iter().any(|spec| {
            let expected = spec
                .root_path
                .clone()
                .unwrap_or_else(|| format!("{}/{}", schema.managed_root, spec.path));
            expected == doc.path
        });
        let should_be_manifested = is_core || doc.path.ends_with("/index.md");
        if should_be_manifested && !manifest_links.contains(&doc.path) {
            push_check(
                checks,
                "manifest-missing-doc",
                "warning",
                &doc.path,
                &format!("{} is not linked from {}", doc.path, manifest_path),
                &[("manifest", manifest_path), ("missing_doc", &doc.path)],
            );
        }
    }

    for doc in docs.iter().filter(|doc| is_navigation_doc(doc)) {
        for link in &doc.links {
            if link.ends_with(".md") && !doc_map.contains_key(link) {
                push_check(
                    checks,
                    "unresolved-managed-link",
                    "warning",
                    &doc.path,
                    &format!("{} links to missing managed doc {}", doc.path, link),
                    &[("source", &doc.path), ("target", link)],
                );
            }
            if link.starts_with(&format!("{}/", schema.managed_root)) && !link.ends_with(".md") {
                push_check(
                    checks,
                    "wrong-docs-navigation-target",
                    "info",
                    &doc.path,
                    &format!("{} navigates to non-document target {}", doc.path, link),
                    &[("source", &doc.path), ("target", link)],
                );
            }
        }
    }
}

pub(super) fn build_manifest_checks(
    schema: &Schema,
    docs: &[IndexedDoc],
    relations: &[DocRelation],
) -> Vec<IndexCheck> {
    let mut checks = Vec::new();
    let doc_map: BTreeMap<String, &IndexedDoc> =
        docs.iter().map(|doc| (doc.path.clone(), doc)).collect();
    let manifest_path = schema
        .entrypoints
        .manifest
        .clone()
        .unwrap_or_else(|| format!("{}/index.md", schema.managed_root));

    for (entrypoint_name, entrypoint_path) in [
        ("agents", schema.entrypoints.agents.as_deref()),
        ("claude", schema.entrypoints.claude.as_deref()),
        ("architecture", schema.entrypoints.architecture.as_deref()),
    ] {
        if let Some(path) = entrypoint_path {
            if !doc_map.contains_key(path) {
                push_check(
                    &mut checks,
                    "missing-entrypoint-doc",
                    "warning",
                    path,
                    &format!(
                        "configured {} entrypoint is missing: {}",
                        entrypoint_name, path
                    ),
                    &[("entrypoint", entrypoint_name), ("path", path)],
                );
            }
        }
    }

    for spec in &schema.core_files {
        let rel_path = if let Some(root_path) = &spec.root_path {
            root_path.clone()
        } else {
            format!("{}/{}", schema.managed_root, spec.path)
        };
        if spec.required && !doc_map.contains_key(&rel_path) {
            push_check(
                &mut checks,
                "missing-required-doc",
                "warning",
                &rel_path,
                &format!("required doc is missing: {}", rel_path),
                &[("path", &rel_path), ("schema_item", &spec.name)],
            );
        }
    }

    if schema.rules.index_required.unwrap_or(false) {
        for spec in &schema.doc_collections {
            let collection_index_path = format!("{}/{}/index.md", schema.managed_root, spec.path);
            if !doc_map.contains_key(&collection_index_path) {
                push_check(
                    &mut checks,
                    "missing-collection-index",
                    "warning",
                    &collection_index_path,
                    &format!("collection index is missing: {}", collection_index_path),
                    &[("collection", &spec.name), ("path", &collection_index_path)],
                );
            }
        }
    }

    for path in [
        schema.entrypoints.agents.as_deref(),
        schema.entrypoints.claude.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        if doc_map.contains_key(path) {
            let points_to_manifest = relations
                .iter()
                .any(|relation| relation.src_path == path && relation.dst_path == manifest_path);
            if !points_to_manifest {
                push_check(
                    &mut checks,
                    "entrypoint-manifest-link",
                    "warning",
                    path,
                    &format!("{} does not point to {}", path, manifest_path),
                    &[("entrypoint", path), ("manifest", &manifest_path)],
                );
            }
        }
    }

    if schema.rules.index_required.unwrap_or(false) {
        add_manifest_constraint_checks(schema, docs, relations, &doc_map, &mut checks);
    }

    for doc in docs
        .iter()
        .filter(|doc| doc.collection.is_some() && !doc.path.ends_with("/index.md"))
    {
        if doc.incoming_links == 0 {
            push_check(
                &mut checks,
                "orphan-managed-doc",
                "info",
                &doc.path,
                &format!("{} has no incoming managed-doc navigation edges", doc.path),
                &[("path", &doc.path)],
            );
        }
    }

    checks
}
