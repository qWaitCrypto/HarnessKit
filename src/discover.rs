use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::fs_utils::normalize_path;
use crate::schema::{
    configured_architecture_path, core_file_rel_path, parse_schema, DocCollectionSpec, Schema,
    BUNDLED_SCHEMA_TEXT,
};
use crate::Result;

pub(crate) fn project_hint(target_dir: &Path, schema: &Schema) -> Option<String> {
    if has_project_docs(target_dir, schema) {
        None
    } else {
        Some(format!(
            "No HarnessKit docs or entrypoints found at {:?}; run `harnesskit init --preview` in that directory.",
            target_dir
        ))
    }
}

fn has_project_docs(target_dir: &Path, schema: &Schema) -> bool {
    target_dir.join(&schema.managed_root).is_dir()
        || [
            &schema.entrypoints.agents,
            &schema.entrypoints.claude,
            &schema.entrypoints.architecture,
            &schema.entrypoints.manifest,
            &schema.entrypoints.project_map,
        ]
        .into_iter()
        .flatten()
        .any(|path| target_dir.join(path).is_file())
}

pub(crate) fn orphan_state_hints(target_dir: &Path, schema: &Schema) -> Result<Vec<String>> {
    let default_schema = parse_schema(BUNDLED_SCHEMA_TEXT)?;
    let mut pending = vec![target_dir.to_path_buf()];
    let mut orphan_paths = Vec::new();
    while let Some(dir) = pending.pop() {
        let state_root = dir.join(".harnesskit");
        if state_root.is_dir() {
            // A nested project may use a different docs root and entrypoints.
            let local_schema = if dir != target_dir {
                fs::read_to_string(state_root.join("schema.yaml"))
                    .ok()
                    .and_then(|text| parse_schema(&text).ok())
            } else {
                None
            };
            let candidate_schema = if dir == target_dir {
                schema
            } else {
                local_schema.as_ref().unwrap_or(&default_schema)
            };
            if !has_project_docs(&dir, candidate_schema) {
                orphan_paths.push(normalize_path(state_root.strip_prefix(target_dir)?));
            }
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Never follow symlinks or inspect VCS/state, hidden, build or dependency trees.
            if entry.file_type()?.is_dir()
                && !name.starts_with('.')
                && !matches!(name.as_ref(), "target" | "node_modules")
            {
                pending.push(entry.path());
            }
        }
    }
    orphan_paths.sort();
    Ok(orphan_paths
        .into_iter()
        .map(|path| {
            format!(
                "Possible orphan HarnessKit state at {:?}: its directory has no configured docs root or entrypoints. Verify target_dir and rerun with the intended project root; preserve .harnesskit/history before any cleanup.",
                path
            )
        })
        .collect())
}

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
