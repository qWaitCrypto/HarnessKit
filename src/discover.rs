use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::fs_utils::normalize_path;
use crate::schema::{configured_architecture_path, core_file_rel_path, DocCollectionSpec, Schema};
use crate::Result;

pub(crate) fn collect_managed_doc_paths(repo_root: &Path, schema: &Schema) -> Result<Vec<String>> {
    let mut paths = Vec::new();

    for path in [
        schema.entrypoints.agents.as_ref(),
        schema.entrypoints.claude.as_ref(),
        schema.entrypoints.architecture.as_ref(),
    ]
    .into_iter()
    .flatten()
    {
        if repo_root.join(path).is_file() {
            paths.push(path.clone());
        }
    }

    for spec in &schema.core_files {
        let rel_path = if let Some(root_path) = &spec.root_path {
            root_path.clone()
        } else {
            format!("{}/{}", schema.managed_root, spec.path)
        };
        if repo_root.join(&rel_path).is_file() {
            paths.push(rel_path);
        }
    }

    for spec in &schema.doc_collections {
        let dir = repo_root.join(&schema.managed_root).join(&spec.path);
        if !dir.exists() {
            continue;
        }
        collect_markdown_files(repo_root, &dir, &mut paths)?;
    }

    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn collect_markdown_files(repo_root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_files(repo_root, &path, out)?;
            continue;
        }
        if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            let rel = path
                .strip_prefix(repo_root)
                .map(normalize_path)
                .unwrap_or_else(|_| normalize_path(&path));
            out.push(rel);
        }
    }
    Ok(())
}

pub(crate) fn collection_lookup(schema: &Schema) -> BTreeMap<String, &DocCollectionSpec> {
    let mut map = BTreeMap::new();
    for spec in &schema.doc_collections {
        map.insert(spec.name.clone(), spec);
    }
    map
}

pub(crate) fn collection_name_for_path(schema: &Schema, path: &str) -> Option<String> {
    for spec in &schema.doc_collections {
        let prefix = format!(
            "{}/{}/",
            schema.managed_root,
            spec.path.trim_end_matches('/')
        );
        if path.starts_with(&prefix) {
            return Some(spec.name.clone());
        }
    }
    None
}

pub(crate) fn infer_index_role(schema: &Schema, path: &str, collection: Option<&str>) -> String {
    if is_entrypoint_path(schema, path) {
        "entrypoint".to_string()
    } else if configured_architecture_path(schema).as_deref() == Some(path) {
        "architecture".to_string()
    } else if path == schema.entrypoints.manifest.as_deref().unwrap_or("") {
        "manifest".to_string()
    } else if path.ends_with("/index.md") {
        "collection-index".to_string()
    } else {
        collection.unwrap_or("doc").to_string()
    }
}

pub(crate) fn is_entrypoint_path(schema: &Schema, path: &str) -> bool {
    schema.entrypoints.agents.as_deref() == Some(path)
        || schema.entrypoints.claude.as_deref() == Some(path)
        || schema.entrypoints.architecture.as_deref() == Some(path)
}

pub(crate) fn is_anchor_candidate(schema: &Schema, path: &str) -> bool {
    if is_entrypoint_path(schema, path) {
        return true;
    }
    if path == schema.entrypoints.manifest.as_deref().unwrap_or("") {
        return true;
    }
    if path == schema.entrypoints.project_map.as_deref().unwrap_or("") {
        return true;
    }
    if schema
        .core_files
        .iter()
        .any(|spec| core_file_rel_path(schema, spec) == path)
    {
        return true;
    }
    collection_name_for_path(schema, path)
        .and_then(|name| schema.doc_collections.iter().find(|spec| spec.name == name))
        .map(|spec| spec.anchor_candidate)
        .unwrap_or(false)
}
